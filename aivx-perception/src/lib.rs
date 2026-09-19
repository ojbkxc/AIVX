//! aivx-perception —— 数据面（DESIGN.md §2-§4）。
//!
//! I5/I11：本 crate **不依赖 tokio**（CI 用 `cargo tree -i tokio -p aivx-perception`
//! 断言为空）。数据面 = 同步 OS 线程 + 无锁结构 + latest-wins；唯一的出口是
//! [`PlaneBridge`]（std::sync_channel + AtomicU64 指标 + AtomicBool 控制位）。
//!
//! P0 落地组件（DESIGN.md §23）：
//! - [`frame::LatestFrameSlot`] —— seqlock 双缓冲（ADR-009）
//! - [`bridge::PlaneBridge`] —— 平面桥：分级事件 + spill 背压（ADR-020/022）
//! - [`motion::EmaMotion`] —— EMA 运动门控骨架（P1 填完整实现）
//! - 分配断言测试（I1）：单帧热路径 0 堆分配

pub mod analyze;
pub mod bridge;
pub mod frame;
pub mod motion;
pub mod stream;

/// 单调钟纳秒（数据面唯一时钟，DESIGN.md §18 混合时钟）。
/// 用不起 std::time::Instant 的跨线程传递，直接拿绝对纳秒。
pub fn mono_ns() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}
