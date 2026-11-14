//! Gemini 协议专项 helper(镜像 Python `domain/protocol/gemini.py`)。
//!
//! 阶段 E 保留项:跨协议互转已收敛到 [`crate::semantic`](方案 20261011_01
//! 阶段 E2);schema 清洗/usage 归一已归位 `semantic::gemini`(唯一消费者
//! 在彼处,20261013_01 B2)。本模块只留 wire 分派侧的两块活代码 ——
//! `interaction_to_gemini`(interactions API 的协议专项 helper:上游
//! `/interactions` 端点本身吃 `input` 形态,不经语义内核,由
//! `protocol::mod` passthrough 分派调用)与 `build_gemini_contents`
//! (其实现体依赖)。Rust 里落 `wr-domain` 合法:纯 JSON 变换
//! (对应 Python domain 层文件,无 httpx/FastAPI 依赖)。

use serde_json::{Map, Value, json};

use crate::json_py::py_json_dumps;
use crate::python_semantics::py_bool;

/// `_text`:字符串原样,其余 `json.dumps(v, ensure_ascii=False)`。
fn text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => py_json_dumps(other),
    }
}

// ---------------------------------------------------------------------------
// contents 构造
// ---------------------------------------------------------------------------

fn part_from_content(block: &Value, tool_names: &mut Map<String, Value>) -> Vec<Value> {
    let kind = block.get("type").and_then(Value::as_str).unwrap_or("");
    match kind {
        "text" | "input_text" | "output_text" => {
            vec![json!({"text": block.get("text").map(text).unwrap_or_default()})]
        }
        "thinking" | "reasoning" => {
            let mut part = Map::new();
            let text_value = block
                .get("thinking")
                .filter(|v| !v.is_null())
                .or_else(|| block.get("text"))
                .map(text)
                .unwrap_or_default();
            part.insert("text".to_owned(), json!(text_value));
            part.insert("thought".to_owned(), json!(true));
            let signature = block
                .get("signature")
                .filter(|v| py_bool(v))
                .or_else(|| block.get("thoughtSignature").filter(|v| py_bool(v)))
                .or_else(|| block.get("encrypted_content").filter(|v| py_bool(v)));
            if let Some(signature) = signature {
                part.insert("thoughtSignature".to_owned(), signature.clone());
            }
            vec![Value::Object(part)]
        }
        "tool_use" => {
            let mut call = Map::new();
            call.insert(
                "name".to_owned(),
                json!(block.get("name").map(text).unwrap_or_default()),
            );
            call.insert(
                "args".to_owned(),
                block
                    .get("input")
                    .filter(|v| py_bool(v))
                    .cloned()
                    .unwrap_or_else(|| json!({})),
            );
            if let Some(id) = block.get("id").filter(|v| py_bool(v)) {
                call.insert("id".to_owned(), json!(text(id)));
            }
            if let Some(sig) = block
                .get("thoughtSignature")
                .filter(|v| py_bool(v))
                .or_else(|| block.get("signature").filter(|v| py_bool(v)))
            {
                call.insert("thoughtSignature".to_owned(), sig.clone());
            }
            vec![json!({"functionCall": Value::Object(call)})]
        }
        "tool_result" => {
            let call_id = ["tool_use_id", "id"]
                .iter()
                .map(|key| block.get(*key).map(text).unwrap_or_default())
                .find(|v| !v.is_empty())
                .unwrap_or_default();
            let mut response = block.get("content").cloned().unwrap_or(json!(""));
            if let Value::Array(items) = &response {
                response = Value::String(
                    items
                        .iter()
                        .map(|x| match x {
                            Value::Object(obj) => obj.get("text").map(text).unwrap_or_default(),
                            other => text(other),
                        })
                        .collect::<Vec<_>>()
                        .concat(),
                );
            }
            let name = block
                .get("name")
                .map(text)
                .filter(|n| !n.is_empty())
                .or_else(|| {
                    tool_names
                        .get(&call_id)
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_default();
            let mut call = Map::new();
            call.insert("name".to_owned(), json!(name));
            call.insert("response".to_owned(), json!({"result": response}));
            if !call_id.is_empty() {
                call.insert("id".to_owned(), json!(call_id));
            }
            vec![json!({"functionResponse": Value::Object(call)})]
        }
        _ => {
            if let Some(t) = block.get("text") {
                return vec![json!({"text": text(t)})];
            }
            if block.get("inline_data").is_some() || block.get("inlineData").is_some() {
                let inline = block
                    .get("inlineData")
                    .or_else(|| block.get("inline_data"))
                    .cloned()
                    .unwrap_or(Value::Null);
                return vec![json!({"inlineData": inline})];
            }
            Vec::new()
        }
    }
}

/// `build_gemini_contents`:messages/contents → Gemini contents +
/// systemInstruction。返回 (contents, system_instruction)。
pub fn build_gemini_contents(payload: &Value, protocol: &str) -> (Vec<Value>, Option<Value>) {
    let system_raw = if protocol == "anthropic" {
        payload.get("system")
    } else {
        payload.get("instructions")
    };
    // `messages` 取值(镜像 Python 两段式):openai_responses 读 input
    // (get 缺省 ""),其余协议 `payload.get("messages") or []`;最终非
    // list 一律包成单条 user 文本消息。
    let messages: Vec<Value> = if protocol == "openai_responses" {
        match payload.get("input") {
            Some(Value::String(s)) => vec![json!({"role": "user", "content": s})],
            Some(Value::Array(items)) => items.clone(),
            Some(other) if !other.is_null() => {
                vec![json!({"role": "user", "content": other.clone()})]
            }
            _ => vec![json!({"role": "user", "content": ""})],
        }
    } else {
        match payload.get("messages").filter(|v| py_bool(v)) {
            Some(Value::Array(items)) => items.clone(),
            Some(other) => vec![json!({"role": "user", "content": other.clone()})],
            None => Vec::new(),
        }
    };
    // Python `if not isinstance(messages, list)` 兜底(上面已保证 list)。
    let mut contents: Vec<Value> = Vec::new();
    let mut tool_names: Map<String, Value> = Map::new();
    for message in &messages {
        let Some(mm) = message.as_object() else {
            contents.push(json!({"role": "user", "parts": [{"text": text(message)}]}));
            continue;
        };
        let role = mm
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user")
            .to_owned();
        let gemini_role = if role == "assistant" || role == "model" {
            "model"
        } else {
            "user"
        };
        let raw = mm.get("content").cloned().unwrap_or(json!(""));
        let blocks: Vec<Value> = match &raw {
            Value::Array(items) => items.clone(),
            other => vec![json!({"type": "text", "text": other.clone()})],
        };
        let mut parts: Vec<Value> = Vec::new();
        for block in &blocks {
            match block {
                Value::Object(_) => parts.extend(part_from_content(block, &mut tool_names)),
                other => {
                    if py_bool(other) {
                        parts.push(json!({"text": text(other)}));
                    }
                }
            }
        }
        let calls = mm
            .get("tool_calls")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for call in calls {
            let function = call.get("function").cloned().unwrap_or(json!({}));
            let call_id = call.get("id").map(text).unwrap_or_default();
            let name = function.get("name").map(text).unwrap_or_default();
            if !call_id.is_empty() {
                tool_names.insert(call_id.clone(), json!(name));
            }
            let mut arguments = function.get("arguments").cloned().unwrap_or(json!({}));
            if arguments.is_null() {
                arguments = json!({});
            }
            if let Value::String(s) = &arguments {
                arguments = serde_json::from_str(s).unwrap_or_else(|_| json!({"raw": s}));
            }
            parts.push(json!({"functionCall": {"id": call_id, "name": name, "args": arguments}}));
        }
        if role == "tool" {
            let synth = json!({
                "type": "tool_result",
                "tool_use_id": mm.get("tool_call_id").cloned().unwrap_or(Value::Null),
                "content": raw,
            });
            parts = part_from_content(&synth, &mut tool_names);
        }
        let gemini_role = if role == "tool" { "user" } else { gemini_role };
        if !parts.is_empty() {
            contents.push(json!({"role": gemini_role, "parts": parts}));
        }
    }
    let system_instruction = system_raw
        .filter(|v| py_bool(v))
        .map(|v| json!({"parts": [{"text": text(v)}]}));
    (contents, system_instruction)
}

/// `interaction_to_gemini`:Interactions payload → Gemini `input`。
pub fn interaction_to_gemini(payload: &Value) -> Value {
    let input_value = payload.get("input").cloned().unwrap_or(json!(""));
    let contents = match &input_value {
        Value::String(s) => json!([{"role": "user", "parts": [{"text": s}]}]),
        Value::Array(items) => {
            let (contents, _) = build_gemini_contents(&json!({"messages": items}), "openai");
            Value::Array(contents)
        }
        other => {
            let (contents, _) = build_gemini_contents(&json!({"messages": other}), "openai");
            Value::Array(contents)
        }
    };
    let mut result = Map::new();
    result.insert("input".to_owned(), contents);
    for (key, value) in payload.as_object().unwrap_or(&Map::new()) {
        if key == "input" || key == "stream" {
            continue;
        }
        result.insert(key.clone(), value.clone());
    }
    Value::Object(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    // openai/anthropic → gemini 的请求/响应转换测试随阶段 E2 迁移:
    // 覆盖已由 semantic::tests::semantic_openai_to_gemini_request_shape
    // 与 differential/符合性测试承接,旧两两实现的专用用例不再保留。

    #[test]
    fn interactions_converters() {
        let converted =
            interaction_to_gemini(&json!({"input": "hi", "stream": true, "model": "m"}));
        assert_eq!(converted["input"][0]["parts"][0]["text"], json!("hi"));
        assert!(converted.get("stream").is_none());
        assert_eq!(converted["model"], json!("m"));
    }

    #[test]
    fn py_str_import_is_used_for_text() {
        // text() 对字符串直通,数字/bool 走 json 文本。
        assert_eq!(text(&json!("a")), "a");
        assert_eq!(text(&json!(1.5)), "1.5");
        assert_eq!(text(&json!(true)), "true");
    }
}
