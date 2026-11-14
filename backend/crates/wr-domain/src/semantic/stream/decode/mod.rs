//! 上游流事实解码:协议分派、快照提取与共享 helper。

use serde_json::{Value, json};

use crate::provider::ProviderProtocol;
use crate::semantic::error::SemanticError;
use crate::semantic::ir::{SnapshotFormat, StreamUpdate};
use crate::semantic::model::SemanticEvent;
use crate::sse::SseEvent;

mod anthropic;
mod gemini;
mod openai;
mod responses;

pub(super) use anthropic::decode_anthropic_upstream;
pub(super) use gemini::decode_gemini_upstream;
pub(super) use openai::decode_openai_upstream;
pub use responses::{responses_done_arguments, responses_function_call_id};
pub(super) use responses::decode_responses_upstream;

/// 一帧里携带的完整响应快照(仍以原始 JSON 保留,不在这里压成 IR)。
/// 快照与内容/usage/finish 事实可以同帧共存(P2-1)。
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedSnapshot {
    pub kind: String,
    pub format: SnapshotFormat,
    pub value: Value,
}

/// 一帧的完整解码结果。观察与下游编码共用这一次协议解释。
/// `events` 是旧扁平词汇(生命周期/聚合),`updates` 是 P4 层级 IR
/// (身份、item/part 生命周期与快照);目标编码器只消费后者。
#[derive(Debug, Clone)]
pub struct DecodedFrame {
    pub events: Vec<SemanticEvent>,
    pub updates: Vec<StreamUpdate>,
    pub snapshot: Option<DecodedSnapshot>,
    /// 协议级序列号(Responses `sequence_number`);用于 R1 同域重放识别。
    pub sequence: Option<u64>,
}

/// 一帧的完整事实解码:语义事件 + 层级更新 + 可选的完整响应快照。
pub fn decode_upstream_frame(protocol: ProviderProtocol, event: &SseEvent) -> DecodedFrame {
    if event.is_done_sentinel() {
        return DecodedFrame {
            events: vec![SemanticEvent::StreamEnd { incomplete: false }],
            updates: vec![StreamUpdate::End { incomplete: false }],
            snapshot: None,
            sequence: None,
        };
    }
    let Some(value) = event.json() else {
        let error = opaque_upstream(protocol, event, Value::Null);
        let update = StreamUpdate::Opaque {
            raw: json!({
                "protocol": protocol.as_str(),
                "event": event.event,
                "data": event.data,
            }),
        };
        return DecodedFrame {
            events: vec![error],
            updates: vec![update],
            snapshot: None,
            sequence: None,
        };
    };
    let snapshot = snapshot_from_payload(protocol, event, &value);
    let mut out = Vec::new();
    let mut updates = Vec::new();
    if let Some(snapshot) = &snapshot {
        updates.push(StreamUpdate::ResponseSnapshot {
            format: snapshot.format,
            value: snapshot.value.clone(),
        });
    }
    let sequence = matches!(protocol, ProviderProtocol::OpenAi)
        .then(|| value.get("sequence_number").and_then(Value::as_u64))
        .flatten();
    let recognized = match protocol {
        ProviderProtocol::OpenAi => decode_openai_upstream(event, &value, &mut out, &mut updates),
        ProviderProtocol::Anthropic => {
            decode_anthropic_upstream(event, &value, &mut out, &mut updates)
        }
        ProviderProtocol::Gemini => decode_gemini_upstream(event, &value, &mut out, &mut updates),
    };
    if !recognized && out.is_empty() {
        out.push(opaque_upstream(protocol, event, value.clone()));
        updates.push(StreamUpdate::Opaque { raw: value });
    }
    DecodedFrame {
        events: out,
        updates,
        snapshot,
        sequence,
    }
}

/// 从已解析负载里提取完整快照;未知事件不猜。
fn snapshot_from_payload(
    protocol: ProviderProtocol,
    event: &SseEvent,
    value: &Value,
) -> Option<DecodedSnapshot> {
    let kind = event_kind(event, value);
    match protocol {
        ProviderProtocol::OpenAi if is_responses_payload(event, value) => {
            if matches!(kind, "response.completed" | "response.incomplete") {
                let snapshot = value.get("response").filter(|value| value.is_object())?;
                return Some(DecodedSnapshot {
                    kind: kind.to_owned(),
                    format: SnapshotFormat::OpenAiResponses,
                    value: snapshot.clone(),
                });
            }
            None
        }
        ProviderProtocol::Anthropic if kind == "message_start" => {
            let snapshot = value.get("message").filter(|value| value.is_object())?;
            Some(DecodedSnapshot {
                kind: kind.to_owned(),
                format: SnapshotFormat::AnthropicMessage,
                value: snapshot.clone(),
            })
        }
        _ => None,
    }
}

/// 上游事件名:`event:` 行优先,其次 payload 内的 `type` 字段。
fn event_kind<'a>(event: &'a SseEvent, value: &'a Value) -> &'a str {
    event
        .event
        .as_deref()
        .or_else(|| value.get("type").and_then(Value::as_str))
        .unwrap_or("")
}

/// payload 是否属于 Responses 事件族(事件名或 `type` 以 `response.` 开头)。
fn is_responses_payload(event: &SseEvent, value: &Value) -> bool {
    event_kind(event, value).starts_with("response.")
}

fn output_index(value: &Value) -> usize {
    value
        .get("output_index")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize
}
/// 上游错误对象 → 语义错误(流内 error 事件共用)。
fn stream_error(error: &Value) -> SemanticError {
    let mut semantic = SemanticError::unsupported(
        error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("upstream stream error"),
    );
    semantic.code = error.get("code").and_then(Value::as_str).map(str::to_owned);
    semantic.status = error
        .get("status")
        .and_then(Value::as_u64)
        .map(|status| status as u16);
    if let Some(kind) = error.get("type").and_then(Value::as_str) {
        semantic.legacy_type = Some(kind.to_owned());
    }
    semantic
}
fn opaque_upstream(protocol: ProviderProtocol, event: &SseEvent, parsed: Value) -> SemanticEvent {
    SemanticEvent::Opaque {
        raw: json!({"protocol": protocol.as_str(), "event": event.event, "data": event.data, "parsed": parsed}),
    }
}
