use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

/// Làm sạch tên file cho Windows + macOS: bỏ ký tự cấm, tên thiết bị DOS, dấu chấm/khoảng trắng cuối.
pub fn sanitize_filename(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') { '_' } else { c })
        .collect();
    s = s.trim().trim_end_matches(['.', ' ']).to_string();
    if s.is_empty() {
        s = "unnamed".to_string();
    }
    let stem = s.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4 && (stem.starts_with("COM") || stem.starts_with("LPT")) && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        s.insert(0, '_');
    }
    if s.chars().count() > 200 {
        // cắt theo ký tự nhưng giữ phần mở rộng
        let ext = Path::new(&s).extension().and_then(|e| e.to_str()).map(|e| format!(".{e}")).unwrap_or_default();
        let keep = 200usize.saturating_sub(ext.chars().count());
        s = s.chars().take(keep).collect::<String>() + &ext;
    }
    s
}

/// `name.ext` → `name (1).ext`, `name (2).ext`, … cho tới khi `taken` trả `false`.
pub fn unique_path(path: &Path, taken: impl Fn(&Path) -> bool) -> PathBuf {
    if !taken(path) {
        return path.to_path_buf();
    }
    let dir = path.parent().unwrap_or_else(|| Path::new(""));
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    let ext = path.extension().and_then(|s| s.to_str());
    for n in 1u32.. {
        let name = match ext {
            Some(e) => format!("{stem} ({n}).{e}"),
            None => format!("{stem} ({n})"),
        };
        let cand = dir.join(name);
        if !taken(&cand) {
            return cand;
        }
    }
    unreachable!()
}

/// Ghi toàn bộ `buf` tại `offset` mà không dịch con trỏ file dùng chung (pwrite / WriteFile+OVERLAPPED).
#[cfg(unix)]
pub fn write_all_at(file: &File, buf: &[u8], offset: u64) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.write_all_at(buf, offset)
}

#[cfg(windows)]
pub fn write_all_at(file: &File, mut buf: &[u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let n = file.seek_write(buf, offset)?;
        if n == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        buf = &buf[n..];
        offset += n as u64;
    }
    Ok(())
}

/// Mở trình quản lý file của OS và chọn sẵn `path` (Explorer / Finder); Linux mở thư mục cha.
pub fn reveal_in_file_manager(path: &Path) -> io::Result<()> {
    use std::process::Command;
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        Command::new("explorer").raw_arg(format!("/select,\"{}\"", path.display())).spawn()?;
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg("-R").arg(path).spawn()?;
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        Command::new("xdg-open").arg(path.parent().unwrap_or(path)).spawn()?;
    }
    Ok(())
}

pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

pub fn now_unix() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Mili-giây kể từ UNIX epoch (đồng hồ hệ thống).
pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

pub fn human_bytes(n: u64) -> String {
    const U: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize() {
        assert_eq!(sanitize_filename("a/b:c*.txt"), "a_b_c_.txt");
        assert_eq!(sanitize_filename("  báo cáo.pdf. "), "báo cáo.pdf");
        assert_eq!(sanitize_filename("CON.txt"), "_CON.txt");
        assert_eq!(sanitize_filename("com1"), "_com1");
        assert_eq!(sanitize_filename("..."), "unnamed");
        let long = format!("{}.zip", "x".repeat(300));
        let out = sanitize_filename(&long);
        assert_eq!(out.chars().count(), 200);
        assert!(out.ends_with(".zip"));
    }

    #[test]
    fn unique() {
        let taken = |p: &Path| matches!(p.file_name().and_then(|s| s.to_str()), Some("a.zip") | Some("a (1).zip"));
        assert_eq!(unique_path(Path::new("/d/a.zip"), taken), PathBuf::from("/d/a (2).zip"));
        assert_eq!(unique_path(Path::new("/d/b.zip"), taken), PathBuf::from("/d/b.zip"));
    }

    #[test]
    fn positional_write() {
        let dir = tempfile::tempdir().unwrap();
        let f = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(dir.path().join("x")).unwrap();
        f.set_len(10).unwrap();
        write_all_at(&f, b"world", 5).unwrap();
        write_all_at(&f, b"hello", 0).unwrap();
        assert_eq!(std::fs::read(dir.path().join("x")).unwrap(), b"helloworld");
    }
}
