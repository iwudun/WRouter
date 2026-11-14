//! Responses 目标编码器(P4-4)。
//!
//! 消费层级 [`StreamUpdate`],维护 response → item → content part 状态:
//! - 文本/拒绝 part 独立开场与收口,不再用一个 output_index 代表所有 part;
//! - 工具参数按快照对账,只输出延长已提交前缀的后缀;
//! - 最终 output 按 output_index 排序,与终态快照对账。

use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::{Value, json};

use crate::semantic::ir::{
    ItemKind, SnapshotFormat, StreamKey, StreamUpdate, ToolCallAssembly, opening_arguments,
};
use crate::semantic::model::{FinishReason, IdOrigin, ToolArguments, ToolCall, ToolCallId};
use crate::semantic::util::{created_now, uuid_short};
use crate::sse::{SsePayload, encode_sse};
use crate::usage::Usage;

pub(crate) struct ResponsesTargetClient {
    sequence: i64,
    started: bool,
    start_emitted: bool,
    response_id: String,
    model: String,
    created_at: i64,
    usage: Usage,
    next_output_index: usize,
    /// output_index → 已完成 item(按 output_index 排序)。
    output: BTreeMap<usize, Value>,
    /// 打开中的 item。
    items: BTreeMap<usize, ResponsesItem>,
    /// 源 item key → output_index。
    item_for_key: HashMap<StreamKey, usize>,
    finish: Option<FinishReason>,
    snapshot: Option<Value>,
    /// 期间产生的参数冲突/不可表达事实(供测试与诊断读取)。
    conflicts: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ResponsesPartKind {
    Text,
    Refusal,
}

struct ResponsesPart {
    kind: ResponsesPartKind,
    text: String,
    started: bool,
    done: bool,
}

struct ResponsesTool {
    call_id: String,
    name: String,
    assembly: ToolCallAssembly,
    opening_emitted: bool,
}

struct ResponsesItem {
    output_index: usize,
    item_id: String,
    kind: ItemKind,
    added: bool,
    parts: BTreeMap<usize, ResponsesPart>,
    text: String,
    reasoning: String,
    tool: Option<ResponsesTool>,
    snapshot: Option<Value>,
}

impl ResponsesItem {
    fn new(output_index: usize, item_id: String, kind: ItemKind) -> Self {
        Self {
            output_index,
            item_id,
            kind,
            added: false,
            parts: BTreeMap::new(),
            text: String::new(),
            reasoning: String::new(),
            tool: None,
            snapshot: None,
        }
    }
}

impl ResponsesTargetClient {
    pub(crate) fn new() -> Self {
        Self {
            sequence: 0,
            started: false,
            start_emitted: false,
            response_id: "resp_wrouter".to_owned(),
            model: String::new(),
            created_at: created_now(),
            usage: Usage::empty(),
            next_output_index: 0,
            output: BTreeMap::new(),
            items: BTreeMap::new(),
            item_for_key: HashMap::new(),
            finish: None,
            snapshot: None,
            conflicts: Vec::new(),
        }
    }

    fn push(&mut self, frames: &mut Vec<String>, mut payload: Value) {
        if let Some(map) = payload.as_object_mut() {
            map.insert("sequence_number".to_owned(), json!(self.sequence));
        }
        self.sequence += 1;
        frames.push(encode_sse(None, SsePayload::Json(&payload)));
    }

    fn envelope(&mut self, status: &str) -> Value {
        json!({
            "id": self.response_id,
            "object": "response",
            "created_at": self.created_at,
            "status": status,
            "model": self.model,
            "output": [],
            "output_text": "",
            "usage": usage_json(self.usage),
        })
    }

    fn usage_with_details(&self) -> Value {
        let input = self.usage.input_tokens;
        let output = self.usage.output_tokens;
        json!({
            "input_tokens": input,
            "output_tokens": output,
            "total_tokens": self.usage.total_tokens.max(input + output),
            "input_tokens_details": {"cached_tokens": self.usage.cache_read_tokens},
            "output_tokens_details": {"reasoning_tokens": self.usage.reasoning_tokens},
        })
    }

    fn ensure_started(&mut self, frames: &mut Vec<String>) {
        if self.start_emitted {
            return;
        }
        if !self.started {
            self.started = true;
        }
        self.start_emitted = true;
        let envelope = self.envelope("in_progress");
        self.push(
            frames,
            json!({"type": "response.created", "response": envelope}),
        );
        let envelope = self.envelope("in_progress");
        self.push(
            frames,
            json!({"type": "response.in_progress", "response": envelope}),
        );
    }

    fn allocate_item(
        &mut self,
        key: StreamKey,
        kind: ItemKind,
        preferred_id: Option<String>,
    ) -> usize {
        let item_key = StreamKey::item(key.candidate, key.item);
        if let Some(&output_index) = self.item_for_key.get(&item_key)
            && self.items.contains_key(&output_index)
        {
            if matches!(self.items[&output_index].kind, ItemKind::Unknown(_))
                && let Some(item) = self.items.get_mut(&output_index)
            {
                item.kind = kind;
            }
            return output_index;
        }
        let output_index = self.next_output_index;
        self.next_output_index += 1;
        let item_id = preferred_id
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| generated_item_id(&kind));
        self.items.insert(
            output_index,
            ResponsesItem::new(output_index, item_id, kind),
        );
        self.item_for_key.insert(item_key, output_index);
        output_index
    }

    fn item_index(&self, key: StreamKey) -> Option<usize> {
        self.item_for_key
            .get(&StreamKey::item(key.candidate, key.item))
            .copied()
            .filter(|index| self.items.contains_key(index))
    }

    fn ensure_item_added(&mut self, output_index: usize, frames: &mut Vec<String>) {
        let Some(item) = self.items.get(&output_index) else {
            return;
        };
        if item.added {
            return;
        }
        let kind = item.kind.clone();
        let item_id = item.item_id.clone();
        let tool = item
            .tool
            .as_ref()
            .map(|tool| (tool.call_id.clone(), tool.name.clone()));
        let payload = match kind {
            ItemKind::Message => json!({
                "type": "response.output_item.added",
                "output_index": output_index,
                "item": {
                    "id": item_id,
                    "type": "message",
                    "role": "assistant",
                    "status": "in_progress",
                    "content": [],
                },
            }),
            ItemKind::Reasoning => json!({
                "type": "response.output_item.added",
                "output_index": output_index,
                "item": {"id": item_id, "type": "reasoning", "summary": []},
            }),
            ItemKind::ToolCall => {
                let Some((call_id, name)) = tool else {
                    return;
                };
                if name.is_empty() {
                    // 名字可能晚到(Responses done 快照);先不发开场,
                    // 避免把暂缺名字写成空字符串。
                    return;
                }
                json!({
                    "type": "response.output_item.added",
                    "output_index": output_index,
                    "item": {
                        "id": item_id,
                        "type": "function_call",
                        "name": name,
                        "call_id": call_id,
                        "arguments": "",
                        "status": "in_progress",
                    },
                })
            }
            ItemKind::Unknown(_) => return,
        };
        self.push(frames, payload);
        if let Some(item) = self.items.get_mut(&output_index) {
            item.added = true;
        }
    }

    fn ensure_part(
        &mut self,
        output_index: usize,
        part_index: usize,
        kind: ResponsesPartKind,
        frames: &mut Vec<String>,
    ) {
        let needs_start = {
            let Some(item) = self.items.get_mut(&output_index) else {
                return;
            };
            let part = item
                .parts
                .entry(part_index)
                .or_insert_with(|| ResponsesPart {
                    kind,
                    text: String::new(),
                    started: false,
                    done: false,
                });
            if part.started {
                false
            } else {
                part.started = true;
                true
            }
        };
        if !needs_start {
            return;
        }
        let item = self.items.get(&output_index).expect("item exists");
        let item_id = item.item_id.clone();
        let part = item.parts.get(&part_index).expect("part exists");
        let payload = match part.kind {
            ResponsesPartKind::Text => json!({
                "type": "response.content_part.added",
                "item_id": item_id,
                "output_index": output_index,
                "content_index": part_index,
                "part": {"type": "output_text", "text": "", "annotations": []},
            }),
            ResponsesPartKind::Refusal => json!({
                "type": "response.content_part.added",
                "item_id": item_id,
                "output_index": output_index,
                "content_index": part_index,
                "part": {"type": "refusal", "refusal": ""},
            }),
        };
        self.push(frames, payload);
    }

    fn append_text(&mut self, key: StreamKey, text: &str, frames: &mut Vec<String>) {
        if text.is_empty() {
            return;
        }
        let output_index = self.ensure_message_item(key);
        self.ensure_item_added(output_index, frames);
        self.ensure_part(output_index, key.part, ResponsesPartKind::Text, frames);
        if let Some(item) = self.items.get_mut(&output_index) {
            item.text.push_str(text);
            if let Some(part) = item.parts.get_mut(&key.part) {
                part.text.push_str(text);
            }
        }
        let item_id = self.items[&output_index].item_id.clone();
        self.push(
            frames,
            json!({
                "type": "response.output_text.delta",
                "output_index": output_index,
                "content_index": key.part,
                "item_id": item_id,
                "delta": text,
            }),
        );
    }

    fn append_refusal(&mut self, key: StreamKey, text: &str, frames: &mut Vec<String>) {
        if text.is_empty() {
            return;
        }
        let output_index = self.ensure_message_item(key);
        self.ensure_item_added(output_index, frames);
        self.ensure_part(output_index, key.part, ResponsesPartKind::Refusal, frames);
        if let Some(item) = self.items.get_mut(&output_index) {
            item.text.push_str(text);
            if let Some(part) = item.parts.get_mut(&key.part) {
                part.text.push_str(text);
            }
        }
        let item_id = self.items[&output_index].item_id.clone();
        self.push(
            frames,
            json!({
                "type": "response.refusal.delta",
                "output_index": output_index,
                "content_index": key.part,
                "item_id": item_id,
                "delta": text,
            }),
        );
    }

    fn ensure_message_item(&mut self, key: StreamKey) -> usize {
        self.allocate_item(key, ItemKind::Message, None)
    }

    fn start_reasoning(&mut self, key: StreamKey, id: Option<String>, frames: &mut Vec<String>) {
        let output_index = self.allocate_item(key, ItemKind::Reasoning, id);
        self.ensure_item_added(output_index, frames);
        let needs_part = self
            .items
            .get(&output_index)
            .and_then(|item| item.parts.get(&0))
            .is_none_or(|part| !part.started);
        if needs_part {
            let item_id = self.items[&output_index].item_id.clone();
            self.push(
                frames,
                json!({
                    "type": "response.reasoning_summary_part.added",
                    "item_id": item_id,
                    "output_index": output_index,
                    "content_index": 0,
                    "part": {"type": "summary_text", "text": ""},
                }),
            );
            if let Some(item) = self.items.get_mut(&output_index) {
                item.parts.insert(
                    0,
                    ResponsesPart {
                        kind: ResponsesPartKind::Text,
                        text: String::new(),
                        started: true,
                        done: false,
                    },
                );
            }
        }
    }

    fn append_reasoning(&mut self, key: StreamKey, text: &str, frames: &mut Vec<String>) {
        if text.is_empty() {
            return;
        }
        self.start_reasoning(key, None, frames);
        let Some(output_index) = self.item_index(key) else {
            return;
        };
        if let Some(item) = self.items.get_mut(&output_index) {
            item.reasoning.push_str(text);
            if let Some(part) = item.parts.get_mut(&0) {
                part.text.push_str(text);
            }
        }
        let item_id = self.items[&output_index].item_id.clone();
        self.push(
            frames,
            json!({
                "type": "response.reasoning_summary_text.delta",
                "output_index": output_index,
                "content_index": 0,
                "item_id": item_id,
                "delta": text,
            }),
        );
    }

    fn tool_start(
        &mut self,
        key: StreamKey,
        tool_index: usize,
        call: &ToolCall,
        frames: &mut Vec<String>,
    ) {
        let output_index = self.allocate_item(key, ItemKind::ToolCall, None);
        let needs_tool = self.items[&output_index].tool.is_none();
        if needs_tool {
            let item_id = crate::semantic::stream::responses_function_call_id(&call.id.local);
            let call_id = if call.id.local.is_empty() {
                item_id.clone()
            } else {
                call.id.local.clone()
            };
            let mut assembly =
                ToolCallAssembly::new(StreamKey::item(key.candidate, key.item), tool_index, call);
            let opening = opening_arguments(call);
            if let Some(fragment) = &opening {
                assembly.push_delta(fragment);
                assembly.mark_committed();
            }
            if let Some(item) = self.items.get_mut(&output_index) {
                if item.item_id.is_empty() {
                    item.item_id = item_id;
                }
                item.tool = Some(ResponsesTool {
                    call_id,
                    name: call.name.clone(),
                    assembly,
                    opening_emitted: false,
                });
            }
        }
        self.ensure_item_added(output_index, frames);
        let should_emit_opening = self
            .items
            .get(&output_index)
            .and_then(|item| item.tool.as_ref())
            .is_some_and(|tool| !tool.opening_emitted)
            && opening_arguments(call).is_some();
        if should_emit_opening {
            let fragment = opening_arguments(call).unwrap_or_default();
            if let Some(item) = self.items.get_mut(&output_index)
                && let Some(tool) = item.tool.as_mut()
            {
                tool.opening_emitted = true;
            }
            let item_id = self.items[&output_index].item_id.clone();
            self.push(
                frames,
                json!({
                    "type": "response.function_call_arguments.delta",
                    "output_index": output_index,
                    "item_id": item_id,
                    "delta": fragment,
                }),
            );
        }
    }

    fn tool_delta(
        &mut self,
        key: StreamKey,
        _tool_index: usize,
        fragment: &str,
        frames: &mut Vec<String>,
    ) {
        let Some(output_index) = self.item_index(key) else {
            return;
        };
        let Some(item) = self.items.get_mut(&output_index) else {
            return;
        };
        let Some(tool) = item.tool.as_mut() else {
            return;
        };
        tool.assembly.push_delta(fragment);
        tool.assembly.mark_committed();
        let item_id = item.item_id.clone();
        self.push(
            frames,
            json!({
                "type": "response.function_call_arguments.delta",
                "output_index": output_index,
                "item_id": item_id,
                "delta": fragment,
            }),
        );
    }

    fn item_snapshot(&mut self, key: StreamKey, snapshot: &Value, frames: &mut Vec<String>) {
        let kind = snapshot.get("type").and_then(Value::as_str).unwrap_or("");
        match kind {
            "function_call" => {
                let call_id = snapshot
                    .get("call_id")
                    .and_then(Value::as_str)
                    .or_else(|| snapshot.get("id").and_then(Value::as_str))
                    .unwrap_or("")
                    .to_owned();
                let name = snapshot
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let arguments = snapshot
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let call = ToolCall {
                    id: ToolCallId {
                        local: call_id.clone(),
                        origin: IdOrigin::Source,
                    },
                    name: name.clone(),
                    arguments: ToolArguments::Raw(arguments.clone()),
                    extensions: Default::default(),
                };
                let output_index = self.allocate_item(
                    key,
                    ItemKind::ToolCall,
                    snapshot
                        .get("id")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                );
                if self.items[&output_index].tool.is_none() {
                    self.items.get_mut(&output_index).unwrap().tool = Some(ResponsesTool {
                        call_id: call_id.clone(),
                        name: name.clone(),
                        assembly: ToolCallAssembly::new(
                            StreamKey::item(key.candidate, key.item),
                            key.item,
                            &call,
                        ),
                        opening_emitted: true,
                    });
                } else if let Some(tool) = self
                    .items
                    .get_mut(&output_index)
                    .and_then(|item| item.tool.as_mut())
                {
                    if tool.name.is_empty() {
                        tool.name = name.clone();
                    }
                    if tool.call_id.is_empty() {
                        tool.call_id = call_id.clone();
                    }
                }
                let outcome = self
                    .items
                    .get_mut(&output_index)
                    .and_then(|item| item.tool.as_mut())
                    .map(|tool| tool.assembly.reconcile_snapshot(&arguments));
                if let Some(outcome) = outcome {
                    self.ensure_item_added(output_index, frames);
                    let item_id = self.items[&output_index].item_id.clone();
                    match outcome {
                        crate::semantic::ir::ArgumentReconciliation::Extension { suffix } => {
                            if !suffix.is_empty() {
                                if let Some(item) = self.items.get_mut(&output_index)
                                    && let Some(tool) = item.tool.as_mut()
                                {
                                    tool.assembly.mark_committed();
                                }
                                self.push(
                                    frames,
                                    json!({
                                        "type": "response.function_call_arguments.delta",
                                        "output_index": output_index,
                                        "item_id": item_id,
                                        "delta": suffix,
                                    }),
                                );
                            }
                        }
                        crate::semantic::ir::ArgumentReconciliation::Snapshot { arguments } => {
                            if !arguments.is_empty() {
                                if let Some(item) = self.items.get_mut(&output_index)
                                    && let Some(tool) = item.tool.as_mut()
                                {
                                    tool.assembly.mark_committed();
                                }
                                self.push(
                                    frames,
                                    json!({
                                        "type": "response.function_call_arguments.delta",
                                        "output_index": output_index,
                                        "item_id": item_id,
                                        "delta": arguments,
                                    }),
                                );
                            }
                        }
                        crate::semantic::ir::ArgumentReconciliation::Conflict {
                            committed,
                            snapshot,
                        } => {
                            self.conflicts
                                .push(format!("tool arguments conflict: committed={committed:?} snapshot={snapshot:?}"));
                        }
                        crate::semantic::ir::ArgumentReconciliation::Duplicate => {}
                    }
                }
                if let Some(item) = self.items.get_mut(&output_index) {
                    item.snapshot = Some(snapshot.clone());
                }
            }
            "message" => {
                let output_index = self.allocate_item(
                    key,
                    ItemKind::Message,
                    snapshot
                        .get("id")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                );
                self.ensure_item_added(output_index, frames);
                if let Some(Value::Array(parts)) = snapshot.get("content") {
                    for (index, part) in parts.iter().enumerate() {
                        let part_type = part.get("type").and_then(Value::as_str).unwrap_or("");
                        let text = part
                            .get("text")
                            .or_else(|| part.get("refusal"))
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        let kind = if part_type == "refusal" {
                            ResponsesPartKind::Refusal
                        } else {
                            ResponsesPartKind::Text
                        };
                        self.reconcile_part(output_index, index, kind, text, frames);
                    }
                }
                if let Some(item) = self.items.get_mut(&output_index) {
                    item.snapshot = Some(snapshot.clone());
                }
            }
            "reasoning" => {
                let output_index = self.allocate_item(
                    key,
                    ItemKind::Reasoning,
                    snapshot
                        .get("id")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                );
                let text = reasoning_text(snapshot);
                self.start_reasoning(key, None, frames);
                let current = self.items[&output_index].reasoning.clone();
                if text.starts_with(&current) {
                    let suffix = &text[current.len()..];
                    if !suffix.is_empty() {
                        self.append_reasoning(key, suffix, frames);
                    }
                }
                if let Some(item) = self.items.get_mut(&output_index) {
                    item.snapshot = Some(snapshot.clone());
                }
            }
            _ => {}
        }
    }

    fn reconcile_part(
        &mut self,
        output_index: usize,
        part_index: usize,
        kind: ResponsesPartKind,
        text: &str,
        frames: &mut Vec<String>,
    ) {
        self.ensure_part(output_index, part_index, kind, frames);
        let current = self
            .items
            .get(&output_index)
            .and_then(|item| item.parts.get(&part_index))
            .map(|part| part.text.clone())
            .unwrap_or_default();
        let suffix = if text.is_empty() {
            None
        } else if current.is_empty() {
            Some(text.to_owned())
        } else if text.starts_with(&current) {
            Some(text[current.len()..].to_owned())
        } else {
            self.conflicts.push(format!(
                "message part conflict: committed={current:?} snapshot={text:?}"
            ));
            None
        };
        let Some(suffix) = suffix.filter(|suffix| !suffix.is_empty()) else {
            return;
        };
        let item_id = self.items[&output_index].item_id.clone();
        if let Some(item) = self.items.get_mut(&output_index) {
            if let Some(part) = item.parts.get_mut(&part_index) {
                part.text.push_str(&suffix);
            }
            item.text.push_str(&suffix);
        }
        let payload = match kind {
            ResponsesPartKind::Text => json!({
                "type": "response.output_text.delta",
                "output_index": output_index,
                "content_index": part_index,
                "item_id": item_id,
                "delta": suffix,
            }),
            ResponsesPartKind::Refusal => json!({
                "type": "response.refusal.delta",
                "output_index": output_index,
                "content_index": part_index,
                "item_id": item_id,
                "delta": suffix,
            }),
        };
        self.push(frames, payload);
    }

    fn part_end(&mut self, key: StreamKey, frames: &mut Vec<String>) {
        let Some(output_index) = self.item_index(key) else {
            return;
        };
        let Some(item) = self.items.get_mut(&output_index) else {
            return;
        };
        match item.kind {
            ItemKind::Message => {
                let Some(part) = item.parts.get_mut(&key.part) else {
                    return;
                };
                if !part.started || part.done {
                    return;
                }
                part.done = true;
                let item_id = item.item_id.clone();
                let output_index = item.output_index;
                let kind = part.kind;
                let text = part.text.clone();
                let frames_payload = match kind {
                    ResponsesPartKind::Text => vec![
                        json!({"type": "response.output_text.done", "output_index": output_index,
                            "content_index": key.part, "item_id": item_id, "text": text}),
                        json!({"type": "response.content_part.done", "item_id": item_id,
                            "output_index": output_index, "content_index": key.part,
                            "part": {"type": "output_text", "text": text, "annotations": []}}),
                    ],
                    ResponsesPartKind::Refusal => vec![
                        json!({"type": "response.refusal.done", "output_index": output_index,
                            "content_index": key.part, "item_id": item_id, "refusal": text}),
                        json!({"type": "response.content_part.done", "item_id": item_id,
                            "output_index": output_index, "content_index": key.part,
                            "part": {"type": "refusal", "refusal": text}}),
                    ],
                };
                for payload in frames_payload {
                    self.push(frames, payload);
                }
            }
            ItemKind::Reasoning => {
                let Some(part) = item.parts.get_mut(&0) else {
                    return;
                };
                if !part.started || part.done {
                    return;
                }
                part.done = true;
                let item_id = item.item_id.clone();
                let output_index = item.output_index;
                let text = item.reasoning.clone();
                self.push(
                    frames,
                    json!({"type": "response.reasoning_summary_text.done",
                        "output_index": output_index, "content_index": 0,
                        "item_id": item_id, "text": text}),
                );
                self.push(
                    frames,
                    json!({"type": "response.reasoning_summary_part.done",
                        "item_id": item_id, "output_index": output_index,
                        "content_index": 0, "part": {"type": "summary_text", "text": text}}),
                );
            }
            ItemKind::ToolCall | ItemKind::Unknown(_) => {}
        }
    }

    fn item_end(&mut self, key: StreamKey, frames: &mut Vec<String>) {
        let Some(output_index) = self.item_index(key) else {
            return;
        };
        self.ensure_started(frames);
        self.ensure_item_added(output_index, frames);
        self.close_parts_for_item(output_index, frames);
        let Some(item) = self.items.remove(&output_index) else {
            return;
        };
        self.forget_item(output_index);
        match item.kind {
            ItemKind::ToolCall => {
                let Some(tool) = item.tool else {
                    return;
                };
                if tool.name.is_empty() {
                    self.conflicts
                        .push("tool call completed without a name".to_owned());
                    self.push(
                        frames,
                        json!({
                            "type": "error",
                            "error": {
                                "type": "invalid_tool_call",
                                "message": "tool call completed without a name",
                            },
                        }),
                    );
                    return;
                }
                let arguments = tool.assembly.final_arguments();
                if !arguments.trim().is_empty() && tool.assembly.final_object().is_none() {
                    self.conflicts.push(format!(
                        "invalid tool arguments for {}: {arguments:?}",
                        tool.name
                    ));
                    self.push(
                        frames,
                        json!({
                            "type": "error",
                            "error": {
                                "type": "invalid_tool_arguments",
                                "message": format!("tool {:?} arguments are not complete JSON", tool.name),
                            },
                        }),
                    );
                    return;
                }
                let arguments = if arguments.trim().is_empty() {
                    "{}".to_owned()
                } else {
                    arguments
                };
                self.push(
                    frames,
                    json!({"type": "response.function_call_arguments.done",
                        "output_index": item.output_index, "item_id": item.item_id,
                        "name": tool.name, "arguments": arguments}),
                );
                let done_item = json!({
                    "id": item.item_id, "type": "function_call", "name": tool.name,
                    "call_id": tool.call_id, "arguments": arguments, "status": "completed",
                });
                self.push(
                    frames,
                    json!({"type": "response.output_item.done",
                        "output_index": item.output_index, "item": done_item}),
                );
                self.output.insert(item.output_index, done_item);
            }
            ItemKind::Message => {
                let mut content = Vec::new();
                let mut part_indices: Vec<usize> = item.parts.keys().copied().collect();
                part_indices.sort_unstable();
                for part_index in part_indices {
                    let Some(part) = item.parts.get(&part_index) else {
                        continue;
                    };
                    match part.kind {
                        ResponsesPartKind::Text => content.push(json!({
                            "type": "output_text", "text": part.text, "annotations": [],
                        })),
                        ResponsesPartKind::Refusal => content.push(json!({
                            "type": "refusal", "refusal": part.text,
                        })),
                    }
                }
                let done_item = json!({
                    "id": item.item_id, "type": "message", "role": "assistant",
                    "status": "completed", "content": content,
                });
                self.push(
                    frames,
                    json!({"type": "response.output_item.done",
                        "output_index": item.output_index, "item": done_item}),
                );
                self.output.insert(item.output_index, done_item);
            }
            ItemKind::Reasoning => {
                let text = item.reasoning.clone();
                let summary = if text.is_empty() {
                    Vec::new()
                } else {
                    vec![json!({"type": "summary_text", "text": text})]
                };
                let done_item = json!({
                    "id": item.item_id, "type": "reasoning", "status": "completed",
                    "summary": summary,
                });
                self.push(
                    frames,
                    json!({"type": "response.output_item.done",
                        "output_index": item.output_index, "item": done_item}),
                );
                self.output.insert(item.output_index, done_item);
            }
            ItemKind::Unknown(_) => {
                if let Some(snapshot) = item.snapshot {
                    let mut snapshot = snapshot;
                    if let Some(map) = snapshot.as_object_mut() {
                        map.insert("status".to_owned(), json!("completed"));
                    }
                    self.push(
                        frames,
                        json!({"type": "response.output_item.done",
                            "output_index": item.output_index, "item": snapshot}),
                    );
                    self.output.insert(item.output_index, snapshot);
                }
            }
        }
    }

    fn forget_item(&mut self, output_index: usize) {
        self.item_for_key
            .retain(|_, mapped| *mapped != output_index);
    }

    /// item 收口前关闭所有仍打开的 part(快照-only / 上游缺 part.done)。
    fn close_parts_for_item(&mut self, output_index: usize, frames: &mut Vec<String>) {
        let Some(item) = self.items.get(&output_index) else {
            return;
        };
        let kind = item.kind.clone();
        let item_id = item.item_id.clone();
        let output_index = item.output_index;
        let mut open: Vec<(usize, ResponsesPartKind, String)> = Vec::new();
        for (index, part) in &item.parts {
            if part.started && !part.done {
                open.push((*index, part.kind, part.text.clone()));
            }
        }
        for (part_index, part_kind, text) in open {
            match kind {
                ItemKind::Message => {
                    let payloads = match part_kind {
                        ResponsesPartKind::Text => vec![
                            json!({"type": "response.output_text.done", "output_index": output_index,
                                "content_index": part_index, "item_id": item_id, "text": text}),
                            json!({"type": "response.content_part.done", "item_id": item_id,
                                "output_index": output_index, "content_index": part_index,
                                "part": {"type": "output_text", "text": text, "annotations": []}}),
                        ],
                        ResponsesPartKind::Refusal => vec![
                            json!({"type": "response.refusal.done", "output_index": output_index,
                                "content_index": part_index, "item_id": item_id, "refusal": text}),
                            json!({"type": "response.content_part.done", "item_id": item_id,
                                "output_index": output_index, "content_index": part_index,
                                "part": {"type": "refusal", "refusal": text}}),
                        ],
                    };
                    for payload in payloads {
                        self.push(frames, payload);
                    }
                }
                ItemKind::Reasoning => {
                    self.push(
                        frames,
                        json!({"type": "response.reasoning_summary_text.done",
                            "output_index": output_index, "content_index": 0,
                            "item_id": item_id, "text": text}),
                    );
                    self.push(
                        frames,
                        json!({"type": "response.reasoning_summary_part.done",
                            "item_id": item_id, "output_index": output_index,
                            "content_index": 0, "part": {"type": "summary_text", "text": text}}),
                    );
                }
                _ => {}
            }
            if let Some(item) = self.items.get_mut(&output_index)
                && let Some(part) = item.parts.get_mut(&part_index)
            {
                part.done = true;
            }
        }
    }

    fn close_open_items(&mut self, frames: &mut Vec<String>) {
        let mut indices: Vec<usize> = self.items.keys().copied().collect();
        indices.sort_unstable_by_key(|index| self.items[index].output_index);
        for index in indices {
            let Some(item) = self.items.get(&index) else {
                continue;
            };
            let key = StreamKey::item(0, item.output_index);
            self.item_end(key, frames);
        }
    }

    fn emit_snapshot_item(&mut self, item: &Value, frames: &mut Vec<String>) {
        self.ensure_started(frames);
        let output_index = self.next_output_index;
        self.next_output_index += 1;
        let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
        let item_id = item
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| generated_item_id(&ItemKind::Message));
        match item_type {
            "message" => {
                self.push(
                    frames,
                    json!({"type": "response.output_item.added", "output_index": output_index,
                        "item": {"id": item_id, "type": "message", "role": "assistant",
                            "status": "in_progress", "content": []}}),
                );
                if let Some(Value::Array(parts)) = item.get("content") {
                    for (index, part) in parts.iter().enumerate() {
                        let part_type = part.get("type").and_then(Value::as_str).unwrap_or("");
                        let text = part
                            .get("text")
                            .or_else(|| part.get("refusal"))
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        let (part_payload, delta, done) = if part_type == "refusal" {
                            (
                                json!({"type": "refusal", "refusal": ""}),
                                json!({"type": "response.refusal.delta",
                                    "output_index": output_index, "content_index": index,
                                    "item_id": item_id, "delta": text}),
                                json!({"type": "response.refusal.done",
                                    "output_index": output_index, "content_index": index,
                                    "item_id": item_id, "refusal": text}),
                            )
                        } else {
                            (
                                json!({"type": "output_text", "text": "", "annotations": []}),
                                json!({"type": "response.output_text.delta",
                                    "output_index": output_index, "content_index": index,
                                    "item_id": item_id, "delta": text}),
                                json!({"type": "response.output_text.done",
                                    "output_index": output_index, "content_index": index,
                                    "item_id": item_id, "text": text}),
                            )
                        };
                        self.push(
                            frames,
                            json!({"type": "response.content_part.added", "item_id": item_id,
                                "output_index": output_index, "content_index": index,
                                "part": part_payload}),
                        );
                        if !text.is_empty() {
                            self.push(frames, delta);
                        }
                        self.push(frames, done);
                        self.push(
                            frames,
                            json!({"type": "response.content_part.done", "item_id": item_id,
                                "output_index": output_index, "content_index": index,
                                "part": part}),
                        );
                    }
                }
                let done_item = item.clone();
                self.push(
                    frames,
                    json!({"type": "response.output_item.done",
                        "output_index": output_index, "item": done_item}),
                );
                self.output.insert(output_index, done_item);
            }
            "function_call" => {
                let arguments = item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("{}");
                self.push(
                    frames,
                    json!({"type": "response.output_item.added", "output_index": output_index,
                        "item": {"id": item_id, "type": "function_call",
                            "name": item.get("name").cloned().unwrap_or(json!("")),
                            "call_id": item.get("call_id").cloned().unwrap_or(json!(item_id)),
                            "arguments": "", "status": "in_progress"}}),
                );
                if !arguments.is_empty() {
                    self.push(
                        frames,
                        json!({"type": "response.function_call_arguments.delta",
                            "output_index": output_index, "item_id": item_id,
                            "delta": arguments}),
                    );
                }
                self.push(
                    frames,
                    json!({"type": "response.function_call_arguments.done",
                        "output_index": output_index, "item_id": item_id,
                        "name": item.get("name").cloned().unwrap_or(json!("")),
                        "arguments": arguments}),
                );
                let done_item = item.clone();
                self.push(
                    frames,
                    json!({"type": "response.output_item.done",
                        "output_index": output_index, "item": done_item}),
                );
                self.output.insert(output_index, done_item);
            }
            "reasoning" => {
                let text = reasoning_text(item);
                self.push(
                    frames,
                    json!({"type": "response.output_item.added", "output_index": output_index,
                        "item": {"id": item_id, "type": "reasoning", "summary": []}}),
                );
                self.push(
                    frames,
                    json!({"type": "response.reasoning_summary_part.added", "item_id": item_id,
                        "output_index": output_index, "content_index": 0,
                        "part": {"type": "summary_text", "text": ""}}),
                );
                if !text.is_empty() {
                    self.push(
                        frames,
                        json!({"type": "response.reasoning_summary_text.delta",
                            "output_index": output_index, "content_index": 0,
                            "item_id": item_id, "delta": text}),
                    );
                }
                self.push(
                    frames,
                    json!({"type": "response.reasoning_summary_text.done",
                        "output_index": output_index, "content_index": 0,
                        "item_id": item_id, "text": text}),
                );
                self.push(
                    frames,
                    json!({"type": "response.reasoning_summary_part.done", "item_id": item_id,
                        "output_index": output_index, "content_index": 0,
                        "part": {"type": "summary_text", "text": text}}),
                );
                let done_item = item.clone();
                self.push(
                    frames,
                    json!({"type": "response.output_item.done",
                        "output_index": output_index, "item": done_item}),
                );
                self.output.insert(output_index, done_item);
            }
            _ => {
                let done_item = item.clone();
                self.output.insert(output_index, done_item);
            }
        }
    }

    pub(crate) fn consume(&mut self, updates: &[StreamUpdate]) -> Vec<String> {
        let mut frames = Vec::new();
        for update in updates {
            match update {
                StreamUpdate::ResponseStart { id, model, .. } => {
                    if let Some(id) = id.as_ref().filter(|id| !id.is_empty()) {
                        self.response_id = id.clone();
                    }
                    if let Some(model) = model {
                        self.model = model.clone();
                    }
                    self.started = true;
                }
                StreamUpdate::ItemStart { key, id, kind, .. } => {
                    self.allocate_item(*key, kind.clone(), id.clone());
                    self.started = true;
                }
                StreamUpdate::PartStart { key, part, .. } => {
                    self.started = true;
                    match part {
                        crate::semantic::model::ContentPart::Text { text } => {
                            let output_index = self.ensure_message_item(*key);
                            self.ensure_item_added(output_index, &mut frames);
                            self.ensure_part(
                                output_index,
                                key.part,
                                ResponsesPartKind::Text,
                                &mut frames,
                            );
                            if !text.is_empty() {
                                self.append_text(*key, text, &mut frames);
                            }
                        }
                        crate::semantic::model::ContentPart::Refusal { text } => {
                            let output_index = self.ensure_message_item(*key);
                            self.ensure_item_added(output_index, &mut frames);
                            self.ensure_part(
                                output_index,
                                key.part,
                                ResponsesPartKind::Refusal,
                                &mut frames,
                            );
                            if !text.is_empty() {
                                self.append_refusal(*key, text, &mut frames);
                            }
                        }
                        _ => {}
                    }
                }
                StreamUpdate::PartDelta { key, text } => {
                    self.append_text(*key, text, &mut frames);
                }
                StreamUpdate::RefusalDelta { key, text } => {
                    self.append_refusal(*key, text, &mut frames);
                }
                StreamUpdate::ReasoningStart { key, id } => {
                    self.start_reasoning(*key, id.clone(), &mut frames);
                }
                StreamUpdate::ReasoningDelta { key, text } => {
                    self.append_reasoning(*key, text, &mut frames);
                }
                StreamUpdate::ReasoningSignature { .. } => {
                    // 签名/加密推理没有跨协议迁移契约:不伪造进 Responses
                    // item(P4-5)。同协议原生路径不经本编码器。
                }
                StreamUpdate::ToolStart {
                    key,
                    tool_index,
                    call,
                } => {
                    self.tool_start(*key, *tool_index, call, &mut frames);
                }
                StreamUpdate::ToolArgumentsDelta {
                    key,
                    tool_index,
                    fragment,
                } => {
                    self.tool_delta(*key, *tool_index, fragment, &mut frames);
                }
                StreamUpdate::PartEnd { key } => {
                    self.part_end(*key, &mut frames);
                }
                StreamUpdate::ItemSnapshot { key, snapshot } => {
                    self.item_snapshot(*key, snapshot, &mut frames);
                }
                StreamUpdate::ItemEnd { key } => {
                    self.item_end(*key, &mut frames);
                }
                StreamUpdate::ResponseSnapshot { format, value } => {
                    if *format == SnapshotFormat::OpenAiResponses {
                        self.snapshot = Some(value.clone());
                    }
                }
                StreamUpdate::Usage(update) => {
                    self.usage.merge_max(update.to_domain());
                }
                StreamUpdate::Finish { reason, .. } => {
                    self.finish = Some(reason.clone());
                }
                StreamUpdate::Error(_) | StreamUpdate::End { .. } => {}
                StreamUpdate::ToolResult { .. } => {}
                StreamUpdate::Opaque { raw } => {
                    // 只转发 Responses 形状的未知事件;异协议 opaque 不冒充。
                    if raw
                        .get("type")
                        .and_then(Value::as_str)
                        .is_some_and(|kind| kind.starts_with("response."))
                    {
                        self.push(&mut frames, raw.clone());
                    }
                }
            }
        }
        if self.started {
            self.ensure_started(&mut frames);
        }
        frames
    }

    pub(crate) fn finalize(&mut self) -> Vec<String> {
        self.finalize_with_incomplete(false)
    }

    pub(crate) fn finalize_incomplete(&mut self) -> Vec<String> {
        self.finalize_with_incomplete(true)
    }

    fn finalize_with_incomplete(&mut self, force_incomplete: bool) -> Vec<String> {
        let mut frames = Vec::new();
        self.close_open_items(&mut frames);
        self.ensure_started(&mut frames);
        let snapshot_items = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.get("output"))
            .and_then(Value::as_array)
            .cloned();
        if let Some(items) = snapshot_items {
            let known: HashSet<String> = self
                .output
                .values()
                .filter_map(|item| item.get("id").and_then(Value::as_str).map(str::to_owned))
                .collect();
            for item in &items {
                let id = item.get("id").and_then(Value::as_str).unwrap_or_default();
                if !known.contains(id) {
                    self.emit_snapshot_item(item, &mut frames);
                }
            }
        }
        let incomplete =
            force_incomplete || matches!(self.finish, Some(FinishReason::IncompleteOther));
        let snapshot_output = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.get("output"))
            .and_then(Value::as_array)
            .filter(|output| !output.is_empty())
            .cloned();
        let snapshot_text = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.get("output_text"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let mut completed = self
            .snapshot
            .clone()
            .unwrap_or_else(|| self.envelope("completed"));
        if let Some(map) = completed.as_object_mut() {
            map.insert("id".to_owned(), json!(self.response_id));
            map.insert("model".to_owned(), json!(self.model));
            map.insert(
                "status".to_owned(),
                json!(if incomplete {
                    "incomplete"
                } else {
                    "completed"
                }),
            );
            map.insert(
                "output".to_owned(),
                Value::Array(
                    snapshot_output.unwrap_or_else(|| self.output.values().cloned().collect()),
                ),
            );
            map.insert(
                "output_text".to_owned(),
                json!(snapshot_text.unwrap_or_else(|| self.output_text())),
            );
            map.insert("usage".to_owned(), self.usage_with_details());
            if incomplete {
                map.insert(
                    "incomplete_details".to_owned(),
                    json!({"reason": "upstream_stream_incomplete"}),
                );
            }
        }
        self.push(
            &mut frames,
            json!({
                "type": if incomplete { "response.incomplete" } else { "response.completed" },
                "response": completed,
            }),
        );
        frames.push(encode_sse(None, SsePayload::Raw("[DONE]")));
        frames
    }

    fn output_text(&self) -> String {
        self.output
            .values()
            .filter_map(|item| item.get("content").and_then(Value::as_array))
            .flat_map(|parts| parts.iter())
            .filter_map(|part| {
                if part.get("type").and_then(Value::as_str) == Some("output_text") {
                    part.get("text").and_then(Value::as_str)
                } else {
                    None
                }
            })
            .collect()
    }
}

fn generated_item_id(kind: &ItemKind) -> String {
    match kind {
        ItemKind::Message => format!("msg_{}", uuid_short()),
        ItemKind::Reasoning => format!("rs_{}", uuid_short()),
        ItemKind::ToolCall => format!("fc_{}", uuid_short()),
        ItemKind::Unknown(_) => format!("item_{}", uuid_short()),
    }
}

fn reasoning_text(item: &Value) -> String {
    for key in ["summary", "content"] {
        if let Some(Value::Array(items)) = item.get(key) {
            let joined = items
                .iter()
                .filter_map(|item| item.get("text").and_then(Value::as_str))
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            if !joined.is_empty() {
                return joined;
            }
        }
    }
    String::new()
}

fn usage_json(usage: Usage) -> Value {
    json!({
        "input_tokens": usage.input_tokens,
        "output_tokens": usage.output_tokens,
        "total_tokens": usage.total_tokens,
    })
}

impl Default for ResponsesTargetClient {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
