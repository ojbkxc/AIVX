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
pub mod data;
pub mod llm;
pub mod runner;
pub mod session;
pub mod tools;

use std::sync::Arc;

/// Agent 工具执行上下文：查真实数据源 + 记录 AgentAction 事件（审计）。
///
/// P8 去桩：`data: SharedDataSource`（设备/报警/录像投影器），
/// `record_action` 发 AgentAction 事件进事件链（审计天然完整）。
#[derive(Clone)]
pub struct ActionContext {
    /// 数据源（设备/报警/录像查询）。
    data: data::SharedDataSource,
    /// 记录动作（agent_action 事件留痕）。
    actions: Arc<std::sync::Mutex<Vec<String>>>,
}

impl Default for ActionContext {
    fn default() -> Self {
        Self {
            data: Arc::new(data::MemDataSource::new()),
            actions: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }
}

impl ActionContext {
    pub fn stub() -> Self {
        Self::default()
    }

    /// 用测试数据源构造（供工具测试/集成测试）。
    pub fn with_data(data: data::SharedDataSource) -> Self {
        Self {
            data,
            actions: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    pub fn devices_summary(&self) -> String {
        let devices = self.data.list_devices();
        if devices.is_empty() {
            return "暂无设备".into();
        }
        let mut out = String::from("设备列表：");
        for d in devices {
            out.push_str(&format!(
                "\n- {} ({}) 状态={} 接入={} 录像={} PTZ={}",
                d.name, d.id, d.status, d.access_type, d.recording, d.ptz_supported
            ));
        }
        out
    }

    pub fn alarms_summary(&self, limit: usize) -> String {
        let alarms = self.data.list_alarms(limit);
        if alarms.is_empty() {
            return format!("最近 {limit} 条报警：无");
        }
        let mut out = format!("最近 {} 条报警：", alarms.len());
        for a in alarms {
            out.push_str(&format!(
                "\n- [{}] {} 设备={} {}",
                a.event_type, a.ts, a.device_id, a.description
            ));
        }
        out
    }

    pub fn search_recording_summary(&self, device_id: &str, start_ts: i64, end_ts: i64) -> String {
        let recs = self.data.search_recording(device_id, start_ts, end_ts);
        if recs.is_empty() {
            return format!("设备 {device_id} 在区间内无录像片段");
        }
        let mut out = format!("设备 {device_id} 录像 {} 段：", recs.len());
        for r in recs {
            out.push_str(&format!(
                "\n- {} 起点={} 时长={}秒",
                r.file_path, r.start_ts, r.duration_secs
            ));
        }
        out
    }

    pub fn diagnostics_summary(&self) -> String {
        self.data.diagnostics()
    }

    /// 写工具审计：记录 AgentAction 事件文本。
    pub fn record_action(&self, tool: &str, args: &serde_json::Value, note: &str) {
        let text = format!("{tool} {args} {note}");
        self.actions.lock().unwrap().push(text);
    }

    /// 测试辅助：已记录动作数。
    pub fn actions_count(&self) -> usize {
        self.actions.lock().unwrap().len()
    }
}
