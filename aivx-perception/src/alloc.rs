//! I1 机器强制：分配计数分配器 + 热路径零分配断言（DESIGN.md §0/§19）。
//!
//! 无外部依赖（dhat 是重依赖且要 nightly 的部分功能）：自定义 `GlobalAlloc`
//! 包装 System，`AtomicU64` 计数。测试里 `count_scope` 包住一段热路径，
//! 断言 `allocs == expected`。
//!
//! 用法（测试内）：
//! ```ignore
//! let (n_allocs, n_bytes) = alloc::count_scope(|| {
//!     let boxes = motion.detect(&static_frame);
//! });
//! assert_eq!(n_allocs, 0, "静止帧热路径必须零分配");
//! ```

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

/// 全局分配计数（进程唯一；测试读它做断言）。
pub static ALLOCS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
/// 开关：计数器只在 scope 内开（避免其他线程干扰断言）。
static COUNTING: AtomicU64 = AtomicU64::new(0);

pub struct CountingAlloc;

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) == 1 {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        System.alloc(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) == 1 {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(new_size as u64, Ordering::Relaxed);
        }
        System.realloc(ptr, layout, new_size)
    }
}

/// 测量 scope 内的分配次数与字节数。
pub fn count_scope<T>(f: impl FnOnce() -> T) -> (u64, u64) {
    ALLOCS.store(0, Ordering::Relaxed);
    BYTES.store(0, Ordering::Relaxed);
    COUNTING.store(1, Ordering::Relaxed);
    let out = f();
    COUNTING.store(0, Ordering::Relaxed);
    (
        ALLOCS.load(Ordering::Relaxed),
        BYTES.load(Ordering::Relaxed),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// I1 断言方法自检：Vec 分配能被计数。
    #[test]
    fn counter_counts() {
        let (n, bytes) = count_scope(|| {
            let v: Vec<u64> = (0..100).collect();
            v.len()
        });
        assert!(n >= 1, "Vec 分配必须被计数");
        assert!(bytes >= 800);
    }
}
