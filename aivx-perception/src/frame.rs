//! LatestFrameSlot —— seqlock 双缓冲（DESIGN.md §2，ADR-009）。
//!
//! 语义：**latest-wins**。写者（拉流线程）发布最新帧；读者（分析线程）总拿到
//! 最新完整帧，旧帧自然被覆盖——无队列、无积压、无延迟累积。
//!
//! 协议（每缓冲一个 seq，奇=写入中，偶=完成）：
//! - 写者：`begin_write()` 锁定非活跃缓冲 → 写入 NV12 → `commit()`（active 翻转）
//! - 读者：`read_latest()` → 读 active 的 seq，偶数才读，读后复核 seq 未变
//!
//! 宽限（DESIGN.md §2 v2.2 修正）：双缓冲下写者要翻转两次（跨一个完整帧周期，
//! 25fps 即 ~40ms）才会覆盖读者正在读的缓冲。seq 复核兜底极端竞争。

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// 帧句柄（读者持有；数据仍在槽内，借用受 seq 复核保护）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRef {
    idx: usize,
    seq: u64,
    /// 帧代数（写者 commit 时递增；读者用它判断"有没有新帧"）。
    pub gen: u64,
}

/// 最新帧槽：每路摄像头一个（DESIGN.md §2）。
///
/// 帧格式 NV12（`w*h` 亮度 + `w/2*h/2` 两个色度平面，总 `w*h*3/2` 字节）。
pub struct LatestFrameSlot {
    bufs: [Vec<u8>; 2],
    /// 当前最新帧所在缓冲（0/1）。
    active: AtomicUsize,
    /// seqlock：奇=写入中，偶=完成。
    seq: [AtomicU64; 2],
    /// 帧代数。写者 commit 递增；读者对比判断新帧。0 = 尚无帧。
    gen: AtomicU64,
    frame_size: usize,
    width: usize,
    height: usize,
}

impl LatestFrameSlot {
    /// 创建槽。`width`/`height` 是分析子码流分辨率（如 640×360）。
    pub fn new(width: usize, height: usize) -> Self {
        let frame_size = width * height * 3 / 2;
        Self {
            bufs: [vec![0; frame_size], vec![0; frame_size]],
            active: AtomicUsize::new(0),
            seq: [AtomicU64::new(0), AtomicU64::new(0)],
            gen: AtomicU64::new(0),
            frame_size,
            width,
            height,
        }
    }

    pub fn width(&self) -> usize {
        self.width
    }
    pub fn height(&self) -> usize {
        self.height
    }
    pub fn frame_size(&self) -> usize {
        self.frame_size
    }

    /// 写者：整值写一帧（测试/校准用便捷接口；生产 T1 走 begin_write/commit_write
    /// 两段式让 ffmpeg `read_exact` 直写缓冲——见 stream.rs）。
    pub fn write_val(&mut self, val: u8) -> u64 {
        let (idx, buf) = self.begin_write();
        buf.iter_mut().for_each(|b| *b = val);
        self.commit_write(idx)
    }

    /// 写者两段式（一）：锁非活跃缓冲（seq 偶→奇）——共享版（Arc 下的 T1）。
    ///
    /// 返回 (缓冲下标, 可写切片)——调用方把 IO 直读进这里，零拷贝。
    /// 半途失败必须 [`rollback_write`]（seq 回偶），否则下次 begin_write 会断言。
    ///
    /// 单写者约定由调用方保证（T1 线程唯一持有 Arc<LatestFrameSlot> 的写权，
    /// P8a：Arc 共享后写 API 走共享入口——内部锁保护 seq 协议）。
    pub fn begin_write_shared(&self) -> (usize, &mut [u8]) {
        let idx = 1 - self.active.load(Ordering::Acquire);
        let cur = self.seq[idx].fetch_add(1, Ordering::AcqRel); // 偶→奇
        debug_assert!(
            cur.is_multiple_of(2),
            "写入中途重入——协议违反(上次写入未 commit/rollback)"
        );
        // SAFETY: 单写者约定（T1 唯一）+ begin/commit 期间 T2 读者对该缓冲
        // 只在 seq 为偶时进入（seq 已为奇 = 互斥达成）。同进程内、同帧池
        // 生命周期内唯一可变借用——等价于独占所有权下的 &mut。
        // as_mut_ptr 经 &self 取裸指针合法（Vec 布局稳定）。
        let buf = unsafe {
            let this = self as *const Self as *mut Self;
            let ptr = (*this).bufs[idx].as_mut_ptr();
            std::slice::from_raw_parts_mut(ptr, self.bufs[idx].len())
        };
        (idx, buf)
    }

    /// 写者两段式（二）：发布（seq 奇→偶 + active 翻转 + gen 递增）——共享版。
    pub fn commit_write_shared(&self, idx: usize) -> u64 {
        self.seq[idx].fetch_add(1, Ordering::AcqRel); // 奇→偶：数据可见
        self.active.store(idx, Ordering::Release);
        self.gen.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// 写者两段式（失败路径）：回滚未完成的写入（seq 奇→偶，帧不发布）——共享版。
    pub fn rollback_write_shared(&self, idx: usize) {
        let cur = self.seq[idx].fetch_add(1, Ordering::AcqRel);
        debug_assert!(!cur.is_multiple_of(2), "rollback 未持锁的缓冲——协议违反");
    }

    /// 写者两段式（一）：锁非活跃缓冲（seq 偶→奇）。
    ///
    /// 返回 (缓冲下标, 可写切片)——调用方把 IO 直读进这里，零拷贝。
    /// 半途失败必须 [`rollback_write`]（seq 回偶），否则下次 begin_write 会断言。
    pub fn begin_write(&mut self) -> (usize, &mut [u8]) {
        let idx = 1 - self.active.load(Ordering::Acquire);
        let cur = self.seq[idx].fetch_add(1, Ordering::AcqRel); // 偶→奇
        debug_assert!(
            cur.is_multiple_of(2),
            "写入中途重入——协议违反(上次写入未 commit/rollback)"
        );
        let buf = &mut self.bufs[idx];
        (idx, &mut buf[..])
    }

    /// 写者两段式（二）：发布（seq 奇→偶 + active 翻转 + gen 递增）。
    pub fn commit_write(&mut self, idx: usize) -> u64 {
        self.commit_locked(idx)
    }

    /// 写者两段式（失败路径）：回滚未完成的写入（seq 奇→偶，帧不发布）。
    pub fn rollback_write(&mut self, idx: usize) {
        let cur = self.seq[idx].fetch_add(1, Ordering::AcqRel);
        debug_assert!(!cur.is_multiple_of(2), "rollback 未持锁的缓冲——协议违反");
    }

    /// 写者：发布缓冲为新帧（写完数据后调用；seq 奇→偶发布，再翻 active）。
    fn commit_locked(&mut self, idx: usize) -> u64 {
        self.seq[idx].fetch_add(1, Ordering::AcqRel); // 奇→偶：数据可见
        self.active.store(idx, Ordering::Release);
        self.gen.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// 读者：取最新帧句柄。`gen=0`（尚无帧）返回 `None`。
    pub fn read_latest(&self) -> Option<FrameRef> {
        for _ in 0..64 {
            let gen = self.gen.load(Ordering::Acquire);
            if gen == 0 {
                return None;
            }
            let idx = self.active.load(Ordering::Acquire);
            let seq = self.seq[idx].load(Ordering::Acquire);
            if !seq.is_multiple_of(2) {
                std::hint::spin_loop(); // active 恰在翻转瞬间——重试
                continue;
            }
            if self.gen.load(Ordering::Acquire) != gen {
                continue; // 期间又有新帧——重来拿更新的
            }
            return Some(FrameRef { idx, seq, gen });
        }
        None
    }

    /// 读者：借 Y 平面（亮度=灰度，运动检测直接消费，零拷贝零转换）。
    ///
    /// 借用要短（DESIGN.md §2：运动检测 ~2ms << 40ms 宽限）；
    /// seq 前后复核，撕裂则 `None`（丢弃本帧，latest-wins 下无害）。
    pub fn borrow_y<F, R>(&self, fr: &FrameRef, f: F) -> Option<R>
    where
        F: FnOnce(&[u8]) -> R,
    {
        let before = self.seq[fr.idx].load(Ordering::Acquire);
        if before != fr.seq || !before.is_multiple_of(2) {
            return None;
        }
        let y_len = self.width * self.height;
        let y = &self.bufs[fr.idx][..y_len];
        let r = f(y);
        let after = self.seq[fr.idx].load(Ordering::Acquire);
        if after != fr.seq {
            return None;
        }
        Some(r)
    }

    /// 读者：整帧 NV12 拷入调用方 scratch（推理前唯一一次物理拷贝，ADR-010）。
    ///
    /// 目标由调用方预分配（InferScratch）——本函数零分配（I1）。
    pub fn copy_nv12_to(&self, fr: &FrameRef, dst: &mut [u8]) -> bool {
        let before = self.seq[fr.idx].load(Ordering::Acquire);
        if before != fr.seq || !before.is_multiple_of(2) {
            return false;
        }
        debug_assert!(dst.len() >= self.frame_size, "scratch 太小");
        let n = self.frame_size.min(dst.len());
        dst[..n].copy_from_slice(&self.bufs[fr.idx][..n]);
        self.seq[fr.idx].load(Ordering::Acquire) == fr.seq
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    fn slot() -> LatestFrameSlot {
        LatestFrameSlot::new(64, 36)
    }

    /// 基本协议：首帧前 None，写后读到最新帧与正确代数。
    #[test]
    fn publish_and_read_latest() {
        let mut s = slot();
        assert!(s.read_latest().is_none(), "gen=0 时应无帧");
        let g = s.write_val(10);
        assert_eq!(g, 1);
        let fr = s.read_latest().expect("应有帧");
        assert_eq!(fr.gen, 1);
        s.borrow_y(&fr, |y| assert!(y.iter().all(|&b| b == 10)))
            .expect("seq 应一致");
    }

    /// latest-wins：连写三帧，读者只见最新。
    #[test]
    fn latest_wins() {
        let mut s = slot();
        s.write_val(1);
        s.write_val(2);
        let g = s.write_val(3);
        assert_eq!(g, 3);
        let fr = s.read_latest().unwrap();
        assert_eq!(fr.gen, 3);
        s.borrow_y(&fr, |y| assert!(y.iter().all(|&b| b == 3)))
            .unwrap();
    }

    /// copy_nv12_to 拷完整帧且数据正确（推理唯一拷贝路径）。
    #[test]
    fn copy_to_scratch() {
        let mut s = slot();
        s.write_val(0xAB);
        let fr = s.read_latest().unwrap();
        let mut scratch = vec![0u8; s.frame_size()];
        assert!(s.copy_nv12_to(&fr, &mut scratch));
        assert!(scratch.iter().all(|&b| b == 0xAB));
    }

    /// 并发压力：写者狂写 + 读者狂读——无 panic、无撕裂（seq 复核兜底）。
    ///
    /// 生产路径是"写者线程独占 &mut Slot"（单写者）。本测试用原始指针
    /// 模拟写者线程与读者线程并发持有同一 Slot 的最坏情况；unsafe 仅出现
    /// 在测试（生产由所有权保证：写者独占 &mut，读者经 Arc 只读方法）。
    #[test]
    fn concurrent_smoke() {
        let s = Arc::new(slot());
        let stop = Arc::new(AtomicBool::new(false));
        // 初始帧：原始指针写一次（仅测试；生产由独占写者线程完成）
        unsafe {
            (*(Arc::as_ptr(&s) as *mut LatestFrameSlot)).write_val(0);
        }
        let writer = {
            let s = s.clone();
            let stop = stop.clone();
            std::thread::spawn(move || {
                let p = Arc::as_ptr(&s) as *mut LatestFrameSlot;
                let mut v = 0u8;
                while !stop.load(Ordering::Relaxed) {
                    unsafe {
                        (*p).write_val(v);
                    }
                    v = v.wrapping_add(1);
                }
            })
        };
        let reader = {
            let s = s.clone();
            let stop = stop.clone();
            std::thread::spawn(move || {
                let mut ok = 0usize;
                let mut stale = 0usize; // seq 变了=该帧已被覆盖，数据陈旧但**不是撕裂**
                let mut torn = 0usize; // 值不自洽=真撕裂（协议 bug 才会出现）
                while !stop.load(Ordering::Relaxed) {
                    if let Some(fr) = s.read_latest() {
                        match s.borrow_y(&fr, |y| {
                            let first = y[0];
                            y.iter().all(|&b| b == first)
                        }) {
                            Some(true) => ok += 1,
                            // None = 借用期间写者翻转覆盖：数据是"旧帧被覆盖"，
                            // 读者拿到的是完整写入的某个中间态——latest-wins 语义
                            // 下合法，丢弃即可。撕裂的判据是 Some(false)：
                            // 值不自洽说明读到半帧，那才是协议错误。
                            None => stale += 1,
                            Some(false) => torn += 1,
                        }
                    }
                }
                (ok, stale, torn)
            })
        };
        std::thread::sleep(std::time::Duration::from_millis(200));
        stop.store(true, Ordering::Relaxed);
        writer.join().unwrap();
        let (ok, stale, torn) = reader.join().unwrap();
        assert!(ok > 100, "读者应有充足成功读取，实际 {ok}");
        assert_eq!(torn, 0, "值不自洽(真撕裂)必须为 0——协议错误");
        let _ = stale; // 陈旧读取是合法竞争，只统计不断言
    }
}
