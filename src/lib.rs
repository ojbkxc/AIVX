//! aivx —— 控制面主 crate（DESIGN.md §5）。
//!
//! P0 组件：
//! - [`pipeline::forwarder`]：数据面 sync_channel → DbWriter（唯一消费者）
//! - [`pipeline::db_writer`]：单写者 + seq 分配 + **落库后顺序 fan-out**（ADR-022）
//! - [`pipeline::projector`]：事件 → 派生表投影 + checkpoint + 僵尸清扫（ADR-028）
//!
//! ADR-022 是 P0 最重要的验证点：事件链上不存在任何"可能丢"的环节。

pub mod agent;
pub mod auth;
pub mod cameras;
pub mod cognition;
pub mod cognition_llm;
pub mod config_store;
pub mod fmp4;
pub mod memory;
pub mod pipeline;
pub mod preview;
