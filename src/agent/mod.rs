//! AI 运维 Agent（DESIGN.md §11）——对齐 AIGX `src/agent/` 的分层。
//!
//! P5 落地范围（与 AIGX agent 对齐）：
//! - [`tools`]：NVR 工具注册表（只读/低危/高危三层风险，抄 AIGX tools.rs）
//! - [`runner`]：多轮循环骨架（LLM 推理 → 工具调用 → 回填 → Final）
//! - [`approval`]：审批矩阵（高危写挂起，抄 AIGX approval.rs）
//! - [`session`]：会话持久化（标题/角色/消息，抄 AIGX session.rs）
//! - [`llm`]：自环推理（OpenAI 兼容 chat，走 AIGX 网关渠道）
//!
//! I5 强制：agent 是控制面模块，只消费事件流/查投影器，不 import 数据面内部结构。
//! 安全底线：写操作分低危（自动+审计）与高危（审批矩阵），默认只读观察员。

pub mod approval;
pub mod llm;
pub mod runner;
pub mod session;
pub mod tools;

use std::sync::Arc;

/// Agent 工具执行上下文：查询投影器/设备 + 记录 AgentAction 事件（审计）。
///
/// P5 桩实现：查询返回样例数据；写操作记录事件。P6 接真实 handler
///（查 aivx-net DeviceAdapter + 控制面投影器）。
#[derive(Clone, Default)]
pub struct ActionContext {
    /// 记录动作（agent_action 事件留痕）——P5 桩。
    actions: Arc<std::sync::Mutex<Vec<String>>>,
}

impl ActionContext {
    pub fn stub() -> Self {
        Self::default()
    }

    pub fn devices_summary(&self) -> String {
        "设备列表（P6 接真实设备表）：cam-1 TP-LINK 在线 录像中；cam-2 离线".into()
    }

    pub fn alarms_summary(&self, limit: usize) -> String {
        format!("报警列表（P6 接投影器）：最近 {limit} 条——暂无（桩）")
    }

    pub fn diagnostics_summary(&self) -> String {
        "系统体检（P6 接 metrics）：cam-1 流健康 OK 分析FPS 16 延迟 64ms；cam-2 断流重连中".into()
    }

    /// 写工具审计：记录 AgentAction 事件文本（P6 发进事件链）。
    pub fn record_action(&self, tool: &str, args: &serde_json::Value, note: &str) {
        let text = format!("{tool} {args} {note}");
        self.actions.lock().unwrap().push(text);
    }

    /// 测试辅助：已记录动作数。
    pub fn actions_count(&self) -> usize {
        self.actions.lock().unwrap().len()
    }
}
