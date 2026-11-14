//! 统一语义模型与协议适配管线(方案 20261011_01 §3;契约
//! 20261012_01 §2)。
//!
//! 纯类型 + 协议边缘适配器:无 I/O、无 HTTP、无认证/凭据 ——
//! provider/路由/端点/headers 一律不进内核。编解码只经适配器,
//! 本模块类型不作为 wire 格式。
//!
//! 阶段 B 覆盖非流面;流事件生命周期与 relay 迁移在阶段 C。

pub mod adapter;
pub mod anthropic;
pub mod client_stream;
pub mod diagnostic;
pub mod envelope;
pub mod error;
pub mod finish;
pub mod gemini;
pub mod ir;
pub mod model;
pub mod openai_chat;
pub mod openai_responses;
pub mod parts;
pub mod responses_target;
pub mod source;
pub mod stream;
pub mod synthesis;
pub mod tool_names;
mod util;

use serde_json::Value;

use crate::provider::ProviderProtocol;
use crate::semantic::adapter::{Decoded, OutboundAdapter, OutboundContext};
use crate::semantic::model::SemanticResponse;

pub use adapter::ensure_combination_supported;
pub use diagnostic::{ConversionDiagnostic, ConversionReport, ConversionStrategy, DiagnosticCode};
pub use error::{ErrorCategory, RetryHint, SemanticError};
pub use model::{
    ContentPart, Extensions, FinishReason, GenerationOptions, IdOrigin, IncompleteReason, Message,
    OpaqueField, ReasoningBlock, RequestKind, ResponseCandidate, Role, SemanticEvent,
    SemanticRequest, SourceProtocol, StreamPreference, ToolArguments, ToolCall, ToolCallId,
    ToolChoice, ToolDefinition, ToolResult, Usage,
};

// ---------------------------------------------------------------------------
// 管线入口(B6 接线语义)
// ---------------------------------------------------------------------------

/// 出站适配器选择:openai 上游按 style 分流(native → Responses 形态)。
fn outbound_adapter(
    target: ProviderProtocol,
    native_responses: bool,
) -> &'static dyn OutboundAdapter {
    match (target, native_responses) {
        (ProviderProtocol::OpenAi, true) => &openai_responses::Outbound,
        _ => adapter::outbound(target),
    }
}

/// 客户端请求 → 上游请求体:Inbound decode → Outbound encode。
/// 组合门禁(`ensure_combination_supported`)由调用方先行;
/// `source == target` 的原生透传格不经本管线(保真约束)。
pub fn convert_request(
    source: SourceProtocol,
    target: ProviderProtocol,
    native_responses: bool,
    payload: &Value,
) -> Result<(Value, ConversionReport), SemanticError> {
    let inbound = adapter::inbound(source).ok_or_else(|| {
        SemanticError::unsupported(format!("入口协议 {} 无语义适配器", source.as_str()))
    })?;
    let decoded_request = inbound.decode_request(payload)?;
    let ctx = OutboundContext {
        native_responses,
        kind: decoded_request.value.kind,
    };
    let encoded =
        outbound_adapter(target, native_responses).encode_request(&decoded_request.value, &ctx)?;
    let mut report = decoded_request.report;
    report.diagnostics.extend(encoded.report.diagnostics);
    report.mark_lossy_if_needed();
    Ok((encoded.value, report))
}

/// 上游非流响应 JSON → 客户端协议形态:Outbound decode → Inbound encode。
pub fn convert_response(
    source: SourceProtocol,
    target: ProviderProtocol,
    native_responses: bool,
    upstream: &Value,
) -> Result<(Value, ConversionReport), SemanticError> {
    let inbound = adapter::inbound(source).ok_or_else(|| {
        SemanticError::unsupported(format!("入口协议 {} 无语义适配器", source.as_str()))
    })?;
    let ctx = OutboundContext {
        native_responses,
        kind: source.request_kind(),
    };
    let decoded_response: Decoded<SemanticResponse> =
        outbound_adapter(target, native_responses).decode_response(upstream, &ctx)?;
    let encoded = inbound.encode_response(&decoded_response.value)?;
    let mut report = decoded_response.report;
    report.diagnostics.extend(encoded.report.diagnostics);
    report.mark_lossy_if_needed();
    Ok((encoded.value, report))
}

/// 客户端协议形态的完整响应 JSON → 语义响应。
///
/// 用于 A6/缓存重放:缓存里存的是**客户端协议形态**,必须先按同一协议
/// 解码回语义响应,再交给目标编码器;不能拿目标 JSON 直接拼帧。
pub fn decode_client_response(
    source: SourceProtocol,
    value: &Value,
) -> Result<SemanticResponse, SemanticError> {
    if !value.is_object() {
        return Err(SemanticError::unsupported(
            "客户端响应不是 JSON 对象,无法按协议语义解码",
        ));
    }
    let mut report = ConversionReport::semantic();
    let response = match source {
        SourceProtocol::OpenAi => openai_chat::decode_chat_completion(value, &mut report),
        SourceProtocol::OpenAiResponses => {
            openai_responses::decode_responses_json(value, &mut report)
        }
        SourceProtocol::Anthropic => anthropic::decode_anthropic_message(value, &mut report),
        SourceProtocol::Gemini | SourceProtocol::GeminiStream => {
            gemini::decode_gemini_response(value, &mut report)
        }
        SourceProtocol::GeminiInteractions | SourceProtocol::OpenAiEmbeddings => {
            return Err(SemanticError::unsupported(format!(
                "入口协议 {} 没有可重放为流的语义响应解码器",
                source.as_str()
            )));
        }
    };
    Ok(response)
}

/// 上游错误信封(B7):语义错误 → `(http_status, client_envelope)`。
pub fn encode_error_for(
    source: SourceProtocol,
    error: &SemanticError,
    attempts: Option<&[String]>,
) -> (u16, Value) {
    match adapter::inbound(source) {
        Some(inbound) => inbound.encode_error(error, attempts),
        // 无适配器入口(embeddings)沿用 openai 形态。
        None => envelope::encode_error(SourceProtocol::OpenAi, error, attempts),
    }
}

#[cfg(test)]
mod tests;
