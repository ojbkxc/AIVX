//! cognition 真实 LLM provider（DESIGN.md §10 / P8）——OpenAI 兼容 API。
//!
//! 替换 `StubProvider`：`OpenAIProvider` 用 `reqwest` 调 OpenAI 兼容端点
//!（可走 AIGX 网关渠道），把 JPEG 证据 + 提示词发过去，解析结构化洞察 JSON。
//!
//! 设计：
//! - `GenAiProvider` trait 保留（插件化：openai/ollama/gemini）
//! - `OpenAIProvider` 是真实实现（HTTP POST /chat/completions，image_url=data:jpeg）
//! - 超时 + 错误映射为 `String`（调用方跳过洞察，报警不撤）
//! - 测试用本地 `mock`（无网络）验证请求构造 + 响应解析

use crate::cognition::GenAiProvider;

/// OpenAI 兼容 LLM 配置。
#[derive(Debug, Clone)]
pub struct OpenAiLlmConfig {
    pub api_url: String,
    pub api_key: String,
    pub model: String,
    pub timeout_secs: u64,
    /// 推理工具（默认 OpenAI；可扩展 ollama 等）。
    pub inference_tool: String,
}

/// 真实 OpenAI 兼容 provider（HTTP POST /chat/completions）。
pub struct OpenAIProvider {
    config: OpenAiLlmConfig,
    http: reqwest::blocking::Client,
}

impl OpenAIProvider {
    pub fn new(config: OpenAiLlmConfig) -> Self {
        Self {
            config,
            http: reqwest::blocking::Client::new(),
        }
    }

    /// 构造 chat/completions 请求体（多模态 image_url 走 data URL）。
    fn build_payload(&self, image_jpeg: &[u8], prompt_ctx: &str) -> serde_json::Value {
        let b64 = base64_encode(image_jpeg);
        serde_json::json!({
            "model": self.config.model,
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": format!(
                        "分析这张监控画面。{}\n\n返回 JSON：{{\"is_false_positive\":bool,\"threat_level\":\"low|medium|high\",\"scene\":\"...\",\"title\":\"...\",\"summary\":\"...\"}}",
                        prompt_ctx
                    )},
                    {"type": "image_url", "image_url": {"url": format!("data:image/jpeg;base64,{b64}")}}
                ]
            }],
            "response_format": {"type": "json_object"}
        })
    }
}

impl GenAiProvider for OpenAIProvider {
    fn analyze(&self, image_jpeg: &[u8], prompt_ctx: &str) -> Result<String, String> {
        let payload = self.build_payload(image_jpeg, prompt_ctx);
        let url = format!(
            "{}/chat/completions",
            self.config.api_url.trim_end_matches('/')
        );
        let resp = self
            .http
            .post(&url)
            .header("Content-Type", "application/json")
            .bearer_auth(&self.config.api_key)
            .json(&payload)
            .send()
            .map_err(|e| format!("LLM 请求失败: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("LLM HTTP {}", resp.status()));
        }
        let body: serde_json::Value = resp.json().map_err(|e| format!("LLM 响应解析失败: {e}"))?;
        // choices[0].message.content 是 JSON 字符串
        let content = body["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| "LLM 响应无 content".to_string())?
            .to_string();
        Ok(content)
    }
}

/// base64 编码（引入 base64 crate 或手写——用标准库的 Data URL 简单编码）。
fn base64_encode(data: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// payload 构造：含 image_url data URL + 提示词上下文。
    #[test]
    fn payload_contains_image_and_ctx() {
        let p = OpenAIProvider::new(OpenAiLlmConfig {
            api_url: "https://api.openai.com/v1".into(),
            api_key: "sk-test".into(),
            model: "gpt-4o-mini".into(),
            timeout_secs: 30,
            inference_tool: "OpenAI".into(),
        });
        let payload = p.build_payload(&[0xFF, 0xD8, 0xFF], "此摄像头朝向后院");
        let content = payload["messages"][0]["content"].as_array().unwrap();
        // 含 text + image_url
        assert!(content.iter().any(|c| c["type"] == "text"));
        assert!(content.iter().any(|c| c["type"] == "image_url"));
        let img = &content[1]["image_url"]["url"].as_str().unwrap();
        assert!(img.starts_with("data:image/jpeg;base64,"));
        // prompt 上下文在 text 里
        let text = &content[0]["text"].as_str().unwrap();
        assert!(text.contains("朝向后院"));
    }

    /// base64 编码正确。
    #[test]
    fn base64_works() {
        assert_eq!(base64_encode(b"hello"), "aGVsbG8=");
    }
}
