//! 无损记录模型(方案 20261106_01 §3.4、P1-3)。
//!
//! 一条 [`RawRecord`] 精确对应输入流中的一段字节:内容 + 记录分隔符。
//! `bytes` 始终是完整原文,`content` 是其中的内容范围,二者相减得到
//! 分隔符字节。解析视图是可复用的共享解释,**不替代**原文。
//!
//! 字节偏移是流内绝对偏移,从 0 开始;`received_at_ms` 是该记录**开始**
//! 被 framer 观察到的时间(跨 chunk 的记录取首块时间,由调用方的时钟
//! 注入,领域层不读取系统时钟)。

use std::ops::Range;
use std::sync::Arc;

use serde_json::Value;

use crate::wire::error::WireError;
use crate::wire::json::JsonView;
use crate::wire::ndjson::NdjsonView;
use crate::wire::sse::SseView;

/// 线格式表示。三者分帧规则不同,禁止互相冒充。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Representation {
    Sse,
    Json,
    Ndjson,
}

impl Representation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sse => "sse",
            Self::Json => "json",
            Self::Ndjson => "ndjson",
        }
    }
}

/// 记录来源方向。缓存重放与合成数据不得冒充上游原始执行。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RecordOrigin {
    Upstream,
    Downstream,
    Cache,
    Synthetic,
}

impl RecordOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Upstream => "upstream",
            Self::Downstream => "downstream",
            Self::Cache => "cache",
            Self::Synthetic => "synthetic",
        }
    }
}

/// 记录来源:方向 + 可选 profile(如 `openai_chat`、`anthropic_messages`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordSource {
    pub origin: RecordOrigin,
    pub profile: Option<Arc<str>>,
}

impl RecordSource {
    pub fn new(origin: RecordOrigin, profile: Option<Arc<str>>) -> Self {
        Self { origin, profile }
    }

    pub fn upstream(profile: impl Into<Arc<str>>) -> Self {
        Self::new(RecordOrigin::Upstream, Some(profile.into()))
    }

    pub fn downstream(profile: impl Into<Arc<str>>) -> Self {
        Self::new(RecordOrigin::Downstream, Some(profile.into()))
    }
}

/// 返回给下游/日志前必须显式携带的解释视图。
#[derive(Debug, Clone)]
pub enum RecordView {
    Sse(Box<SseView>),
    Json(Arc<JsonView>),
    Ndjson(Box<NdjsonView>),
    /// 未解释的字节(未知表示);不得据此猜测协议。
    Opaque,
}

impl RecordView {
    pub fn kind(&self) -> Option<Representation> {
        match self {
            Self::Sse(_) => Some(Representation::Sse),
            Self::Json(_) => Some(Representation::Json),
            Self::Ndjson(_) => Some(Representation::Ndjson),
            Self::Opaque => None,
        }
    }
}

/// 一线记录:原文 + 范围 + 分隔符 + 来源 + 到达时间 + 共享解析视图。
#[derive(Debug, Clone)]
pub struct RawRecord {
    bytes: Arc<[u8]>,
    content: Range<usize>,
    offset: u64,
    received_at_ms: i64,
    representation: Representation,
    source: RecordSource,
    view: Arc<RecordView>,
    terminated: bool,
}

impl RawRecord {
    /// 外部构造入口:校验内容范围。framer 内部使用更轻的
    /// [`Self::from_parts`]。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        bytes: impl Into<Arc<[u8]>>,
        content: Range<usize>,
        offset: u64,
        received_at_ms: i64,
        representation: Representation,
        source: RecordSource,
        view: RecordView,
        terminated: bool,
    ) -> Result<Self, WireError> {
        let bytes: Arc<[u8]> = bytes.into();
        if content.start > content.end || content.end > bytes.len() {
            return Err(WireError::InvalidRecordRange {
                start: content.start,
                end: content.end,
                len: bytes.len(),
            });
        }
        Ok(Self {
            bytes,
            content,
            offset,
            received_at_ms,
            representation,
            source,
            view: Arc::new(view),
            terminated,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_parts(
        bytes: Arc<[u8]>,
        content: Range<usize>,
        offset: u64,
        received_at_ms: i64,
        representation: Representation,
        source: RecordSource,
        view: Arc<RecordView>,
        terminated: bool,
    ) -> Self {
        Self {
            bytes,
            content,
            offset,
            received_at_ms,
            representation,
            source,
            view,
            terminated,
        }
    }

    /// 完整原文(内容 + 分隔符)。
    pub fn raw_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// 内容字节(不含记录分隔符)。
    pub fn content(&self) -> &[u8] {
        &self.bytes[self.content.clone()]
    }

    /// 记录分隔符字节;EOF 残片为空。
    pub fn terminator(&self) -> &[u8] {
        &self.bytes[self.content.end..]
    }

    pub fn byte_len(&self) -> usize {
        self.bytes.len()
    }

    pub fn content_len(&self) -> usize {
        self.content.len()
    }

    /// 记录在输入流中的起始绝对字节偏移。
    pub fn offset(&self) -> u64 {
        self.offset
    }

    pub fn end_offset(&self) -> u64 {
        self.offset + self.bytes.len() as u64
    }

    pub fn received_at_ms(&self) -> i64 {
        self.received_at_ms
    }

    pub fn representation(&self) -> Representation {
        self.representation
    }

    pub fn source(&self) -> &RecordSource {
        &self.source
    }

    pub fn view(&self) -> &RecordView {
        &self.view
    }

    /// 是否由完整记录分隔符终止。SSE 的 `false` 表示 EOF 残片,
    /// 不得当作已分派事件;NDJSON 的末行可以合法地不带换行。
    pub fn is_terminated(&self) -> bool {
        self.terminated
    }

    pub fn sse(&self) -> Option<&SseView> {
        match self.view.as_ref() {
            RecordView::Sse(view) => Some(view),
            _ => None,
        }
    }

    pub fn json(&self) -> Option<&JsonView> {
        match self.view.as_ref() {
            RecordView::Json(view) => Some(view),
            _ => None,
        }
    }

    pub fn ndjson(&self) -> Option<&NdjsonView> {
        match self.view.as_ref() {
            RecordView::Ndjson(view) => Some(view),
            _ => None,
        }
    }

    /// 按原字节追加到输出缓冲,用于 ByteExact 重组。
    pub fn append_raw_to(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.bytes);
    }

    /// 尽力把 SSE data 解析为 JSON(与原 `SseEvent::json` 语义一致,
    /// 供迁移期消费者使用)。非 SSE 记录返回 `None`。
    pub fn sse_json(&self) -> Option<Value> {
        self.sse().and_then(SseView::json)
    }
}

/// 原字节重组:所有记录的完整原文按顺序拼接。
pub fn reassemble_raw(records: &[RawRecord]) -> Vec<u8> {
    let mut out = Vec::with_capacity(records.iter().map(RawRecord::byte_len).sum());
    for record in records {
        record.append_raw_to(&mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> RecordView {
        RecordView::Opaque
    }

    #[test]
    fn splits_content_and_terminator() {
        let record = RawRecord::new(
            b"data: x\n\n".as_slice(),
            0..8,
            0,
            1,
            Representation::Sse,
            RecordSource::upstream("openai_chat"),
            view(),
            true,
        )
        .unwrap();
        assert_eq!(record.content(), b"data: x\n");
        assert_eq!(record.terminator(), b"\n");
        assert_eq!(record.end_offset(), 9);
    }

    #[test]
    fn rejects_invalid_ranges() {
        let err = RawRecord::new(
            b"abc".as_slice(),
            1..9,
            0,
            0,
            Representation::Sse,
            RecordSource::upstream("x"),
            view(),
            true,
        )
        .unwrap_err();
        assert!(matches!(err, WireError::InvalidRecordRange { .. }));
    }

    #[test]
    fn reassembles_exactly() {
        let make = |bytes: &[u8], content_end: usize, offset: u64| {
            RawRecord::new(
                bytes,
                0..content_end,
                offset,
                0,
                Representation::Sse,
                RecordSource::upstream("x"),
                view(),
                true,
            )
            .unwrap()
        };
        let records = vec![make(b"data: a\n\n", 8, 0), make(b"data: b\n\n", 8, 9)];
        assert_eq!(reassemble_raw(&records), b"data: a\n\ndata: b\n\n");
    }
}
