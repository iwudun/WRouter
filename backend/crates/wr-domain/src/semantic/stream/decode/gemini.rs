//! Gemini `streamGenerateContent` 上游流解码。

use serde_json::{Value, json};

use crate::semantic::diagnostic::ConversionReport;
use crate::semantic::error::SemanticError;
use crate::semantic::finish;
use crate::semantic::ir::{StreamKey, StreamUpdate};
use crate::semantic::model::{
    ContentPart, FinishReason, SemanticEvent, ToolArguments, ToolCall,
    ToolCallId,
};
use crate::sse::SseEvent;

use super::stream_error;
use crate::semantic::stream::usage::gemini_usage;

/// Gemini `:streamGenerateContent` chunk → 语义事件。
pub(in crate::semantic::stream) fn decode_gemini_upstream(
    _event: &SseEvent,
    value: &Value,
    out: &mut Vec<SemanticEvent>,
    updates: &mut Vec<StreamUpdate>,
) -> bool {
    let recognized = value.get("candidates").is_some()
        || value.get("usageMetadata").is_some()
        || value.get("promptFeedback").is_some()
        || value.get("error").is_some();
    if let Some(block) = value
        .pointer("/promptFeedback/blockReason")
        .and_then(Value::as_str)
    {
        let error = SemanticError::unsupported(format!("gemini prompt blocked: {block}"));
        out.push(SemanticEvent::Error(error.clone()));
        updates.push(StreamUpdate::Error(error));
    }
    if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
        let error = stream_error(error);
        out.push(SemanticEvent::Error(error.clone()));
        updates.push(StreamUpdate::Error(error));
    }
    if let Some(usage) = value.get("usageMetadata") {
        let usage = gemini_usage(usage);
        out.push(SemanticEvent::Usage(usage));
        updates.push(StreamUpdate::Usage(usage));
    }
    let mut terminal = false;
    let mut incomplete_terminal = false;
    if let Some(candidates) = value.get("candidates").and_then(Value::as_array) {
        for (candidate_index, candidate) in candidates.iter().enumerate() {
            if let Some(parts) = candidate
                .pointer("/content/parts")
                .and_then(Value::as_array)
            {
                for (part_index, part) in parts.iter().enumerate() {
                    decode_gemini_part(candidate_index, part_index, part, out, updates);
                }
            }
            if let Some(ratings) = candidate.get("safetyRatings") {
                let raw = json!({
                    "protocol": "gemini",
                    "candidate": candidate_index,
                    "safetyRatings": ratings,
                });
                out.push(SemanticEvent::Opaque { raw: raw.clone() });
                updates.push(StreamUpdate::Opaque { raw });
            }
            if let Some(reason) = candidate.get("finishReason").and_then(Value::as_str) {
                let mut report = ConversionReport::semantic();
                let finish_reason =
                    finish::from_gemini(Some(reason), &mut report, "candidates.finishReason");
                // STOP/MAX_TOKENS 是正常终止(长度受限不算断流);
                // 安全/未映射终止视为不完整。
                let incomplete =
                    !matches!(finish_reason, FinishReason::Stop | FinishReason::Length);
                out.push(SemanticEvent::Finish {
                    candidate: candidate_index,
                    reason: finish_reason.clone(),
                });
                updates.push(StreamUpdate::Finish {
                    candidate: candidate_index,
                    reason: finish_reason,
                });
                terminal = true;
                incomplete_terminal |= incomplete;
            }
        }
    }
    // Gemini 的 finishReason 是流终止信号:任一候选给出即结束,不要求
    // 恰好是数组最后一个(多候选时旧实现会漏掉 StreamEnd)。
    if terminal {
        out.push(SemanticEvent::StreamEnd {
            incomplete: incomplete_terminal,
        });
        updates.push(StreamUpdate::End {
            incomplete: incomplete_terminal,
        });
    }
    recognized
}

fn decode_gemini_part(
    candidate: usize,
    index: usize,
    part: &Value,
    out: &mut Vec<SemanticEvent>,
    updates: &mut Vec<StreamUpdate>,
) {
    let key = StreamKey::new(candidate, 0, index);
    if let Some(text) = part.get("text").and_then(Value::as_str) {
        let thought = part
            .get("thought")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if thought {
            out.push(SemanticEvent::ReasoningDelta {
                candidate,
                index,
                text: text.to_owned(),
            });
            updates.push(StreamUpdate::ReasoningDelta {
                key,
                text: text.to_owned(),
            });
        } else {
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
    if let Some(signature) = part.get("thoughtSignature").and_then(Value::as_str) {
        out.push(SemanticEvent::ReasoningSignature {
            candidate,
            index,
            signature: signature.to_owned(),
        });
        updates.push(StreamUpdate::ReasoningSignature {
            key,
            signature: signature.to_owned(),
        });
    }
    if let Some(call_value) = part.get("functionCall") {
        let call = ToolCall {
            id: ToolCallId {
                local: call_value
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                origin: crate::semantic::model::IdOrigin::Source,
            },
            name: call_value
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            arguments: ToolArguments::Parsed(
                call_value.get("args").cloned().unwrap_or_else(|| json!({})),
            ),
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
    if let Some(response) = part.get("functionResponse") {
        let result = crate::semantic::model::ToolResult {
            call_ref: response
                .get("id")
                .and_then(Value::as_str)
                .map(|id| ToolCallId {
                    local: id.to_owned(),
                    origin: crate::semantic::model::IdOrigin::Source,
                }),
            resolved_by_name: response
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_owned),
            content: response
                .get("response")
                .cloned()
                .unwrap_or_else(|| response.clone()),
            is_error: false,
            extensions: Default::default(),
        };
        out.push(SemanticEvent::ToolResult {
            candidate,
            result: result.clone(),
        });
        updates.push(StreamUpdate::ToolResult { key, result });
    }
    if let Some(inline) = part.get("inlineData") {
        let part = ContentPart::ImageInline {
            media_type: inline
                .get("mimeType")
                .and_then(Value::as_str)
                .map(str::to_owned),
            data_base64: inline
                .get("data")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
        };
        out.push(SemanticEvent::PartStart {
            candidate,
            index,
            part: part.clone(),
        });
        updates.push(StreamUpdate::PartStart {
            key,
            id: None,
            part,
            extensions: crate::semantic::model::Extensions::default(),
        });
    }
    if let Some(file) = part.get("fileData") {
        let part = ContentPart::File {
            file_id: file
                .get("fileUri")
                .and_then(Value::as_str)
                .map(str::to_owned),
            filename: None,
            data_base64: None,
        };
        out.push(SemanticEvent::PartStart {
            candidate,
            index,
            part: part.clone(),
        });
        updates.push(StreamUpdate::PartStart {
            key,
            id: None,
            part,
            extensions: crate::semantic::model::Extensions::default(),
        });
    }
}
