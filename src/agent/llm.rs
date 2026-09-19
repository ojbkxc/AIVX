//! 自环 LLM 推理（DESIGN.md §11）——对齐 AIGX `src/agent/llm.rs`。
//!
//! 走 OpenAI 兼容 API（可接 AIGX 网关渠道）。P5 用可注入 provider 桩；
//! P6 接 reqwest + AIGX 网关（config.agent.model 指定模型）。

use serde_json::Value;

/// LLM 对话角色。
#[derive(Debug, Clone)]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// 对话消息。
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

/// 工具调用。
#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub function_name: String,
    pub arguments: String,
}

/// 一条消息里可能带工具调用。
#[derive(Debug, Clone)]
pub struct LlmMessage {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
}

/// LLM 推理接口（P5 桩；P6 接真实 OpenAI 兼容 API）。
pub trait ChatProvider: Send + Sync {
    fn chat(&self, messages: &[ChatMessage], tools: Option<&[Value]>)
        -> Result<LlmMessage, String>;
}

/// 桩 provider：收到带工具的消息返回一个固定的工具调用，否则返回文本。
/// 用于驱动 runner 多轮循环测试。
#[derive(Default)]
pub struct StubProvider;

impl ChatProvider for StubProvider {
    fn chat(
        &self,
        messages: &[ChatMessage],
        _tools: Option<&[Value]>,
    ) -> Result<LlmMessage, String> {
        // 驱动 runner：第一轮（仅用户消息）请求工具；之后（含工具结果回填）
        // 返回最终答复——按消息数而非内容关键字判断，避免用户输入误触发。
        if messages.len() <= 1 {
            Ok(LlmMessage {
                content: String::new(),
                tool_calls: vec![ToolCall {
                    id: "call-1".into(),
                    function_name: "nvr_list_devices".into(),
                    arguments: "{}".into(),
                }],
            })
        } else {
            Ok(LlmMessage {
                content: "已完成查询，这是结果。".into(),
                tool_calls: vec![],
            })
        }
    }
}

/// OpenAI 兼容工具 schema 构造（供 P6 传给真实 API）。
pub fn openai_tools(specs: &[crate::agent::tools::ToolSpec]) -> Vec<Value> {
    specs
        .iter()
        .map(|s| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": s.name,
                    "description": s.description,
                    "parameters": s.schema,
                }
            })
        })
        .collect()
}

/// 构造消息。
pub fn system_message(content: String) -> ChatMessage {
    ChatMessage {
        role: Role::System,
        content,
    }
}

pub fn user_message(content: String) -> ChatMessage {
    ChatMessage {
        role: Role::User,
        content,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_provider_tool_then_final() {
        let p = StubProvider;
        // 首条消息（1 条）→ 返回工具调用
        let r = p.chat(&[user_message("查设备".into())], None).unwrap();
        assert_eq!(r.tool_calls.len(), 1);
        assert_eq!(r.tool_calls[0].function_name, "nvr_list_devices");
        // 含工具结果回填（多条）→ 返回文本
        let r2 = p
            .chat(
                &[
                    user_message("查设备".into()),
                    ChatMessage {
                        role: Role::Tool,
                        content: "工具结果".into(),
                    },
                ],
                None,
            )
            .unwrap();
        assert!(r2.tool_calls.is_empty());
        assert!(r2.content.contains("结果"));
    }

    #[test]
    fn openai_tools_serialize() {
        let specs = crate::agent::tools::tool_specs();
        let tools = openai_tools(&specs);
        assert!(!tools.is_empty());
        // 每个工具都是 function 类型
        assert!(tools.iter().all(|t| t["type"] == "function"));
        assert!(tools.iter().all(|t| t["function"]["name"].is_string()));
    }
}
