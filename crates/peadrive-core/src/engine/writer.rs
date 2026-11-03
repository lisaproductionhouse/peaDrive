//! Disk writer: MỘT thread chuyên dụng (blocking pool) rút dữ liệu từ tất cả ring và `pwrite` xuống file `.part`
//! đúng offset. Tách khỏi runtime async nên I/O đĩa chậm không làm nghẽn việc đọc socket; ngược lại, khi đĩa
//! chậm, các ring đầy → worker tự dừng đọc → backpressure xuống TCP, không tràn RAM.

use std::fs::File;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use super::plan::ChunkMap;
use super::ring::{Consumer, Wake};
use super::state::{Control, Progress};
use crate::error::{Error, Result};
use crate::util::write_all_at;

pub struct WriterCtx {
    pub file: Arc<File>,
    pub map: Arc<ChunkMap>,
    pub progress: Arc<Progress>,
    /// Control của phiên: lỗi ghi đĩa sẽ `fail()` để mọi worker dừng.
    pub ctl: Arc<Control>,
    pub wake: Arc<Wake>,
    /// Đặt `true` SAU KHI mọi worker đã flush và kết thúc; writer xả nốt rồi thoát.
    pub stop: Arc<AtomicBool>,
}

pub fn spawn(consumers: Vec<Consumer>, ctx: WriterCtx) -> tokio::task::JoinHandle<Result<()>> {
    tokio::task::spawn_blocking(move || run(consumers, ctx))
}

fn run(mut rings: Vec<Consumer>, ctx: WriterCtx) -> Result<()> {
    loop {
        // Đọc cờ TRƯỚC khi quét: nếu `stop` đã bật mà quét không thấy gì thì chắc chắn đã xả hết.
        let finished = ctx.stop.load(Ordering::Acquire);
        let mut progressed = false;

        for ring in rings.iter_mut() {
            while let Some(rec) = ring.next_record() {
                let (a, b) = ring.payload(&rec);
                let res = write_all_at(&ctx.file, a, rec.offset).and_then(|_| {
                    if b.is_empty() {
                        Ok(())
                    } else {
                        write_all_at(&ctx.file, b, rec.offset + a.len() as u64)
                    }
                });
                if let Err(e) = res {
                    let err = Error::Other(format!("lỗi ghi đĩa: {e}"));
                    ctx.ctl.fail(err.to_string());
                    return Err(err);
                }
                if rec.len > 0 {
                    ctx.progress.written.fetch_add(rec.len as u64, Ordering::Relaxed);
                    if ctx.map.on_written(rec.offset, rec.len as u64) {
                        ctx.progress.chunks_done.fetch_add(1, Ordering::Relaxed);
                    }
                }
                ring.consume(&rec);
                progressed = true;
            }
        }

        if !progressed {
            if finished {
                return Ok(());
            }
            ctx.wake.wait(Duration::from_millis(100));
        }
    }
}
