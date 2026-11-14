//! vendor 请求参数修正(镜像 Python `vendor_compat.py`)。
//!
//! 阶段 E 保留项:这是**传输层 vendor hook**,不是协议转换 —— 协议间
//! 转换已全部收敛到 [`crate::semantic`](方案 20261011_01 阶段 E2)。本
//! 模块只在出站编码之后、HTTP 发出之前,按 base_url host 命中厂商
//! 怪癖表修正参数;每次候选从原始 payload 重新施加(幂等,不含累计改写)。
//!
//! 旧的 Responses↔Chat、Anthropic↔OpenAI 两两转换函数已随阶段 E2 删除;
//! 其行为基线由 `semantic` 适配器与 differential/符合性测试承接。

use serde_json::{Map, Value, json};

use crate::provider::Provider;
use crate::python_semantics::py_bool;

fn get_str(map: &Value, key: &str) -> Option<String> {
    map.get(key).and_then(Value::as_str).map(str::to_owned)
}

// ---------------------------------------------------------------------------
// vendor_compat(上游 chat-completions 形态时的厂商参数修正)
// ---------------------------------------------------------------------------

/// `apply_vendor_request_fixups`:按 base_url host 命中厂商怪癖表;
/// 未命中原样返回。
pub fn apply_vendor_request_fixups(provider: &Provider, payload: &Value) -> Value {
    let host = url_host(provider.base_url.as_str());
    match host.as_str() {
        "api.deepseek.com" => fixup_deepseek(payload.clone()),
        "open.bigmodel.cn" => fixup_zhipu(payload.clone()),
        "api.minimax.chat" | "api.minimaxi.com" => fixup_minimax(payload.clone()),
        "api.xiaomimimo.com" => fixup_mimo(payload.clone()),
        _ => payload.clone(),
    }
}

/// `urlparse(base_url).hostname`(小写;解析失败空串)。
fn url_host(base_url: &str) -> String {
    let Some(scheme_end) = base_url.find("://") else {
        return String::new();
    };
    let rest = &base_url[scheme_end + 3..];
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .rsplit('@')
        .next()
        .unwrap_or("");
    let host = match authority.split_once(':') {
        Some((h, port)) if port.chars().all(|c| c.is_ascii_digit()) => h,
        _ => authority,
    };
    host.to_ascii_lowercase()
}

fn map_of(payload: &Value) -> Map<String, Value> {
    payload.as_object().cloned().unwrap_or_default()
}

fn fixup_deepseek(payload: Value) -> Value {
    let mut body = map_of(&payload);
    if let Some(effort) = body.get("reasoning_effort").cloned() {
        let mapped = match effort.as_str() {
            Some("low") | Some("medium") => json!("high"),
            Some("xhigh") => json!("max"),
            _ => effort,
        };
        body.insert("reasoning_effort".to_owned(), mapped);
    }
    if let Some(Value::Object(thinking)) = body.get("thinking").cloned() {
        let enabled = thinking.get("type").and_then(Value::as_str) == Some("enabled");
        body.insert(
            "thinking".to_owned(),
            json!({"type": if enabled { "enabled" } else { "disabled" }}),
        );
    }
    body.remove("reasoning_split");
    body.remove("interleaved_thinking");
    if let Some(Value::Object(mut output_config)) = body.get("output_config").cloned() {
        output_config.remove("effort");
        body.insert("output_config".to_owned(), Value::Object(output_config));
    }
    Value::Object(body)
}

fn fixup_zhipu(payload: Value) -> Value {
    let mut body = map_of(&payload);
    if let Some(Value::Array(messages)) = body.get_mut("messages") {
        for message in messages.iter_mut() {
            if let Some(map) = message.as_object_mut() {
                map.remove("reasoning_content");
                map.remove("reasoning_details");
            }
        }
    }
    Value::Object(body)
}

fn fixup_minimax(payload: Value) -> Value {
    let mut body = map_of(&payload);
    body.insert("reasoning_split".to_owned(), json!(true));
    body.remove("thinking");
    body.remove("reasoning_effort");
    body.remove("output_config");
    Value::Object(body)
}

fn fixup_mimo(payload: Value) -> Value {
    let mut body = map_of(&payload);
    body.remove("reasoning_split");
    body.remove("reasoning_effort");
    body.remove("output_config");
    let Some(Value::Array(messages)) = body.get("messages").cloned() else {
        return Value::Object(body);
    };
    let mut cleaned: Vec<Value> = Vec::new();
    for message in messages {
        let Some(mut map) = message.as_object().cloned() else {
            cleaned.push(message);
            continue;
        };
        for key in ["reasoning_details", "reasoning", "thinking"] {
            map.remove(key);
        }
        let has_content = map.get("content").map(py_bool).unwrap_or(false);
        let has_tool_calls = map.get("tool_calls").map(py_bool).unwrap_or(false);
        let role = get_str(&Value::Object(map.clone()), "role").unwrap_or_default();
        if !has_content && !has_tool_calls && role != "tool" && role != "system" {
            continue;
        }
        cleaned.push(Value::Object(map));
    }
    body.insert("messages".to_owned(), Value::Array(cleaned));
    Value::Object(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn provider_at(base_url: &str) -> Provider {
        let mut raw = serde_json::Map::new();
        raw.insert("id".to_owned(), json!("p"));
        raw.insert("api_base_url".to_owned(), json!(base_url));
        raw.insert("protocol".to_owned(), json!("openai"));
        Provider::from_raw(&raw).unwrap()
    }

    #[test]
    fn vendor_fixups_by_host() {
        let deepseek = provider_at("https://api.deepseek.com/v1");
        let payload = json!({
            "messages": [{"role": "user", "content": "hi"}],
            "reasoning_effort": "xhigh",
            "reasoning_split": true,
        });
        let fixed = apply_vendor_request_fixups(&deepseek, &payload);
        assert_eq!(fixed["reasoning_effort"], json!("max"));
        assert!(fixed.get("reasoning_split").is_none());

        let minimax = provider_at("https://api.minimax.chat/v1");
        let fixed = apply_vendor_request_fixups(&minimax, &payload);
        assert_eq!(fixed["reasoning_split"], json!(true));
        assert!(fixed.get("reasoning_effort").is_none());

        let unknown = provider_at("https://api.example.com/v1");
        let fixed = apply_vendor_request_fixups(&unknown, &payload);
        assert_eq!(fixed, payload);
    }

    #[test]
    fn url_host_parsing() {
        assert_eq!(url_host("https://api.deepseek.com/v1"), "api.deepseek.com");
        assert_eq!(url_host("http://API.DeepSeek.com:8080/x"), "api.deepseek.com");
        assert_eq!(url_host("https://user@host.example/path"), "host.example");
        assert_eq!(url_host("not-a-url"), "");
    }
}
