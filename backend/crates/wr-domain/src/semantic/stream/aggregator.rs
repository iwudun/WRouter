//! 流聚合与生命周期判定。

use serde_json::Value;

use crate::semantic::diagnostic::ConversionReport;
use crate::semantic::ir::SnapshotFormat;
use crate::semantic::model::{ContentPart, SemanticEvent, ToolArguments, Usage};

use super::decode::responses_done_arguments;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamStatus {
    Open,
    Complete,
    Incomplete,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamViolation {
    EventAfterTerminal,
    DuplicateFinish(usize),
    Incomplete,
    Failed,
}

pub struct StreamAggregator {
    response: crate::semantic::model::SemanticResponse,
    lifecycle: StreamLifecycle,
    tool_positions: std::collections::HashMap<(usize, usize), usize>,
    /// 可选内存预算(仅缓存装配启用);`None` 表示不限制。
    budget: Option<usize>,
    retained_bytes: usize,
    /// 预算超限后拒绝继续累积,调用方不得把部分结果写入缓存。
    suppressed: bool,
}

/// 每个事件的结构开销估算(候选/索引/枚举标签等),与内容字节数相加。
const EVENT_OVERHEAD_BYTES: usize = 128;

impl Default for StreamAggregator {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamAggregator {
    pub fn new() -> Self {
        Self::with_budget_option(None)
    }

    /// 有界聚合器:估算累计字节超过 `max_bytes` 即停止累积并标记
    /// [`Self::is_suppressed`],避免缓存装配在长流上无界占用内存。
    pub fn with_budget(max_bytes: usize) -> Self {
        Self::with_budget_option(Some(max_bytes))
    }

    fn with_budget_option(budget: Option<usize>) -> Self {
        Self {
            response: crate::semantic::model::SemanticResponse::new(),
            lifecycle: StreamLifecycle::new(),
            tool_positions: std::collections::HashMap::new(),
            budget,
            retained_bytes: 0,
            suppressed: false,
        }
    }

    pub fn lifecycle(&self) -> &StreamLifecycle {
        &self.lifecycle
    }

    /// 是否因预算超限停止累积。为 `true` 时聚合结果不完整,禁止入缓存。
    pub fn is_suppressed(&self) -> bool {
        self.suppressed
    }

    fn reserve(&mut self, weight: usize) -> bool {
        let Some(budget) = self.budget else {
            return true;
        };
        let Some(total) = self.retained_bytes.checked_add(weight) else {
            self.suppressed = true;
            return false;
        };
        if total > budget {
            self.suppressed = true;
            return false;
        }
        self.retained_bytes = total;
        true
    }

    pub fn push(&mut self, event: SemanticEvent) -> Result<(), StreamViolation> {
        if self.suppressed {
            return Ok(());
        }
        self.lifecycle.observe(&event)?;
        if !self.reserve(semantic_event_weight(&event)) {
            return Ok(());
        }
        match event {
            SemanticEvent::MessageStart { id, model } => {
                self.response.id = id.or(self.response.id.take());
                self.response.model = model.or(self.response.model.take());
            }
            SemanticEvent::PartStart {
                candidate,
                index,
                part,
            } => {
                let target = self.candidate(candidate);
                while target.content.len() <= index {
                    target.content.push(ContentPart::Opaque {
                        kind_hint: None,
                        value: Value::Null,
                    });
                }
                target.content[index] = part;
            }
            SemanticEvent::PartDelta {
                candidate,
                index,
                text,
            } => {
                let target = self.candidate(candidate);
                while target.content.len() <= index {
                    target.content.push(ContentPart::Text {
                        text: String::new(),
                    });
                }
                if let ContentPart::Text { text: current } = &mut target.content[index] {
                    current.push_str(&text);
                } else {
                    target.content[index] = ContentPart::Text { text };
                }
            }
            SemanticEvent::RefusalDelta {
                candidate,
                index,
                text,
            } => {
                let target = self.candidate(candidate);
                while target.content.len() <= index {
                    target.content.push(ContentPart::Opaque {
                        kind_hint: None,
                        value: Value::Null,
                    });
                }
                if let ContentPart::Refusal { text: current } = &mut target.content[index] {
                    current.push_str(&text);
                } else {
                    target.content[index] = ContentPart::Refusal { text };
                }
            }
            SemanticEvent::ReasoningStart { candidate, .. } => {
                self.candidate(candidate).reasoning.get_or_insert(
                    crate::semantic::model::ReasoningBlock {
                        text: String::new(),
                        signature: None,
                        opaque: Vec::new(),
                    },
                );
            }
            SemanticEvent::ReasoningDelta {
                candidate, text, ..
            } => {
                let target = self.candidate(candidate);
                target
                    .reasoning
                    .get_or_insert(crate::semantic::model::ReasoningBlock {
                        text: String::new(),
                        signature: None,
                        opaque: Vec::new(),
                    })
                    .text
                    .push_str(&text);
            }
            SemanticEvent::ReasoningSignature {
                candidate,
                signature,
                ..
            } => {
                let target = self.candidate(candidate);
                let block =
                    target
                        .reasoning
                        .get_or_insert(crate::semantic::model::ReasoningBlock {
                            text: String::new(),
                            signature: None,
                            opaque: Vec::new(),
                        });
                match block.signature.as_mut() {
                    Some(existing) => existing.push_str(&signature),
                    None => block.signature = Some(signature),
                }
            }
            // 结构边界事件:内容已在 PartStart/PartDelta 落位,这里只需
            // 保证候选存在(保证候选顺序与上游一致)。
            SemanticEvent::PartEnd { candidate, .. } => {
                self.candidate(candidate);
            }
            SemanticEvent::ToolCallStart {
                candidate,
                index,
                call,
            } => {
                let target = self.candidate(candidate);
                let position = target
                    .tool_calls
                    .iter()
                    .position(|existing| existing.id.local == call.id.local)
                    .unwrap_or_else(|| {
                        target.tool_calls.push(call);
                        target.tool_calls.len() - 1
                    });
                self.tool_positions.insert((candidate, index), position);
            }
            SemanticEvent::ToolArgumentsDelta {
                candidate,
                index,
                fragment,
            } => {
                if let Some(position) = self.tool_positions.get(&(candidate, index)).copied()
                    && let Some(call) = self.candidate(candidate).tool_calls.get_mut(position)
                {
                    match &mut call.arguments {
                        ToolArguments::Raw(raw) => raw.push_str(&fragment),
                        ToolArguments::Parsed(value) => {
                            // 空对象是 Anthropic `content_block_start` 的占位
                            // (`input: {}`),真正的参数由随后的 input_json_delta
                            // 分片给出;此时必须**替换**而不是把 `{}` 串在前面,
                            // 否则会拼出 `{}{"loc":"SF"}` 这种非法 JSON。
                            if is_empty_placeholder(value) {
                                call.arguments = ToolArguments::Raw(fragment);
                            } else {
                                let mut raw = crate::json_py::py_json_dumps(value);
                                raw.push_str(&fragment);
                                call.arguments = ToolArguments::Raw(raw);
                            }
                        }
                    }
                }
            }
            SemanticEvent::Usage(usage) => {
                self.response.usage = Some(self.lifecycle.usage().unwrap_or(usage))
            }
            SemanticEvent::Finish { candidate, reason } => {
                let target = self.candidate(candidate);
                target.finish = Some(reason.clone());
                self.response.finish = Some(reason);
            }
            SemanticEvent::StreamEnd { incomplete } => {
                if incomplete {
                    self.response.incomplete =
                        Some(crate::semantic::model::IncompleteReason::Other(
                            "upstream_stream_incomplete".into(),
                        ));
                }
            }
            SemanticEvent::Error(error) => {
                self.response
                    .opaque
                    .push(crate::semantic::model::OpaqueField {
                        path: "stream_error".into(),
                        value: serde_json::to_value(error).unwrap_or(Value::Null),
                    })
            }
            SemanticEvent::ToolResult { .. } => {}
            SemanticEvent::Opaque { raw } => {
                // 2026-11-06:Responses 上游的 `output_item.done` 帧带**完整**
                // 参数。若整个流里一个参数增量都没到(兼容实现只发 done),
                // 用它补齐 —— 只在「参数仍为空」时补,绝不重复累加
                // (与 wire 路径的补齐同源,见 `responses_done_arguments`)。
                if let Some((candidate, index, arguments)) = responses_done_arguments(&raw)
                    && !arguments.is_empty()
                    && let Some(position) = self.tool_positions.get(&(candidate, index)).copied()
                    && let Some(call) = self.candidate(candidate).tool_calls.get_mut(position)
                {
                    let empty = match &call.arguments {
                        ToolArguments::Raw(raw) => raw.is_empty(),
                        ToolArguments::Parsed(value) => is_empty_placeholder(value),
                    };
                    if empty {
                        call.arguments = ToolArguments::Raw(arguments);
                    }
                }
                self.response
                    .opaque
                    .push(crate::semantic::model::OpaqueField {
                        path: "opaque_stream_event".into(),
                        value: raw,
                    })
            }
        }
        Ok(())
    }
    fn candidate(&mut self, index: usize) -> &mut crate::semantic::model::ResponseCandidate {
        if let Some(position) = self
            .response
            .candidates
            .iter()
            .position(|candidate| candidate.index == index)
        {
            return &mut self.response.candidates[position];
        }
        self.response
            .candidates
            .push(crate::semantic::model::ResponseCandidate::new(index));
        self.response
            .candidates
            .last_mut()
            .expect("candidate just inserted")
    }
    pub fn finish(mut self) -> Result<crate::semantic::model::SemanticResponse, StreamViolation> {
        self.lifecycle.complete();
        match self.lifecycle.status() {
            StreamStatus::Incomplete => Err(StreamViolation::Incomplete),
            StreamStatus::Failed => Err(StreamViolation::Failed),
            StreamStatus::Open => Err(StreamViolation::Incomplete),
            StreamStatus::Complete => Ok(self.response),
        }
    }

    /// 用完整响应快照对账(F-04/P4-1)。Responses 终态快照是权威 output:
    /// 有内容时替换流式候选,避免快照与 delta 重复;Anthropic
    /// `message_start` 快照只提供元数据/用量,不覆盖已收到内容。
    pub fn merge_snapshot(&mut self, format: SnapshotFormat, value: &Value) {
        if self.suppressed || !self.reserve(EVENT_OVERHEAD_BYTES + json_weight(value)) {
            return;
        }
        let mut report = ConversionReport::semantic();
        let mut snapshot = match format {
            SnapshotFormat::OpenAiResponses => {
                crate::semantic::openai_responses::decode_responses_json(value, &mut report)
            }
            SnapshotFormat::AnthropicMessage => {
                crate::semantic::anthropic::decode_anthropic_message(value, &mut report)
            }
        };
        if snapshot.id.is_some() {
            self.response.id = snapshot.id.take();
        }
        if snapshot.model.is_some() {
            self.response.model = snapshot.model.take();
        }
        if let Some(usage) = snapshot.usage {
            self.response.usage = Some(match self.response.usage {
                Some(current) => merge_usage(current, usage),
                None => usage,
            });
        }
        match format {
            SnapshotFormat::OpenAiResponses => {
                let authoritative = snapshot.candidates.iter().any(|candidate| {
                    !candidate.content.is_empty()
                        || !candidate.tool_calls.is_empty()
                        || candidate.reasoning.is_some()
                });
                if authoritative || self.response.candidates.is_empty() {
                    self.response.candidates = std::mem::take(&mut snapshot.candidates);
                    self.response.finish = snapshot.finish;
                    self.response.incomplete = snapshot.incomplete;
                } else {
                    if self.response.finish.is_none() {
                        self.response.finish = snapshot.finish;
                    }
                    if self.response.incomplete.is_none() {
                        self.response.incomplete = snapshot.incomplete;
                    }
                }
            }
            SnapshotFormat::AnthropicMessage => {
                if let Some(reason) = snapshot.finish
                    && self.response.finish.is_none()
                    && value.get("stop_reason").and_then(Value::as_str).is_some()
                {
                    self.response.finish = Some(reason);
                }
            }
        }
        self.response.opaque.extend(snapshot.opaque);
    }

    /// 不做终态判定的收尾:返回已经聚合到的部分响应,并把「没收到
    /// terminal」的事实记进 `incomplete`(C6:断流不得伪装成成功)。
    pub fn into_partial_response(mut self) -> crate::semantic::model::SemanticResponse {
        self.lifecycle.complete();
        if self.lifecycle.status() != StreamStatus::Complete && self.response.incomplete.is_none() {
            let reason = match self.lifecycle.status() {
                StreamStatus::Failed => {
                    crate::semantic::model::IncompleteReason::Other("upstream_stream_failed".into())
                }
                _ => crate::semantic::model::IncompleteReason::Other(
                    "upstream_stream_incomplete".into(),
                ),
            };
            self.response.incomplete = Some(reason);
        }
        self.response
    }
}

/// 聚合保留字节的保守估算:内容长度 + 结构开销。
fn semantic_event_weight(event: &SemanticEvent) -> usize {
    let payload = match event {
        SemanticEvent::PartStart { part, .. } => match part {
            ContentPart::Text { text } | ContentPart::Refusal { text } => text.len(),
            ContentPart::ImageRef { url } => url.len(),
            ContentPart::ImageInline { data_base64, .. } => data_base64.len(),
            ContentPart::Audio { data_base64, .. } => {
                data_base64.as_deref().map_or(0, str::len)
            }
            ContentPart::File {
                file_id,
                filename,
                data_base64,
            } => {
                file_id.as_deref().map_or(0, str::len)
                    + filename.as_deref().map_or(0, str::len)
                    + data_base64.as_deref().map_or(0, str::len)
            }
            ContentPart::Opaque { value, .. } => json_weight(value),
        },
        SemanticEvent::PartDelta { text, .. }
        | SemanticEvent::RefusalDelta { text, .. }
        | SemanticEvent::ReasoningDelta { text, .. } => text.len(),
        SemanticEvent::ReasoningSignature { signature, .. } => signature.len(),
        SemanticEvent::ToolCallStart { call, .. } => {
            call.name.len()
                + match &call.arguments {
                    ToolArguments::Raw(raw) => raw.len(),
                    ToolArguments::Parsed(value) => json_weight(value),
                }
        }
        SemanticEvent::ToolArgumentsDelta { fragment, .. } => fragment.len(),
        SemanticEvent::ToolResult { result, .. } => json_weight(&result.content),
        SemanticEvent::Opaque { raw } => json_weight(raw),
        _ => 0,
    };
    EVENT_OVERHEAD_BYTES.saturating_add(payload)
}

fn json_weight(value: &Value) -> usize {
    serde_json::to_vec(value).map_or(0, |bytes| bytes.len())
}
#[derive(Debug)]
pub struct StreamLifecycle {    status: StreamStatus,
    finished_candidates: Vec<usize>,
    usage: Option<Usage>,
    event_count: u64,
    terminal: bool,
}

impl Default for StreamLifecycle {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamLifecycle {
    pub fn new() -> Self {
        Self {
            status: StreamStatus::Open,
            finished_candidates: Vec::new(),
            usage: None,
            event_count: 0,
            terminal: false,
        }
    }
    pub fn status(&self) -> StreamStatus {
        self.status
    }
    pub fn usage(&self) -> Option<Usage> {
        self.usage
    }
    pub fn event_count(&self) -> u64 {
        self.event_count
    }

    pub fn observe(&mut self, event: &SemanticEvent) -> Result<(), StreamViolation> {
        if self.status != StreamStatus::Open || self.terminal {
            return Err(StreamViolation::EventAfterTerminal);
        }
        self.event_count += 1;
        match event {
            SemanticEvent::Usage(update) => {
                self.usage = Some(match self.usage {
                    Some(old) => merge_usage(old, *update),
                    None => *update,
                });
            }
            SemanticEvent::Finish { candidate, .. } => {
                if self.finished_candidates.contains(candidate) {
                    return Err(StreamViolation::DuplicateFinish(*candidate));
                }
                self.finished_candidates.push(*candidate);
            }
            SemanticEvent::StreamEnd { incomplete } => {
                self.terminal = true;
                self.status = if *incomplete {
                    StreamStatus::Incomplete
                } else {
                    StreamStatus::Complete
                };
            }
            SemanticEvent::Error(_) => self.status = StreamStatus::Failed,
            _ => {}
        }
        Ok(())
    }

    pub fn complete(&mut self) -> bool {
        if self.status == StreamStatus::Complete {
            return true;
        }
        if self.status != StreamStatus::Open {
            return false;
        }
        self.status = StreamStatus::Incomplete;
        false
    }
    pub fn interrupt(&mut self) {
        if self.status == StreamStatus::Open {
            self.status = StreamStatus::Incomplete;
        }
    }
}
/// 参数占位:空对象 / 空数组 / null(协议在流开始处发的「还没有参数」)。
fn is_empty_placeholder(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Object(map) => map.is_empty(),
        Value::Array(items) => items.is_empty(),
        _ => false,
    }
}

fn merge_usage(old: Usage, update: Usage) -> Usage {
    Usage {
        input: update.input.or(old.input),
        output: update.output.or(old.output),
        total: update.total.or(old.total),
        reasoning: update.reasoning.or(old.reasoning),
        cache_read: update.cache_read.or(old.cache_read),
        cache_write: update.cache_write.or(old.cache_write),
    }
}
