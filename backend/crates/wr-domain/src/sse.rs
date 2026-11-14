//! SSE 字节流解析与编码(镜像 Python `application/sse/parser.py`,R2.4)。
//!
//! 语义对齐:
//! - `\r` 全部剔除(CRLF → LF);
//! - `\n\n` 分块;注释行(`:` 开头)忽略;`event:` / `data:` 字段解析,
//!   `data:` 后仅剥一个前导空格;多 `data:` 行以 `\n` 连接;
//! - 末尾未以空行终止的块仍是合法事件(drain 时补收);
//! - `[DONE]` 哨兵保留在 data 中,由 relay 识别。

use serde_json::Value;

/// 一个完整 SSE 事件。
#[derive(Debug, Clone, PartialEq)]
pub struct SseEvent {
    /// `event:` 行的值;上游未携带时为 None。
    pub event: Option<String>,
    /// 所有 `data:` 行以 `\n` 连接的负载(哨兵 `"[DONE]"` 原样保留)。
    pub data: String,
    /// 原始块文本(A9.7 日志捕获用;R4 落库)。
    pub raw: String,
}

impl SseEvent {
    /// `data.strip() == "[DONE]"`。
    pub fn is_done_sentinel(&self) -> bool {
        self.data.trim() == "[DONE]"
    }

    /// 尽力 JSON 解析;非 JSON 或 `[DONE]` 返回 None。
    /// 顶层非 dict 的 JSON 值包成 `{"value": ...}`(镜像 Python)。
    pub fn json(&self) -> Option<Value> {
        let text = self.data.trim();
        if text.is_empty() || text == "[DONE]" {
            return None;
        }
        match serde_json::from_str::<Value>(text) {
            Ok(value @ Value::Object(_)) => Some(value),
            Ok(other) => Some(serde_json::json!({ "value": other })),
            Err(_) => None,
        }
    }
}

/// 解析一个 `\n\n` 分隔的块;空块 / 纯注释块返回 None。
pub fn parse_sse_block(block: &str) -> Option<SseEvent> {
    if block.trim().is_empty() {
        return None;
    }
    let mut event_name: Option<String> = None;
    let mut data_lines: Vec<&str> = Vec::new();
    for line in block.split('\n') {
        if line.starts_with(':') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("event:") {
            event_name = Some(rest.trim().to_owned());
            continue;
        }
        if let Some(rest) = line.strip_prefix("data:") {
            let payload = rest.strip_prefix(' ').unwrap_or(rest);
            data_lines.push(payload);
        }
        // id:/retry: 等未知字段忽略(镜像 Python)。
    }
    if data_lines.is_empty() {
        return None;
    }
    Some(SseEvent {
        event: event_name,
        data: data_lines.join("\n"),
        raw: block.to_owned(),
    })
}

/// relay 输出载荷:原样字符串(哨兵 / 已格式化文本)或 JSON 值。
#[derive(Debug, Clone)]
pub enum SsePayload<'a> {
    Raw(&'a str),
    Json(&'a Value),
}

/// 序列化为一个 SSE 块(以 `\n\n` 结尾)。JSON 载荷用 Python
/// `json.dumps(ensure_ascii=False)` 等价序列化(见 [`crate::json_py`])。
pub fn encode_sse(event: Option<&str>, payload: SsePayload<'_>) -> String {
    let body = match payload {
        SsePayload::Raw(text) => text.to_owned(),
        SsePayload::Json(value) => crate::json_py::py_json_dumps(value),
    };
    match event {
        Some(name) => format!("event: {name}\ndata: {body}\n\n"),
        None => format!("data: {body}\n\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_event_and_data_lines() {
        let event =
            parse_sse_block("event: message_start\ndata: {\"type\":\"message_start\"}").unwrap();
        assert_eq!(event.event.as_deref(), Some("message_start"));
        assert_eq!(event.data, "{\"type\":\"message_start\"}");
        assert!(!event.is_done_sentinel());
    }

    #[test]
    fn strips_one_leading_space_and_joins_multiline() {
        let event = parse_sse_block("data: line1\ndata:line2").unwrap();
        assert_eq!(event.data, "line1\nline2");
    }

    #[test]
    fn ignores_comments_and_empty_blocks() {
        assert!(parse_sse_block(": keepalive").is_none());
        assert!(parse_sse_block("").is_none());
    }

    #[test]
    fn encode_sse_roundtrip() {
        let value = serde_json::json!({"id": "c1"});
        let wire = encode_sse(Some("ev"), SsePayload::Json(&value));
        assert_eq!(wire, "event: ev\ndata: {\"id\": \"c1\"}\n\n");
        let raw = encode_sse(None, SsePayload::Raw("[DONE]"));
        assert_eq!(raw, "data: [DONE]\n\n");
    }
}
