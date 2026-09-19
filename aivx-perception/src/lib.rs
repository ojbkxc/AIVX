//! aivx-perception —— 数据面（DESIGN.md §2-§4）。
//!
//! I5/I11：本 crate **不依赖 tokio**（CI 用 `cargo tree -i tokio -p aivx-perception`
//! 断言为空）。数据面 = 同步 OS 线程 + 无锁结构 + latest-wins；唯一的出口是
//! [`bridge::PlaneBridge`]（std::sync_channel + AtomicU64 指标 + AtomicBool 控制位）。
//!
//! P0/P1/P2 组件（DESIGN.md §23）：
//! - [`frame::LatestFrameSlot`] —— seqlock 双缓冲（ADR-009）
//! - [`stream`] —— T1 拉流 + 断流状态机（ADR-019，std::process ffmpeg）
//! - [`analyze`] —— T2 热路径（I3 事件驱动推理）
//! - [`motion::EmaMotion`] —— EMA 运动门控（ADR-023 校准期）
//! - [`track::ByteTracker`] —— min_hits 防幽灵（抄 ai-nvr）
//! - [`rules`] —— 条件树 + 状态机（I8 去重，抄 ai-nvr/rebucca）
//! - [`alloc`] —— I1 分配计数分配器（热路径零分配断言）
//!
//! 全局分配器：`alloc::CountingAlloc`（I1 机器强制的载体）。

#[global_allocator]
static GLOBAL: crate::alloc::CountingAlloc = crate::alloc::CountingAlloc;

pub mod alloc;
pub mod analyze;
pub mod bridge;
pub mod config;
pub mod frame;
pub mod motion;
pub mod pool;
pub mod record;
pub mod rules;
pub mod stream;
pub mod track;
#[cfg(feature = "ort-yolo")]
pub mod yolo;

/// 单调钟纳秒（数据面唯一时钟，DESIGN.md §18 混合时钟）。
/// 用不起 std::time::Instant 的跨线程传递，直接拿绝对纳秒。
pub fn mono_ns() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}
