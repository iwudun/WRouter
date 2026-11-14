//! 共享源事实与源侧观察(方案 20261106_01 P2-1/P2-2)。
//!
//! 一个上游帧只做**一次**协议解释: [`SourceFacts`] 同时携带语义事件、
//! usage、finish、终态和完整快照。下游编码器消费 `events`,`SourceObserver`
//! 消费同一份事实做生命周期、用量、接收/接受统计和续写投影 —— 双方不再
//! 各自解释原始 payload。
//!
//! 三类事实的边界:
//! - **Received**:上游实际发出的每个记录(含被生命周期门拒绝的事件);
//! - **Accepted**:通过生命周期门、进入有效状态的事实;
//! - **Committed**:由投递层确认已入队的投影(见 `SourceObserver::mark_committed`)。
//!
//! 观察者不读取系统时钟:到达时间由调用方从 [`FrameIdentity`] 注入。

use std::sync::Arc;

use serde_json::Value;

use crate::provider::ProviderProtocol;
use crate::semantic::ir::{SnapshotFormat, StreamUpdate};
use crate::semantic::model::{FinishReason, SemanticEvent, Usage as SemanticUsage};
use crate::semantic::stream::{DecodedFrame, StreamStatus, StreamViolation, decode_upstream_frame};
use crate::sse::SseEvent;
use crate::usage::Usage;
use crate::wire::record::{RawRecord, RecordOrigin, RecordView, Representation};
use crate::wire::sse::SseView;

/// 帧身份:协议、profile、流内位置、到达时间、表示与原始来源。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameIdentity {
    pub protocol: ProviderProtocol,
    pub profile: Option<Arc<str>>,
    pub offset: u64,
    pub received_at_ms: i64,
    pub representation: Representation,
    pub origin: RecordOrigin,
    /// 原始记录字节数(内容 + 分隔符)。
    pub raw_len: usize,
    /// 是否由完整记录分隔符终止(SSE EOF 残片为 false;NDJSON 末行可
    /// 合法地不带换行)。
    pub terminated: bool,
}

impl FrameIdentity {
    pub fn from_record(protocol: ProviderProtocol, record: &RawRecord) -> Self {
        Self {
            protocol,
            profile: record.source().profile.clone(),
            offset: record.offset(),
            received_at_ms: record.received_at_ms(),
            representation: record.representation(),
            origin: record.source().origin,
            raw_len: record.byte_len(),
            terminated: record.is_terminated(),
        }
    }

    /// 兼容旧 `SseEvent` 消费路径(无原始记录身份;仅测试/过渡使用)。
    pub fn synthetic(protocol: ProviderProtocol, representation: Representation) -> Self {
        Self {
            protocol,
            profile: None,
            offset: 0,
            received_at_ms: 0,
            representation,
            origin: RecordOrigin::Upstream,
            raw_len: 0,
            terminated: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FinishFact {
    pub candidate: usize,
    pub reason: FinishReason,
}

/// 一帧携带的终态事实。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameTerminal {
    Complete,
    Incomplete,
    Failed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SnapshotFact {
    pub kind: String,
    pub format: SnapshotFormat,
    pub value: Value,
}

/// 一帧的完整源事实。内容/usage/finish/快照/终态可以同帧共存,
/// 不使用互斥的单值事件分类。
#[derive(Debug, Clone)]
pub struct SourceFacts {
    pub identity: FrameIdentity,
    /// 旧扁平词汇(生命周期/聚合)。
    pub events: Vec<SemanticEvent>,
    /// P4 层级 IR(身份、item/part 生命周期、快照)。
    pub updates: Vec<StreamUpdate>,
    pub usage: Vec<SemanticUsage>,
    pub finishes: Vec<FinishFact>,
    pub terminal: Option<FrameTerminal>,
    pub snapshot: Option<SnapshotFact>,
    pub opaque: Vec<Value>,
    /// 该帧是否被协议前端识别(未知事件也保留原文,但不算识别)。
    pub recognized: bool,
    /// 协议级序列号(Responses `sequence_number`),供 R1 同域重放识别。
    pub sequence: Option<u64>,
}

impl SourceFacts {
    pub fn from_decoded(identity: FrameIdentity, decoded: DecodedFrame) -> Self {
        let sequence = decoded.sequence;
        let mut usage = Vec::new();
        let mut finishes = Vec::new();
        let mut terminal = None;
        let mut opaque = Vec::new();
        let mut recognized = decoded.snapshot.is_some();
        for event in &decoded.events {
            match event {
                SemanticEvent::Usage(update) => {
                    usage.push(*update);
                    recognized = true;
                }
                SemanticEvent::Finish { candidate, reason } => {
                    finishes.push(FinishFact {
                        candidate: *candidate,
                        reason: reason.clone(),
                    });
                    recognized = true;
                }
                SemanticEvent::StreamEnd { incomplete } => {
                    terminal = Some(if *incomplete {
                        FrameTerminal::Incomplete
                    } else {
                        FrameTerminal::Complete
                    });
                    recognized = true;
                }
                SemanticEvent::Error(_) => {
                    terminal = Some(FrameTerminal::Failed);
                    recognized = true;
                }
                SemanticEvent::Opaque { raw } => opaque.push(raw.clone()),
                _ => recognized = true,
            }
        }
        Self {
            identity,
            events: decoded.events,
            updates: decoded.updates,
            usage,
            finishes,
            terminal,
            snapshot: decoded.snapshot.map(|snapshot| SnapshotFact {
                kind: snapshot.kind,
                format: snapshot.format,
                value: snapshot.value,
            }),
            opaque,
            recognized,
            sequence,
        }
    }

    /// 从旧 `SseEvent` 构造(**测试便利入口**;生产接线使用
    /// [`Self::from_record`] / [`Self::from_decoded`])。
    pub fn from_sse(protocol: ProviderProtocol, event: &SseEvent) -> Self {
        let identity = FrameIdentity {
            raw_len: event.raw.len(),
            ..FrameIdentity::synthetic(protocol, Representation::Sse)
        };
        Self::from_decoded(identity, decode_upstream_frame(protocol, event))
    }

    /// 从无损记录构造;SSE 与 NDJSON 各自按其视图解释,不互相冒充。
    pub fn from_record(
        protocol: ProviderProtocol,
        record: &RawRecord,
    ) -> Result<Self, SourceFactError> {
        let identity = FrameIdentity::from_record(protocol, record);
        match record.view() {
            RecordView::Sse(view) => Ok(Self::from_decoded(
                identity,
                decode_upstream_frame(protocol, &sse_event_of(view)),
            )),
            RecordView::Ndjson(view) => {
                let content = record.content();
                if view.blank {
                    return Ok(Self {
                        identity,
                        events: Vec::new(),
                        updates: Vec::new(),
                        usage: Vec::new(),
                        finishes: Vec::new(),
                        terminal: None,
                        snapshot: None,
                        opaque: Vec::new(),
                        recognized: false,
                        sequence: None,
                    });
                }
                let data = String::from_utf8_lossy(content).into_owned();
                let event = SseEvent {
                    event: None,
                    data,
                    raw: String::new(),
                };
                Ok(Self::from_decoded(
                    identity,
                    decode_upstream_frame(protocol, &event),
                ))
            }
            RecordView::Json(_) | RecordView::Opaque => Err(
                SourceFactError::UnsupportedRepresentation(record.representation()),
            ),
        }
    }
}

fn sse_event_of(view: &SseView) -> SseEvent {
    SseEvent {
        event: view.event.clone(),
        data: view.data.clone(),
        raw: String::new(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceFactError {
    UnsupportedRepresentation(Representation),
}

impl std::fmt::Display for SourceFactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedRepresentation(representation) => write!(
                f,
                "{} is not a stream source representation",
                representation.as_str()
            ),
        }
    }
}

impl std::error::Error for SourceFactError {}

/// 用量质量:未知 → 部分(仅开场/中途上报) → 终值(结束帧携带)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UsageQuality {
    #[default]
    Unknown,
    Partial,
    Final,
}

impl UsageQuality {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Partial => "partial",
            Self::Final => "final",
        }
    }

    fn merge(self, other: Self) -> Self {
        match (self, other) {
            (Self::Final, _) | (_, Self::Final) => Self::Final,
            (Self::Partial, _) | (_, Self::Partial) => Self::Partial,
            _ => Self::Unknown,
        }
    }
}

/// 用量作用域:单次 attempt 还是整条响应(续写/多次尝试合并)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageScope {
    Attempt,
    Response,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsageObservation {
    pub usage: Usage,
    pub quality: UsageQuality,
    pub scope: UsageScope,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReceiveStats {
    pub frames: u64,
    pub bytes: u64,
    pub first_at_ms: Option<i64>,
    pub last_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AcceptedStats {
    /// 至少有一个事件通过生命周期门的帧数。
    pub frames: u64,
    pub events: u64,
    /// 被生命周期门拒绝的事件数(仍计入 Received)。
    pub rejected_events: u64,
    pub content_events: u64,
    pub text_bytes: u64,
    pub tool_argument_bytes: u64,
    pub usage_events: u64,
    pub items: u64,
    pub parts: u64,
    pub tool_calls: u64,
    pub snapshots: u64,
}

/// 续写前缀上限(1 MiB)。超过后 `accepted_text` 不再增长并禁止续写,
/// 避免长回复在观察者里复制一份无界正文。
pub const CONTINUATION_PREFIX_MAX_BYTES: usize = 1024 * 1024;

/// 源侧观察者:生命周期、用量、接收/接受统计、续写投影的唯一所有者。
#[derive(Debug)]
pub struct SourceObserver {
    protocol: ProviderProtocol,
    lifecycle: crate::semantic::stream::StreamLifecycle,
    usage: Usage,
    attempt_usage: Usage,
    quality: UsageQuality,
    continuation: bool,
    received: ReceiveStats,
    accepted: AcceptedStats,
    accepted_text: String,
    committed_text_len: usize,
    /// 续写前缀的联合内存上界:超过即放弃续写,不无限复制正文。
    continuation_text_exceeded: bool,
    saw_finish: bool,
    violations: u64,
    /// R1 同域序列域:Responses `sequence_number` 水位线与重放指纹。
    last_sequence: Option<u64>,
    last_sequence_raw_len: usize,
    last_sequence_hash: u64,
    replayed_frames: u64,
    sequence_conflicts: u64,
    items_started: std::collections::HashSet<crate::semantic::ir::StreamKey>,
    parts_started: std::collections::HashSet<crate::semantic::ir::StreamKey>,
}

impl SourceObserver {
    pub fn new(protocol: ProviderProtocol) -> Self {
        Self {
            protocol,
            lifecycle: crate::semantic::stream::StreamLifecycle::new(),
            usage: Usage::empty(),
            attempt_usage: Usage::empty(),
            quality: UsageQuality::Unknown,
            continuation: false,
            received: ReceiveStats::default(),
            accepted: AcceptedStats::default(),
            accepted_text: String::new(),
            committed_text_len: 0,
            continuation_text_exceeded: false,
            saw_finish: false,
            violations: 0,
            last_sequence: None,
            last_sequence_raw_len: 0,
            last_sequence_hash: 0,
            replayed_frames: 0,
            sequence_conflicts: 0,
            items_started: std::collections::HashSet::new(),
            parts_started: std::collections::HashSet::new(),
        }
    }

    pub fn protocol(&self) -> ProviderProtocol {
        self.protocol
    }

    /// 消费一帧的全部源事实。未知/无效事件仍计入 Received。
    ///
    /// 返回 false 表示该帧是同一序列域内的确切重放(同序号 + 同内容),
    /// 调用方不得再把它喂给目标编码器;Received 仍已计数。
    pub fn observe(&mut self, facts: &SourceFacts) -> bool {
        self.received.frames = self.received.frames.saturating_add(1);
        self.received.bytes = self
            .received
            .bytes
            .saturating_add(facts.identity.raw_len as u64);
        let at = facts.identity.received_at_ms;
        if at > 0 {
            self.received.first_at_ms.get_or_insert(at);
            self.received.last_at_ms = Some(at);
        }
        if !self.admit_sequence(facts) {
            return false;
        }

        let mut accepted_any = false;
        for event in &facts.events {
            match self.lifecycle.observe(event) {
                Ok(()) => {
                    accepted_any = true;
                    self.accepted.events = self.accepted.events.saturating_add(1);
                    self.accept_event(event);
                }
                Err(violation) => {
                    self.violations = self.violations.saturating_add(1);
                    self.accepted.rejected_events = self.accepted.rejected_events.saturating_add(1);
                    if matches!(violation, StreamViolation::DuplicateFinish(_)) {
                        self.saw_finish = true;
                    }
                }
            }
        }
        if accepted_any {
            self.accepted.frames = self.accepted.frames.saturating_add(1);
        }

        for update in &facts.updates {
            match update {
                StreamUpdate::ItemStart { key, .. } => {
                    self.accepted.items = self.accepted.items.saturating_add(1);
                    if !self.items_started.insert(*key) {
                        self.violations = self.violations.saturating_add(1);
                    }
                }
                StreamUpdate::PartStart { key, .. } => {
                    self.accepted.parts = self.accepted.parts.saturating_add(1);
                    if !self.parts_started.insert(*key) {
                        self.violations = self.violations.saturating_add(1);
                    }
                }
                StreamUpdate::ToolStart { .. } => {
                    self.accepted.tool_calls = self.accepted.tool_calls.saturating_add(1);
                }
                StreamUpdate::ItemEnd { key } => {
                    self.items_started.remove(key);
                    let item = crate::semantic::ir::StreamKey::item(key.candidate, key.item);
                    self.parts_started.retain(|part| {
                        crate::semantic::ir::StreamKey::item(part.candidate, part.item) != item
                    });
                }
                StreamUpdate::PartEnd { key } => {
                    self.parts_started.remove(key);
                }
                StreamUpdate::ItemSnapshot { .. } | StreamUpdate::ResponseSnapshot { .. } => {
                    self.accepted.snapshots = self.accepted.snapshots.saturating_add(1);
                }
                _ => {}
            }
        }

        if !facts.usage.is_empty() {
            let terminal_usage =
                !facts.finishes.is_empty() || facts.terminal.is_some() || self.saw_finish;
            self.quality = self.quality.merge(if terminal_usage {
                UsageQuality::Final
            } else {
                UsageQuality::Partial
            });
        }
        true
    }

    /// R1 同域序列域:同序号同内容 = 确切重放(拒收);同序号异内容 =
    /// 冲突(保留并计违规);降序/缺口不猜测,保留并计违规。只有协议
    /// 明确提供 `sequence_number` 时才启用,不把普通流当成序列域。
    fn admit_sequence(&mut self, facts: &SourceFacts) -> bool {
        let Some(sequence) = facts.sequence else {
            return true;
        };
        if let Some(last) = self.last_sequence {
            if sequence < last {
                self.violations = self.violations.saturating_add(1);
                self.sequence_conflicts = self.sequence_conflicts.saturating_add(1);
                return true;
            }
            if sequence == last {
                let fingerprint = facts_fingerprint(facts);
                if facts.identity.raw_len == self.last_sequence_raw_len
                    && fingerprint == self.last_sequence_hash
                {
                    self.replayed_frames = self.replayed_frames.saturating_add(1);
                    return false;
                }
                self.violations = self.violations.saturating_add(1);
                self.sequence_conflicts = self.sequence_conflicts.saturating_add(1);
                return true;
            }
        }
        self.last_sequence = Some(sequence);
        self.last_sequence_raw_len = facts.identity.raw_len;
        self.last_sequence_hash = facts_fingerprint(facts);
        true
    }

    fn accept_event(&mut self, event: &SemanticEvent) {
        match event {
            SemanticEvent::Usage(update) => {
                self.attempt_usage.merge_max(update.to_domain());
                self.accepted.usage_events = self.accepted.usage_events.saturating_add(1);
            }
            SemanticEvent::Finish { .. } => self.saw_finish = true,
            SemanticEvent::PartDelta { text, .. } if !text.is_empty() => {
                self.accepted.content_events = self.accepted.content_events.saturating_add(1);
                self.accepted.text_bytes =
                    self.accepted.text_bytes.saturating_add(text.len() as u64);
                self.push_accepted_text(text);
            }
            SemanticEvent::PartStart {
                part: crate::semantic::model::ContentPart::Text { text },
                ..
            } if !text.is_empty() => {
                self.accepted.content_events = self.accepted.content_events.saturating_add(1);
                self.accepted.text_bytes =
                    self.accepted.text_bytes.saturating_add(text.len() as u64);
                self.push_accepted_text(text);
            }
            SemanticEvent::ToolArgumentsDelta { fragment, .. } if !fragment.is_empty() => {
                self.accepted.tool_argument_bytes = self
                    .accepted
                    .tool_argument_bytes
                    .saturating_add(fragment.len() as u64);
            }
            _ => {}
        }
    }

    fn push_accepted_text(&mut self, text: &str) {
        if self.continuation_text_exceeded {
            return;
        }
        if self.accepted_text.len().saturating_add(text.len()) > CONTINUATION_PREFIX_MAX_BYTES {
            self.continuation_text_exceeded = true;
            self.committed_text_len = self.committed_text_len.min(self.accepted_text.len());
            return;
        }
        self.accepted_text.push_str(text);
    }

    /// 已接受正文是否超过续写前缀上限(超过后不得再续写)。
    pub fn continuation_prefix_exceeded(&self) -> bool {
        self.continuation_text_exceeded
    }

    /// 新 attempt 开始:保留跨 attempt 的累计用量,重置本 attempt 的
    /// 生命周期、质量与未提交投影。只在尚未提交任何下游字节时调用。
    pub fn begin_attempt(&mut self, _attempt: u32) {
        self.checkpoint_attempt_usage();
        self.lifecycle = crate::semantic::stream::StreamLifecycle::new();
        self.quality = UsageQuality::Unknown;
        self.saw_finish = false;
        self.accepted_text.clear();
        self.committed_text_len = 0;
        self.continuation_text_exceeded = false;
        self.last_sequence = None;
        self.last_sequence_raw_len = 0;
        self.last_sequence_hash = 0;
        self.items_started.clear();
        self.parts_started.clear();
    }

    /// 进入生成式续写:上一次尝试的用量累加(两段各花各的额度),
    /// 已提交正文保留为下一次续写的前缀。
    pub fn begin_continuation(&mut self) {
        self.checkpoint_attempt_usage();
        self.lifecycle = crate::semantic::stream::StreamLifecycle::new();
        self.quality = UsageQuality::Unknown;
        self.saw_finish = false;
        self.continuation = true;
        self.continuation_text_exceeded = false;
        self.last_sequence = None;
        self.last_sequence_raw_len = 0;
        self.last_sequence_hash = 0;
        self.items_started.clear();
        self.parts_started.clear();
    }

    fn checkpoint_attempt_usage(&mut self) {
        if self.attempt_usage != Usage::empty() {
            self.usage.merge_add(self.attempt_usage);
            self.attempt_usage = Usage::empty();
        }
    }

    pub fn in_continuation(&self) -> bool {
        self.continuation
    }

    pub fn status(&self) -> StreamStatus {
        self.lifecycle.status()
    }

    /// 是否已观察到语义 `Finish`(stop reason)。20261108_01 §4.1:
    /// 这是「答案完成」的里程碑 —— 请求耗时在此冻结、并发许可在此释放。
    /// 注意与 [`Self::terminal`] 的区别:后者等协议终态帧
    /// (`[DONE]` / `message_stop` / `response.completed`)或 EOF 完整性门。
    pub fn saw_finish(&self) -> bool {
        self.saw_finish
    }

    pub fn terminal(&self) -> bool {
        matches!(
            self.lifecycle.status(),
            StreamStatus::Complete | StreamStatus::Incomplete
        )
    }

    /// EOF 完整性门(与旧 ClientStream 语义一致,但由观察者独占)。
    pub fn complete(&mut self) -> bool {
        self.lifecycle.complete()
    }

    /// 整条响应的累计用量(含续写前的尝试)。
    pub fn usage(&self) -> Usage {
        let mut total = self.usage;
        total.merge_add(self.attempt_usage);
        total
    }

    pub fn attempt_usage(&self) -> Usage {
        self.attempt_usage
    }

    pub fn usage_observation(&self) -> UsageObservation {
        UsageObservation {
            usage: self.usage(),
            quality: self.quality,
            scope: UsageScope::Response,
        }
    }

    pub fn attempt_usage_observation(&self) -> UsageObservation {
        UsageObservation {
            usage: self.attempt_usage,
            quality: self.quality,
            scope: UsageScope::Attempt,
        }
    }

    pub fn usage_quality(&self) -> UsageQuality {
        self.quality
    }

    pub fn received(&self) -> ReceiveStats {
        self.received
    }

    pub fn accepted(&self) -> AcceptedStats {
        self.accepted
    }

    /// 已接受但未必已提交的正文投影(只含可续写文本)。
    pub fn accepted_text(&self) -> &str {
        &self.accepted_text
    }

    /// 已确认入队的正文前缀(续写 prefill 的唯一来源)。
    pub fn committed_text(&self) -> &str {
        &self.accepted_text[..self.committed_text_len]
    }

    /// 投递层确认当前已接受的正文全部入队。
    pub fn mark_committed(&mut self) {
        self.committed_text_len = self.accepted_text.len();
    }

    pub fn take_violations(&mut self) -> u64 {
        std::mem::replace(&mut self.violations, 0)
    }

    /// 被 R1 序列域识别并拒收的确切重放帧数。
    pub fn replayed_frames(&self) -> u64 {
        self.replayed_frames
    }

    /// 同序号异内容/降序帧数(保留但计违规)。
    pub fn sequence_conflicts(&self) -> u64 {
        self.sequence_conflicts
    }
}

fn facts_fingerprint(facts: &SourceFacts) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    format!("{:?}", facts.events).hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::error::WireLimits;
    use crate::wire::sse::SseFramer;

    fn sse_facts(protocol: ProviderProtocol, text: &str) -> Vec<SourceFacts> {
        let mut framer = SseFramer::new(
            WireLimits::default(),
            crate::wire::record::RecordSource::upstream("test"),
        );
        let records = framer.feed(text.as_bytes(), 10).unwrap();
        records
            .iter()
            .map(|record| SourceFacts::from_record(protocol, record).unwrap())
            .collect()
    }

    #[test]
    fn usage_and_finish_can_share_one_frame() {
        let text = "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":4}}\n\n";
        let facts = sse_facts(ProviderProtocol::Anthropic, text);
        assert_eq!(facts.len(), 1);
        let facts = &facts[0];
        assert!(!facts.usage.is_empty());
        assert_eq!(facts.finishes.len(), 1);
        assert!(facts.recognized);
        assert_eq!(facts.identity.offset, 0);
        assert_eq!(facts.identity.received_at_ms, 10);
        assert!(!facts.events.is_empty());
    }

    #[test]
    fn responses_terminal_snapshot_coexists_with_usage_and_finish() {
        let text = concat!(
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"usage\":{\"input_tokens\":3,\"output_tokens\":5,\"total_tokens\":8}}}\n\n",
        );
        let facts = sse_facts(ProviderProtocol::OpenAi, text);
        let facts = &facts[0];
        assert!(facts.snapshot.is_some());
        assert_eq!(facts.usage.len(), 1);
        assert_eq!(facts.finishes.len(), 1);
        assert_eq!(facts.terminal, Some(FrameTerminal::Complete));
    }

    #[test]
    fn observer_marks_final_usage_and_rejects_events_after_terminal() {
        let mut observer = SourceObserver::new(ProviderProtocol::Anthropic);
        let frames = sse_facts(
            ProviderProtocol::Anthropic,
            concat!(
                "event: message_start\n",
                "data: {\"type\":\"message_start\",\"message\":{\"id\":\"m1\",\"usage\":{\"input_tokens\":5}}}\n\n",
                "event: message_delta\n",
                "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":4}}\n\n",
                "event: message_stop\n",
                "data: {\"type\":\"message_stop\"}\n\n",
                "event: message_delta\n",
                "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":99}}\n\n",
            ),
        );
        for facts in &frames {
            observer.observe(facts);
        }
        assert_eq!(observer.status(), StreamStatus::Complete);
        assert_eq!(observer.usage().input_tokens, 5);
        assert_eq!(observer.usage().output_tokens, 4);
        assert_eq!(
            observer.usage().total_tokens,
            9,
            "上游未报 total 时观察者与流式编码同口径"
        );
        assert_eq!(observer.usage_quality(), UsageQuality::Final);
        assert_eq!(observer.received().frames, 4);
        assert!(observer.accepted().rejected_events >= 1);
    }

    #[test]
    fn continuation_accumulates_usage_and_keeps_committed_text() {
        let mut observer = SourceObserver::new(ProviderProtocol::Anthropic);
        for facts in sse_facts(
            ProviderProtocol::Anthropic,
            concat!(
                "event: content_block_delta\n",
                "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"前半\"}}\n\n",
                "event: message_delta\n",
                "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":4}}\n\n",
            ),
        ) {
            observer.observe(&facts);
        }
        observer.mark_committed();
        assert_eq!(observer.committed_text(), "前半");
        observer.begin_continuation();
        assert!(observer.in_continuation());
        assert_eq!(observer.committed_text(), "前半");
        for facts in sse_facts(
            ProviderProtocol::Anthropic,
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"后半\"}}\n\n",
        ) {
            observer.observe(&facts);
        }
        observer.mark_committed();
        assert_eq!(observer.usage().output_tokens, 4);
        assert_eq!(observer.committed_text(), "前半后半");
    }

    #[test]
    fn sequence_watermark_rejects_exact_replay_but_keeps_conflicts() {
        let frame = |text: &str, seq: u64| {
            sse_facts(
                ProviderProtocol::OpenAi,
                &format!(
                    "event: response.output_text.delta\ndata: {{\"type\":\"response.output_text.delta\",\"sequence_number\":{seq},\"output_index\":0,\"content_index\":0,\"delta\":\"{text}\"}}\n\n"
                ),
            )
            .remove(0)
        };
        let mut observer = SourceObserver::new(ProviderProtocol::OpenAi);
        let first = frame("aa", 5);
        assert!(observer.observe(&first));
        let replay = frame("aa", 5);
        assert!(!observer.observe(&replay), "同序号同内容应判确切重放");
        assert_eq!(observer.replayed_frames(), 1);
        let conflict = frame("bb", 5);
        assert!(observer.observe(&conflict), "同序号异内容应保留并计冲突");
        assert_eq!(observer.sequence_conflicts(), 1);
        assert!(observer.observe(&frame("cc", 6)), "递增序号正常接收");
        assert!(observer.observe(&frame("dd", 4)), "降序不猜测,保留并计冲突");
        assert_eq!(observer.sequence_conflicts(), 2);
    }

    #[test]
    fn continuation_prefix_is_bounded_and_disables_continuation() {
        let text = "x".repeat(CONTINUATION_PREFIX_MAX_BYTES + 1);
        let frame = format!(
            "event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"{text}\"}}}}\n\n"
        );
        let mut observer = SourceObserver::new(ProviderProtocol::Anthropic);
        for facts in sse_facts(ProviderProtocol::Anthropic, &frame) {
            observer.observe(&facts);
        }
        assert!(observer.continuation_prefix_exceeded());
        assert!(observer.accepted_text().len() <= CONTINUATION_PREFIX_MAX_BYTES);
        assert_eq!(
            observer.accepted().text_bytes,
            (CONTINUATION_PREFIX_MAX_BYTES + 1) as u64,
            "统计仍按真实接收量计数"
        );
    }

    #[test]
    fn new_attempt_discards_uncommitted_text_but_keeps_usage() {
        let mut observer = SourceObserver::new(ProviderProtocol::Anthropic);
        for facts in sse_facts(
            ProviderProtocol::Anthropic,
            concat!(
                "event: content_block_delta\n",
                "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"uncommitted\"}}\n\n",
                "event: message_delta\n",
                "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":7}}\n\n",
            ),
        ) {
            observer.observe(&facts);
        }
        observer.begin_attempt(1);
        assert!(observer.accepted_text().is_empty());
        assert_eq!(observer.committed_text(), "");
        assert_eq!(observer.usage().output_tokens, 7);
        assert_eq!(observer.usage_quality(), UsageQuality::Unknown);
    }

    #[test]
    fn usage_quality_stays_unknown_until_a_usage_fact_arrives() {
        let mut observer = SourceObserver::new(ProviderProtocol::OpenAi);
        assert_eq!(observer.usage_quality(), UsageQuality::Unknown);
        assert_eq!(observer.usage(), Usage::empty());
        for facts in sse_facts(
            ProviderProtocol::OpenAi,
            "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\n",
        ) {
            observer.observe(&facts);
        }
        assert_eq!(observer.usage_quality(), UsageQuality::Unknown);
        assert_eq!(observer.usage(), Usage::empty());
        assert_eq!(observer.accepted().text_bytes, 2);
    }

    #[test]
    fn duplicate_finish_is_rejected_and_replayed_usage_not_double_counted() {
        // P3-3(R1/R5 原生适用部分):同域同序号的重复终态识别为冲突,
        // 只计入 Received/违规;重放 usage 用 merge_max,不重复计费。
        let mut observer = SourceObserver::new(ProviderProtocol::OpenAi);
        for facts in sse_facts(
            ProviderProtocol::OpenAi,
            concat!(
                "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":3,\"total_tokens\":8}}\n\n",
                "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":3,\"total_tokens\":8}}\n\n",
            ),
        ) {
            observer.observe(&facts);
        }
        assert!(observer.received().frames >= 1);
        assert!(observer.accepted().rejected_events >= 1);
        assert_eq!(observer.usage().input_tokens, 5);
        assert_eq!(observer.usage().output_tokens, 3);
    }

    #[test]
    fn observer_counts_item_part_lifecycle_and_rejects_duplicate_start() {
        let mut observer = SourceObserver::new(ProviderProtocol::OpenAi);
        for facts in sse_facts(
            ProviderProtocol::OpenAi,
            concat!(
                "event: response.output_item.added\n",
                "data: {\"output_index\":0,\"item\":{\"id\":\"msg_1\",\"type\":\"message\",\"content\":[]}}\n\n",
                "event: response.content_part.added\n",
                "data: {\"output_index\":0,\"content_index\":0,\"item_id\":\"msg_1\",\"part\":{\"type\":\"output_text\",\"text\":\"\"}}\n\n",
                "event: response.content_part.added\n",
                "data: {\"output_index\":0,\"content_index\":0,\"item_id\":\"msg_1\",\"part\":{\"type\":\"output_text\",\"text\":\"\"}}\n\n",
                "event: response.output_text.done\n",
                "data: {\"output_index\":0,\"content_index\":0,\"item_id\":\"msg_1\",\"text\":\"hi\"}\n\n",
                "event: response.output_item.done\n",
                "data: {\"output_index\":0,\"item\":{\"id\":\"msg_1\",\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"hi\"}]}}\n\n",
            ),
        ) {
            observer.observe(&facts);
        }
        let accepted = observer.accepted();
        assert_eq!(accepted.items, 1);
        assert_eq!(accepted.parts, 2, "重复开场仍计入 Received/Accepted 次数");
        assert!(accepted.snapshots >= 1);
        assert!(
            observer.take_violations() >= 1,
            "同一 item/part 的重复开场必须计违规"
        );
    }

    #[test]
    fn eof_tail_identity_is_not_terminated() {
        let mut framer = SseFramer::new(
            WireLimits::default(),
            crate::wire::record::RecordSource::upstream("test"),
        );
        let records = framer.finish(1).unwrap();
        assert!(records.is_empty());
        let mut framer = SseFramer::new(
            WireLimits::default(),
            crate::wire::record::RecordSource::upstream("test"),
        );
        assert!(framer.feed(b"data: tail", 1).unwrap().is_empty());
        let tail = framer.finish(2).unwrap();
        let facts = SourceFacts::from_record(ProviderProtocol::OpenAi, &tail[0]).unwrap();
        assert!(!facts.identity.terminated);
    }

    #[test]
    fn ndjson_done_sentinel_is_terminal_not_sse_data() {
        let mut framer = crate::wire::ndjson::NdjsonFramer::new(
            WireLimits::default(),
            crate::wire::record::RecordSource::upstream("test"),
        );
        let records = framer.feed(b"[DONE]\n", 1).unwrap();
        let facts = SourceFacts::from_record(ProviderProtocol::OpenAi, &records[0]).unwrap();
        assert_eq!(facts.terminal, Some(FrameTerminal::Complete));
        assert!(
            facts
                .events
                .iter()
                .any(|event| matches!(event, SemanticEvent::StreamEnd { incomplete: false }))
        );
    }

    #[test]
    fn json_whole_document_is_not_a_stream_source() {
        let record = RawRecord::new(
            b"{\"a\":1}".as_slice(),
            0..7,
            0,
            0,
            Representation::Json,
            crate::wire::record::RecordSource::upstream("test"),
            RecordView::Opaque,
            true,
        )
        .unwrap();
        let error = SourceFacts::from_record(ProviderProtocol::OpenAi, &record).unwrap_err();
        assert_eq!(
            error,
            SourceFactError::UnsupportedRepresentation(Representation::Json)
        );
    }
}
