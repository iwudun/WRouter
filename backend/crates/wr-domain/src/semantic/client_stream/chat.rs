//! OpenAI Chat Completions 客户端状态机。

use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

use crate::semantic::finish;
use crate::semantic::model::{ContentPart, FinishReason, SemanticEvent};
use crate::semantic::util::created_now;
use crate::sse::{SsePayload, encode_sse};
use crate::usage::Usage;

use super::opening_arguments;

pub(super) struct ChatClient {
    completion_id: String,
    model: String,
    /// 每个候选的角色帧是否已发(多候选各自独立)。
    first_delta: HashSet<usize>,
    /// 每个候选的结束原因。整条流每个候选只允许一个 finish_reason。
    finishes: HashMap<usize, FinishReason>,
    /// 出现过的候选(即使没有内容,finish 兜底也要覆盖)。
    seen_candidates: std::collections::BTreeSet<usize>,
    /// usage 是否已随某个帧下发给客户端。
    usage_sent: bool,
    /// 上游/投影的 block index → 目标 chat 的 tool_calls index(每候选连续)。
    tool_indices: HashMap<(usize, usize), usize>,
    next_tool_index: HashMap<usize, usize>,
}

impl ChatClient {
    pub(super) fn new() -> Self {
        Self {
            completion_id: "chatcmpl-wrouter".to_owned(),
            model: String::new(),
            first_delta: HashSet::new(),
            finishes: HashMap::new(),
            seen_candidates: std::collections::BTreeSet::new(),
            usage_sent: false,
            tool_indices: HashMap::new(),
            next_tool_index: HashMap::new(),
        }
    }

    fn chunk(&self, candidate: usize, delta: &Value) -> Value {
        json!({
            "id": self.completion_id,
            "object": "chat.completion.chunk",
            "created": created_now(),
            "model": self.model,
            "choices": [{"index": candidate, "delta": delta, "finish_reason": Value::Null}],
        })
    }

    /// delta 帧;每个候选的首个内容 delta 携带 role=assistant。
    fn delta_chunk(&mut self, candidate: usize, delta: serde_json::Map<String, Value>) -> String {
        let mut ordered = serde_json::Map::new();
        if self.first_delta.insert(candidate) {
            ordered.insert("role".to_owned(), json!("assistant"));
        }
        for (key, value) in delta {
            ordered.insert(key, value);
        }
        encode_sse(
            None,
            SsePayload::Json(&self.chunk(candidate, &Value::Object(ordered))),
        )
    }

    /// 工具开场块:恒带 role=assistant。
    fn tool_opening_chunk(&mut self, candidate: usize, entry: Value) -> String {
        let mut delta = serde_json::Map::new();
        delta.insert("role".to_owned(), json!("assistant"));
        delta.insert("tool_calls".to_owned(), json!([entry]));
        self.first_delta.insert(candidate);
        encode_sse(
            None,
            SsePayload::Json(&self.chunk(candidate, &Value::Object(delta))),
        )
    }

    fn tool_delta(&mut self, candidate: usize, entry: Value) -> String {
        let mut delta = serde_json::Map::new();
        delta.insert("tool_calls".to_owned(), json!([entry]));
        self.delta_chunk(candidate, delta)
    }

    fn finish_chunk(&mut self, candidate: usize, reason: &FinishReason, usage: Usage) -> String {
        let mut chunk = json!({
            "id": self.completion_id,
            "object": "chat.completion.chunk",
            "created": created_now(),
            "model": self.model,
            "choices": [{
                "index": candidate,
                "delta": {},
                "finish_reason": finish::to_openai_chat(reason),
            }],
        });
        if usage.total_tokens != 0 && !self.usage_sent {
            chunk["usage"] = json!({
                "prompt_tokens": usage.input_tokens,
                "completion_tokens": usage.output_tokens,
                "total_tokens": usage.total_tokens,
                "prompt_tokens_details": {"cached_tokens": usage.cache_read_tokens},
            });
            self.usage_sent = true;
        }
        encode_sse(None, SsePayload::Json(&chunk))
    }

    pub(super) fn consume(&mut self, events: &[SemanticEvent], usage: Usage) -> Vec<String> {
        let mut frames = Vec::new();
        for event in events {
            match event {
                SemanticEvent::MessageStart { id, model } => {
                    if let Some(id) = id.as_ref().filter(|id| !id.is_empty()) {
                        self.completion_id = id.clone();
                    }
                    if let Some(model) = model {
                        self.model = model.clone();
                    }
                }
                SemanticEvent::PartStart {
                    candidate,
                    part: ContentPart::Text { text },
                    ..
                }
                | SemanticEvent::PartDelta {
                    candidate, text, ..
                } => {
                    self.seen_candidates.insert(*candidate);
                    if text.is_empty() {
                        continue;
                    }
                    let mut delta = serde_json::Map::new();
                    delta.insert("content".to_owned(), json!(text));
                    frames.push(self.delta_chunk(*candidate, delta));
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
                    let mut delta = serde_json::Map::new();
                    delta.insert("refusal".to_owned(), json!(text));
                    frames.push(self.delta_chunk(*candidate, delta));
                }
                SemanticEvent::ReasoningDelta {
                    candidate, text, ..
                } => {
                    self.seen_candidates.insert(*candidate);
                    if text.is_empty() {
                        continue;
                    }
                    let mut delta = serde_json::Map::new();
                    delta.insert("reasoning_content".to_owned(), json!(text));
                    frames.push(self.delta_chunk(*candidate, delta));
                }
                SemanticEvent::ToolCallStart {
                    candidate,
                    index,
                    call,
                } => {
                    self.seen_candidates.insert(*candidate);
                    let tool_index = *self.next_tool_index.entry(*candidate).or_insert(0);
                    self.tool_indices.insert((*candidate, *index), tool_index);
                    *self
                        .next_tool_index
                        .get_mut(candidate)
                        .expect("just inserted") += 1;
                    let mut entry = serde_json::Map::new();
                    entry.insert("index".to_owned(), json!(tool_index));
                    if !call.id.local.is_empty() {
                        entry.insert("id".to_owned(), json!(call.id.local));
                    }
                    entry.insert("type".to_owned(), json!("function"));
                    entry.insert(
                        "function".to_owned(),
                        json!({"name": call.name, "arguments": ""}),
                    );
                    frames.push(self.tool_opening_chunk(*candidate, Value::Object(entry)));
                    if let Some(fragment) = opening_arguments(call) {
                        frames.push(self.tool_delta(
                            *candidate,
                            json!({"index": tool_index, "function": {"arguments": fragment}}),
                        ));
                    }
                }
                SemanticEvent::ToolArgumentsDelta {
                    candidate,
                    index,
                    fragment,
                } => {
                    self.seen_candidates.insert(*candidate);
                    let tool_index = self
                        .tool_indices
                        .get(&(*candidate, *index))
                        .copied()
                        .unwrap_or(*index);
                    frames.push(self.tool_delta(
                        *candidate,
                        json!({"index": tool_index, "function": {"arguments": fragment}}),
                    ));
                }
                SemanticEvent::Finish { candidate, reason } => {
                    self.seen_candidates.insert(*candidate);
                    if self.finishes.contains_key(candidate) {
                        continue;
                    }
                    self.finishes.insert(*candidate, reason.clone());
                    frames.push(self.finish_chunk(*candidate, reason, usage));
                }
                // Usage 由外壳累计;StreamEnd/Error/Opaque/块边界交由
                // 完整性门与 502 路径。
                _ => {}
            }
        }
        frames
    }

    /// 正常收口:每个候选最多一个 finish_reason;缺失才补 stop。
    pub(super) fn finalize(&mut self, usage: Usage) -> Vec<String> {
        let mut frames = Vec::new();
        if self.seen_candidates.is_empty() {
            self.seen_candidates.insert(0);
        }
        let candidates: Vec<usize> = self.seen_candidates.iter().copied().collect();
        for candidate in candidates {
            if self.finishes.contains_key(&candidate) {
                continue;
            }
            let mut chunk = self.chunk(candidate, &json!({}));
            chunk["choices"][0]["finish_reason"] = json!("stop");
            if usage.total_tokens != 0 && !self.usage_sent {
                chunk["usage"] = json!({
                    "prompt_tokens": usage.input_tokens,
                    "completion_tokens": usage.output_tokens,
                    "total_tokens": usage.total_tokens,
                    "prompt_tokens_details": {"cached_tokens": usage.cache_read_tokens},
                });
                self.usage_sent = true;
            }
            frames.push(encode_sse(None, SsePayload::Json(&chunk)));
            self.finishes.insert(candidate, FinishReason::Stop);
        }
        if !self.usage_sent && usage.total_tokens != 0 {
            frames.push(self.usage_chunk(usage));
            self.usage_sent = true;
        }
        frames.push(encode_sse(None, SsePayload::Raw("[DONE]")));
        frames
    }

    /// usage 尾块(OpenAI `stream_options.include_usage` 形状):`choices: []`
    /// + usage —— **不带 finish_reason**,不与上游的结束原因冲突。
    fn usage_chunk(&self, usage: Usage) -> String {
        let mut chunk = json!({
            "id": self.completion_id,
            "object": "chat.completion.chunk",
            "created": created_now(),
            "model": self.model,
            "choices": [],
        });
        chunk["usage"] = json!({
            "prompt_tokens": usage.input_tokens,
            "completion_tokens": usage.output_tokens,
            "total_tokens": usage.total_tokens,
            "prompt_tokens_details": {"cached_tokens": usage.cache_read_tokens},
        });
        encode_sse(None, SsePayload::Json(&chunk))
    }
}
