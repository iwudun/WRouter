//! Gemini `streamGenerateContent` 客户端状态机。

use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

use crate::semantic::finish;
use crate::semantic::model::{ContentPart, SemanticEvent};
use crate::sse::{SsePayload, encode_sse};

use super::opening_arguments;

// ---------------------------------------------------------------------------
// Gemini 客户端
// ---------------------------------------------------------------------------

#[derive(Default)]
pub(super) struct GeminiClient {
    /// 尚未输出的工具调用(参数完整后才发 functionCall)。
    tools: HashMap<(usize, usize), GeminiTool>,
    seen_candidates: std::collections::BTreeSet<usize>,
    finishes: HashSet<usize>,
}

struct GeminiTool {
    id: String,
    name: String,
    arguments: String,
}

impl GeminiClient {
    fn frame(&self, candidate: usize, content: Value) -> Value {
        json!({"candidates": [{"index": candidate, "content": content}]})
    }

    fn text_frame(&self, candidate: usize, parts: Value) -> String {
        encode_sse(
            None,
            SsePayload::Json(&self.frame(candidate, json!({"role": "model", "parts": parts}))),
        )
    }

    fn flush_tool(&mut self, candidate: usize, index: usize, frames: &mut Vec<String>) {
        let Some(tool) = self.tools.remove(&(candidate, index)) else {
            return;
        };
        let args = if tool.arguments.trim().is_empty() {
            Some(json!({}))
        } else {
            serde_json::from_str::<Value>(&tool.arguments).ok()
        };
        let Some(args) = args else {
            // 参数残片不是完整 JSON:明确报错,不伪造 `{}` 让调用方
            // 以为工具可以执行。
            frames.push(encode_sse(
                None,
                SsePayload::Json(&json!({
                    "error": {
                        "code": "invalid_tool_arguments",
                        "status": "INVALID_ARGUMENT",
                        "message": format!(
                            "tool {:?} arguments are not complete JSON",
                            tool.name
                        ),
                    }
                })),
            ));
            return;
        };
        let mut call = serde_json::Map::new();
        if !tool.id.is_empty() {
            call.insert("id".to_owned(), json!(tool.id));
        }
        call.insert("name".to_owned(), json!(tool.name));
        call.insert("args".to_owned(), args);
        frames.push(self.text_frame(candidate, json!([{"functionCall": Value::Object(call)}])));
    }

    fn flush_candidate_tools(&mut self, candidate: usize, frames: &mut Vec<String>) {
        let mut indices: Vec<usize> = self
            .tools
            .keys()
            .filter_map(|(owner, index)| (*owner == candidate).then_some(*index))
            .collect();
        indices.sort_unstable();
        for index in indices {
            self.flush_tool(candidate, index, frames);
        }
    }

    pub(super) fn consume(&mut self, events: &[SemanticEvent]) -> Vec<String> {
        let mut frames = Vec::new();
        for event in events {
            match event {
                SemanticEvent::PartStart {
                    candidate,
                    part: ContentPart::Text { text },
                    ..
                } => {
                    self.seen_candidates.insert(*candidate);
                    if text.is_empty() {
                        continue;
                    }
                    frames.push(self.text_frame(*candidate, json!([{"text": text}])));
                }
                SemanticEvent::PartDelta {
                    candidate, text, ..
                } => {
                    self.seen_candidates.insert(*candidate);
                    if text.is_empty() {
                        continue;
                    }
                    frames.push(self.text_frame(*candidate, json!([{"text": text}])));
                }
                SemanticEvent::PartStart {
                    candidate,
                    part: ContentPart::Refusal { text },
                    ..
                }
                | SemanticEvent::RefusalDelta {
                    candidate, text, ..
                } => {
                    self.seen_candidates.insert(*candidate);
                    if text.is_empty() {
                        continue;
                    }
                    frames.push(self.text_frame(*candidate, json!([{"text": text}])));
                }
                SemanticEvent::PartStart {
                    candidate,
                    part:
                        ContentPart::ImageInline {
                            media_type,
                            data_base64,
                        },
                    ..
                } => {
                    self.seen_candidates.insert(*candidate);
                    frames.push(self.text_frame(
                        *candidate,
                        json!([{"inlineData": {
                            "mimeType": media_type.clone().unwrap_or_else(|| "image/png".to_owned()),
                            "data": data_base64,
                        }}]),
                    ));
                }
                SemanticEvent::PartStart {
                    candidate,
                    part: ContentPart::File { file_id, .. },
                    ..
                } => {
                    self.seen_candidates.insert(*candidate);
                    frames.push(
                        self.text_frame(*candidate, json!([{"fileData": {"fileUri": file_id}}])),
                    );
                }
                SemanticEvent::ReasoningDelta {
                    candidate, text, ..
                } => {
                    self.seen_candidates.insert(*candidate);
                    frames.push(
                        self.text_frame(*candidate, json!([{"text": text, "thought": true}])),
                    );
                }
                SemanticEvent::ReasoningSignature { .. } => {
                    // 跨协议签名不可移植:不伪造 thoughtSignature。
                }
                SemanticEvent::ToolCallStart {
                    candidate,
                    index,
                    call,
                } => {
                    self.seen_candidates.insert(*candidate);
                    self.tools
                        .entry((*candidate, *index))
                        .or_insert_with(|| GeminiTool {
                            id: call.id.local.clone(),
                            name: call.name.clone(),
                            arguments: opening_arguments(call).unwrap_or_default(),
                        });
                }
                SemanticEvent::ToolArgumentsDelta {
                    candidate,
                    index,
                    fragment,
                } => {
                    if let Some(tool) = self.tools.get_mut(&(*candidate, *index)) {
                        tool.arguments.push_str(fragment);
                    }
                }
                SemanticEvent::PartEnd {
                    candidate: _,
                    index: _,
                } => {
                    // 工具参数由 Finish/finalize 统一完整后输出;任意
                    // part 结束(尤其文本 content_index)不能误触发工具
                    // 提前 flush。
                }
                SemanticEvent::ToolResult { candidate, result } => {
                    self.seen_candidates.insert(*candidate);
                    let name = result
                        .resolved_by_name
                        .clone()
                        .or_else(|| result.call_ref.as_ref().map(|id| id.local.clone()))
                        .unwrap_or_default();
                    frames.push(self.text_frame(
                        *candidate,
                        json!([{"functionResponse": {
                            "name": name,
                            "response": result.content,
                        }}]),
                    ));
                }
                SemanticEvent::Usage(update) => {
                    let domain = update.to_domain();
                    let mut metadata = json!({
                        "promptTokenCount": domain.input_tokens,
                        "candidatesTokenCount": domain.output_tokens,
                        "totalTokenCount": domain.total_tokens,
                    });
                    if domain.reasoning_tokens > 0 {
                        metadata["thoughtsTokenCount"] = json!(domain.reasoning_tokens);
                    }
                    frames.push(encode_sse(
                        None,
                        SsePayload::Json(&json!({"usageMetadata": metadata})),
                    ));
                }
                SemanticEvent::Finish { candidate, reason } => {
                    self.seen_candidates.insert(*candidate);
                    self.flush_candidate_tools(*candidate, &mut frames);
                    self.finishes.insert(*candidate);
                    frames.push(encode_sse(
                        None,
                        SsePayload::Json(&json!({
                            "candidates": [{
                                "index": candidate,
                                "finishReason": finish::to_gemini(reason),
                            }],
                        })),
                    ));
                }
                SemanticEvent::Opaque { raw } => {
                    // Gemini 原生 safetyRatings 解码为 opaque:还原为
                    // candidates 帧;其余 opaque 不上 wire。
                    if let Some(ratings) = raw.get("safetyRatings") {
                        frames.push(encode_sse(
                            None,
                            SsePayload::Json(&json!({"candidates": [{
                                "safetyRatings": ratings,
                            }]})),
                        ));
                    }
                }
                _ => {}
            }
        }
        frames
    }

    pub(super) fn finalize(&mut self) -> Vec<String> {
        let mut frames = Vec::new();
        let candidates: Vec<usize> = self.seen_candidates.iter().copied().collect();
        for candidate in candidates {
            self.flush_candidate_tools(candidate, &mut frames);
        }
        frames
    }
}
