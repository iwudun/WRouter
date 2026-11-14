//! OpenAI Responses 上游流解码。

use serde_json::{Value, json};

use crate::semantic::ir::{ItemKind, StreamKey, StreamUpdate};
use crate::semantic::model::{
    ContentPart, FinishReason, SemanticEvent, SourceProtocol, ToolArguments, ToolCall,
    ToolCallId,
};
use crate::sse::SseEvent;

use super::{event_kind, output_index, stream_error};
use crate::semantic::stream::usage::openai_responses_usage;

/// Responses 的 `function_call.id` 恒以 `fc_` 开头(规范要求:服务端回执
/// id)。来源 id 是 chat 的 `call_` 开头时另生成一个回执 id,来源 id 归位
/// 到 `call_id`(关联键)。
///
/// 2026-11-06:旧实现在三条渲染路径上都把来源 id 同时填进 `id` 与
/// `call_id` → 严格校验节点 400(实测 AgentRouter:
/// `Invalid 'input[7].id': 'call_yDez…'. Expected an ID that begins with
/// 'fc'`)。
pub fn responses_function_call_id(source: &str) -> String {
    if source.starts_with("fc_") {
        source.to_owned()
    } else {
        format!("fc_{}", uuid::Uuid::new_v4().simple())
    }
}

/// Responses 事件族 → 语义事件。item id 无法落进内核的 content 槽位,
/// 因此在 `output_item.done` 上以 opaque 事件携带完整 item(含 id),
/// 既保留标识又不污染文本内容(契约 §2.4:协议本地 id 不参与内核关联)。
///
/// **done 事件只收口、不重发内容**(2026-11-06):
/// - `response.output_item.done` 不再推第二次 `ToolCallStart`(同一个
///   function_call 已在 `output_item.added` 开过场);
/// - `response.content_part.done` 的全文**仅作校验**,不进 delta 通道。
///
/// 两条都曾导致客户端把同一份内容收到两遍(工具参数被污染成 `{}`、
/// 文本重复两遍),详见 `docs/LESSONS.md` L-029。
pub(in crate::semantic::stream) fn decode_responses_upstream(
    event: &SseEvent,
    value: &Value,
    out: &mut Vec<SemanticEvent>,
    updates: &mut Vec<StreamUpdate>,
) {
    let kind = event_kind(event, value);
    let index = output_index(value);
    match kind {
        "response.created" | "response.in_progress" => {
            let id = value
                .pointer("/response/id")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let model = value
                .pointer("/response/model")
                .and_then(Value::as_str)
                .map(str::to_owned);
            out.push(SemanticEvent::MessageStart {
                id: id.clone(),
                model: model.clone(),
            });
            updates.push(StreamUpdate::ResponseStart {
                source: SourceProtocol::OpenAiResponses,
                id,
                model,
            });
        }
        "response.output_item.added" => {
            if let Some(item) = value.get("item") {
                let item_id = item.get("id").and_then(Value::as_str).map(str::to_owned);
                let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
                let item_kind = match item_type {
                    "message" => ItemKind::Message,
                    "reasoning" => ItemKind::Reasoning,
                    "function_call" => ItemKind::ToolCall,
                    other => ItemKind::Unknown(other.to_owned()),
                };
                let key = StreamKey::item(0, index);
                updates.push(StreamUpdate::ItemStart {
                    key,
                    id: item_id.clone(),
                    kind: item_kind,
                    extensions: crate::semantic::model::Extensions::default(),
                });
                match item_type {
                    "function_call" => {
                        let call = responses_tool_call(item);
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
                    "reasoning" => {
                        out.push(SemanticEvent::ReasoningStart {
                            candidate: 0,
                            index,
                        });
                        updates.push(StreamUpdate::ReasoningStart { key, id: item_id });
                    }
                    "message" => {}
                    _ => {}
                }
            }
        }
        "response.output_item.done" => {
            if let Some(item) = value.get("item") {
                // item id / 完整 item 以 opaque 保留(旧聚合路径);层级
                // 更新用 ItemSnapshot 带权威终值。
                out.push(SemanticEvent::Opaque {
                    raw: json!({
                        "protocol": "openai_responses",
                        "type": "response.output_item.done",
                        "output_index": index,
                        "item": item,
                    }),
                });
                updates.push(StreamUpdate::ItemSnapshot {
                    key: StreamKey::item(0, index),
                    snapshot: item.clone(),
                });
            }
            updates.push(StreamUpdate::ItemEnd {
                key: StreamKey::item(0, index),
            });
        }
        "response.content_part.added" => {
            if let Some(part) = value.get("part") {
                let content_index = value
                    .get("content_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize;
                let part_id = part
                    .get("id")
                    .and_then(Value::as_str)
                    .or_else(|| value.get("item_id").and_then(Value::as_str))
                    .map(str::to_owned);
                let key = StreamKey::new(0, index, content_index);
                let part_type = part.get("type").and_then(Value::as_str).unwrap_or("");
                match part_type {
                    "output_text" | "text" => {
                        let text = part.get("text").and_then(Value::as_str).unwrap_or("");
                        out.push(SemanticEvent::PartStart {
                            candidate: 0,
                            index: content_index,
                            part: ContentPart::Text {
                                text: String::new(),
                            },
                        });
                        updates.push(StreamUpdate::PartStart {
                            key,
                            id: part_id,
                            part: ContentPart::Text {
                                text: String::new(),
                            },
                            extensions: crate::semantic::model::Extensions::default(),
                        });
                        if !text.is_empty() {
                            out.push(SemanticEvent::PartDelta {
                                candidate: 0,
                                index: content_index,
                                text: text.to_owned(),
                            });
                            updates.push(StreamUpdate::PartDelta {
                                key,
                                text: text.to_owned(),
                            });
                        }
                    }
                    "refusal" => {
                        out.push(SemanticEvent::PartStart {
                            candidate: 0,
                            index: content_index,
                            part: ContentPart::Refusal {
                                text: String::new(),
                            },
                        });
                        updates.push(StreamUpdate::PartStart {
                            key,
                            id: part_id,
                            part: ContentPart::Refusal {
                                text: String::new(),
                            },
                            extensions: crate::semantic::model::Extensions::default(),
                        });
                        if let Some(text) = part
                            .get("refusal")
                            .or_else(|| part.get("text"))
                            .and_then(Value::as_str)
                            .filter(|text| !text.is_empty())
                        {
                            out.push(SemanticEvent::RefusalDelta {
                                candidate: 0,
                                index: content_index,
                                text: text.to_owned(),
                            });
                            updates.push(StreamUpdate::RefusalDelta {
                                key,
                                text: text.to_owned(),
                            });
                        }
                    }
                    other => {
                        let part = ContentPart::Opaque {
                            kind_hint: Some(other.to_owned()),
                            value: part.clone(),
                        };
                        out.push(SemanticEvent::PartStart {
                            candidate: 0,
                            index: content_index,
                            part: part.clone(),
                        });
                        updates.push(StreamUpdate::PartStart {
                            key,
                            id: part_id,
                            part,
                            extensions: crate::semantic::model::Extensions::default(),
                        });
                    }
                }
            }
        }
        "response.content_part.done" => {}
        "response.output_text.delta" => {
            if let Some(text) = value.get("delta").and_then(Value::as_str) {
                let content_index = value
                    .get("content_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize;
                out.push(SemanticEvent::PartDelta {
                    candidate: 0,
                    index: content_index,
                    text: text.to_owned(),
                });
                updates.push(StreamUpdate::PartDelta {
                    key: StreamKey::new(0, index, content_index),
                    text: text.to_owned(),
                });
            }
        }
        "response.refusal.delta" => {
            if let Some(text) = value.get("delta").and_then(Value::as_str) {
                let content_index = value
                    .get("content_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize;
                out.push(SemanticEvent::RefusalDelta {
                    candidate: 0,
                    index: content_index,
                    text: text.to_owned(),
                });
                updates.push(StreamUpdate::RefusalDelta {
                    key: StreamKey::new(0, index, content_index),
                    text: text.to_owned(),
                });
            }
        }
        "response.output_text.done" => {
            let content_index = value
                .get("content_index")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize;
            out.push(SemanticEvent::PartEnd {
                candidate: 0,
                index: content_index,
            });
            updates.push(StreamUpdate::PartEnd {
                key: StreamKey::new(0, index, content_index),
            });
        }
        "response.refusal.done" => {
            let content_index = value
                .get("content_index")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize;
            out.push(SemanticEvent::PartEnd {
                candidate: 0,
                index: content_index,
            });
            updates.push(StreamUpdate::PartEnd {
                key: StreamKey::new(0, index, content_index),
            });
        }
        "response.reasoning_summary_part.added" => {
            out.push(SemanticEvent::ReasoningStart {
                candidate: 0,
                index,
            });
            updates.push(StreamUpdate::ReasoningStart {
                key: StreamKey::item(0, index),
                id: value
                    .get("item_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            });
        }
        "response.reasoning_summary_text.delta" => {
            if let Some(text) = value.get("delta").and_then(Value::as_str) {
                out.push(SemanticEvent::ReasoningDelta {
                    candidate: 0,
                    index,
                    text: text.to_owned(),
                });
                updates.push(StreamUpdate::ReasoningDelta {
                    key: StreamKey::item(0, index),
                    text: text.to_owned(),
                });
            }
        }
        "response.reasoning_summary_text.done"
        | "response.reasoning_summary_part.done"
        | "response.function_call_arguments.done" => {
            out.push(SemanticEvent::PartEnd {
                candidate: 0,
                index,
            });
            updates.push(StreamUpdate::PartEnd {
                key: StreamKey::item(0, index),
            });
        }
        "response.function_call_arguments.delta" => {
            if let Some(fragment) = value.get("delta").and_then(Value::as_str) {
                out.push(SemanticEvent::ToolArgumentsDelta {
                    candidate: 0,
                    index,
                    fragment: fragment.to_owned(),
                });
                updates.push(StreamUpdate::ToolArgumentsDelta {
                    key: StreamKey::item(0, index),
                    tool_index: index,
                    fragment: fragment.to_owned(),
                });
            }
        }
        "response.completed" | "response.incomplete" => {
            if let Some(usage) = value.pointer("/response/usage") {
                let usage = openai_responses_usage(usage);
                out.push(SemanticEvent::Usage(usage));
                updates.push(StreamUpdate::Usage(usage));
            }
            let incomplete = kind == "response.incomplete";
            let reason = if incomplete {
                match value
                    .pointer("/response/incomplete_details/reason")
                    .and_then(Value::as_str)
                {
                    Some("max_output_tokens") => FinishReason::Length,
                    Some("content_filter") => FinishReason::ContentFilter,
                    _ => FinishReason::IncompleteOther,
                }
            } else {
                FinishReason::Stop
            };
            out.push(SemanticEvent::Finish {
                candidate: 0,
                reason: reason.clone(),
            });
            out.push(SemanticEvent::StreamEnd { incomplete });
            updates.push(StreamUpdate::Finish {
                candidate: 0,
                reason,
            });
            updates.push(StreamUpdate::End { incomplete });
        }
        "response.failed" => {
            let error = stream_error(value.pointer("/response/error").unwrap_or(&Value::Null));
            out.push(SemanticEvent::Error(error.clone()));
            updates.push(StreamUpdate::Error(error));
        }
        "response.cancelled" => {
            out.push(SemanticEvent::StreamEnd { incomplete: true });
            updates.push(StreamUpdate::End { incomplete: true });
        }
        "error" => {
            let error = stream_error(value.get("error").unwrap_or(&Value::Null));
            out.push(SemanticEvent::Error(error.clone()));
            updates.push(StreamUpdate::Error(error));
        }
        _ => {
            // 未知 Responses 事件保留(不再因为属于 response.* 家族而静默丢弃)。
            let raw = json!({
                "protocol": "openai_responses",
                "type": kind,
                "data": value,
            });
            out.push(SemanticEvent::Opaque { raw: raw.clone() });
            updates.push(StreamUpdate::Opaque { raw });
        }
    }
}

/// Responses `response.output_item.done` 帧里携带的**完整工具参数**。
///
/// 上游的 done 帧是「这个 item 结束了」的收口信号,它带的 `arguments`
/// 是该次调用的**最终值**(不是增量)。旧实现把它当增量重发 → 参数被
/// 累加两遍。这里只把它作为**补齐**来源:仅当整个流里一个参数增量都
/// 没到过时,才用它补一次(绝不重发)。
///
/// 返回 `(candidate, index, arguments)`;不是 Responses 的 done 帧、
/// 或不带非空参数时返回 `None`。
pub fn responses_done_arguments(raw: &Value) -> Option<(usize, usize, String)> {
    if raw.get("type").and_then(Value::as_str) != Some("response.output_item.done") {
        return None;
    }
    let index = raw.get("output_index").and_then(Value::as_u64).unwrap_or(0) as usize;
    let arguments = raw.pointer("/item/arguments").and_then(Value::as_str)?;
    if arguments.is_empty() {
        return None;
    }
    Some((0, index, arguments.to_owned()))
}

fn responses_tool_call(item: &Value) -> ToolCall {
    ToolCall {
        id: ToolCallId {
            local: item
                .get("call_id")
                .or_else(|| item.get("id"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            origin: crate::semantic::model::IdOrigin::Source,
        },
        name: item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        arguments: ToolArguments::Raw(
            item.get("arguments")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
        ),
        extensions: Default::default(),
    }
}
