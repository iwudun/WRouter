//! Anthropic Messages 客户端状态机。

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::semantic::finish;
use crate::semantic::model::{ContentPart, FinishReason, SemanticEvent};
use crate::semantic::util::uuid_short;
use crate::sse::{SsePayload, encode_sse};
use crate::usage::Usage;

use super::opening_arguments;

// ---------------------------------------------------------------------------
// Anthropic Messages 客户端
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum BlockKind {
    Text,
    Thinking,
    Tool,
    Opaque,
}

pub(super) struct AnthropicClient {
    message_id: String,
    model: String,
    /// 持有的 message_start(等 usage 合并后再发出,保证首帧携带
    /// 已知输入用量)。
    pending_start: Option<Value>,
    blocks: HashMap<usize, BlockKind>,
    /// 源 (candidate, part) → Anthropic content block index。多 part
    /// 消息不再被合并成单个文本块。
    part_blocks: HashMap<(usize, usize), usize>,
    next_index: usize,
    finish: Option<FinishReason>,
    has_tools: bool,
    /// 阶段五 C-3:续写模式 —— 抑制重复的 message_start(续写是同一条
    /// 回复的延续;Anthropic 协议里 message_start 只能出现一次)。
    continuation: bool,
}

impl AnthropicClient {
    pub(super) fn new() -> Self {
        Self {
            message_id: "msg_wrouter".to_owned(),
            model: String::new(),
            pending_start: None,
            blocks: HashMap::new(),
            part_blocks: HashMap::new(),
            next_index: 0,
            finish: None,
            has_tools: false,
            continuation: false,
        }
    }

    /// 阶段五 C-3:进入续写模式(抑制重复的 message_start)。
    pub(super) fn begin_continuation(&mut self) {
        self.continuation = true;
    }

    /// 事件自带 index 时采纳;跨协议上游复用同一 index 时分配新块号。
    fn allocate_index(&mut self, hint: usize) -> usize {
        if self.blocks.contains_key(&hint) {
            let allocated = self.next_index;
            self.next_index += 1;
            allocated
        } else {
            if hint >= self.next_index {
                self.next_index = hint + 1;
            }
            hint
        }
    }

    fn frame(&self, event: &str, data: &Value) -> String {
        encode_sse(Some(event), SsePayload::Json(data))
    }

    /// 发出持有的 message_start(任何后续帧或 finalize 前)。
    fn flush_start(&mut self, frames: &mut Vec<String>) {
        if let Some(start) = self.pending_start.take() {
            frames.push(encode_sse(Some("message_start"), SsePayload::Json(&start)));
        }
    }

    fn block_start(&mut self, index: usize, kind: BlockKind, content_block: Value) -> String {
        self.blocks.insert(index, kind);
        self.frame(
            "content_block_start",
            &json!({"type": "content_block_start", "index": index, "content_block": content_block}),
        )
    }

    fn block_delta(&self, index: usize, delta: Value) -> String {
        self.frame(
            "content_block_delta",
            &json!({"type": "content_block_delta", "index": index, "delta": delta}),
        )
    }

    fn block_stop(&mut self, index: usize) -> Option<String> {
        if self.blocks.remove(&index).is_some() {
            Some(self.frame(
                "content_block_stop",
                &json!({"type": "content_block_stop", "index": index}),
            ))
        } else {
            None
        }
    }

    /// 按源 part 身份找到(或开启)块。不同 part 不复用同一 block。
    fn ensure_block_for(
        &mut self,
        candidate: usize,
        hint: usize,
        kind: BlockKind,
    ) -> (usize, Option<String>) {
        if let Some(index) = self.part_blocks.get(&(candidate, hint)).copied() {
            return (index, None);
        }
        let index = self.allocate_index(hint);
        self.part_blocks.insert((candidate, hint), index);
        let content_block = match kind {
            BlockKind::Text => Some(json!({"type": "text", "text": ""})),
            BlockKind::Thinking => Some(json!({"type": "thinking", "thinking": ""})),
            BlockKind::Tool | BlockKind::Opaque => None,
        };
        (
            index,
            content_block.map(|block| self.block_start(index, kind, block)),
        )
    }

    fn block_for(&self, candidate: usize, hint: usize) -> Option<usize> {
        self.part_blocks.get(&(candidate, hint)).copied()
    }

    fn close_key(&mut self, candidate: usize, hint: usize) -> Option<String> {
        let index = self.part_blocks.remove(&(candidate, hint))?;
        self.block_stop(index)
    }

    pub(super) fn consume(&mut self, events: &[SemanticEvent], usage: Usage) -> Vec<String> {
        let mut frames = Vec::new();
        for event in events {
            match event {
                SemanticEvent::MessageStart { id, model } => {
                    // 阶段五 C-3:续写模式下**抑制** message_start ——
                    // 客户端已经收到过一次(续写是同一条回复的延续,不是
                    // 新消息);Anthropic 协议里 message_start 只能出现
                    // 一次,重复下发会破坏客户端的消息序列。
                    if self.continuation {
                        continue;
                    }
                    if let Some(id) = id.as_ref().filter(|id| !id.is_empty()) {
                        self.message_id = id.clone();
                    }
                    if let Some(model) = model {
                        self.model = model.clone();
                    }
                    self.pending_start = Some(json!({
                        "type": "message_start",
                        "message": {
                            "id": self.message_id,
                            "type": "message",
                            "role": "assistant",
                            "model": self.model,
                            "content": [],
                            "stop_reason": Value::Null,
                            "usage": anthropic_usage_value(usage),
                        },
                    }));
                }
                SemanticEvent::Usage(_) => {
                    // message_start 尚未发出时把已知用量并进去。
                    if let Some(start) = &mut self.pending_start
                        && let Some(message) =
                            start.get_mut("message").and_then(Value::as_object_mut)
                    {
                        message.insert("usage".to_owned(), anthropic_usage_value(usage));
                    }
                }
                SemanticEvent::PartStart {
                    candidate,
                    index,
                    part,
                } => {
                    if *candidate != 0 {
                        continue;
                    }
                    match part {
                        ContentPart::Text { text } => {
                            let (index, start) =
                                self.ensure_block_for(*candidate, *index, BlockKind::Text);
                            self.flush_start(&mut frames);
                            if let Some(start) = start {
                                frames.push(start);
                            }
                            if !text.is_empty() {
                                frames.push(self.block_delta(
                                    index,
                                    json!({"type": "text_delta", "text": text}),
                                ));
                            }
                        }
                        ContentPart::Refusal { text } => {
                            let (index, start) =
                                self.ensure_block_for(*candidate, *index, BlockKind::Text);
                            self.flush_start(&mut frames);
                            if let Some(start) = start {
                                frames.push(start);
                            }
                            if !text.is_empty() {
                                frames.push(self.block_delta(
                                    index,
                                    json!({"type": "text_delta", "text": text}),
                                ));
                            }
                        }
                        // 未知/redacted 块:以原样块形状透传(保真)。
                        ContentPart::Opaque { value, .. } => {
                            let should_start = value.get("type").and_then(Value::as_str).is_some()
                                && !self.part_blocks.contains_key(&(*candidate, *index));
                            if should_start {
                                let (index, _) =
                                    self.ensure_block_for(*candidate, *index, BlockKind::Opaque);
                                self.flush_start(&mut frames);
                                frames.push(self.block_start(
                                    index,
                                    BlockKind::Opaque,
                                    value.clone(),
                                ));
                            }
                        }
                        _ => {}
                    }
                }
                SemanticEvent::PartDelta {
                    candidate,
                    index,
                    text,
                } => {
                    if *candidate != 0 {
                        continue;
                    }
                    let (index, start) = self.ensure_block_for(*candidate, *index, BlockKind::Text);
                    self.flush_start(&mut frames);
                    if let Some(start) = start {
                        frames.push(start);
                    }
                    frames
                        .push(self.block_delta(index, json!({"type": "text_delta", "text": text})));
                }
                SemanticEvent::RefusalDelta {
                    candidate,
                    index,
                    text,
                } => {
                    if *candidate != 0 {
                        continue;
                    }
                    let (index, start) = self.ensure_block_for(*candidate, *index, BlockKind::Text);
                    self.flush_start(&mut frames);
                    if let Some(start) = start {
                        frames.push(start);
                    }
                    frames
                        .push(self.block_delta(index, json!({"type": "text_delta", "text": text})));
                }
                SemanticEvent::PartEnd { candidate, index } => {
                    if *candidate != 0 {
                        continue;
                    }
                    self.flush_start(&mut frames);
                    if let Some(frame) = self.close_key(*candidate, *index) {
                        frames.push(frame);
                    }
                }
                SemanticEvent::ReasoningStart {
                    candidate, index, ..
                } => {
                    if *candidate != 0 {
                        continue;
                    }
                    let (_, start) = self.ensure_block_for(*candidate, *index, BlockKind::Thinking);
                    self.flush_start(&mut frames);
                    if let Some(start) = start {
                        frames.push(start);
                    }
                }
                SemanticEvent::ReasoningDelta {
                    candidate,
                    index,
                    text,
                    ..
                } => {
                    if *candidate != 0 {
                        continue;
                    }
                    let (index, start) =
                        self.ensure_block_for(*candidate, *index, BlockKind::Thinking);
                    self.flush_start(&mut frames);
                    if let Some(start) = start {
                        frames.push(start);
                    }
                    frames.push(
                        self.block_delta(
                            index,
                            json!({"type": "thinking_delta", "thinking": text}),
                        ),
                    );
                }
                SemanticEvent::ReasoningSignature { .. } => {
                    // 跨协议签名不可移植:不伪造目标签名。
                }
                SemanticEvent::ToolCallStart {
                    candidate,
                    index,
                    call,
                } => {
                    if *candidate != 0 {
                        continue;
                    }
                    if self.part_blocks.contains_key(&(*candidate, *index)) {
                        continue;
                    }
                    let (block, _) = self.ensure_block_for(*candidate, *index, BlockKind::Tool);
                    self.flush_start(&mut frames);
                    self.has_tools = true;
                    frames.push(self.block_start(
                        block,
                        BlockKind::Tool,
                        json!({
                            "type": "tool_use",
                            "id": if call.id.local.is_empty() {
                                format!("toolu_{}", uuid_short())
                            } else {
                                call.id.local.clone()
                            },
                            "name": call.name,
                            "input": {},
                        }),
                    ));
                    if let Some(fragment) = opening_arguments(call) {
                        frames.push(self.block_delta(
                            block,
                            json!({"type": "input_json_delta", "partial_json": fragment}),
                        ));
                    }
                }
                SemanticEvent::ToolArgumentsDelta {
                    candidate,
                    index,
                    fragment,
                } => {
                    if *candidate != 0 {
                        continue;
                    }
                    if let Some(block) = self.block_for(*candidate, *index) {
                        frames.push(self.block_delta(
                            block,
                            json!({"type": "input_json_delta", "partial_json": fragment}),
                        ));
                    }
                }
                SemanticEvent::Finish {
                    candidate: 0,
                    reason,
                } => {
                    self.finish = Some(reason.clone());
                }
                // Usage 由外壳统一累计;StreamEnd/Error/Opaque 由网关
                // 完整性门与 502 路径处理。
                _ => {}
            }
        }
        frames
    }

    /// 正常收口:关闭未闭合块 + `message_delta`(语义 stop_reason +
    /// 归一 usage)+ `message_stop`。整条流只输出一次终结序列。
    pub(super) fn finalize(&mut self, usage: Usage) -> Vec<String> {
        let mut frames = Vec::new();
        self.flush_start(&mut frames);
        let mut open_blocks: Vec<usize> = self.blocks.keys().copied().collect();
        open_blocks.sort_unstable();
        for index in open_blocks {
            if let Some(frame) = self.block_stop(index) {
                frames.push(frame);
            }
        }
        let stop_reason = match self.finish.as_ref().map(finish::to_anthropic) {
            Some(reason) => reason,
            None if self.has_tools => "tool_use".to_owned(),
            None => "end_turn".to_owned(),
        };
        frames.push(self.frame(
            "message_delta",
            &json!({
                "type": "message_delta",
                "delta": {"stop_reason": stop_reason, "stop_sequence": Value::Null},
                "usage": anthropic_usage_value(usage),
            }),
        ));
        frames.push(self.frame("message_stop", &json!({"type": "message_stop"})));
        frames
    }
}

fn anthropic_usage_value(usage: Usage) -> Value {
    json!({
        "input_tokens": usage.input_tokens,
        "output_tokens": usage.output_tokens,
        "cache_read_input_tokens": usage.cache_read_tokens,
        "cache_creation_input_tokens": usage.cache_write_tokens,
    })
}
