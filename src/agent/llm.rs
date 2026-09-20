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

/// 真实 OpenAI 兼容 chat provider（P8e）：POST {api_url}/chat/completions。
/// 凭据从 env 读（AIVX_LLM_URL / AIVX_LLM_KEY / AIVX_LLM_MODEL——不入库）；
/// 走 AIGX 网关渠道（OpenAI 兼容）。reqwest blocking：runner 是同步循环，
/// HTTP 层用 spawn_blocking 包裹（同 forwarder 模式）。
pub struct OpenAiChatProvider {
    api_url: String,
    api_key: String,
    model: String,
    http: reqwest::blocking::Client,
}

impl OpenAiChatProvider {
    /// env 未配全时返回 None（调用方回落 StubProvider）。
    pub fn from_env() -> Option<Self> {
        let api_url = std::env::var("AIVX_LLM_URL").ok()?;
        let api_key = std::env::var("AIVX_LLM_KEY").ok()?;
        let model = std::env::var("AIVX_LLM_MODEL")
            .ok()
            .unwrap_or_else(|| "gpt-4o-mini".into());
        if api_url.is_empty() {
            return None;
        }
        Some(Self {
            api_url,
            api_key,
            model,
            http: reqwest::blocking::Client::new(),
        })
    }

    fn role_str(r: &Role) -> &'static str {
        match r {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        }
    }
}

impl ChatProvider for OpenAiChatProvider {
    fn chat(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[Value]>,
    ) -> Result<LlmMessage, String> {
        let msgs: Vec<Value> = messages
            .iter()
            .map(|m| serde_json::json!({"role": Self::role_str(&m.role), "content": m.content}))
            .collect();
        let mut payload = serde_json::json!({"model": self.model, "messages": msgs});
        if let Some(ts) = tools {
            payload["tools"] = serde_json::Value::Array(ts.to_vec());
            payload["tool_choice"] = serde_json::json!("auto");
        }
        let url = format!("{}/chat/completions", self.api_url.trim_end_matches('/'));
        let resp = self
            .http
            .post(&url)
            .header("Content-Type", "application/json")
            .bearer_auth(&self.api_key)
            .json(&payload)
            .send()
            .map_err(|e| format!("LLM 请求失败: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("LLM HTTP {}", resp.status()));
        }
        let body: Value = resp.json().map_err(|e| format!("LLM 响应解析失败: {e}"))?;
        let msg = &body["choices"][0]["message"];
        let content = msg["content"].as_str().unwrap_or("").to_string();
        let mut tool_calls = Vec::new();
        if let Some(calls) = msg["tool_calls"].as_array() {
            for (i, c) in calls.iter().enumerate() {
                tool_calls.push(ToolCall {
                    id: c["id"]
                        .as_str()
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| format!("call-{i}")),
                    function_name: c["function"]["name"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    arguments: c["function"]["arguments"]
                        .as_str()
                        .unwrap_or("{}")
                        .to_string(),
                });
            }
        }
        Ok(LlmMessage {
            content,
            tool_calls,
        })
    }
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
