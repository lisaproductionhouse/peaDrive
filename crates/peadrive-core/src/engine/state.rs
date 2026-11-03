use std::sync::atomic::{AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

pub type JobId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Queued,
    Running,
    Paused,
    Completed,
    Failed,
    Cancelled,
    Skipped,
}

const RUN: u8 = 0;
const PAUSE: u8 = 1;
const CANCEL: u8 = 2;
const FAIL: u8 = 3;

/// Cờ điều khiển dùng chung giữa manager, worker và writer. Có phân cấp: phiên tải (session)
/// là con của job, nên lỗi của một phiên không làm hỏng điều khiển cấp job (cần cho copy dự phòng).
pub struct Control {
    state: AtomicU8,
    msg: Mutex<String>,
    parent: Option<Arc<Control>>,
}

impl Control {
    pub fn root() -> Arc<Self> {
        Arc::new(Self { state: AtomicU8::new(RUN), msg: Mutex::new(String::new()), parent: None })
    }

    pub fn child(parent: &Arc<Control>) -> Arc<Self> {
        Arc::new(Self { state: AtomicU8::new(RUN), msg: Mutex::new(String::new()), parent: Some(parent.clone()) })
    }

    pub fn pause(&self) {
        let _ = self.state.compare_exchange(RUN, PAUSE, Ordering::AcqRel, Ordering::Acquire);
    }

    pub fn cancel(&self) {
        self.state.store(CANCEL, Ordering::Release);
    }

    /// Đánh dấu lỗi nặng (ghi đĩa hỏng, worker gặp lỗi không thể thử lại…) → mọi tác vụ trong phiên dừng.
    pub fn fail(&self, msg: impl Into<String>) {
        if self.state.compare_exchange(RUN, FAIL, Ordering::AcqRel, Ordering::Acquire).is_ok() {
            *self.msg.lock().unwrap() = msg.into();
        }
    }

    /// `Ok` nếu được phép chạy tiếp.
    pub fn check(&self) -> Result<()> {
        if let Some(p) = &self.parent {
            p.check()?;
        }
        match self.state.load(Ordering::Acquire) {
            RUN => Ok(()),
            PAUSE => Err(Error::Paused),
            CANCEL => Err(Error::Cancelled),
            _ => Err(Error::Other(self.msg.lock().unwrap().clone())),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Phase {
    Preparing = 0,
    Downloading = 1,
    CopyingToDrive = 2,
    Verifying = 3,
    Finalizing = 4,
}

impl Phase {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Phase::Downloading,
            2 => Phase::CopyingToDrive,
            3 => Phase::Verifying,
            4 => Phase::Finalizing,
            _ => Phase::Preparing,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Phase::Preparing => "Đang chuẩn bị",
            Phase::Downloading => "Đang tải",
            Phase::CopyingToDrive => "Hết quota tải — đang copy vào Drive cá nhân",
            Phase::Verifying => "Đang kiểm tra MD5",
            Phase::Finalizing => "Đang hoàn tất",
        }
    }
}

/// Bộ đếm dùng chung, đọc không khoá bởi UI.
#[derive(Default)]
pub struct Progress {
    /// Byte đã nhận từ mạng và đưa vào ring buffer.
    pub downloaded: AtomicU64,
    /// Byte đã ghi xuống đĩa.
    pub written: AtomicU64,
    pub chunks_done: AtomicUsize,
    pub chunks_total: AtomicUsize,
    pub workers: AtomicUsize,
    pub rate_limit_hits: AtomicU64,
    phase: AtomicU8,
}

impl Progress {
    pub fn set_phase(&self, p: Phase) {
        self.phase.store(p as u8, Ordering::Relaxed);
    }

    pub fn phase(&self) -> Phase {
        Phase::from_u8(self.phase.load(Ordering::Relaxed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_hierarchy() {
        let root = Control::root();
        let child = Control::child(&root);
        assert!(child.check().is_ok());
        child.fail("boom");
        assert!(matches!(child.check(), Err(Error::Other(m)) if m == "boom"));
        assert!(root.check().is_ok(), "lỗi của phiên không lan lên job");
        let child2 = Control::child(&root);
        root.pause();
        assert!(matches!(child2.check(), Err(Error::Paused)));
        root.cancel();
        assert!(matches!(child2.check(), Err(Error::Cancelled)));
    }
}
