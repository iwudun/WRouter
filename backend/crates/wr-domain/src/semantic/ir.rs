//! 层级流 IR(方案 20261106_01 P4-1/P4-2)。
//!
//! [`crate::semantic::model::SemanticEvent`] 是候选/块级别的通用词汇,
//! 无法表达 Responses 的 item/part 身份。本模块提供一层显式层级:
//! response → candidate → item → part → tool call,并区分开始、增量、
//! 快照与结束。
//!
//! 消费边界(2026-11-07 收口):Responses 目标编码器消费
//! [`StreamUpdate`];观察者与 Chat/Anthropic/Gemini 目标编码器仍消费
//! 扁平 [`crate::semantic::model::SemanticEvent`]。两组词汇共享同一份
//! 源事实,但不是“所有编码器已统一”的承诺。

use std::collections::BTreeMap;

use serde_json::Value;

use crate::semantic::error::SemanticError;
use crate::semantic::model::{
    ContentPart, Extensions, FinishReason, SourceProtocol, ToolCall, ToolResult, Usage,
};

/// 流内层级位置。`candidate` 是候选序号,`item` 是候选内 item/output
/// 序号,`part` 是 item 内 part/content 序号。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct StreamKey {
    pub candidate: usize,
    pub item: usize,
    pub part: usize,
}

impl StreamKey {
    pub const fn new(candidate: usize, item: usize, part: usize) -> Self {
        Self {
            candidate,
            item,
            part,
        }
    }

    pub fn item(candidate: usize, item: usize) -> Self {
        Self::new(candidate, item, 0)
    }
}

/// item 的语义种类(不是协议私有 type 字符串;未知值保留在
/// [`ItemKind::Unknown`] 中)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemKind {
    Message,
    Reasoning,
    ToolCall,
    Unknown(String),
}

impl ItemKind {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Message => "message",
            Self::Reasoning => "reasoning",
            Self::ToolCall => "tool_call",
            Self::Unknown(kind) => kind.as_str(),
        }
    }
}

/// 完整响应快照的来源方言。快照不是「又一帧 delta」,而是权威终值,
/// 对账时必须知道按哪套协议解释。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotFormat {
    OpenAiResponses,
    AnthropicMessage,
}

/// 层级流更新。一条更新可以是一帧事实的一部分;同一帧内也可以同时
/// 产生 Usage/Finish/快照。
#[derive(Debug, Clone)]
pub enum StreamUpdate {
    ResponseStart {
        source: SourceProtocol,
        id: Option<String>,
        model: Option<String>,
    },
    ItemStart {
        key: StreamKey,
        id: Option<String>,
        kind: ItemKind,
        extensions: Extensions,
    },
    PartStart {
        key: StreamKey,
        id: Option<String>,
        part: ContentPart,
        extensions: Extensions,
    },
    PartDelta {
        key: StreamKey,
        text: String,
    },
    RefusalDelta {
        key: StreamKey,
        text: String,
    },
    ReasoningStart {
        key: StreamKey,
        id: Option<String>,
    },
    ReasoningDelta {
        key: StreamKey,
        text: String,
    },
    ReasoningSignature {
        key: StreamKey,
        signature: String,
    },
    ToolStart {
        key: StreamKey,
        tool_index: usize,
        call: ToolCall,
    },
    ToolArgumentsDelta {
        key: StreamKey,
        tool_index: usize,
        fragment: String,
    },
    ToolResult {
        key: StreamKey,
        result: ToolResult,
    },
    PartEnd {
        key: StreamKey,
    },
    ItemSnapshot {
        key: StreamKey,
        snapshot: Value,
    },
    ItemEnd {
        key: StreamKey,
    },
    ResponseSnapshot {
        format: SnapshotFormat,
        value: Value,
    },
    Usage(Usage),
    Finish {
        candidate: usize,
        reason: FinishReason,
    },
    End {
        incomplete: bool,
    },
    Error(SemanticError),
    Opaque {
        raw: Value,
    },
}

/// 工具参数对账结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgumentReconciliation {
    /// 快照与已接收内容一致(重放),不再输出字节。
    Duplicate,
    /// 快照严格延长已接收前缀;只输出新增后缀。
    Extension { suffix: String },
    /// 尚无任何参数内容;快照可作为首值输出。
    Snapshot { arguments: String },
    /// 快照与已提交前缀冲突;保留已提交内容并显式记录。
    Conflict { committed: String, snapshot: String },
}

/// 单个工具调用的参数组装状态。索引 `committed` 是已经推送给客户端的
/// 字节长度;快照只允许延长这个前缀,不允许改写它。
#[derive(Debug, Clone)]
pub struct ToolCallAssembly {
    pub key: StreamKey,
    pub tool_index: usize,
    pub id: String,
    pub name: String,
    received: String,
    committed: usize,
    snapshot: Option<String>,
    conflict: Option<ArgumentReconciliation>,
}

impl ToolCallAssembly {
    pub fn new(key: StreamKey, tool_index: usize, call: &ToolCall) -> Self {
        Self {
            key,
            tool_index,
            id: call.id.local.clone(),
            name: call.name.clone(),
            received: String::new(),
            committed: 0,
            snapshot: None,
            conflict: None,
        }
    }

    /// 开场帧已经携带参数:视为已推送内容。
    pub fn seed_opening(&mut self, call: &ToolCall) {
        if let Some(opening) = opening_arguments(call) {
            self.received.push_str(&opening);
            self.committed = self.received.len();
        }
    }

    pub fn push_delta(&mut self, fragment: &str) {
        self.received.push_str(fragment);
    }

    /// 调用方确认当前已接收内容全部入队。
    pub fn mark_committed(&mut self) {
        self.committed = self.received.len();
    }

    pub fn committed(&self) -> &str {
        &self.received[..self.committed]
    }

    pub fn received(&self) -> &str {
        &self.received
    }

    pub fn snapshot(&self) -> Option<&str> {
        self.snapshot.as_deref()
    }

    pub fn conflict(&self) -> Option<&ArgumentReconciliation> {
        self.conflict.as_ref()
    }

    /// 用完整 item 快照对账。返回值告诉调用方是否还能安全输出后缀。
    pub fn reconcile_snapshot(&mut self, final_arguments: &str) -> ArgumentReconciliation {
        self.snapshot = Some(final_arguments.to_owned());
        let outcome = reconcile_arguments(self.committed(), final_arguments);
        if matches!(outcome, ArgumentReconciliation::Conflict { .. }) {
            self.conflict = Some(outcome.clone());
        }
        outcome
    }

    /// 最终输出参数:冲突时保留已提交前缀,否则优先权威快照。
    pub fn final_arguments(&self) -> String {
        if self.conflict.is_some() {
            return self.committed().to_owned();
        }
        self.snapshot
            .clone()
            .unwrap_or_else(|| self.received().to_owned())
    }

    /// 最终对象形态;参数不是完整合法 JSON 时返回 `None`,不用 `{}`
    /// 掩盖残片。
    pub fn final_object(&self) -> Option<Value> {
        let text = self.final_arguments();
        if text.trim().is_empty() {
            return Some(serde_json::json!({}));
        }
        serde_json::from_str::<Value>(&text).ok()
    }
}

/// 纯函数对账:已提交前缀 vs 权威快照。
pub fn reconcile_arguments(committed: &str, snapshot: &str) -> ArgumentReconciliation {
    if committed == snapshot {
        ArgumentReconciliation::Duplicate
    } else if committed.is_empty() {
        ArgumentReconciliation::Snapshot {
            arguments: snapshot.to_owned(),
        }
    } else if let Some(suffix) = snapshot.strip_prefix(committed) {
        ArgumentReconciliation::Extension {
            suffix: suffix.to_owned(),
        }
    } else {
        ArgumentReconciliation::Conflict {
            committed: committed.to_owned(),
            snapshot: snapshot.to_owned(),
        }
    }
}

/// 工具调用集合:按层级键维护组装状态,保留顺序。
#[derive(Debug, Clone, Default)]
pub struct ToolCallSet {
    calls: BTreeMap<StreamKey, ToolCallAssembly>,
}

impl ToolCallSet {
    pub fn start(
        &mut self,
        key: StreamKey,
        tool_index: usize,
        call: &ToolCall,
    ) -> &mut ToolCallAssembly {
        self.calls
            .entry(key)
            .or_insert_with(|| ToolCallAssembly::new(key, tool_index, call))
    }

    pub fn get(&self, key: &StreamKey) -> Option<&ToolCallAssembly> {
        self.calls.get(key)
    }

    pub fn get_mut(&mut self, key: &StreamKey) -> Option<&mut ToolCallAssembly> {
        self.calls.get_mut(key)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&StreamKey, &ToolCallAssembly)> {
        self.calls.iter()
    }

    pub fn remove(&mut self, key: &StreamKey) -> Option<ToolCallAssembly> {
        self.calls.remove(key)
    }

    pub fn is_empty(&self) -> bool {
        self.calls.is_empty()
    }
}

/// 工具开场参数:Raw 非空或 Parsed 非空对象/数组。
pub fn opening_arguments(call: &ToolCall) -> Option<String> {
    match &call.arguments {
        crate::semantic::model::ToolArguments::Raw(raw) if !raw.is_empty() => Some(raw.clone()),
        crate::semantic::model::ToolArguments::Parsed(value) => {
            if value.as_object().is_some_and(|map| !map.is_empty()) || value.is_array() {
                Some(crate::json_py::py_json_dumps(value))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// 快照输出与流式 output 的语义键集合(忽略 status/序号等展示字段)。
/// 用于 Responses 终态对账测试与运行时报错归因。
pub fn output_item_identity(item: &Value) -> Option<(&str, &str)> {
    let kind = item.get("type").and_then(Value::as_str)?;
    let id = item
        .get("id")
        .or_else(|| item.get("call_id"))
        .and_then(Value::as_str)
        .unwrap_or("");
    Some((kind, id))
}

/// 输出数组是否按身份与内容一致(不比较 status/顺序以外的易变字段)。
pub fn outputs_reconcile(streamed: &[Value], snapshot: &[Value]) -> bool {
    if streamed.len() != snapshot.len() {
        return false;
    }
    streamed.iter().zip(snapshot).all(|(left, right)| {
        output_item_identity(left) == output_item_identity(right)
            && item_content(left) == item_content(right)
    })
}

fn item_content(item: &Value) -> Option<&Value> {
    item.get("content")
        .or_else(|| item.get("arguments"))
        .or_else(|| item.get("summary"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantic::model::{IdOrigin, ToolArguments, ToolCallId};
    use serde_json::json;

    fn call(args: ToolArguments) -> ToolCall {
        ToolCall {
            id: ToolCallId {
                local: "call_1".into(),
                origin: IdOrigin::Source,
            },
            name: "lookup".into(),
            arguments: args,
            extensions: Extensions::default(),
        }
    }

    #[test]
    fn reconciliation_distinguishes_duplicate_extension_snapshot_and_conflict() {
        assert_eq!(
            reconcile_arguments("{\"a\":1}", "{\"a\":1}"),
            ArgumentReconciliation::Duplicate
        );
        assert_eq!(
            reconcile_arguments("{\"a\":", "{\"a\":1}"),
            ArgumentReconciliation::Extension {
                suffix: "1}".into()
            }
        );
        assert_eq!(
            reconcile_arguments("", "{\"a\":1}"),
            ArgumentReconciliation::Snapshot {
                arguments: "{\"a\":1}".into()
            }
        );
        assert!(matches!(
            reconcile_arguments("{\"a\":2}", "{\"a\":1}"),
            ArgumentReconciliation::Conflict { .. }
        ));
    }

    #[test]
    fn assembly_keeps_committed_prefix_on_conflict() {
        let mut assembly = ToolCallAssembly::new(
            StreamKey::new(0, 2, 0),
            0,
            &call(ToolArguments::Raw(String::new())),
        );
        assembly.push_delta("{\"a\":2}");
        assembly.mark_committed();
        let outcome = assembly.reconcile_snapshot("{\"a\":1}");
        assert!(matches!(outcome, ArgumentReconciliation::Conflict { .. }));
        assert_eq!(assembly.final_arguments(), "{\"a\":2}");
        assert_eq!(assembly.final_object(), Some(json!({"a": 2})));
    }

    #[test]
    fn assembly_emits_snapshot_only_when_nothing_was_committed() {
        let mut assembly = ToolCallAssembly::new(
            StreamKey::new(0, 2, 0),
            0,
            &call(ToolArguments::Parsed(json!({}))),
        );
        let outcome = assembly.reconcile_snapshot("{\"q\":1}");
        assert_eq!(
            outcome,
            ArgumentReconciliation::Snapshot {
                arguments: "{\"q\":1}".into()
            }
        );
        assert_eq!(assembly.final_object(), Some(json!({"q": 1})));
    }

    #[test]
    fn opening_arguments_ignores_empty_placeholders() {
        assert_eq!(
            opening_arguments(&call(ToolArguments::Parsed(json!({})))),
            None
        );
        assert_eq!(
            opening_arguments(&call(ToolArguments::Parsed(json!({"q": 1})))),
            Some("{\"q\": 1}".into())
        );
    }

    #[test]
    fn outputs_reconcile_compares_identity_and_content() {
        let left = vec![
            json!({"id":"m1","type":"message","content":[{"type":"output_text","text":"hi"}]}),
        ];
        let right = vec![
            json!({"id":"m1","type":"message","status":"completed","content":[{"type":"output_text","text":"hi"}]}),
        ];
        assert!(outputs_reconcile(&left, &right));
        let different = vec![
            json!({"id":"m1","type":"message","content":[{"type":"output_text","text":"bye"}]}),
        ];
        assert!(!outputs_reconcile(&left, &different));
    }
}
