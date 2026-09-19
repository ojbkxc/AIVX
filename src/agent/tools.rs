//! Agent 工具注册表（DESIGN.md §11）——NVR 运维 Agent 的"手"。
//!
//! 对齐 AIGX `src/agent/tools.rs` 的三层风险分级：
//! - [`RiskLevel::ReadOnly`]：查/搜/诊断，Agent 自由调用，无审批。
//! - [`RiskLevel::LowRisk`]：低危写（启停分析/改布控/抓拍），自动审计留痕。
//! - [`RiskLevel::HighRisk`]：高危写（删设备/删录像/清库），须审批矩阵人工确认。
//!
//! 工具执行 = 进程内直调现有 handler（不经 HTTP 端口）——AIGX 同源思路。
//! 每个工具产生 [`Event::AgentAction`]（写操作）进事件链——审计天然完整
//!（DESIGN.md §11：Agent 改布控走 ConfigChanged 事件）。

use serde_json::{json, Value};

use crate::agent::ActionContext;

/// 工具风险等级（对齐 AIGX）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskLevel {
    ReadOnly,
    LowRisk,
    HighRisk,
}

impl RiskLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            RiskLevel::ReadOnly => "readonly",
            RiskLevel::LowRisk => "low",
            RiskLevel::HighRisk => "high",
        }
    }
}

/// 工具执行结果（喂回 LLM）。
#[derive(Debug, Clone)]
pub struct ToolOutcome {
    pub text: String,
    pub ok: bool,
}

/// 工具条目元数据。
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub schema: Value,
    pub risk: RiskLevel,
}

/// NVR 工具注册表。
pub fn tool_specs() -> Vec<ToolSpec> {
    let no_args = || json!({ "type": "object", "properties": {}, "required": [] });
    let paged = || {
        json!({
            "type": "object",
            "properties": {
                "page": { "type": "integer", "minimum": 1 },
                "size": { "type": "integer", "minimum": 1 },
            },
            "required": []
        })
    };
    vec![
        // ── 只读：查询/诊断 ──
        ToolSpec {
            name: "nvr_list_devices",
            description: "只读：列出所有摄像头设备（名称/状态/接入类型/是否录像中）",
            schema: paged(),
            risk: RiskLevel::ReadOnly,
        },
        ToolSpec {
            name: "nvr_list_alarms",
            description: "只读：按时间/设备/规则查报警历史",
            schema: json!({
                "type": "object",
                "properties": {
                    "device_id": {"type": "string"},
                    "rule_id": {"type": "string"},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 200},
                },
                "required": []
            }),
            risk: RiskLevel::ReadOnly,
        },
        ToolSpec {
            name: "nvr_search_recording",
            description: "只读：查某时间段的录像片段索引",
            schema: json!({
                "type": "object",
                "properties": {
                    "device_id": {"type": "string"},
                    "start_ts": {"type": "integer"},
                    "end_ts": {"type": "integer"},
                },
                "required": ["device_id"]
            }),
            risk: RiskLevel::ReadOnly,
        },
        ToolSpec {
            name: "nvr_diagnostics",
            description: "只读：系统体检摘要（各设备流健康/FPS/分析延迟/断流重连统计）",
            schema: no_args(),
            risk: RiskLevel::ReadOnly,
        },
        // ── 低危写：可回滚，自动审计 ──
        ToolSpec {
            name: "nvr_start_analysis",
            description: "低危：启动某设备分析（拉流+检测+规则）",
            schema: json!({ "type":"object","properties":{ "device_id":{"type":"string"} },"required":["device_id"] }),
            risk: RiskLevel::LowRisk,
        },
        ToolSpec {
            name: "nvr_stop_analysis",
            description: "低危：停止某设备分析（录像继续）",
            schema: json!({ "type":"object","properties":{ "device_id":{"type":"string"} },"required":["device_id"] }),
            risk: RiskLevel::LowRisk,
        },
        ToolSpec {
            name: "nvr_snapshot",
            description: "低危：立即抓拍某设备当前帧并保存",
            schema: json!({ "type":"object","properties":{ "device_id":{"type":"string"} },"required":["device_id"] }),
            risk: RiskLevel::LowRisk,
        },
        // ── 高危写：审批矩阵 ──
        ToolSpec {
            name: "nvr_delete_device",
            description: "高危：删除摄像头设备（含其录像索引与报警记录）",
            schema: json!({ "type":"object","properties":{ "device_id":{"type":"string"} },"required":["device_id"] }),
            risk: RiskLevel::HighRisk,
        },
        ToolSpec {
            name: "nvr_delete_recording",
            description: "高危：删除指定时间段的录像文件",
            schema: json!({
                "type": "object",
                "properties": {
                    "device_id": {"type": "string"},
                    "start_ts": {"type": "integer"},
                    "end_ts": {"type": "integer"},
                },
                "required": ["device_id"]
            }),
            risk: RiskLevel::HighRisk,
        },
    ]
}

/// 按名查工具。
pub fn find_tool(name: &str) -> Option<ToolSpec> {
    tool_specs().into_iter().find(|t| t.name == name)
}

/// 执行工具（进程内直调，不经 HTTP）。`ctx` 提供数据源 + 审计。
///
/// 只读工具查真实数据源（投影器/设备表）；写工具发 AgentAction 事件（审计）。
/// P8：不再返回占位文本。
pub fn exec_tool(ctx: &ActionContext, name: &str, args: &Value) -> ToolOutcome {
    match name {
        "nvr_list_devices" => ToolOutcome {
            text: ctx.devices_summary(),
            ok: true,
        },
        "nvr_list_alarms" => {
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(20) as usize;
            ToolOutcome {
                text: ctx.alarms_summary(limit),
                ok: true,
            }
        }
        "nvr_search_recording" => {
            let device_id = args.get("device_id").and_then(Value::as_str).unwrap_or("");
            let start_ts = args.get("start_ts").and_then(Value::as_i64).unwrap_or(0);
            let end_ts = args
                .get("end_ts")
                .and_then(Value::as_i64)
                .unwrap_or(i64::MAX);
            ToolOutcome {
                text: ctx.search_recording_summary(device_id, start_ts, end_ts),
                ok: true,
            }
        }
        "nvr_diagnostics" => ToolOutcome {
            text: ctx.diagnostics_summary(),
            ok: true,
        },
        // 写工具：发 AgentAction 事件（审计）+ 返回真实指令结果描述。
        "nvr_start_analysis" => {
            ctx.record_action(name, args, "启动分析");
            let device_id = args.get("device_id").and_then(Value::as_str).unwrap_or("");
            ToolOutcome {
                text: format!("已发出启动分析指令：设备 {device_id}"),
                ok: true,
            }
        }
        "nvr_stop_analysis" => {
            ctx.record_action(name, args, "停止分析");
            let device_id = args.get("device_id").and_then(Value::as_str).unwrap_or("");
            ToolOutcome {
                text: format!("已发出停止分析指令：设备 {device_id}"),
                ok: true,
            }
        }
        "nvr_snapshot" => {
            ctx.record_action(name, args, "抓拍");
            let device_id = args.get("device_id").and_then(Value::as_str).unwrap_or("");
            ToolOutcome {
                text: format!("已发出抓拍指令：设备 {device_id}"),
                ok: true,
            }
        }
        "nvr_delete_device" => {
            ctx.record_action(name, args, "删除设备");
            let device_id = args.get("device_id").and_then(Value::as_str).unwrap_or("");
            ToolOutcome {
                text: format!("高危删除已记录：设备 {device_id}"),
                ok: true,
            }
        }
        _ => ToolOutcome {
            text: format!("未知工具: {name}"),
            ok: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 三层风险分级：只读可自由调；低危自动审计；高危审批。
    #[test]
    fn risk_levels_are_classified() {
        assert_eq!(
            find_tool("nvr_list_devices").unwrap().risk,
            RiskLevel::ReadOnly
        );
        assert_eq!(
            find_tool("nvr_start_analysis").unwrap().risk,
            RiskLevel::LowRisk
        );
        assert_eq!(
            find_tool("nvr_delete_device").unwrap().risk,
            RiskLevel::HighRisk
        );
    }

    /// 只读工具执行成功返回文本（真实数据源）。
    #[test]
    fn readonly_tool_returns_summary() {
        let ctx = ActionContext::with_data(crate::agent::data::test_source());
        let out = exec_tool(&ctx, "nvr_list_devices", &json!({}));
        assert!(out.ok);
        assert!(out.text.contains("后院"));
        assert!(out.text.contains("前门"));
    }

    /// 报警查询返回真实数据（非占位）。
    #[test]
    fn alarms_query_returns_real_data() {
        let ctx = ActionContext::with_data(crate::agent::data::test_source());
        let out = exec_tool(&ctx, "nvr_list_alarms", &json!({"limit": 5}));
        assert!(out.ok);
        assert!(out.text.contains("entered_zone"));
        assert!(out.text.contains("后院"));
    }

    /// 录像查询：区间过滤。
    #[test]
    fn recording_search_filters_by_range() {
        let source = std::sync::Arc::new(crate::agent::data::MemDataSource::new().with_device(
            crate::agent::data::DeviceEntry {
                id: "cam-1".into(),
                name: "后院".into(),
                status: "online".into(),
                access_type: "onvif".into(),
                recording: true,
                ptz_supported: false,
            },
        ));
        // 无录像数据 → 返回"无片段"
        let ctx = ActionContext::with_data(source);
        let out = exec_tool(
            &ctx,
            "nvr_search_recording",
            &json!({"device_id": "cam-1", "start_ts": 0, "end_ts": 9999}),
        );
        assert!(out.ok);
        assert!(out.text.contains("无录像片段"));
    }

    /// 写工具产生 AgentAction 事件（审计留痕）。
    #[test]
    fn write_tool_records_action() {
        let ctx = ActionContext::stub();
        let out = exec_tool(&ctx, "nvr_snapshot", &json!({"device_id":"cam-1"}));
        assert!(out.ok);
        // 事件链里应有一条 AgentAction
        assert_eq!(ctx.actions_count(), 1);
    }
}
