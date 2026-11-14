//! Anthropic Messages 上游流解码。

use serde_json::{Value, json};

use crate::semantic::diagnostic::ConversionReport;
use crate::semantic::finish;
use crate::semantic::ir::{StreamKey, StreamUpdate};
use crate::semantic::model::{
    ContentPart, SemanticEvent, SourceProtocol, ToolArguments, ToolCall, ToolCallId,
};
use crate::sse::SseEvent;

use super::{event_kind, stream_error};
use crate::semantic::stream::usage::anthropic_usage;

/// Anthropic message_stream → 语义事件。`ping` 是 keepalive,无语义,
/// 映射为空事件但不按 unknown 记 opaque。
pub(in crate::semantic::stream) fn decode_anthropic_upstream(
    event: &SseEvent,
    value: &Value,
    out: &mut Vec<SemanticEvent>,
    updates: &mut Vec<StreamUpdate>,
) -> bool {
    let kind = event_kind(event, value);
    match kind {
        "message_start" => {
            let id = value
                .pointer("/message/id")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let model = value
                .pointer("/message/model")
                .and_then(Value::as_str)
                .map(str::to_owned);
            out.push(SemanticEvent::MessageStart {
                id: id.clone(),
                model: model.clone(),
            });
            updates.push(StreamUpdate::ResponseStart {
                source: SourceProtocol::Anthropic,
                id,
                model,
            });
            if let Some(usage) = value.pointer("/message/usage") {
                let usage = anthropic_usage(usage);
                out.push(SemanticEvent::Usage(usage));
                updates.push(StreamUpdate::Usage(usage));
            }
        }
        "content_block_start" => {
            if let Some(block) = value.get("content_block") {
                let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let key = StreamKey::new(0, 0, index);
                let block_id = block.get("id").and_then(Value::as_str).map(str::to_owned);
                match block.get("type").and_then(Value::as_str).unwrap_or("") {
                    "tool_use" => {
                        let call = ToolCall {
                            id: ToolCallId {
                                local: block_id.clone().unwrap_or_default(),
                                origin: crate::semantic::model::IdOrigin::Source,
                            },
                            name: crate::semantic::tool_names::desanitize(
                                block.get("name").and_then(Value::as_str).unwrap_or(""),
                            ),
                            arguments: ToolArguments::Parsed(
                                block.get("input").cloned().unwrap_or_else(|| json!({})),
                            ),
                            extensions: Default::default(),
                        };
                        out.push(SemanticEvent::ToolCallStart {
                            candidate: 0,
                            index,
                            call: call.clone(),
                        });
                        updates.push(StreamUpdate::ToolStart {
                            key,
                            tool_index: index,
                            call,
                        });
                    }
                    "thinking" => {
                        out.push(SemanticEvent::ReasoningStart {
                            candidate: 0,
                            index,
                        });
                        updates.push(StreamUpdate::ReasoningStart { key, id: block_id });
                    }
                    "redacted_thinking" => {
                        let part = ContentPart::Opaque {
                            kind_hint: Some("redacted_thinking".to_owned()),
                            value: block.clone(),
                        };
                        out.push(SemanticEvent::PartStart {
                            candidate: 0,
                            index,
                            part: part.clone(),
                        });
                        updates.push(StreamUpdate::PartStart {
                            key,
                            id: block_id,
                            part,
                            extensions: crate::semantic::model::Extensions::default(),
                        });
                    }
                    "text" => {
                        let text = block
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned();
                        out.push(SemanticEvent::PartStart {
                            candidate: 0,
                            index,
                            part: ContentPart::Text { text: text.clone() },
                        });
                        updates.push(StreamUpdate::PartStart {
                            key,
                            id: block_id,
                            part: ContentPart::Text {
                                text: String::new(),
                            },
                            extensions: crate::semantic::model::Extensions::default(),
                        });
                        if !text.is_empty() {
                            updates.push(StreamUpdate::PartDelta { key, text });
                        }
                    }
                    _ => {
                        let part = ContentPart::Opaque {
                            kind_hint: block.get("type").and_then(Value::as_str).map(str::to_owned),
                            value: block.clone(),
                        };
                        out.push(SemanticEvent::PartStart {
                            candidate: 0,
                            index,
                            part: part.clone(),
                        });
                        updates.push(StreamUpdate::PartStart {
                            key,
                            id: block_id,
                            part,
                            extensions: crate::semantic::model::Extensions::default(),
                        });
                    }
                }
            }
        }
        "content_block_delta" => {
            let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            let key = StreamKey::new(0, 0, index);
            if let Some(delta) = value.get("delta") {
                match delta.get("type").and_then(Value::as_str).unwrap_or("") {
                    "text_delta" => {
                        if let Some(text) = delta.get("text").and_then(Value::as_str) {
                            out.push(SemanticEvent::PartDelta {
                                candidate: 0,
                                index,
                                text: text.to_owned(),
                            });
                            updates.push(StreamUpdate::PartDelta {
                                key,
                                text: text.to_owned(),
                            });
                        }
                    }
                    "thinking_delta" => {
                        if let Some(text) = delta.get("thinking").and_then(Value::as_str) {
                            out.push(SemanticEvent::ReasoningDelta {
                                candidate: 0,
                                index,
                                text: text.to_owned(),
                            });
                            updates.push(StreamUpdate::ReasoningDelta {
                                key,
                                text: text.to_owned(),
                            });
                        }
                    }
                    "signature_delta" => {
                        if let Some(signature) = delta.get("signature").and_then(Value::as_str) {
                            out.push(SemanticEvent::ReasoningSignature {
                                candidate: 0,
                                index,
                                signature: signature.to_owned(),
                            });
                            updates.push(StreamUpdate::ReasoningSignature {
                                key,
                                signature: signature.to_owned(),
                            });
                        }
                    }
                    "input_json_delta" => {
                        if let Some(fragment) = delta.get("partial_json").and_then(Value::as_str) {
                            out.push(SemanticEvent::ToolArgumentsDelta {
                                candidate: 0,
                                index,
                                fragment: fragment.to_owned(),
                            });
                            updates.push(StreamUpdate::ToolArgumentsDelta {
                                key,
                                tool_index: index,
                                fragment: fragment.to_owned(),
                            });
                        }
                    }
                    _ => {
                        let raw = json!({
                            "protocol": "anthropic",
                            "event": kind,
                            "index": index,
                            "delta": delta,
                        });
                        out.push(SemanticEvent::Opaque { raw: raw.clone() });
                        updates.push(StreamUpdate::Opaque { raw });
                    }
                }
            }
        }
        "content_block_stop" => {
            let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            out.push(SemanticEvent::PartEnd {
                candidate: 0,
                index,
            });
            updates.push(StreamUpdate::PartEnd {
                key: StreamKey::new(0, 0, index),
            });
        }
        "message_delta" => {
            if let Some(usage) = value.get("usage") {
                let usage = anthropic_usage(usage);
                out.push(SemanticEvent::Usage(usage));
                updates.push(StreamUpdate::Usage(usage));
            }
            if value
                .pointer("/delta/stop_reason")
                .and_then(Value::as_str)
                .is_some()
            {
                let mut report = ConversionReport::semantic();
                let reason = finish::from_anthropic(
                    value.pointer("/delta/stop_reason").and_then(Value::as_str),
                    &mut report,
                    "delta.stop_reason",
                );
                out.push(SemanticEvent::Finish {
                    candidate: 0,
                    reason: reason.clone(),
                });
                updates.push(StreamUpdate::Finish {
                    candidate: 0,
                    reason,
                });
            }
        }
        "message_stop" => {
            out.push(SemanticEvent::StreamEnd { incomplete: false });
            updates.push(StreamUpdate::End { incomplete: false });
        }
        "error" => {
            let error = stream_error(value.get("error").unwrap_or(&Value::Null));
            out.push(SemanticEvent::Error(error.clone()));
            updates.push(StreamUpdate::Error(error));
        }
        "ping" => {}
        _ => return false,
    }
    true
}
