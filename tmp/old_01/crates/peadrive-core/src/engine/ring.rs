//! Ring buffer SPSC cho đường dữ liệu "mạng → đĩa".
//!
//! Mỗi worker sở hữu MỘT ring (producer = worker, consumer = writer thread duy nhất), nên không cần khoá:
//! chỉ hai con trỏ đơn điệu `head` (consumer) và `tail` (producer đã công bố) với Acquire/Release.
//!
//! Dữ liệu đi theo *record*: `[offset: u64 LE][len: u32 LE][payload…]`. Producer copy payload thẳng vào ring
//! (một lần memcpy từ frame mạng), vá header khi đóng record, rồi `commit` (store Release lên `tail`).
//! Writer chỉ thấy record đã commit đủ, nên mỗi record ghi xuống đĩa bằng 1–2 lệnh `pwrite` lớn (~1 MiB).
//! Khi ring đầy, producer `await` chờ chỗ trống → ngừng đọc socket → TCP backpressure; RAM luôn bị chặn
//! trên bởi `workers × dung lượng ring`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::Notify;

use super::state::Control;
use crate::error::Result;

/// Header mỗi record: offset (u64) + độ dài payload (u32).
pub const HDR: usize = 12;

#[repr(align(64))]
struct Padded<T>(T);

struct Shared {
    ptr: *mut u8,
    cap: usize,
    mask: usize,
    /// Con trỏ tiêu thụ (chỉ consumer ghi).
    head: Padded<AtomicUsize>,
    /// Con trỏ đã công bố (chỉ producer ghi).
    tail: Padded<AtomicUsize>,
    /// Consumer báo "đã có chỗ trống" cho producer đang chờ.
    space: Notify,
    /// Producer báo "có record mới" cho writer thread.
    wake: Arc<Wake>,
}

// SAFETY: vùng [head, tail) chỉ consumer đọc; vùng [tail, head + cap) chỉ producer ghi. Hai vùng không giao nhau
// nhờ cặp Acquire/Release trên head/tail; Producer và Consumer đều không Clone nên mỗi đầu chỉ có một chủ.
unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}

impl Drop for Shared {
    fn drop(&mut self) {
        // SAFETY: ptr/cap đến từ `Box<[u8]>::into_raw` trong `channel`.
        unsafe { drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(self.ptr, self.cap))) }
    }
}

/// Đánh thức writer thread (Condvar có cờ để không mất tín hiệu).
#[derive(Default)]
pub struct Wake {
    flag: Mutex<bool>,
    cv: Condvar,
}

impl Wake {
    pub fn notify(&self) {
        *self.flag.lock().unwrap() = true;
        self.cv.notify_one();
    }

    pub fn wait(&self, timeout: Duration) {
        let mut g = self.flag.lock().unwrap();
        if !*g {
            g = self.cv.wait_timeout(g, timeout).unwrap().0;
        }
        *g = false;
    }
}

/// Tạo một ring (dung lượng làm tròn lên luỹ thừa 2, tối thiểu 8 KiB).
pub fn channel(min_capacity: usize, wake: Arc<Wake>) -> (Producer, Consumer) {
    let cap = min_capacity.max(8 * 1024).next_power_of_two();
    let buf = vec![0u8; cap].into_boxed_slice();
    let ptr = Box::into_raw(buf) as *mut u8;
    let sh = Arc::new(Shared {
        ptr,
        cap,
        mask: cap - 1,
        head: Padded(AtomicUsize::new(0)),
        tail: Padded(AtomicUsize::new(0)),
        space: Notify::new(),
        wake,
    });
    let producer = Producer {
        sh: sh.clone(),
        wtail: 0,
        open: None,
        commit_bytes: (1 << 20).min(cap / 4),
        commit_every: Duration::from_millis(250),
    };
    (producer, Consumer { sh, head: 0 })
}

struct Open {
    hdr_pos: usize,
    offset: u64,
    len: usize,
    since: Instant,
}

pub struct Producer {
    sh: Arc<Shared>,
    /// Con trỏ đã "đặt chỗ" (có thể chưa công bố).
    wtail: usize,
    open: Option<Open>,
    commit_bytes: usize,
    commit_every: Duration,
}

impl Producer {
    pub fn capacity(&self) -> usize {
        self.sh.cap
    }

    fn free(&self) -> usize {
        let head = self.sh.head.0.load(Ordering::Acquire);
        self.sh.cap - self.wtail.wrapping_sub(head)
    }

    fn copy_in(&mut self, data: &[u8]) -> usize {
        let n = data.len().min(self.free());
        if n == 0 {
            return 0;
        }
        self.write_at(self.wtail, &data[..n]);
        self.wtail = self.wtail.wrapping_add(n);
        n
    }

    /// Ghi `data` tại vị trí tuyệt đối `pos` (có quấn vòng). Chỉ dùng trong vùng producer sở hữu.
    fn write_at(&self, pos: usize, data: &[u8]) {
        let sh = &*self.sh;
        let start = pos & sh.mask;
        let first = data.len().min(sh.cap - start);
        // SAFETY: vùng đích nằm trong [tail, head + cap) – thuộc riêng producer; không vượt quá cap.
        unsafe {
            std::ptr::copy_nonoverlapping(data.as_ptr(), sh.ptr.add(start), first);
            if data.len() > first {
                std::ptr::copy_nonoverlapping(data.as_ptr().add(first), sh.ptr, data.len() - first);
            }
        }
    }

    /// Đóng record đang mở (vá header) và công bố cho writer.
    fn seal(&mut self) {
        if let Some(o) = self.open.take() {
            let mut hdr = [0u8; HDR];
            hdr[..8].copy_from_slice(&o.offset.to_le_bytes());
            hdr[8..].copy_from_slice(&(o.len as u32).to_le_bytes());
            self.write_at(o.hdr_pos, &hdr);
            self.sh.tail.0.store(self.wtail, Ordering::Release);
            self.sh.wake.notify();
        }
    }

    /// Công bố mọi dữ liệu đang giữ (gọi khi hết chunk, khi lỗi, khi tạm dừng).
    pub fn flush(&mut self) {
        self.seal();
    }

    async fn wait_free(&self, need: usize, ctl: &Control) -> Result<()> {
        loop {
            ctl.check()?;
            if self.free() >= need {
                return Ok(());
            }
            tokio::select! {
                _ = self.sh.space.notified() => {}
                _ = tokio::time::sleep(Duration::from_millis(50)) => {}
            }
        }
    }

    /// Đẩy `data` (liên tiếp, bắt đầu tại `offset` trong file) vào ring. Chờ khi ring đầy.
    pub async fn push(&mut self, mut offset: u64, mut data: &[u8], ctl: &Control) -> Result<()> {
        while !data.is_empty() {
            if let Some(o) = &self.open {
                if o.offset + o.len as u64 != offset {
                    self.seal();
                }
            }
            if self.open.is_none() {
                self.wait_free(HDR + 1, ctl).await?;
                let hdr_pos = self.wtail;
                self.copy_in(&[0u8; HDR]);
                self.open = Some(Open { hdr_pos, offset, len: 0, since: Instant::now() });
            }
            let n = self.copy_in(data);
            if n == 0 {
                // Ring đầy: công bố phần đã có để writer xả, rồi chờ chỗ trống.
                self.seal();
                self.wait_free(HDR + 1, ctl).await?;
                continue;
            }
            let (commit_bytes, commit_every) = (self.commit_bytes, self.commit_every);
            let o = self.open.as_mut().expect("record đang mở");
            o.len += n;
            offset += n as u64;
            data = &data[n..];
            if o.len >= commit_bytes || o.since.elapsed() >= commit_every {
                self.seal();
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Record {
    pub offset: u64,
    pub len: usize,
}

pub struct Consumer {
    sh: Arc<Shared>,
    head: usize,
}

impl Consumer {
    /// Record kế tiếp đã công bố (chưa tiêu thụ).
    pub fn next_record(&self) -> Option<Record> {
        let avail = self.sh.tail.0.load(Ordering::Acquire).wrapping_sub(self.head);
        if avail == 0 {
            return None;
        }
        assert!(avail >= HDR, "ring hỏng: header chưa đủ");
        let (a, b) = self.slices(0, HDR);
        let mut h = [0u8; HDR];
        h[..a.len()].copy_from_slice(a);
        h[a.len()..].copy_from_slice(b);
        let offset = u64::from_le_bytes(h[..8].try_into().unwrap());
        let len = u32::from_le_bytes(h[8..].try_into().unwrap()) as usize;
        assert!(avail >= HDR + len, "ring hỏng: payload chưa đủ");
        Some(Record { offset, len })
    }

    /// Payload của record dưới dạng tối đa hai đoạn liên tục (đoạn 2 rỗng nếu không quấn vòng).
    pub fn payload(&self, rec: &Record) -> (&[u8], &[u8]) {
        self.slices(HDR, rec.len)
    }

    /// Giải phóng record → producer có thêm chỗ trống.
    pub fn consume(&mut self, rec: &Record) {
        self.head = self.head.wrapping_add(HDR + rec.len);
        self.sh.head.0.store(self.head, Ordering::Release);
        self.sh.space.notify_one();
    }

    fn slices(&self, from: usize, len: usize) -> (&[u8], &[u8]) {
        let sh = &*self.sh;
        let start = self.head.wrapping_add(from) & sh.mask;
        let first = len.min(sh.cap - start);
        // SAFETY: vùng [head, tail) chỉ consumer đọc; producer không ghi vào đó cho tới khi head tiến lên.
        unsafe {
            (
                std::slice::from_raw_parts(sh.ptr.add(start), first),
                std::slice::from_raw_parts(sh.ptr, len - first),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn pat(i: u64) -> u8 {
        (i.wrapping_mul(2_654_435_761) >> 11) as u8
    }

    #[tokio::test]
    async fn single_thread_roundtrip_with_wraparound() {
        let (mut p, mut c) = channel(8192, Arc::new(Wake::default()));
        assert_eq!(p.capacity(), 8192);
        let ctl = Control::root();
        let (mut off, mut seen) = (0u64, 0u64);
        for round in 0..300usize {
            let len = 100 + (round * 37) % 3000;
            let data: Vec<u8> = (0..len as u64).map(|i| pat(off + i)).collect();
            p.push(off, &data, &ctl).await.unwrap();
            p.flush();
            off += len as u64;
            while let Some(r) = c.next_record() {
                assert_eq!(r.offset, seen);
                let (a, b) = c.payload(&r);
                for (i, byte) in a.iter().chain(b.iter()).enumerate() {
                    assert_eq!(*byte, pat(r.offset + i as u64));
                }
                seen += r.len as u64;
                c.consume(&r);
            }
        }
        assert_eq!(seen, off);
        assert!(off > 8192 * 20, "đã quấn vòng nhiều lần");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stress_producer_consumer_with_backpressure() {
        const TOTAL: u64 = 48 << 20;
        let wake = Arc::new(Wake::default());
        let (mut p, mut c) = channel(64 * 1024, wake.clone());
        let ctl = Control::root();
        let done = Arc::new(AtomicBool::new(false));

        let (d2, w2) = (done.clone(), wake.clone());
        let consumer = std::thread::spawn(move || {
            let mut next = 0u64;
            loop {
                let finished = d2.load(Ordering::Acquire);
                let mut progressed = false;
                while let Some(r) = c.next_record() {
                    assert_eq!(r.offset, next, "record sai thứ tự");
                    let (a, b) = c.payload(&r);
                    let mut o = r.offset;
                    for byte in a.iter().chain(b.iter()) {
                        assert_eq!(*byte, pat(o), "dữ liệu hỏng tại {o}");
                        o += 1;
                    }
                    next += r.len as u64;
                    c.consume(&r);
                    progressed = true;
                }
                if !progressed {
                    if finished {
                        return next;
                    }
                    w2.wait(Duration::from_millis(20));
                }
            }
        });

        let (mut off, mut seed) = (0u64, 12345u64);
        let mut buf = vec![0u8; 50_000];
        while off < TOTAL {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            let len = (1 + (seed >> 33) % 40_000).min(TOTAL - off) as usize;
            for (i, b) in buf[..len].iter_mut().enumerate() {
                *b = pat(off + i as u64);
            }
            p.push(off, &buf[..len], &ctl).await.unwrap();
            off += len as u64;
        }
        p.flush();
        done.store(true, Ordering::Release);
        wake.notify();
        assert_eq!(consumer.join().unwrap(), TOTAL);
    }

    #[tokio::test]
    async fn cancel_unblocks_full_ring() {
        let (mut p, _c) = channel(8192, Arc::new(Wake::default()));
        let ctl = Control::root();
        let ctl2 = ctl.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            ctl2.cancel();
        });
        // Không ai tiêu thụ: ring sẽ đầy rồi phải thoát nhờ cancel.
        let big = vec![7u8; 1 << 20];
        let r = p.push(0, &big, &ctl).await;
        assert!(matches!(r, Err(crate::error::Error::Cancelled)));
    }
}
