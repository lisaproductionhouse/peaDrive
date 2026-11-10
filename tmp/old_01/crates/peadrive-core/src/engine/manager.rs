//! Manager: hàng đợi job, giới hạn số file chạy đồng thời, pause/resume/cancel, phát snapshot cho UI và
//! lưu `jobs.json` để khôi phục phiên sau khi tắt app (job khôi phục ở trạng thái Paused; manifest giữ tiến độ chunk).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio::time::MissedTickBehavior;

use super::job::{self, JobRequest, RunCtx};
use super::manifest::Manifest;
use super::manual::{UploadRequest, UploadStarted};
use super::plan::Plan;
use super::smart::{SmartRequest, SmartStarted, WatchInfo};
use super::state::{Control, JobId, JobStatus, Phase, Progress};
use super::uploader::Uploader;
use crate::config::{load_json, save_json, Settings};
use crate::drive::model::export_target;
use crate::drive::{parse_drive_input, ClientPool, DriveClient, DriveFile, DriveRef};
use crate::error::{Error, Result};
use crate::util::{sanitize_filename, unique_path};

/// Vì sao một job đang ở trạng thái Paused (UI phân biệt "tạm dừng", "chưa xong từ lần trước", "để sau").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Hold {
    None,
    /// Khôi phục từ phiên trước, đang chờ người dùng quyết định.
    Restored,
    /// Người dùng chọn "Để sau".
    Held,
    /// Tạm dừng riêng từng job.
    User,
    /// Do "Tạm dừng tất cả" (hoặc thoát app).
    Global,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobSnapshot {
    pub id: JobId,
    pub file_id: String,
    pub account: String,
    pub hold: Hold,
    pub name: String,
    pub dest: String,
    pub size: u64,
    pub status: JobStatus,
    /// Pha hiện tại khi đang chạy ("Đang tải", "Đang copy vào Drive cá nhân"…).
    pub phase: String,
    /// Byte đã nhận từ mạng (đã vào ring buffer).
    pub downloaded: u64,
    /// Byte đã ghi xuống đĩa.
    pub written: u64,
    pub speed: u64,
    pub eta_secs: Option<u64>,
    pub workers: usize,
    pub chunks_done: usize,
    pub chunks_total: usize,
    pub rate_limited: u64,
    pub error: Option<String>,
    pub note: Option<String>,
}

/// Một dòng của hàng đợi theo đúng định dạng giao diện mong đợi (snake_case).
#[derive(Debug, Clone, Serialize)]
pub struct QueueItem {
    pub id: String,
    /// waiting | working | paused | restored | held | completed | skipped | failed | cancelled
    pub status: &'static str,
    pub direction: &'static str,
    pub name: String,
    pub source: String,
    pub destination: String,
    pub done_bytes: u64,
    pub total_bytes: u64,
    pub speed: u64,
    pub error_msg: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct QueueView {
    pub paused: bool,
    pub items: Vec<QueueItem>,
    /// Số thư mục Smart Upload đang được theo dõi (giao diện hiện nút quản lý khi > 0).
    pub smart: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionInfo {
    pub restored: usize,
    pub bytes_left: u64,
    pub auto_resume: bool,
}

impl JobSnapshot {
    pub fn ui_status(&self) -> &'static str {
        match (self.status, self.hold) {
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

    pub fn to_queue_item(&self) -> QueueItem {
        // Giao diện hiển thị `error_msg` ngay sau trạng thái: dùng luôn để báo pha đặc biệt (copy dự phòng, kiểm tra MD5…).
        let error_msg = match self.status {
            JobStatus::Failed => self.error.clone().unwrap_or_default(),
            JobStatus::Running if !self.phase.is_empty() && self.phase != Phase::Downloading.label() => self.phase.clone(),
            _ => String::new(),
        };
        QueueItem {
            id: self.id.to_string(),
            status: self.ui_status(),
            direction: "download",
            name: self.name.clone(),
            source: format!("Google Drive · {}", if self.account.is_empty() { &self.file_id } else { &self.account }),
            destination: self.dest.clone(),
            done_bytes: self.downloaded,
            total_bytes: self.size,
            speed: self.speed,
            error_msg,
        }
    }
}

fn manifest_done_bytes(dest: &Path, file: &DriveFile) -> u64 {
    Manifest::load(&Manifest::path_for(dest))
        .filter(|m| m.is_for(file))
        .map(|m| {
            let plan = Plan::new(m.size, m.chunk_size);
            m.done_flags(plan.chunks).iter().enumerate().filter(|(_, d)| **d).map(|(i, _)| plan.len(i)).sum()
        })
        .unwrap_or(0)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnqueueResult {
    pub jobs: Vec<JobSnapshot>,
    pub warnings: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct JobSpec {
    id: JobId,
    file: DriveFile,
    dest: PathBuf,
    /// Tài khoản Google đã tạo tác vụ (rỗng = tác vụ cũ: dùng tài khoản đang chọn).
    #[serde(default)]
    account: String,
}

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

struct JobState {
    status: JobStatus,
    error: Option<String>,
    note: Option<String>,
    ctl: Option<Arc<Control>>,
    progress: Arc<Progress>,
    meter: Meter,
    dirty: bool,
    hold: Hold,
    /// Lý do sẽ ghi vào `hold` khi job đang chạy dừng lại do pause().
    pending_hold: Hold,
}

struct Job {
    id: JobId,
    /// Tài khoản Google sở hữu tác vụ: tác vụ luôn chạy bằng token của tài khoản này.
    account: String,
    file: DriveFile,
    dest: Mutex<PathBuf>,
    st: Mutex<JobState>,
}

impl Job {
    fn new(id: JobId, account: String, file: DriveFile, dest: PathBuf, status: JobStatus, note: Option<String>) -> Self {
        Self {
            id,
            account,
            file,
            dest: Mutex::new(dest),
            st: Mutex::new(JobState {
                status,
                error: None,
                note,
                ctl: None,
                progress: Arc::new(Progress::default()),
                meter: Meter::new(),
                dirty: true,
                hold: Hold::None,
                pending_hold: Hold::Global,
            }),
        }
    }

    fn status(&self) -> JobStatus {
        self.st.lock().unwrap().status
    }

    fn view(&self) -> JobSnapshot {
        let mut st = self.st.lock().unwrap();
        self.build(&mut st, false)
    }

    /// Snapshot cho ticker: job đang chạy luôn có; job khác chỉ khi vừa đổi trạng thái.
    fn tick_view(&self) -> Option<JobSnapshot> {
        let mut st = self.st.lock().unwrap();
        if st.status == JobStatus::Running || st.dirty {
            st.dirty = false;
            Some(self.build(&mut st, true))
        } else {
            None
        }
    }

    fn build(&self, st: &mut JobState, tick: bool) -> JobSnapshot {
        let dest = self.dest.lock().unwrap().clone();
        let p = st.progress.clone();
        let size = self.file.size.unwrap_or(0);
        let running = st.status == JobStatus::Running;
        let (downloaded, written) = match st.status {
            JobStatus::Completed | JobStatus::Skipped => {
                let s = size.max(p.written.load(Ordering::Relaxed));
                (s, s)
            }
            _ => (p.downloaded.load(Ordering::Relaxed), p.written.load(Ordering::Relaxed)),
        };
        let speed = if running {
            if tick {
                st.meter.update(downloaded)
            } else {
                st.meter.ema
            }
        } else {
            0.0
        } as u64;
        let eta_secs = (running && speed > 0 && size > downloaded).then(|| (size - downloaded) / speed);
        JobSnapshot {
            id: self.id,
            file_id: self.file.id.clone(),
            account: self.account.clone(),
            hold: st.hold,
            name: dest.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| self.file.name.clone()),
            dest: dest.to_string_lossy().into_owned(),
            size,
            status: st.status,
            phase: if running { p.phase().label().to_string() } else { String::new() },
            downloaded,
            written,
            speed,
            eta_secs,
            workers: p.workers.load(Ordering::Relaxed),
            chunks_done: p.chunks_done.load(Ordering::Relaxed),
            chunks_total: p.chunks_total.load(Ordering::Relaxed),
            rate_limited: p.rate_limit_hits.load(Ordering::Relaxed),
            error: st.error.clone(),
            note: st.note.clone(),
        }
    }
}

struct Inner {
    pool: Arc<ClientPool>,
    rt: tokio::runtime::Handle,
    jobs_file: PathBuf,
    settings: RwLock<Settings>,
    jobs: Mutex<Vec<Arc<Job>>>,
    /// Dùng chung với `up`: tác vụ tải xuống và tải lên không bao giờ trùng ID.
    next_id: Arc<AtomicU64>,
    events: broadcast::Sender<Vec<JobSnapshot>>,
    persist_dirty: AtomicBool,
    /// "Tạm dừng tất cả": không khởi động job mới cho tới khi resume_all().
    global_paused: AtomicBool,
    /// Hàng đợi tải lên + Smart Upload.
    up: Uploader,
}

enum Planned {
    Duplicate,
    Skip(PathBuf),
    Run(PathBuf),
}

#[derive(Clone)]
pub struct Manager {
    inner: Arc<Inner>,
}

impl Manager {
    /// Phải gọi trong ngữ cảnh Tokio runtime (ticker nền được spawn ở đây).
    pub fn new(pool: Arc<ClientPool>, data_dir: PathBuf, settings: Settings) -> Manager {
        let (events, _) = broadcast::channel(64);
        let settings = settings.sanitized();
        let next_id = Arc::new(AtomicU64::new(1));
        let up = Uploader::new(pool.clone(), next_id.clone(), settings.clone());
        let inner = Arc::new(Inner {
            pool,
            rt: tokio::runtime::Handle::current(),
            jobs_file: data_dir.join("jobs.json"),
            settings: RwLock::new(settings),
            jobs: Mutex::new(Vec::new()),
            next_id,
            events,
            persist_dirty: AtomicBool::new(false),
            global_paused: AtomicBool::new(false),
            up,
        });
        inner.restore();
        inner.pump();
        inner.rt.spawn(ticker(inner.clone()));
        Manager { inner }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Vec<JobSnapshot>> {
        self.inner.events.subscribe()
    }

    pub fn settings(&self) -> Settings {
        self.inner.settings.read().unwrap().clone()
    }

    pub fn set_settings(&self, s: Settings) {
        let s = s.sanitized();
        *self.inner.settings.write().unwrap() = s.clone();
        self.inner.up.set_settings(s);
        self.inner.pump();
    }

    pub fn list(&self) -> Vec<JobSnapshot> {
        let jobs = self.inner.jobs.lock().unwrap().clone();
        jobs.iter().map(|j| j.view()).collect()
    }

    /// Phân tích link (file, thư mục, shortcut), liệt kê đệ quy thư mục, rồi xếp hàng. Lỗi từng link → `warnings`.
    pub async fn enqueue(&self, inputs: &[String], dest_dir: &Path) -> Result<EnqueueResult> {
        let (account, c) = self.inner.pool.active().await?;
        let c = &c;
        let mut leaves: Vec<(DriveFile, PathBuf)> = Vec::new();
        let mut warnings = Vec::new();
        let mut seen_folders = HashSet::new();

        for input in inputs {
            let step: Result<()> = async {
                let r = parse_drive_input(input)?;
                let meta = c.get_file(&r).await?;
                let mut stack = vec![(meta, dest_dir.to_path_buf())];
                while let Some((f, dir)) = stack.pop() {
                    let f = if f.is_shortcut() { resolve_shortcut(c, &f).await? } else { f };
                    if f.is_folder() {
                        if !seen_folders.insert(f.id.clone()) {
                            continue;
                        }
                        let sub = dir.join(sanitize_filename(&f.name));
                        let fr = DriveRef { id: f.id.clone(), resource_key: f.resource_key.clone() };
                        for child in c.list_children(&fr).await?.into_iter().rev() {
                            stack.push((child, sub.clone()));
                        }
                    } else {
                        leaves.push((f, dir));
                    }
                }
                Ok(())
            }
            .await;
            match step {
                Ok(()) => {}
                Err(e @ (Error::NotSignedIn | Error::Auth(_))) => return Err(e),
                Err(e) => warnings.push(format!("{input}: {e}")),
            }
        }

        let mut jobs = Vec::new();
        for (file, dir) in leaves {
            let mut name = sanitize_filename(&file.name);
            if file.is_google_doc() {
                match export_target(&file.mime_type) {
                    Some((_, ext)) => {
                        if !name.to_lowercase().ends_with(&format!(".{ext}")) {
                            name = format!("{name}.{ext}");
                        }
                    }
                    None => {
                        warnings.push(format!("{}: loại file Google này không thể xuất ra file", file.name));
                        continue;
                    }
                }
            }
            match self.inner.plan_destination(dir.join(name), &file) {
                Planned::Duplicate => warnings.push(format!("{}: đã có trong hàng đợi", file.name)),
                Planned::Skip(path) => {
                    let job = self.inner.add_job(&account, file, path, JobStatus::Skipped, Some("Đã có file cùng tên và dung lượng".into()));
                    jobs.push(job.view());
                }
                Planned::Run(path) => {
                    let job = self.inner.add_job(&account, file, path, JobStatus::Queued, None);
                    jobs.push(job.view());
                }
            }
        }
        self.inner.persist_dirty.store(true, Ordering::Relaxed);
        self.inner.pump();
        Ok(EnqueueResult { jobs, warnings })
    }

    /// Hàng đợi cho UI: mọi job chưa xong + tối đa 300 job đã xong gần nhất.
    pub fn queue_view(&self) -> QueueView {
        let jobs = self.inner.jobs.lock().unwrap().clone();
        let (mut items, mut finished) = (Vec::new(), Vec::new());
        for j in &jobs {
            let snap = j.view();
            let item = snap.to_queue_item();
            if matches!(snap.status, JobStatus::Completed | JobStatus::Skipped | JobStatus::Cancelled) {
                finished.push(item);
            } else {
                items.push(item);
            }
        }
        let (up_active, up_done) = self.inner.up.queue_items();
        items.extend(up_active);
        finished.extend(up_done);
        let skip = finished.len().saturating_sub(300);
        items.extend(finished.into_iter().skip(skip));
        QueueView { paused: self.is_paused(), items, smart: self.inner.up.smart_count() }
    }

    /// Các job khôi phục từ phiên trước đang chờ quyết định.
    pub fn session_info(&self) -> SessionInfo {
        let jobs = self.inner.jobs.lock().unwrap().clone();
        let (mut restored, mut left) = (0usize, 0u64);
        for j in &jobs {
            let is_restored = {
                let st = j.st.lock().unwrap();
                st.status == JobStatus::Paused && st.hold == Hold::Restored
            };
            if is_restored {
                restored += 1;
                let dest = j.dest.lock().unwrap().clone();
                left += j.file.size.unwrap_or(0).saturating_sub(manifest_done_bytes(&dest, &j.file));
            }
        }
        SessionInfo { restored, bytes_left: left, auto_resume: self.settings().auto_resume }
    }

    /// `resume` (chạy tiếp) | `discard` (bỏ, xoá file tạm) | `later` (để sau). Trả `false` nếu action lạ.
    pub fn session_action(&self, action: &str) -> bool {
        let restored: Vec<Arc<Job>> = self
            .inner
            .jobs
            .lock()
            .unwrap()
            .iter()
            .filter(|j| {
                let st = j.st.lock().unwrap();
                st.status == JobStatus::Paused && st.hold == Hold::Restored
            })
            .cloned()
            .collect();
        match action {
            "resume" => {
                for j in &restored {
                    let mut st = j.st.lock().unwrap();
                    st.status = JobStatus::Queued;
                    st.hold = Hold::None;
                    st.dirty = true;
                }
                self.inner.pump();
            }
            "discard" => {
                for j in &restored {
                    self.inner.cancel_job(j);
                }
                self.inner.jobs.lock().unwrap().retain(|j| !restored.iter().any(|r| r.id == j.id));
            }
            "later" => {
                for j in &restored {
                    let mut st = j.st.lock().unwrap();
                    st.hold = Hold::Held;
                    st.dirty = true;
                }
            }
            _ => return false,
        }
        self.inner.persist_dirty.store(true, Ordering::Relaxed);
        true
    }

    pub fn set_auto_resume(&self, on: bool) {
        self.inner.settings.write().unwrap().auto_resume = on;
    }

    /// Phân giải một link/ID Drive thành metadata (shortcut được theo tới đích).
    pub async fn resolve(&self, input: &str) -> Result<DriveFile> {
        let (_, c) = self.inner.pool.active().await?;
        let meta = c.get_file(&parse_drive_input(input)?).await?;
        if meta.is_shortcut() {
            resolve_shortcut(&c, &meta).await
        } else {
            Ok(meta)
        }
    }

    // ───────────────────────── Smart Upload (tải lên tự động) ─────────────────────────

    /// Bắt đầu theo dõi các thư mục trên máy và tự tải file mới (đã "yên" ≥ 2 phút) lên Drive. Xem `engine::smart`.
    pub async fn smart_start(&self, req: SmartRequest) -> Result<SmartStarted> {
        self.inner.up.smart_start(req).await
    }

    /// Tải lên thủ công các file/thư mục đã chọn trên máy (một lần). Xem `engine::manual`.
    pub async fn upload_start(&self, req: UploadRequest) -> Result<UploadStarted> {
        self.inner.up.upload_start(req).await
    }

    /// Các thư mục Smart Upload đang được theo dõi.
    pub fn smart_list(&self) -> Vec<WatchInfo> {
        self.inner.up.smart_list()
    }

    /// Dừng theo dõi một thư mục (`Some(id)`) hoặc tất cả (`None`). Tác vụ đã vào hàng đợi vẫn chạy tiếp.
    pub fn smart_stop(&self, id: Option<JobId>) -> usize {
        self.inner.up.smart_stop(id)
    }

    /// Chu kỳ quét thư mục của Smart Upload (mặc định 10 giây). Chủ yếu để kiểm thử.
    pub fn set_smart_scan_interval(&self, d: Duration) {
        self.inner.up.set_scan_interval(d);
    }

    pub fn pause(&self, id: JobId) {
        if let Some(job) = self.inner.find(id) {
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
                    st.dirty = true;
                }
                _ => {}
            }
        }
        self.inner.persist_dirty.store(true, Ordering::Relaxed);
    }

    /// Tiếp tục một job (Paused / Failed / restored / held).
    pub fn resume(&self, id: JobId) {
        if let Some(job) = self.inner.find(id) {
            let mut st = job.st.lock().unwrap();
            if matches!(st.status, JobStatus::Paused | JobStatus::Failed) {
                st.status = JobStatus::Queued;
                st.hold = Hold::None;
                st.error = None;
                st.dirty = true;
            }
        } else {
            // Không phải tác vụ tải xuống: thử tác vụ tải lên (cùng bộ đếm ID nên không thể nhầm).
            self.inner.up.resume(id);
        }
        self.inner.pump();
    }

    pub fn cancel(&self, id: JobId) {
        match self.inner.find(id) {
            Some(job) => self.inner.cancel_job(&job),
            None => {
                self.inner.up.cancel(id);
            }
        }
    }

    pub fn remove(&self, id: JobId) -> bool {
        let Some(job) = self.inner.find(id) else { return false };
        self.inner.cancel_job(&job);
        self.inner.jobs.lock().unwrap().retain(|j| j.id != id);
        self.inner.persist_dirty.store(true, Ordering::Relaxed);
        true
    }

    /// Tạm dừng mọi tác vụ của một tài khoản (khi gỡ tài khoản đó): giữ nguyên file dở để tiếp tục sau khi đăng nhập lại.
    pub fn pause_account(&self, account: &str) {
        self.inner.up.pause_account(account);
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
                    st.dirty = true;
                }
                _ => {}
            }
        }
        self.inner.persist_dirty.store(true, Ordering::Relaxed);
    }

    /// "Tạm dừng tất cả": dừng các job đang chạy và không khởi động job mới.
    pub fn pause_all(&self) {
        self.inner.up.pause_all();
        self.inner.global_paused.store(true, Ordering::SeqCst);
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
        self.inner.persist_dirty.store(true, Ordering::Relaxed);
    }

    /// "Tiếp tục": bỏ cờ tạm dừng chung và đưa các job bị `pause_all` về hàng đợi.
    pub fn resume_all(&self) {
        self.inner.up.resume_all();
        self.inner.global_paused.store(false, Ordering::SeqCst);
        let jobs = self.inner.jobs.lock().unwrap().clone();
        for job in jobs {
            let mut st = job.st.lock().unwrap();
            if st.status == JobStatus::Paused && st.hold == Hold::Global {
                st.status = JobStatus::Queued;
                st.hold = Hold::None;
                st.dirty = true;
            }
        }
        self.inner.pump();
    }

    pub fn is_paused(&self) -> bool {
        self.inner.global_paused.load(Ordering::SeqCst)
    }

    /// Huỷ mọi job đang chờ / đang chạy / đang tạm dừng (job lỗi giữ nguyên để thử lại).
    pub fn cancel_all(&self) {
        self.inner.up.cancel_all();
        let jobs = self.inner.jobs.lock().unwrap().clone();
        for job in jobs {
            if matches!(job.status(), JobStatus::Queued | JobStatus::Running | JobStatus::Paused) {
                self.inner.cancel_job(&job);
            }
        }
    }

    /// Xoá khỏi danh sách các job đã xong / bỏ qua / đã huỷ. Trả về id đã xoá.
    pub fn clear_finished(&self) -> Vec<JobId> {
        let mut removed = Vec::new();
        self.inner.jobs.lock().unwrap().retain(|j| {
            let done = matches!(j.status(), JobStatus::Completed | JobStatus::Skipped | JobStatus::Cancelled);
            if done {
                removed.push(j.id);
            }
            !done
        });
        removed.extend(self.inner.up.clear_finished());
        self.inner.persist_dirty.store(true, Ordering::Relaxed);
        removed
    }

    /// Gọi khi thoát app: tạm dừng mọi job (manifest được lưu), chờ tối đa ~5s rồi ghi `jobs.json`.
    pub async fn shutdown(&self) {
        self.pause_all();
        for _ in 0..50 {
            let jobs = self.inner.jobs.lock().unwrap().clone();
            if jobs.iter().all(|j| j.status() != JobStatus::Running) && !self.inner.up.has_running() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        self.inner.persist().await;
    }
}

async fn resolve_shortcut(c: &DriveClient, f: &DriveFile) -> Result<DriveFile> {
    let d = f.shortcut_details.as_ref().ok_or_else(|| Error::other("shortcut không có đích"))?;
    c.get_file(&DriveRef { id: d.target_id.clone(), resource_key: d.target_resource_key.clone() }).await
}

impl Inner {
    fn find(&self, id: JobId) -> Option<Arc<Job>> {
        self.jobs.lock().unwrap().iter().find(|j| j.id == id).cloned()
    }

    /// Khôi phục job dở dang của phiên trước: chờ người dùng quyết định (hoặc chạy luôn nếu bật tự tiếp tục).
    fn restore(&self) {
        let specs: Vec<JobSpec> = load_json(&self.jobs_file).unwrap_or_default();
        let auto = self.settings.read().unwrap().auto_resume;
        let mut jobs = self.jobs.lock().unwrap();
        for s in specs {
            self.next_id.fetch_max(s.id + 1, Ordering::Relaxed);
            let status = if auto { JobStatus::Queued } else { JobStatus::Paused };
            let job = Job::new(s.id, s.account, s.file, s.dest, status, Some("Khôi phục từ phiên trước".into()));
            if !auto {
                job.st.lock().unwrap().hold = Hold::Restored;
            }
            jobs.push(Arc::new(job));
        }
    }

    fn add_job(&self, account: &str, file: DriveFile, dest: PathBuf, status: JobStatus, note: Option<String>) -> Arc<Job> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let job = Arc::new(Job::new(id, account.to_string(), file, dest, status, note));
        self.jobs.lock().unwrap().push(job.clone());
        job
    }

    fn dest_is_active(&self, p: &Path, file_id: Option<&str>) -> bool {
        let jobs = self.jobs.lock().unwrap().clone();
        jobs.iter().any(|j| {
            let active = matches!(j.status(), JobStatus::Queued | JobStatus::Running | JobStatus::Paused);
            active && *j.dest.lock().unwrap() == p && file_id.is_none_or(|id| j.file.id == id)
        })
    }

    /// Quyết định đường dẫn đích: tiếp tục bản dở, bỏ qua file đã có, hoặc đổi tên `tên (1)`.
    fn plan_destination(&self, path: PathBuf, file: &DriveFile) -> Planned {
        if self.dest_is_active(&path, Some(&file.id)) {
            return Planned::Duplicate;
        }
        let (part, mpath) = (Manifest::part_path(&path), Manifest::path_for(&path));
        if Manifest::load(&mpath).is_some_and(|m| m.is_for(file)) && part.exists() {
            return Planned::Run(path);
        }
        if !file.is_google_doc() {
            if let Ok(md) = std::fs::metadata(&path) {
                if md.is_file() && Some(md.len()) == file.size {
                    return Planned::Skip(path);
                }
            }
        }
        Planned::Run(unique_path(&path, |p| {
            p.exists() || Manifest::part_path(p).exists() || Manifest::path_for(p).exists() || self.dest_is_active(p, None)
        }))
    }

    /// Chạy các job Queued cho tới khi đủ `parallel_files` job đang chạy.
    fn pump(self: &Arc<Self>) {
        if self.global_paused.load(Ordering::SeqCst) {
            return;
        }
        let limit = self.settings.read().unwrap().parallel_files;
        let jobs = self.jobs.lock().unwrap().clone();
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
                    st.note = None;
                    st.ctl = Some(ctl.clone());
                    st.progress = progress.clone();
                    st.meter = Meter::new();
                    st.dirty = true;
                    Some((ctl, progress))
                }
            };
            if let Some((ctl, progress)) = started {
                running += 1;
                self.rt.spawn(run_job(self.clone(), job, ctl, progress));
            }
        }
    }

    fn cancel_job(self: &Arc<Self>, job: &Arc<Job>) {
        let cleanup = {
            let mut st = job.st.lock().unwrap();
            match st.status {
                JobStatus::Running => {
                    if let Some(c) = &st.ctl {
                        c.cancel();
                    }
                    false
                }
                JobStatus::Queued | JobStatus::Paused | JobStatus::Failed => {
                    st.status = JobStatus::Cancelled;
                    st.dirty = true;
                    true
                }
                _ => false,
            }
        };
        if cleanup {
            let (inner, job) = (self.clone(), job.clone());
            self.rt.spawn(async move { inner.cleanup_partial(&job).await });
        }
        self.persist_dirty.store(true, Ordering::Relaxed);
    }

    /// Xoá `.part`, `.peadrive` và bản copy tạm trên Drive (nếu có) của job bị huỷ.
    async fn cleanup_partial(&self, job: &Job) {
        let dest = job.dest.lock().unwrap().clone();
        let (part, mpath) = (Manifest::part_path(&dest), Manifest::path_for(&dest));
        if let Some(id) = Manifest::load(&mpath).and_then(|m| m.copy_id) {
            if let Ok((_, c)) = self.pool.client_for(&job.account).await {
                let _ = c.delete_file(&id).await;
            }
        }
        let _ = tokio::fs::remove_file(&part).await;
        let _ = tokio::fs::remove_file(&mpath).await;
    }

    async fn persist(&self) {
        let specs: Vec<JobSpec> = self
            .jobs
            .lock()
            .unwrap()
            .iter()
            .filter(|j| matches!(j.status(), JobStatus::Queued | JobStatus::Running | JobStatus::Paused | JobStatus::Failed))
            .map(|j| JobSpec { id: j.id, account: j.account.clone(), file: j.file.clone(), dest: j.dest.lock().unwrap().clone() })
            .collect();
        let path = self.jobs_file.clone();
        let _ = tokio::task::spawn_blocking(move || save_json(&path, &specs)).await;
    }
}

async fn run_job(inner: Arc<Inner>, job: Arc<Job>, ctl: Arc<Control>, progress: Arc<Progress>) {
    let settings = inner.settings.read().unwrap().clone();
    let req = JobRequest { file: job.file.clone(), dest: job.dest.lock().unwrap().clone() };
    // Luôn chạy bằng client (token) của tài khoản đã tạo tác vụ, không phụ thuộc tài khoản đang chọn trên UI.
    let res = match inner.pool.client_for(&job.account).await {
        Ok((_, client)) => job::download(&RunCtx { client, ctl, progress, settings }, &req).await,
        Err(e) => Err(e),
    };

    let mut cleanup = false;
    {
        let mut st = job.st.lock().unwrap();
        st.ctl = None;
        st.dirty = true;
        match res {
            Ok(path) => {
                st.status = JobStatus::Completed;
                st.error = None;
                *job.dest.lock().unwrap() = path;
            }
            Err(Error::Paused) => {
                st.status = JobStatus::Paused;
                st.hold = st.pending_hold;
            }
            Err(Error::Cancelled) => {
                st.status = JobStatus::Cancelled;
                cleanup = true;
            }
            Err(e) => {
                st.status = JobStatus::Failed;
                st.error = Some(e.to_string());
            }
        }
    }
    if cleanup {
        inner.cleanup_partial(&job).await;
    }
    inner.persist_dirty.store(true, Ordering::Relaxed);
    inner.pump();
}

async fn ticker(inner: Arc<Inner>) {
    let mut iv = tokio::time::interval(Duration::from_millis(250));
    iv.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut last_persist = Instant::now();
    loop {
        iv.tick().await;
        let jobs = inner.jobs.lock().unwrap().clone();
        let batch: Vec<JobSnapshot> = jobs.iter().filter_map(|j| j.tick_view()).collect();
        if !batch.is_empty() {
            let _ = inner.events.send(batch);
        }
        if inner.persist_dirty.load(Ordering::Relaxed) && last_persist.elapsed() >= Duration::from_secs(2) {
            inner.persist_dirty.store(false, Ordering::Relaxed);
            last_persist = Instant::now();
            inner.persist().await;
        }
    }
}
