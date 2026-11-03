//! Hàng đợi tải lên: các tác vụ tải lên (do Smart Upload và Tải lên thủ công tạo) chạy song song tới `parallel_files`, có tạm dừng /
//! tiếp tục / huỷ / thử lại như hàng đợi tải xuống và hiện chung trong danh sách "Hàng đợi" của giao diện
//! (`direction = "upload"`).
//!
//! Khác tải xuống: tác vụ tải lên KHÔNG được lưu qua lần mở app sau. Smart Upload quét lại thư mục và bỏ qua file đã
//! có trên Drive (xem `smart::decide`), nên không bao giờ tải trùng.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use tokio::runtime::Handle;
use tokio::time::MissedTickBehavior;

use super::manager::{Hold, QueueItem};
use super::manual::{self, UploadRequest, UploadStarted};
use super::smart::{self, SmartRequest, SmartStarted, Watch, WatchInfo};
use super::state::{Control, JobId, JobStatus, Phase, Progress};
use super::upload::{self, UpOutcome, UpRun, UpSession, UpSpec};
use crate::config::Settings;
use crate::drive::ClientPool;
use crate::error::{Error, Result};

/// Smart Upload: tác vụ lỗi tự được xếp hàng lại sau khoảng này (mất mạng, Drive đầy rồi được dọn…).
const RETRY_AFTER: Duration = Duration::from_secs(300);
/// Chu kỳ quét thư mục mặc định của Smart Upload.
pub const DEFAULT_SCAN_MS: u64 = 10_000;
/// Giữ tối đa chừng này tác vụ đã xong trong danh sách (cũ nhất bị bỏ trước).
const KEEP_FINISHED: usize = 300;

struct Meter {
    at: Instant,
    bytes: u64,
    ema: f64,
}

impl Meter {
    fn new() -> Self {
        Self { at: Instant::now(), bytes: 0, ema: 0.0 }
    }

    fn update(&mut self, bytes: u64) -> f64 {
        let now = Instant::now();
        let dt = now.duration_since(self.at).as_secs_f64();
        if dt >= 0.2 {
            let inst = bytes.saturating_sub(self.bytes) as f64 / dt;
            self.ema = if self.ema == 0.0 { inst } else { 0.3 * inst + 0.7 * self.ema };
            self.at = now;
            self.bytes = bytes;
        }
        self.ema
    }
}

struct UpState {
    status: JobStatus,
    error: Option<String>,
    ctl: Option<Arc<Control>>,
    progress: Arc<Progress>,
    meter: Meter,
    hold: Hold,
    /// Lý do sẽ ghi vào `hold` khi tác vụ đang chạy dừng lại do pause.
    pending_hold: Hold,
    failed_at: Option<Instant>,
}

struct UpJob {
    id: JobId,
    /// Tài khoản Google sở hữu tác vụ: luôn chạy bằng token của tài khoản này.
    account: String,
    spec: UpSpec,
    /// Nhãn đích hiển thị trong hàng đợi ("Drive của tôi › Đích › Thư mục nguồn").
    label: String,
    /// Dung lượng file nguồn lúc xếp hàng (để vẽ thanh tiến độ).
    size: u64,
    /// Watcher Smart Upload đã tạo tác vụ này.
    watch: Option<Arc<Watch>>,
    st: Mutex<UpState>,
    session: Mutex<UpSession>,
}

impl UpJob {
    fn status(&self) -> JobStatus {
        self.st.lock().unwrap().status
    }

    fn item(&self) -> QueueItem {
        let mut st = self.st.lock().unwrap();
        let running = st.status == JobStatus::Running;
        let done = match st.status {
            JobStatus::Completed | JobStatus::Skipped => self.size,
            _ => st.progress.downloaded.load(Ordering::Relaxed).min(self.size),
        };
        let speed = (if running { st.meter.update(done) } else { 0.0 }) as u64;
        let phase = st.progress.phase();
        // Giao diện hiển thị `error_msg` ngay sau trạng thái: dùng cho lỗi và cho pha đặc biệt (chuẩn bị, hoàn tất).
        let error_msg = match st.status {
            JobStatus::Failed => st.error.clone().unwrap_or_default(),
            JobStatus::Running if phase != Phase::Downloading => phase.label().to_string(),
            _ => String::new(),
        };
        QueueItem {
            id: self.id.to_string(),
            status: ui_status(st.status, st.hold),
            direction: "upload",
            name: self.spec.name.clone(),
            source: self.spec.src.to_string_lossy().into_owned(),
            destination: self.label.clone(),
            done_bytes: done,
            total_bytes: self.size,
            speed,
            error_msg,
        }
    }
}

fn ui_status(status: JobStatus, hold: Hold) -> &'static str {
    match (status, hold) {
        (JobStatus::Queued, _) => "waiting",
        (JobStatus::Running, _) => "working",
        (JobStatus::Paused, Hold::Restored) => "restored",
        (JobStatus::Paused, Hold::Held) => "held",
        (JobStatus::Paused, _) => "paused",
        (JobStatus::Completed, _) => "completed",
        (JobStatus::Skipped, _) => "skipped",
        (JobStatus::Failed, _) => "failed",
        (JobStatus::Cancelled, _) => "cancelled",
    }
}

fn is_finished(s: JobStatus) -> bool {
    matches!(s, JobStatus::Completed | JobStatus::Skipped | JobStatus::Cancelled)
}

pub(super) struct UpInner {
    pub(super) pool: Arc<ClientPool>,
    pub(super) rt: Handle,
    /// Dùng chung bộ đếm ID với hàng đợi tải xuống: `cancel(id)` / `resume(id)` không bao giờ nhầm tác vụ.
    pub(super) next_id: Arc<AtomicU64>,
    settings: RwLock<Settings>,
    jobs: Mutex<Vec<Arc<UpJob>>>,
    pub(super) watches: Mutex<Vec<Arc<Watch>>>,
    /// "Tạm dừng tất cả": không khởi động tác vụ mới, watcher không quét.
    pub(super) paused: AtomicBool,
    /// Chu kỳ quét thư mục (ms).
    pub(super) scan_ms: AtomicU64,
}

#[derive(Clone)]
pub struct Uploader {
    inner: Arc<UpInner>,
}

impl Uploader {
    /// Phải gọi trong ngữ cảnh Tokio runtime (tác vụ nền được spawn ở đây).
    pub fn new(pool: Arc<ClientPool>, next_id: Arc<AtomicU64>, settings: Settings) -> Uploader {
        let inner = Arc::new(UpInner {
            pool,
            rt: Handle::current(),
            next_id,
            settings: RwLock::new(settings),
            jobs: Mutex::new(Vec::new()),
            watches: Mutex::new(Vec::new()),
            paused: AtomicBool::new(false),
            scan_ms: AtomicU64::new(DEFAULT_SCAN_MS),
        });
        inner.rt.spawn(housekeeping(inner.clone()));
        Uploader { inner }
    }

    pub fn set_settings(&self, s: Settings) {
        *self.inner.settings.write().unwrap() = s;
        pump(&self.inner);
    }

    /// Chu kỳ quét thư mục của Smart Upload (mặc định 10 giây; tối thiểu 10 ms — chủ yếu cho kiểm thử).
    pub fn set_scan_interval(&self, d: Duration) {
        self.inner.scan_ms.store((d.as_millis() as u64).max(10), Ordering::Relaxed);
    }

    /// (đang chạy / chờ / tạm dừng / lỗi, đã xong / bỏ qua / huỷ) dưới dạng dòng của hàng đợi giao diện.
    pub fn queue_items(&self) -> (Vec<QueueItem>, Vec<QueueItem>) {
        let jobs = self.inner.jobs.lock().unwrap().clone();
        let (mut active, mut finished) = (Vec::new(), Vec::new());
        for j in &jobs {
            let item = j.item();
            if is_finished(j.status()) {
                finished.push(item);
            } else {
                active.push(item);
            }
        }
        (active, finished)
    }

    pub fn has_running(&self) -> bool {
        let jobs = self.inner.jobs.lock().unwrap().clone();
        jobs.iter().any(|j| j.status() == JobStatus::Running)
    }

    /// Huỷ một tác vụ tải lên. `false` nếu không có tác vụ tải lên nào mang ID này.
    pub fn cancel(&self, id: JobId) -> bool {
        match self.find(id) {
            Some(job) => {
                cancel_job(&job);
                true
            }
            None => false,
        }
    }

    /// Thử lại / tiếp tục một tác vụ (Paused hoặc Failed). `false` nếu không có tác vụ tải lên nào mang ID này.
    pub fn resume(&self, id: JobId) -> bool {
        let Some(job) = self.find(id) else { return false };
        {
            let mut st = job.st.lock().unwrap();
            if matches!(st.status, JobStatus::Paused | JobStatus::Failed) {
                st.status = JobStatus::Queued;
                st.hold = Hold::None;
                st.error = None;
                st.failed_at = None;
            }
        }
        pump(&self.inner);
        true
    }

    pub fn pause_all(&self) {
        self.inner.paused.store(true, Ordering::SeqCst);
        let jobs = self.inner.jobs.lock().unwrap().clone();
        for job in jobs {
            let mut st = job.st.lock().unwrap();
            if st.status == JobStatus::Running {
                st.pending_hold = Hold::Global;
                if let Some(c) = &st.ctl {
                    c.pause();
                }
            }
        }
    }

    pub fn resume_all(&self) {
        self.inner.paused.store(false, Ordering::SeqCst);
        let jobs = self.inner.jobs.lock().unwrap().clone();
        for job in jobs {
            let mut st = job.st.lock().unwrap();
            if st.status == JobStatus::Paused && st.hold == Hold::Global {
                st.status = JobStatus::Queued;
                st.hold = Hold::None;
            }
        }
        pump(&self.inner);
    }

    /// Huỷ mọi tác vụ đang chờ / đang chạy / tạm dừng (tác vụ lỗi giữ nguyên để thử lại).
    pub fn cancel_all(&self) {
        let jobs = self.inner.jobs.lock().unwrap().clone();
        for job in jobs {
            if matches!(job.status(), JobStatus::Queued | JobStatus::Running | JobStatus::Paused) {
                cancel_job(&job);
            }
        }
    }

    /// Xoá khỏi danh sách các tác vụ đã xong / bỏ qua / đã huỷ. Trả về ID đã xoá.
    pub fn clear_finished(&self) -> Vec<JobId> {
        let mut removed = Vec::new();
        self.inner.jobs.lock().unwrap().retain(|j| {
            let done = is_finished(j.status());
            if done {
                removed.push(j.id);
            }
            !done
        });
        removed
    }

    /// Gỡ tài khoản: dừng watcher của tài khoản đó và tạm dừng tác vụ của nó (giữ nguyên để chạy tiếp sau khi đăng nhập lại).
    pub fn pause_account(&self, account: &str) {
        smart::stop_account(&self.inner, account);
        let jobs = self.inner.jobs.lock().unwrap().clone();
        for job in jobs.iter().filter(|j| j.account == account) {
            let mut st = job.st.lock().unwrap();
            match st.status {
                JobStatus::Running => {
                    st.pending_hold = Hold::User;
                    if let Some(c) = &st.ctl {
                        c.pause();
                    }
                }
                JobStatus::Queued => {
                    st.status = JobStatus::Paused;
                    st.hold = Hold::User;
                }
                _ => {}
            }
        }
    }

    // ───────────────────────── Smart Upload ─────────────────────────

    /// Bắt đầu theo dõi các thư mục trên máy và tự tải file mới lên Drive. Xem `smart`.
    pub async fn smart_start(&self, req: SmartRequest) -> Result<SmartStarted> {
        smart::start(&self.inner, req).await
    }

    pub fn smart_list(&self) -> Vec<WatchInfo> {
        smart::list(&self.inner)
    }

    /// Dừng một watcher (`Some(id)`) hoặc tất cả (`None`). Tác vụ đã vào hàng đợi vẫn chạy tiếp. Trả về số watcher đã dừng.
    pub fn smart_stop(&self, id: Option<JobId>) -> usize {
        smart::stop(&self.inner, id)
    }

    pub fn smart_count(&self) -> usize {
        self.inner.watches.lock().unwrap().len()
    }

    // ───────────────────────── Tải lên thủ công ─────────────────────────

    /// Tải lên các file/thư mục đã chọn trên máy (một lần, không theo dõi). Xem `manual`.
    pub async fn upload_start(&self, req: UploadRequest) -> Result<UploadStarted> {
        manual::start(&self.inner, req).await
    }

    fn find(&self, id: JobId) -> Option<Arc<UpJob>> {
        self.inner.jobs.lock().unwrap().iter().find(|j| j.id == id).cloned()
    }
}

/// Xếp một tác vụ tải lên vào hàng đợi (chạy ngay nếu còn chỗ). Trả về ID tác vụ.
pub(super) fn enqueue(inner: &Arc<UpInner>, account: &str, spec: UpSpec, label: String, size: u64, watch: Option<Arc<Watch>>) -> JobId {
    let id = inner.next_id.fetch_add(1, Ordering::Relaxed);
    let job = Arc::new(UpJob {
        id,
        account: account.to_string(),
        spec,
        label,
        size,
        watch,
        st: Mutex::new(UpState {
            status: JobStatus::Queued,
            error: None,
            ctl: None,
            progress: Arc::new(Progress::default()),
            meter: Meter::new(),
            hold: Hold::None,
            pending_hold: Hold::Global,
            failed_at: None,
        }),
        session: Mutex::new(UpSession::default()),
    });
    {
        let mut jobs = inner.jobs.lock().unwrap();
        jobs.push(job);
        prune(&mut jobs);
    }
    pump(inner);
    id
}

/// Bỏ bớt các tác vụ đã xong cũ nhất khi danh sách quá dài (Smart Upload chạy nhiều ngày).
fn prune(jobs: &mut Vec<Arc<UpJob>>) {
    let finished = jobs.iter().filter(|j| is_finished(j.status())).count();
    if finished <= KEEP_FINISHED + 100 {
        return;
    }
    let mut drop_n = finished - KEEP_FINISHED;
    jobs.retain(|j| {
        if drop_n > 0 && is_finished(j.status()) {
            drop_n -= 1;
            false
        } else {
            true
        }
    });
}

/// Chạy các tác vụ Queued cho tới khi đủ `parallel_files` tác vụ đang chạy.
fn pump(inner: &Arc<UpInner>) {
    if inner.paused.load(Ordering::SeqCst) {
        return;
    }
    let limit = inner.settings.read().unwrap().parallel_files;
    let jobs = inner.jobs.lock().unwrap().clone();
    let mut running = jobs.iter().filter(|j| j.status() == JobStatus::Running).count();
    for job in jobs {
        if running >= limit {
            break;
        }
        let started = {
            let mut st = job.st.lock().unwrap();
            if st.status != JobStatus::Queued {
                None
            } else {
                let (ctl, progress) = (Control::root(), Arc::new(Progress::default()));
                st.status = JobStatus::Running;
                st.error = None;
                st.ctl = Some(ctl.clone());
                st.progress = progress.clone();
                st.meter = Meter::new();
                Some((ctl, progress))
            }
        };
        if let Some((ctl, progress)) = started {
            running += 1;
            inner.rt.spawn(run_job(inner.clone(), job, ctl, progress));
        }
    }
}

fn cancel_job(job: &Arc<UpJob>) {
    let mut st = job.st.lock().unwrap();
    match st.status {
        JobStatus::Running => {
            if let Some(c) = &st.ctl {
                c.cancel();
            }
        }
        JobStatus::Queued | JobStatus::Paused | JobStatus::Failed => {
            st.status = JobStatus::Cancelled;
        }
        _ => {}
    }
}

async fn run_job(inner: Arc<UpInner>, job: Arc<UpJob>, ctl: Arc<Control>, progress: Arc<Progress>) {
    let settings = inner.settings.read().unwrap().clone();
    // Luôn chạy bằng client (token) của tài khoản đã tạo tác vụ, không phụ thuộc tài khoản đang chọn trên giao diện.
    let res = match inner.pool.client_for(&job.account).await {
        Ok((_, client)) => upload::upload(&UpRun { client, ctl, progress, settings }, &job.spec, &job.session).await,
        Err(e) => Err(e),
    };
    {
        let mut st = job.st.lock().unwrap();
        st.ctl = None;
        match res {
            Ok(UpOutcome::Uploaded(_)) => {
                st.status = JobStatus::Completed;
                st.error = None;
                if let Some(w) = &job.watch {
                    w.on_uploaded();
                }
            }
            // Drive đã có đúng phiên bản này (do tác vụ khác tải xong trước, hoặc lần chạy trước): không gửi lại.
            Ok(UpOutcome::Existing(_)) => {
                st.status = JobStatus::Skipped;
                st.error = None;
                if let Some(w) = &job.watch {
                    w.add_skipped();
                }
            }
            Err(Error::Paused) => {
                st.status = JobStatus::Paused;
                st.hold = st.pending_hold;
            }
            Err(Error::Cancelled) => {
                st.status = JobStatus::Cancelled;
            }
            Err(e) => {
                st.status = JobStatus::Failed;
                st.error = Some(e.to_string());
                st.failed_at = Some(Instant::now());
            }
        }
    }
    pump(&inner);
}

/// Nền: xếp hàng lại các tác vụ Smart Upload bị lỗi sau `RETRY_AFTER` (khi watcher của chúng còn chạy).
async fn housekeeping(inner: Arc<UpInner>) {
    let mut iv = tokio::time::interval(Duration::from_secs(5));
    iv.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        iv.tick().await;
        if inner.paused.load(Ordering::SeqCst) {
            continue;
        }
        let jobs = inner.jobs.lock().unwrap().clone();
        let mut requeued = false;
        for job in jobs {
            let Some(w) = &job.watch else { continue };
            if w.is_stopped() {
                continue;
            }
            let mut st = job.st.lock().unwrap();
            if st.status == JobStatus::Failed && st.failed_at.is_some_and(|t| t.elapsed() >= RETRY_AFTER) {
                st.status = JobStatus::Queued;
                st.error = None;
                st.failed_at = None;
                requeued = true;
            }
        }
        if requeued {
            pump(&inner);
        }
    }
}
