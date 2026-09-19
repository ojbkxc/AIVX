//! Agent 多轮循环（DESIGN.md §11）——对齐 AIGX `src/agent/runner.rs`。
//!
//! LLM 推理 → 解析工具调用 → 审批（高危）→ 执行 → 回填，直到 Final。
//! 默认观察员角色：只读工具放行，写工具拒绝。

use std::sync::atomic::{AtomicU64, Ordering};

use crate::agent::approval::{approval_timeout_from, AgentApprovals, ApprovalResult};
use crate::agent::llm::{self, ChatMessage, ChatProvider, LlmMessage};
use crate::agent::session::AgentRole;
use crate::agent::tools::{self, RiskLevel};
use crate::agent::ActionContext;

/// 请求序号（审批 request_id 唯一性）。
static REQ_SEQ: AtomicU64 = AtomicU64::new(0);

pub fn next_req_seq() -> u64 {
    REQ_SEQ.fetch_add(1, Ordering::Relaxed)
}

/// 一轮事件（前端流式渲染）。
#[derive(Debug, Clone)]
pub enum AgentEvent {
    Thinking {
        turn: usize,
    },
    ToolCall {
        name: String,
        arguments: String,
    },
    ToolResult {
        name: String,
        ok: bool,
        text: String,
    },
    ApprovalRequest {
        request_id: String,
        name: String,
        arguments: String,
    },
    ApprovalResolved {
        name: String,
        approved: bool,
    },
    Final {
        content: String,
    },
    Error {
        message: String,
    },
}

/// 运行多轮循环（同步骨架；P6 包装为 async + SSE）。
pub fn run(
    provider: &dyn ChatProvider,
    approvals: &AgentApprovals,
    ctx: &ActionContext,
    session_id: &str,
    role: AgentRole,
    messages: Vec<ChatMessage>,
    max_turns: usize,
    approval_timeout_secs: u64,
) -> Vec<AgentEvent> {
    let mut convo = messages;
    let mut events = Vec::new();
    let timeout = approval_timeout_from(approval_timeout_secs);
    let _ = timeout;

    for turn in 1..=max_turns {
        events.push(AgentEvent::Thinking { turn });

        // 工具 schema 传给 provider
        let tools = tools::openai_tools(&tools::tool_specs());
        let resp: LlmMessage = match provider.chat(&convo, Some(&tools)) {
            Ok(r) => r,
            Err(e) => {
                events.push(AgentEvent::Error { message: e });
                return events;
            }
        };

        if resp.tool_calls.is_empty() {
            events.push(AgentEvent::Final {
                content: resp.content.clone(),
            });
            return events;
        }

        // 把 assistant 消息（含工具调用）加入对话
        convo.push(llm::ChatMessage {
            role: llm::Role::Assistant,
            content: resp.content.clone(),
        });

        for call in &resp.tool_calls {
            events.push(AgentEvent::ToolCall {
                name: call.function_name.clone(),
                arguments: call.arguments.clone(),
            });

            let spec = tools::find_tool(&call.function_name);
            let outcome = match spec {
                None => tools::ToolOutcome {
                    text: format!("未知工具: {}", call.function_name),
                    ok: false,
                },
                Some(ref s) if !role.allows_write() && s.risk != RiskLevel::ReadOnly => {
                    tools::ToolOutcome {
                        text: format!("观察员禁止写工具 {}", call.function_name),
                        ok: false,
                    }
                }
                Some(s) if s.risk == RiskLevel::HighRisk => {
                    // 审批矩阵：挂起等人工确认；超时/拒绝则返回失败
                    let already = approvals.is_remembered(session_id, &call.function_name);
                    let result = if already {
                        ApprovalResult::Approved
                    } else {
                        let (req_id, rx) = approvals.request(&call.function_name);
                        events.push(AgentEvent::ApprovalRequest {
                            request_id: req_id,
                            name: call.function_name.clone(),
                            arguments: call.arguments.clone(),
                        });
                        // P5 同步桩：审批由测试 resolve；超时视为拒绝
                        rx.recv_timeout(approval_timeout_from(approval_timeout_secs))
                            .unwrap_or(ApprovalResult::Denied)
                    };
                    if result == ApprovalResult::RememberAllow {
                        approvals.remember(session_id, &call.function_name);
                    }
                    let approved = result != ApprovalResult::Denied;
                    events.push(AgentEvent::ApprovalResolved {
                        name: call.function_name.clone(),
                        approved,
                    });
                    if !approved {
                        tools::ToolOutcome {
                            text: "高危写操作被拒绝（未获人工审批）".into(),
                            ok: false,
                        }
                    } else {
                        tools::exec_tool(
                            ctx,
                            &call.function_name,
                            &serde_json::from_str(&call.arguments).unwrap_or_default(),
                        )
                    }
                }
                Some(s) => tools::exec_tool(
                    ctx,
                    &call.function_name,
                    &serde_json::from_str(&call.arguments).unwrap_or_default(),
                ),
            };

            events.push(AgentEvent::ToolResult {
                name: call.function_name.clone(),
                ok: outcome.ok,
                text: outcome.text.clone(),
            });

            // 回填工具结果到对话
            convo.push(llm::ChatMessage {
                role: llm::Role::Tool,
                content: outcome.text.clone(),
            });
        }
    }

    events.push(AgentEvent::Error {
        message: format!("已达最大轮数 {max_turns}，强制结束"),
    });
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::llm::StubProvider;

    fn user_msg(s: &str) -> ChatMessage {
        llm::user_message(s.into())
    }

    /// 观察员：只读工具执行，写工具被拒。
    #[test]
    fn observer_runs_readonly_tool() {
        let provider = StubProvider;
        let ap = AgentApprovals::new();
        let ctx = ActionContext::stub();
        let events = run(
            &provider,
            &ap,
            &ctx,
            "s-1",
            AgentRole::Observer,
            vec![user_msg("final 查设备")],
            5,
            0,
        );
        // StubProvider 会先请求 nvr_list_devices（只读）→ 再收到含 final 的
        // 工具结果 → 返回 Final。验证走到 Final。
        let has_final = events.iter().any(|e| matches!(e, AgentEvent::Final { .. }));
        assert!(has_final, "多轮循环应以 Final 结束");
        // 只读工具成功
        let has_ok = events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolResult { ok: true, .. }));
        assert!(has_ok);
    }

    /// 观察员：写工具被拒（低危也不放行）。
    #[test]
    fn observer_rejects_write_tool() {
        // 手动构造一次含 nvr_snapshot 的请求
        let ctx = ActionContext::stub();
        let ap = AgentApprovals::new();
        // 用自定义 provider 直接请求写工具
        struct WriteProvider;
        impl ChatProvider for WriteProvider {
            fn chat(&self, _: &[ChatMessage], _: Option<&[Value]>) -> Result<LlmMessage, String> {
                Ok(LlmMessage {
                    content: String::new(),
                    tool_calls: vec![llm::ToolCall {
                        id: "c1".into(),
                        function_name: "nvr_snapshot".into(),
                        arguments: "{\"device_id\":\"cam-1\"}".into(),
                    }],
                })
            }
        }
        let events = run(
            &WriteProvider,
            &ap,
            &ctx,
            "s-2",
            AgentRole::Observer,
            vec![user_msg("抓拍")],
            2,
            0,
        );
        let rejected = events.iter().any(|e| {
            matches!(e, AgentEvent::ToolResult { ok: false, text, .. } if text.contains("观察员禁止"))
        });
        assert!(rejected, "观察员应拒绝写工具");
        assert_eq!(ctx.actions_count(), 0, "写工具不得执行");
    }
}
