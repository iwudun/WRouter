//! 客户端错误信封编码(B7)。形态与旧 `format_error_for_protocol`
//! 五种信封逐字段一致(`protocol/mod.rs`;gemini_interactions 沿用
//! openai 形态的缺口为 **声明沿用**,见矩阵 §1.6 #11 的 B7 决策)。

use serde_json::{Map, Value, json};

use crate::semantic::error::SemanticError;
use crate::semantic::model::SourceProtocol;

/// 语义错误 → 客户端协议错误信封 `(http_status, body)`。
/// `attempts` 非空时附加 `wrouter_attempts`(A4)。
pub fn encode_error(
    protocol: SourceProtocol,
    error: &SemanticError,
    attempts: Option<&[String]>,
) -> (u16, Value) {
    let status = error.effective_status();
    let error_type = error.effective_type();
    let attempts_json = |obj: &mut Map<String, Value>| {
        if let Some(list) = attempts
            && !list.is_empty()
        {
            obj.insert(
                "wrouter_attempts".to_owned(),
                Value::Array(list.iter().map(|a| Value::String(a.clone())).collect()),
            );
        }
    };
    let body = match protocol {
        SourceProtocol::OpenAi | SourceProtocol::OpenAiEmbeddings => {
            let mut error_obj = Map::new();
            error_obj.insert("message".to_owned(), Value::String(error.message.clone()));
            error_obj.insert("type".to_owned(), Value::String(error_type));
            if let Some(code) = &error.code {
                error_obj.insert("code".to_owned(), Value::String(code.clone()));
            }
            if let Some(param) = &error.param {
                error_obj.insert("param".to_owned(), Value::String(param.clone()));
            }
            attempts_json(&mut error_obj);
            json!({ "error": Value::Object(error_obj) })
        }
        SourceProtocol::OpenAiResponses => {
            let mut error_obj = Map::new();
            error_obj.insert(
                "code".to_owned(),
                Value::String(error.code.clone().unwrap_or_else(|| error_type.clone())),
            );
            error_obj.insert("message".to_owned(), Value::String(error.message.clone()));
            attempts_json(&mut error_obj);
            json!({ "type": "error", "error": Value::Object(error_obj) })
        }
        SourceProtocol::Anthropic => {
            let mut error_obj = Map::new();
            error_obj.insert("type".to_owned(), Value::String(error_type));
            error_obj.insert("message".to_owned(), Value::String(error.message.clone()));
            attempts_json(&mut error_obj);
            json!({ "type": "error", "error": Value::Object(error_obj) })
        }
        SourceProtocol::Gemini | SourceProtocol::GeminiStream => {
            let mut error_obj = Map::new();
            error_obj.insert("code".to_owned(), json!(status));
            error_obj.insert("status".to_owned(), Value::String(error_type));
            error_obj.insert("message".to_owned(), Value::String(error.message.clone()));
            if let Some(code) = &error.code {
                error_obj.insert(
                    "details".to_owned(),
                    json!([{ "@type": "ErrorReason", "reason": code }]),
                );
            }
            attempts_json(&mut error_obj);
            json!({ "error": Value::Object(error_obj) })
        }
        // B7 决策:gemini_interactions 错误信封声明沿用 openai 形态
        // (与其成功响应的 Responses 信封并存;矩阵 §1.6 #11)。
        SourceProtocol::GeminiInteractions => {
            let mut error_obj = Map::new();
            error_obj.insert("message".to_owned(), Value::String(error.message.clone()));
            error_obj.insert("type".to_owned(), Value::String(error_type));
            attempts_json(&mut error_obj);
            json!({ "error": Value::Object(error_obj) })
        }
    };
    (status, body)
}
