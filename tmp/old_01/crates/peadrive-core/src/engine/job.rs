//! Pipeline tải MỘT file:
//! prepare (manifest + preallocate) → session (worker pool → ring → writer) → [copy dự phòng nếu hết quota]
//! → verify MD5 (tuỳ chọn) → rename `.part` → tên cuối.

use std::fs::{File, OpenOptions};
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use md5::{Digest, Md5};
use tokio::task::JoinSet;

use super::manifest::Manifest;
use super::plan::{ChunkMap, ChunkQueue, Plan};
use super::ring::{self, Wake};
use super::state::{Control, Phase, Progress};
use super::worker::{self, Governor, Source, WorkerShared};
use super::writer::{self, WriterCtx};
use crate::config::Settings;
use crate::drive::model::export_target;
use crate::drive::{DriveClient, DriveFile, DriveRef};
use crate::error::{Error, Result};
use crate::util::{hex, human_bytes, unique_path};

pub struct RunCtx {
    pub client: Arc<DriveClient>,
    pub ctl: Arc<Control>,
    pub progress: Arc<Progress>,
    pub settings: Settings,
}

#[derive(Clone)]
pub struct JobRequest {
    pub file: DriveFile,
    /// Đường dẫn cuối cùng mong muốn.
    pub dest: PathBuf,
}

struct Prepared {
    plan: Plan,
    map: Arc<ChunkMap>,
    file: Arc<File>,
    manifest: Mutex<Manifest>,
    part: PathBuf,
    manifest_path: PathBuf,
}

fn join_err(e: tokio::task::JoinError) -> Error {
    Error::other(format!("tác vụ nền gặp lỗi: {e}"))
}

/// Tải file về `req.dest`; trả đường dẫn cuối cùng (có thể khác nếu tên bị chiếm giữa chừng).
pub async fn download(ctx: &RunCtx, req: &JobRequest) -> Result<PathBuf> {
    ctx.progress.set_phase(Phase::Preparing);
    ctx.ctl.check()?;
    if let Some(parent) = req.dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    if req.file.is_google_doc() {
        return export_native(ctx, req).await;
    }
    let size = req.file.size.ok_or_else(|| Error::other("Drive không cho biết dung lượng file này"))?;
    if size == 0 {
        tokio::fs::File::create(&req.dest).await?;
        return Ok(req.dest.clone());
    }

    let st = Arc::new(prepare(req, size, &ctx.settings).await?);
    let done = st.map.done_bytes();
    ctx.progress.chunks_total.store(st.plan.chunks, Ordering::Relaxed);
    ctx.progress.chunks_done.store(st.map.done_count(), Ordering::Relaxed);
    ctx.progress.downloaded.store(done, Ordering::Relaxed);
    ctx.progress.written.store(done, Ordering::Relaxed);

    let main = Source { id: req.file.id.clone(), resource_key: req.file.resource_key.clone() };
    let mut result = run_session(ctx, &st, main).await;
    if ctx.settings.copy_fallback && matches!(&result, Err(e) if e.is_download_quota()) {
        result = fallback(ctx, req, &st, size).await;
    }
    match result {
        Ok(()) => finalize(ctx, req, &st).await,
        Err(e) => {
            checkpoint(&st).await;
            Err(e)
        }
    }
}

async fn prepare(req: &JobRequest, size: u64, settings: &Settings) -> Result<Prepared> {
    let (file, dest, chunk_size) = (req.file.clone(), req.dest.clone(), settings.chunk_size());
    tokio::task::spawn_blocking(move || -> Result<Prepared> {
        let part = Manifest::part_path(&dest);
        let manifest_path = Manifest::path_for(&dest);
        let part_ok = std::fs::metadata(&part).map(|m| m.len() == size).unwrap_or(false);
        let reusable = Manifest::load(&manifest_path).filter(|m| m.is_for(&file) && part_ok);
        let fresh = reusable.is_none();
        let manifest = match reusable {
            Some(m) => m,
            None => Manifest::new(&file, chunk_size, Plan::new(size, chunk_size).chunks),
        };
        // Resume giữ nguyên chunk_size của phiên trước để bitmap còn ý nghĩa.
        let plan = Plan::new(size, manifest.chunk_size);
        let flags = if fresh { vec![false; plan.chunks] } else { manifest.done_flags(plan.chunks) };

        let f = OpenOptions::new().read(true).write(true).create(true).truncate(fresh).open(&part)?;
        if fresh || f.metadata()?.len() != size {
            f.set_len(size)?; // preallocate: ghi ngẫu nhiên theo offset không phải nới file dần
        }
        manifest.save(&manifest_path)?;
        Ok(Prepared {
            plan,
            map: Arc::new(ChunkMap::new(plan, &flags)),
            file: Arc::new(f),
            manifest: Mutex::new(manifest),
            part,
            manifest_path,
        })
    })
    .await
    .map_err(join_err)?
}

/// Một phiên tải từ `source`: dựng ring/worker/writer, chạy tới khi xong, lỗi, tạm dừng hoặc huỷ.
async fn run_session(ctx: &RunCtx, st: &Arc<Prepared>, source: Source) -> Result<()> {
    let remaining = st.plan.chunks - st.map.done_count();
    if remaining == 0 {
        return Ok(());
    }
    ctx.progress.set_phase(Phase::Downloading);
    let s = &ctx.settings;
    let session_ctl = Control::child(&ctx.ctl);
    let n = s.workers.min(remaining).max(1);

    let wake = Arc::new(Wake::default());
    let (mut producers, mut consumers) = (Vec::with_capacity(n), Vec::with_capacity(n));
    for _ in 0..n {
        let (p, c) = ring::channel(s.ring_bytes(), wake.clone());
        producers.push(p);
        consumers.push(c);
    }

    let stop = Arc::new(AtomicBool::new(false));
    let writer = writer::spawn(
        consumers,
        WriterCtx {
            file: st.file.clone(),
            map: st.map.clone(),
            progress: ctx.progress.clone(),
            ctl: session_ctl.clone(),
            wake: wake.clone(),
            stop: stop.clone(),
        },
    );

    let shared = Arc::new(WorkerShared {
        client: ctx.client.clone(),
        source,
        plan: st.plan,
        queue: ChunkQueue::new(st.map.clone()),
        progress: ctx.progress.clone(),
        ctl: session_ctl.clone(),
        governor: Governor::new(n),
        max_retries: s.max_retries,
    });
    let mut workers = JoinSet::new();
    for (i, p) in producers.into_iter().enumerate() {
        workers.spawn(worker::run_worker(shared.clone(), i, p));
    }
    let ticker = tokio::spawn(checkpoint_loop(st.clone(), shared.clone()));

    // Lỗi đầu tiên là nguyên nhân gốc; các worker còn lại sẽ dừng vì `fail()` và báo lỗi thứ cấp.
    let mut first: Option<Error> = None;
    while let Some(joined) = workers.join_next().await {
        let err = match joined {
            Ok(Ok(())) => None,
            Ok(Err(e)) => Some(e),
            Err(je) => Some(Error::other(format!("worker gặp lỗi nội bộ: {je}"))),
        };
        if let Some(e) = err {
            if first.is_none() {
                if !matches!(e, Error::Paused | Error::Cancelled) {
                    session_ctl.fail(e.to_string());
                }
                first = Some(e);
            }
        }
    }

    stop.store(true, Ordering::Release);
    wake.notify();
    let writer_res = writer.await.map_err(join_err)?;
    ticker.abort();
    checkpoint(st).await;

    writer_res?;
    match first {
        Some(e) => Err(e),
        None if st.map.all_done() => Ok(()),
        None => Err(Error::other("phiên tải kết thúc nhưng còn chunk chưa ghi")),
    }
}

async fn checkpoint_loop(st: Arc<Prepared>, shared: Arc<WorkerShared>) {
    let mut last = Instant::now();
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        shared.governor.tick();
        if last.elapsed() >= Duration::from_secs(3) && st.map.take_dirty() {
            checkpoint(&st).await;
            last = Instant::now();
        }
    }
}

/// Lưu manifest. Thứ tự: chụp bitmap → fsync dữ liệu → ghi manifest ⇒ bit đã set thì dữ liệu chắc chắn đã bền.
async fn checkpoint(st: &Arc<Prepared>) {
    let bitmap = st.map.bitmap();
    let manifest = {
        let mut g = st.manifest.lock().unwrap();
        g.set_done(&bitmap);
        g.clone()
    };
    let (file, path) = (st.file.clone(), st.manifest_path.clone());
    let _ = tokio::task::spawn_blocking(move || {
        let _ = file.sync_data();
        let _ = manifest.save(&path);
    })
    .await;
}

/// Hết quota tải của file gốc → copy server-side vào Drive cá nhân rồi tải từ bản copy.
async fn fallback(ctx: &RunCtx, req: &JobRequest, st: &Arc<Prepared>, size: u64) -> Result<()> {
    ctx.progress.set_phase(Phase::CopyingToDrive);
    ctx.ctl.check()?;

    let existing = st.manifest.lock().unwrap().copy_id.clone();
    let reuse = match existing {
        Some(id) if ctx.client.get_file(&DriveRef { id: id.clone(), resource_key: None }).await.is_ok() => Some(id),
        _ => None,
    };
    let copy_id = match reuse {
        Some(id) => id,
        None => {
            let about = ctx.client.about().await?;
            if let Some(free) = about.free() {
                if free < size {
                    return Err(Error::Quota(format!(
                        "file hết quota tải, nhưng Drive cá nhân chỉ còn {} trống — không đủ để copy {}",
                        human_bytes(free),
                        human_bytes(size)
                    )));
                }
            }
            let folder = ctx.client.ensure_temp_folder().await?;
            let src = DriveRef { id: req.file.id.clone(), resource_key: req.file.resource_key.clone() };
            let name = format!("{} (peaDrive tmp)", req.file.name);
            let copy = ctx
                .client
                .copy_file(&src, &name, &folder)
                .await
                .map_err(|e| Error::Quota(format!("file hết quota tải và không copy được vào Drive cá nhân: {e}")))?;
            st.manifest.lock().unwrap().copy_id = Some(copy.id.clone());
            checkpoint(st).await;
            copy.id
        }
    };

    let result = run_session(ctx, st, Source { id: copy_id.clone(), resource_key: None }).await;
    if !matches!(result, Err(Error::Paused)) {
        // Xong, lỗi hoặc huỷ: dọn bản copy. Chỉ tạm dừng mới giữ lại để resume khỏi copy lại.
        if !ctx.settings.keep_copy {
            let _ = ctx.client.delete_file(&copy_id).await;
        }
        st.manifest.lock().unwrap().copy_id = None;
    }
    result
}

async fn finalize(ctx: &RunCtx, req: &JobRequest, st: &Arc<Prepared>) -> Result<PathBuf> {
    ctx.progress.set_phase(Phase::Finalizing);
    let f = st.file.clone();
    tokio::task::spawn_blocking(move || f.sync_all()).await.map_err(join_err)??;

    if ctx.settings.verify_md5 {
        if let Some(md5) = &req.file.md5_checksum {
            ctx.progress.set_phase(Phase::Verifying);
            verify_md5(st.part.clone(), md5.clone(), ctx.ctl.clone()).await?;
        }
    }
    let final_path = unique_path(&req.dest, |p| p.exists());
    tokio::fs::rename(&st.part, &final_path).await?;
    let _ = tokio::fs::remove_file(&st.manifest_path).await;
    Ok(final_path)
}

async fn verify_md5(path: PathBuf, expected: String, ctl: Arc<Control>) -> Result<()> {
    tokio::task::spawn_blocking(move || -> Result<()> {
        let mut f = File::open(&path)?;
        let mut hasher = Md5::new();
        let mut buf = vec![0u8; 4 << 20];
        loop {
            ctl.check()?;
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        let got = hex(&hasher.finalize());
        if got.eq_ignore_ascii_case(&expected) {
            Ok(())
        } else {
            Err(Error::Integrity(format!("MD5 không khớp: tải về {got}, Drive báo {expected}")))
        }
    })
    .await
    .map_err(join_err)?
}

/// Google Docs/Sheets/Slides: không có byte nhị phân → `files.export` (≤ 10 MB) một lần.
async fn export_native(ctx: &RunCtx, req: &JobRequest) -> Result<PathBuf> {
    let (mime, _) = export_target(&req.file.mime_type)
        .ok_or_else(|| Error::other(format!("không hỗ trợ xuất loại file Google này: {}", req.file.mime_type)))?;
    ctx.ctl.check()?;
    let src = DriveRef { id: req.file.id.clone(), resource_key: req.file.resource_key.clone() };
    let bytes = ctx.client.export(&src, mime).await?;
    let n = bytes.len() as u64;
    ctx.progress.chunks_total.store(1, Ordering::Relaxed);
    ctx.progress.chunks_done.store(1, Ordering::Relaxed);
    ctx.progress.downloaded.store(n, Ordering::Relaxed);
    ctx.progress.written.store(n, Ordering::Relaxed);
    let part = Manifest::part_path(&req.dest);
    tokio::fs::write(&part, &bytes).await?;
    let final_path = unique_path(&req.dest, |p| p.exists());
    tokio::fs::rename(&part, &final_path).await?;
    Ok(final_path)
}
