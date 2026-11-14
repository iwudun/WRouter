//! OpenAI Chat Completions 上游流解码。

use serde_json::Value;

use crate::semantic::diagnostic::ConversionReport;
use crate::semantic::finish;
use crate::semantic::ir::{StreamKey, StreamUpdate};
use crate::semantic::model::{
    SemanticEvent, SourceProtocol, ToolArguments, ToolCall, ToolCallId,
};
use crate::sse::SseEvent;

use super::{decode_responses_upstream, is_responses_payload, stream_error};
use crate::semantic::stream::usage::openai_usage;

pub(in crate::semantic::stream) fn decode_openai_upstream(
    event: &SseEvent,
    value: &Value,
    out: &mut Vec<SemanticEvent>,
    updates: &mut Vec<StreamUpdate>,
) -> bool {
    if is_responses_payload(event, value) {
        decode_responses_upstream(event, value, out, updates);
        return true;
    }
    let recognized = value.get("choices").is_some()
        || value.get("id").is_some()
        || value.get("usage").is_some()
        || value.get("error").is_some();
    if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
        let semantic = stream_error(error);
        out.push(SemanticEvent::Error(semantic.clone()));
        updates.push(StreamUpdate::Error(semantic));
    }
    if let Some(id) = value.get("id").and_then(Value::as_str) {
        let model = value
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_owned);
        out.push(SemanticEvent::MessageStart {
            id: Some(id.to_owned()),
            model: model.clone(),
        });
        updates.push(StreamUpdate::ResponseStart {
            source: SourceProtocol::OpenAi,
            id: Some(id.to_owned()),
            model,
        });
    }
    if let Some(choices) = value.get("choices").and_then(Value::as_array) {
        for choice in choices {
            let candidate = choice.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            if let Some(delta) = choice.get("delta") {
                decode_chat_delta(delta, candidate, out, updates);
            }
            if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                let mut report = ConversionReport::semantic();
                let reason = finish::from_openai(Some(reason), &mut report, "stream.finish_reason");
                out.push(SemanticEvent::Finish {
                    candidate,
                    reason: reason.clone(),
                });
                updates.push(StreamUpdate::Finish { candidate, reason });
            }
        }
    }
    if let Some(usage) = value.get("usage").filter(|usage| !usage.is_null()) {
        let usage = openai_usage(usage);
        out.push(SemanticEvent::Usage(usage));
        updates.push(StreamUpdate::Usage(usage));
    }
    recognized
}

fn decode_chat_delta(
    delta: &Value,
    candidate: usize,
    out: &mut Vec<SemanticEvent>,
    updates: &mut Vec<StreamUpdate>,
) {
    match delta.get("content") {
        Some(Value::String(text)) if !text.is_empty() => {
            out.push(SemanticEvent::PartDelta {
                candidate,
                index: 0,
                text: text.clone(),
            });
            updates.push(StreamUpdate::PartDelta {
                key: StreamKey::new(candidate, 0, 0),
                text: text.clone(),
            });
        }
        // 少数供应商在 delta 里发 content parts 数组。
        Some(Value::Array(parts)) => {
            for (index, part) in parts.iter().enumerate() {
                let Some(text) = part.get("text").and_then(Value::as_str) else {
                    continue;
                };
                if text.is_empty() {
                    continue;
                }
                let key = StreamKey::new(candidate, 0, index);
                out.push(SemanticEvent::PartDelta {
                    candidate,
                    index,
                    text: text.to_owned(),
                });
                updates.push(StreamUpdate::PartDelta {
                    key,
                    text: text.to_owned(),
                });
            }
        }
        _ => {}
    }
    if let Some(text) = delta
        .get("reasoning_content")
        .or_else(|| delta.get("reasoning"))
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        out.push(SemanticEvent::ReasoningDelta {
            candidate,
            index: 0,
            text: text.to_owned(),
        });
        updates.push(StreamUpdate::ReasoningDelta {
            key: StreamKey::new(candidate, 1, 0),
            text: text.to_owned(),
        });
    }
    if let Some(text) = delta
        .get("refusal")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        out.push(SemanticEvent::RefusalDelta {
            candidate,
            index: 0,
            text: text.to_owned(),
        });
        updates.push(StreamUpdate::RefusalDelta {
            key: StreamKey::new(candidate, 0, 1),
            text: text.to_owned(),
        });
    }
    if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
        for item in calls {
            let index = item.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            let function = item.get("function").unwrap_or(&Value::Null);
            let key = StreamKey::new(candidate, 2 + index, 0);
            if let Some(name) = function.get("name").and_then(Value::as_str) {
                let id = item
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let call = ToolCall {
                    id: ToolCallId {
                        local: id,
                        origin: crate::semantic::model::IdOrigin::Source,
                    },
                    name: name.to_owned(),
                    arguments: ToolArguments::Raw(String::new()),
                    extensions: Default::default(),
                };
                out.push(SemanticEvent::ToolCallStart {
                    candidate,
                    index,
                    call: call.clone(),
                });
                updates.push(StreamUpdate::ToolStart {
                    key,
                    tool_index: index,
                    call,
                });
            }
            if let Some(fragment) = function
                .get("arguments")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
            {
                out.push(SemanticEvent::ToolArgumentsDelta {
                    candidate,
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
    }
}
