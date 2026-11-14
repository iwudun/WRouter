//! 协议内容块解码共享助手(语义适配器公用)。
//!
//! 全部遵循 Python 值语义(`py_bool`/`py_str`/`py_int`/`py_json_dumps`),
//! 与既有镜像实现保持一致;未知块原样进 `ContentPart::Opaque`。

use serde_json::{Value, json};

use crate::python_semantics::{py_bool, py_str};
use crate::semantic::diagnostic::{ConversionReport, DiagnosticCode};
use crate::semantic::model::ContentPart;

pub(crate) fn opt_str(value: Option<&Value>) -> Option<String> {
    value.and_then(py_str)
}


/// openai chat `content`(string | parts 数组 | null)→ 内容块。
pub(crate) fn parts_from_chat_content(value: &Value) -> Vec<ContentPart> {
    match value {
        Value::Null => Vec::new(),
        Value::String(text) => vec![ContentPart::Text {
            text: text.clone(),
        }],
        Value::Array(items) => items.iter().map(part_from_chat_block).collect(),
        other => vec![ContentPart::Opaque {
            kind_hint: None,
            value: other.clone(),
        }],
    }
}

/// openai chat 单个 content part。
pub(crate) fn part_from_chat_block(block: &Value) -> ContentPart {
    let kind = block.get("type").and_then(Value::as_str).unwrap_or("");
    match kind {
        "text" | "input_text" | "output_text" => ContentPart::Text {
            text: opt_str(block.get("text")).unwrap_or_default(),
        },
        "image_url" => {
            let url = block
                .get("image_url")
                .and_then(|v| v.get("url"))
                .or_else(|| block.get("url"))
                .and_then(py_str)
                .unwrap_or_default();
            ContentPart::ImageRef { url }
        }
        "input_image" => ContentPart::ImageRef {
            url: opt_str(block.get("image_url"))
                .or_else(|| opt_str(block.get("file_id")))
                .unwrap_or_default(),
        },
        "refusal" => ContentPart::Refusal {
            text: opt_str(block.get("refusal")).unwrap_or_default(),
        },
        // 带协议特有字段(cache_control 等)的块原样保留。
        _ => ContentPart::Opaque {
            kind_hint: Some(kind.to_owned()),
            value: block.clone(),
        },
    }
}

/// 消息级未知键 → 扩展保留(raw 与 semantic 并存;编码器决定是否
/// 重放,丢弃时记 DroppedField)。
pub(crate) fn preserve_extra_keys(
    message: &Value,
    modeled: &[&str],
    extensions: &mut crate::semantic::model::Extensions,
    report: &mut ConversionReport,
    path: &str,
) {
    let Some(map) = message.as_object() else {
        return;
    };
    for (key, value) in map {
        if !modeled.contains(&key.as_str()) {
            extensions.insert("openai", &format!("extra/{key}"), value.clone());
            report.push(
                DiagnosticCode::OpaquePreserved,
                format!("{path}.{key}"),
                "消息级未识别键以扩展保留".to_owned(),
            );
        }
    }
}

/// 空 content 的文本聚合(与旧实现 text concat 语义一致:仅 Text 块)。
pub(crate) fn joined_text(parts: &[ContentPart]) -> String {
    let mut out = String::new();
    for part in parts {
        if let ContentPart::Text { text } = part {
            out.push_str(text);
        }
    }
    out
}

/// 工具定义 schema 的真值兜底(falsy → `{}`,镜像 `or_default(&json!({}))`)。
pub(crate) fn schema_or_empty(value: Option<&Value>) -> Value {
    match value {
        Some(v) if py_bool(v) => v.clone(),
        _ => json!({}),
    }
}
