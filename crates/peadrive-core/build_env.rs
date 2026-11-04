//! Bộ đọc file `.env.<flavor>` (KEY=VALUE) cho `build.rs`.
//!
//! File này được nạp hai nơi: `build.rs` (để nhúng OAuth client lúc build) và `config.rs` dưới `cfg(test)` (qua
//! `#[path]`, để có unit test cho bộ phân tích mà không cần chạy cargo build thật). Không dùng thư viện ngoài:
//! build script không nên kéo thêm phụ thuộc.

/// Phân tích nội dung một file `.env`:
/// - mỗi dòng `KEY=VALUE`, có thể có tiền tố `export `; dòng trống và dòng bắt đầu bằng `#` bị bỏ;
/// - bỏ BOM UTF-8 ở đầu file (Notepad / PowerShell hay thêm), chấp nhận cả CRLF;
/// - giá trị có thể nằm trong cặp nháy đơn hoặc nháy kép; giá trị không nháy thì bỏ phần chú thích cuối dòng ` # ...`.
pub fn parse(text: &str) -> Vec<(String, String)> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").map(str::trim_start).unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else { continue };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        out.push((key.to_string(), unquote(value.trim()).to_string()));
    }
    out
}

/// Giá trị của `key`; khoá xuất hiện nhiều lần thì lấy lần cuối (giống `source`). `None` nếu không có.
pub fn get<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

fn unquote(v: &str) -> &str {
    for q in ['"', '\''] {
        if v.len() >= 2 && v.starts_with(q) && v.ends_with(q) {
            return &v[1..v.len() - 1];
        }
    }
    match v.find(" #") {
        Some(i) => v[..i].trim_end(),
        None => v,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(text: &str, key: &str) -> Option<String> {
        get(&parse(text), key).map(str::to_string)
    }

    #[test]
    fn reads_plain_pairs_and_ignores_comments_and_blank_lines() {
        let t = "# Client của bản public\n\nPEADRIVE_CLIENT_ID=abc.apps.googleusercontent.com\n  PEADRIVE_CLIENT_SECRET = sec-123  \n";
        assert_eq!(value(t, "PEADRIVE_CLIENT_ID").as_deref(), Some("abc.apps.googleusercontent.com"));
        assert_eq!(value(t, "PEADRIVE_CLIENT_SECRET").as_deref(), Some("sec-123"));
        assert_eq!(value(t, "KHONG_CO"), None);
    }

    #[test]
    fn survives_windows_editors_crlf_and_bom() {
        let t = "\u{feff}PEADRIVE_CLIENT_ID=id1\r\nPEADRIVE_CLIENT_SECRET=sec1\r\n";
        assert_eq!(value(t, "PEADRIVE_CLIENT_ID").as_deref(), Some("id1"), "BOM không được dính vào tên khoá đầu tiên");
        assert_eq!(value(t, "PEADRIVE_CLIENT_SECRET").as_deref(), Some("sec1"), "CRLF không được dính vào giá trị");
    }

    #[test]
    fn accepts_export_prefix_and_quotes() {
        let t = "export PEADRIVE_CLIENT_ID=\"quoted id\"\nPEADRIVE_CLIENT_SECRET='single'\n";
        assert_eq!(value(t, "PEADRIVE_CLIENT_ID").as_deref(), Some("quoted id"));
        assert_eq!(value(t, "PEADRIVE_CLIENT_SECRET").as_deref(), Some("single"));
    }

    #[test]
    fn strips_trailing_comment_only_when_unquoted() {
        let t = "A=value # chú thích\nB=\"keep # inside\"\nC=no#space\n";
        assert_eq!(value(t, "A").as_deref(), Some("value"));
        assert_eq!(value(t, "B").as_deref(), Some("keep # inside"));
        assert_eq!(value(t, "C").as_deref(), Some("no#space"));
    }

    #[test]
    fn blank_value_is_empty_and_last_duplicate_wins() {
        let t = "PEADRIVE_CLIENT_ID=\nX=1\nPEADRIVE_CLIENT_ID=second\nPEADRIVE_CLIENT_SECRET=\n";
        assert_eq!(value(t, "PEADRIVE_CLIENT_ID").as_deref(), Some("second"));
        assert_eq!(value(t, "PEADRIVE_CLIENT_SECRET").as_deref(), Some(""), "giá trị để trống = chưa điền");
        assert_eq!(value("PEADRIVE_CLIENT_ID=\n", "PEADRIVE_CLIENT_ID").as_deref(), Some(""));
    }

    #[test]
    fn lines_without_equals_are_ignored() {
        assert_eq!(parse("khong co dau bang\n=orphan\n"), Vec::<(String, String)>::new());
    }
}
