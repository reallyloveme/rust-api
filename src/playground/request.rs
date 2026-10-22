use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatRequest {
    pub provider_id: String,
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub parameters: Map<String, Value>,
}

impl ChatRequest {
    pub fn payload(&self) -> Result<Value, &'static str> {
        if self.messages.is_empty() || self.messages.len() > 100 {
            return Err("messages 须包含 1–100 条消息");
        }
        if self.messages.iter().any(|m| {
            !["system", "developer", "user", "assistant"].contains(&m.role.as_str())
                || m.content.trim().is_empty()
        }) {
            return Err("消息角色无效或内容为空；当前仅支持文本消息");
        }
        if ["model", "messages", "stream", "stream_options"]
            .iter()
            .any(|key| self.parameters.contains_key(*key))
        {
            return Err("parameters 不能覆盖 model、messages、stream 或 stream_options");
        }
        for (key, max) in [("temperature", 2.0), ("top_p", 1.0)] {
            if let Some(value) = self.parameters.get(key) {
                if !value.as_f64().is_some_and(|v| (0.0..=max).contains(&v)) {
                    return Err("temperature 须为 0–2，top_p 须为 0–1");
                }
            }
        }
        for key in ["max_tokens", "max_completion_tokens"] {
            if let Some(value) = self.parameters.get(key) {
                if !value.as_u64().is_some_and(|v| v > 0) {
                    return Err("Token 上限须为正整数");
                }
            }
        }
        let mut payload = self.parameters.clone();
        payload.insert("model".into(), json!(self.model));
        payload.insert("messages".into(), json!(self.messages));
        payload.insert("stream".into(), Value::Bool(false));
        Ok(Value::Object(payload))
    }
}
