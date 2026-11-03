//! Worker pool: mỗi worker là một task async, lặp: nhận chunk → `GET Range` → đẩy từng frame vào ring của mình.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use reqwest::header::CONTENT_RANGE;

use super::plan::{ChunkQueue, Plan};
use super::ring::Producer;
use super::state::{Control, Progress};
use crate::drive::quota::{self, Class};
use crate::drive::DriveClient;
use crate::error::{Error, Result};

const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Nguồn đọc dữ liệu: file gốc, hoặc bản copy trong Drive cá nhân.
#[derive(Clone, Debug)]
pub struct Source {
    pub id: String,
    pub resource_key: Option<String>,
}

/// AIMD: bị rate limit → giảm một nửa số worker được phép chạy; yên ổn 10s → tăng thêm 1.
pub struct Governor {
    max: usize,
    limit: AtomicUsize,
    last_change: Mutex<Instant>,
}

impl Governor {
    pub fn new(max: usize) -> Self {
        Self { max: max.max(1), limit: AtomicUsize::new(max.max(1)), last_change: Mutex::new(Instant::now()) }
    }

    pub fn limit(&self) -> usize {
        self.limit.load(Ordering::Relaxed)
    }

    pub fn on_rate_limit(&self) {
        let mut t = self.last_change.lock().unwrap();
        if t.elapsed() < Duration::from_secs(2) {
            return; // nhiều worker cùng báo lỗi một lúc → chỉ giảm một lần
        }
        let cur = self.limit.load(Ordering::Relaxed);
        self.limit.store((cur / 2).max(1), Ordering::Relaxed);
        *t = Instant::now();
    }

    pub fn tick(&self) {
        let mut t = self.last_change.lock().unwrap();
        let cur = self.limit.load(Ordering::Relaxed);
        if cur < self.max && t.elapsed() >= Duration::from_secs(10) {
            self.limit.store(cur + 1, Ordering::Relaxed);
            *t = Instant::now();
        }
    }
}

pub struct WorkerShared {
    pub client: Arc<DriveClient>,
    pub source: Source,
    pub plan: Plan,
    pub queue: ChunkQueue,
    pub progress: Arc<Progress>,
    pub ctl: Arc<Control>,
    pub governor: Governor,
    pub max_retries: u32,
}

async fn sleep_ctl(ctl: &Control, total: Duration) -> Result<()> {
    let mut left = total;
    while !left.is_zero() {
        ctl.check()?;
        let step = left.min(Duration::from_millis(100));
        tokio::time::sleep(step).await;
        left -= step;
    }
    ctl.check()
}

pub async fn run_worker(sh: Arc<WorkerShared>, idx: usize, mut ring: Producer) -> Result<()> {
    loop {
        // Governor: worker có chỉ số ≥ giới hạn hiện tại thì nghỉ.
        while idx >= sh.governor.limit() {
            sleep_ctl(&sh.ctl, Duration::from_millis(200)).await?;
        }
        sh.ctl.check()?;
        let Some(chunk) = sh.queue.claim() else { break };

        sh.progress.workers.fetch_add(1, Ordering::Relaxed);
        let res = fetch_chunk(&sh, chunk, &mut ring).await;
        sh.progress.workers.fetch_sub(1, Ordering::Relaxed);
        ring.flush(); // luôn công bố phần đã nhận (kể cả khi lỗi/tạm dừng) để writer ghi nốt
        res?;
    }
    ring.flush();
    Ok(())
}

/// Tải một chunk; tự thử lại từ đúng byte đã nhận được (không tải lại từ đầu chunk).
async fn fetch_chunk(sh: &WorkerShared, chunk: usize, ring: &mut Producer) -> Result<()> {
    let (start, end) = sh.plan.range(chunk);
    let mut pos = start;
    let mut failures = 0u32;
    while pos <= end {
        sh.ctl.check()?;
        let before = pos;
        let err = match stream_range(sh, &mut pos, end, ring).await {
            Ok(()) => continue,
            Err(e) => e,
        };
        ring.flush();
        match quota::classify(&err) {
            Class::Fatal | Class::DownloadQuota | Class::Auth => return Err(err),
            Class::RateLimited => {
                sh.progress.rate_limit_hits.fetch_add(1, Ordering::Relaxed);
                sh.governor.on_rate_limit();
            }
            Class::Transient => {}
        }
        failures = if pos > before { 1 } else { failures + 1 };
        if failures >= sh.max_retries {
            return Err(err);
        }
        tracing::debug!("chunk {chunk}: {err} — thử lại lần {failures} từ byte {pos}");
        sleep_ctl(&sh.ctl, quota::backoff(failures)).await?;
    }
    Ok(())
}

/// Một request Range `pos..=end`; cập nhật `pos` theo số byte đã đẩy vào ring.
async fn stream_range(sh: &WorkerShared, pos: &mut u64, end: u64, ring: &mut Producer) -> Result<()> {
    let resp = sh.client.media(&sh.source.id, sh.source.resource_key.as_deref(), Some((*pos, end))).await?;
    let status = resp.status().as_u16();
    if status == 200 && *pos != 0 {
        return Err(Error::Other("Drive trả 200 thay vì 206: không hỗ trợ Range cho file này".into()));
    }
    if status == 206 {
        let got = resp
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.strip_prefix("bytes "))
            .and_then(|s| s.split('-').next())
            .and_then(|s| s.parse::<u64>().ok());
        if got.is_some_and(|g| g != *pos) {
            return Err(Error::Transient(format!("Content-Range lệch: nhận {got:?}, cần {}", *pos)));
        }
    }

    let mut stream = resp.bytes_stream();
    while *pos <= end {
        let item = tokio::time::timeout(IDLE_TIMEOUT, stream.next())
            .await
            .map_err(|_| Error::Transient("không nhận được dữ liệu trong 30s".into()))?;
        let Some(frame) = item else { break };
        let frame = frame?;
        let take = (frame.len() as u64).min(end + 1 - *pos) as usize;
        if take == 0 {
            continue;
        }
        ring.push(*pos, &frame[..take], &sh.ctl).await?;
        *pos += take as u64;
        sh.progress.downloaded.fetch_add(take as u64, Ordering::Relaxed);
    }
    if *pos <= end {
        return Err(Error::Transient("luồng dữ liệu kết thúc sớm".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn governor_halves_then_recovers() {
        let g = Governor::new(8);
        assert_eq!(g.limit(), 8);
        // lần đầu trong 2s đầu bị debounce
        g.on_rate_limit();
        assert_eq!(g.limit(), 8);
        *g.last_change.lock().unwrap() = Instant::now() - Duration::from_secs(3);
        g.on_rate_limit();
        assert_eq!(g.limit(), 4);
        *g.last_change.lock().unwrap() = Instant::now() - Duration::from_secs(11);
        g.tick();
        assert_eq!(g.limit(), 5);
    }
}
