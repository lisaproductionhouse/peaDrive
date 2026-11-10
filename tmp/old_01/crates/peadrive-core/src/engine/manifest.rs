use std::path::{Path, PathBuf};

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};

use super::plan::bitmap_to_flags;
use crate::config::{load_json, save_json};
use crate::drive::DriveFile;

/// File sidecar `<tên>.peadrive` cạnh `<tên>.part`: cho phép tiếp tục tải từ chunk đã ghi xong.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub file_id: String,
    pub name: String,
    pub size: u64,
    pub chunk_size: u64,
    pub md5: Option<String>,
    pub modified: Option<String>,
    /// Bitmap chunk đã xong, base64.
    pub done: String,
    /// Bản copy dự phòng trong Drive cá nhân (giữ lại khi tạm dừng để resume không phải copy lại).
    #[serde(default)]
    pub copy_id: Option<String>,
}

fn with_suffix(dest: &Path, suffix: &str) -> PathBuf {
    let mut s = dest.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

impl Manifest {
    pub fn new(file: &DriveFile, chunk_size: u64, chunks: usize) -> Self {
        Self {
            version: 1,
            file_id: file.id.clone(),
            name: file.name.clone(),
            size: file.size.unwrap_or(0),
            chunk_size,
            md5: file.md5_checksum.clone(),
            modified: file.modified_time.clone(),
            done: STANDARD.encode(vec![0u8; chunks.div_ceil(8)]),
            copy_id: None,
        }
    }

    pub fn part_path(dest: &Path) -> PathBuf {
        with_suffix(dest, ".part")
    }

    pub fn path_for(dest: &Path) -> PathBuf {
        with_suffix(dest, ".peadrive")
    }

    pub fn load(path: &Path) -> Option<Self> {
        load_json(path)
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        save_json(path, self)
    }

    /// Manifest còn dùng được cho đúng phiên bản file này trên Drive?
    pub fn is_for(&self, file: &DriveFile) -> bool {
        self.version == 1
            && self.file_id == file.id
            && Some(self.size) == file.size
            && self.md5 == file.md5_checksum
            && self.modified == file.modified_time
    }

    pub fn done_flags(&self, chunks: usize) -> Vec<bool> {
        bitmap_to_flags(&STANDARD.decode(&self.done).unwrap_or_default(), chunks)
    }

    pub fn set_done(&mut self, bitmap: &[u8]) {
        self.done = STANDARD.encode(bitmap);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file() -> DriveFile {
        DriveFile { id: "id1".into(), name: "a.bin".into(), size: Some(100), md5_checksum: Some("ab".into()), ..Default::default() }
    }

    #[test]
    fn roundtrip_and_match() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("a.bin");
        let mp = Manifest::path_for(&dest);
        assert!(mp.to_string_lossy().ends_with("a.bin.peadrive"));
        assert!(Manifest::part_path(&dest).to_string_lossy().ends_with("a.bin.part"));

        let mut m = Manifest::new(&file(), 40, 3);
        m.set_done(&[0b101]);
        m.save(&mp).unwrap();
        let back = Manifest::load(&mp).unwrap();
        assert_eq!(back.done_flags(3), vec![true, false, true]);
        assert!(back.is_for(&file()));

        let mut changed = file();
        changed.md5_checksum = Some("cd".into());
        assert!(!back.is_for(&changed));
    }
}
