use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

/// Chia file thành các chunk kích thước cố định (chunk cuối có thể ngắn hơn).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    pub size: u64,
    pub chunk_size: u64,
    pub chunks: usize,
}

impl Plan {
    pub fn new(size: u64, chunk_size: u64) -> Self {
        let chunk_size = chunk_size.max(1);
        Self { size, chunk_size, chunks: size.div_ceil(chunk_size) as usize }
    }

    /// (offset đầu, offset cuối) — cuối tính cả, đúng dạng `Range: bytes=a-b`.
    pub fn range(&self, i: usize) -> (u64, u64) {
        let start = i as u64 * self.chunk_size;
        (start, (start + self.chunk_size).min(self.size) - 1)
    }

    pub fn len(&self, i: usize) -> u64 {
        let (a, b) = self.range(i);
        b - a + 1
    }

    pub fn index_of(&self, offset: u64) -> usize {
        (offset / self.chunk_size) as usize
    }
}

/// Theo dõi từng chunk đã được GHI XUỐNG ĐĨA đủ chưa (không phải mới tải về).
pub struct ChunkMap {
    plan: Plan,
    done: Vec<AtomicBool>,
    written: Vec<AtomicU64>,
    done_count: AtomicUsize,
    dirty: AtomicBool,
}

impl ChunkMap {
    /// `initial[i] == true` nếu chunk i đã xong từ phiên trước (resume).
    pub fn new(plan: Plan, initial: &[bool]) -> Self {
        let done: Vec<AtomicBool> = (0..plan.chunks).map(|i| AtomicBool::new(initial.get(i).copied().unwrap_or(false))).collect();
        let done_count = done.iter().filter(|d| d.load(Ordering::Relaxed)).count();
        Self {
            plan,
            done,
            written: (0..plan.chunks).map(|_| AtomicU64::new(0)).collect(),
            done_count: AtomicUsize::new(done_count),
            dirty: AtomicBool::new(false),
        }
    }

    pub fn plan(&self) -> &Plan {
        &self.plan
    }

    pub fn is_done(&self, i: usize) -> bool {
        self.done[i].load(Ordering::Acquire)
    }

    /// Writer gọi sau mỗi record. Trả `true` nếu record này làm chunk hoàn tất.
    pub fn on_written(&self, offset: u64, len: u64) -> bool {
        let i = self.plan.index_of(offset);
        let total = self.written[i].fetch_add(len, Ordering::AcqRel) + len;
        if total >= self.plan.len(i) && !self.done[i].swap(true, Ordering::AcqRel) {
            self.done_count.fetch_add(1, Ordering::AcqRel);
            self.dirty.store(true, Ordering::Release);
            return true;
        }
        false
    }

    pub fn done_count(&self) -> usize {
        self.done_count.load(Ordering::Acquire)
    }

    pub fn all_done(&self) -> bool {
        self.done_count() >= self.plan.chunks
    }

    pub fn done_bytes(&self) -> u64 {
        (0..self.plan.chunks).filter(|&i| self.is_done(i)).map(|i| self.plan.len(i)).sum()
    }

    /// Bitmap đóng gói: bit i của byte i/8.
    pub fn bitmap(&self) -> Vec<u8> {
        let mut out = vec![0u8; self.plan.chunks.div_ceil(8)];
        for i in 0..self.plan.chunks {
            if self.is_done(i) {
                out[i / 8] |= 1 << (i % 8);
            }
        }
        out
    }

    pub fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::AcqRel)
    }
}

/// Các worker lần lượt nhận chunk theo thứ tự tăng dần (ghi gần tuần tự → tốt cho NTFS/APFS),
/// bỏ qua chunk đã xong.
pub struct ChunkQueue {
    next: AtomicUsize,
    map: Arc<ChunkMap>,
}

impl ChunkQueue {
    pub fn new(map: Arc<ChunkMap>) -> Self {
        Self { next: AtomicUsize::new(0), map }
    }

    pub fn claim(&self) -> Option<usize> {
        loop {
            let i = self.next.fetch_add(1, Ordering::Relaxed);
            if i >= self.map.plan.chunks {
                return None;
            }
            if !self.map.is_done(i) {
                return Some(i);
            }
        }
    }
}

pub fn bitmap_to_flags(bitmap: &[u8], chunks: usize) -> Vec<bool> {
    (0..chunks).map(|i| bitmap.get(i / 8).map(|b| b & (1 << (i % 8)) != 0).unwrap_or(false)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_cover_file_exactly() {
        let p = Plan::new(10_500, 4_000);
        assert_eq!(p.chunks, 3);
        assert_eq!(p.range(0), (0, 3_999));
        assert_eq!(p.range(2), (8_000, 10_499));
        assert_eq!((0..p.chunks).map(|i| p.len(i)).sum::<u64>(), 10_500);
        assert_eq!(p.index_of(7_999), 1);
        assert_eq!(Plan::new(8_000, 4_000).chunks, 2);
    }

    #[test]
    fn map_completion_bitmap_and_resume() {
        let p = Plan::new(10_500, 4_000);
        let m = ChunkMap::new(p, &[true, false, false]);
        assert_eq!(m.done_count(), 1);
        assert_eq!(m.done_bytes(), 4_000);
        assert!(!m.on_written(4_000, 3_000));
        assert!(m.on_written(7_000, 1_000));
        assert!(m.take_dirty() && !m.take_dirty());
        assert!(m.on_written(8_000, 2_500));
        assert!(m.all_done());
        let bm = m.bitmap();
        assert_eq!(bitmap_to_flags(&bm, 3), vec![true, true, true]);
    }

    #[test]
    fn queue_skips_done() {
        let p = Plan::new(5 * 100, 100);
        let q = ChunkQueue::new(Arc::new(ChunkMap::new(p, &[true, false, true, false, false])));
        let got: Vec<_> = std::iter::from_fn(|| q.claim()).collect();
        assert_eq!(got, vec![1, 3, 4]);
    }
}
