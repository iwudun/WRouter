//! 单条无损记录 → 源事实 → 待提交帧的编码器(自原流式管线模块纯搬移)。

use super::delivery::{EmissionBatch, SourceSpan};
use super::*;
use wr_domain::semantic::client_stream::ClientStream;
use wr_domain::semantic::source::{SourceFacts, SourceObserver};
use wr_domain::semantic::stream::StreamAggregator;
use wr_domain::wire::record::RawRecord;

/// 把一条无损记录解释成源事实并编码为待提交帧。
pub(super) struct RecordEncoder<'a> {
    pub(super) provider: &'a Provider,
    pub(super) observer: &'a mut SourceObserver,
    pub(super) pump: &'a mut ClientStream,
    /// P3-1:原生 Raw 执行器 —— 输出原记录字节,不经 ClientStream 重建。
    pub(super) native_raw: bool,
    /// 聚合别名(R8):Raw 路径上仅按协议路径回写 model;None = 纯 ByteExact。
    pub(super) aggregate_model: Option<&'a str>,
    pub(super) capture_enabled: bool,
    pub(super) capture_budget: usize,
    pub(super) captured_bytes: &'a mut usize,
    pub(super) observed_bytes: &'a mut usize,
    pub(super) response_chunks: &'a mut Vec<String>,
    pub(super) trace_items: &'a mut Vec<Value>,
    /// 缓存装配:仅缓存启用时累积语义事件/快照,结束时编码成客户端
    /// 完整响应再入缓存(绝不能把原始 SSE 当 JSON 存储)。
    pub(super) cache_aggregator: Option<&'a mut StreamAggregator>,
}

impl RecordEncoder<'_> {
    pub(super) fn encode(
        &mut self,
        record: &RawRecord,
        observation_only: bool,
    ) -> Result<Option<EmissionBatch>, String> {
        let facts = SourceFacts::from_record(self.provider.protocol, record)
            .map_err(|error| error.to_string())?;
        *self.observed_bytes = self.observed_bytes.saturating_add(record.byte_len());
        if self.capture_enabled {
            let len = record.byte_len();
            if len <= self.capture_budget.saturating_sub(*self.captured_bytes) {
                *self.captured_bytes = self.captured_bytes.saturating_add(len);
                self.response_chunks
                    .push(String::from_utf8_lossy(record.raw_bytes()).into_owned());
                if self.trace_items.len() < 200 {
                    let kind = match record.representation() {
                        wr_domain::wire::record::Representation::Ndjson => "ndjson_line",
                        _ => "sse_event",
                    };
                    self.trace_items.push(json!({
                        "kind": kind,
                        "payload": String::from_utf8_lossy(record.content()),
                        "ordinal": self.trace_items.len(),
                    }));
                }
            }
        }
        // Received 始终记录;生命周期门在接受侧拒绝的尾随事件也会留下
        // 违规计数,但不进入已提交投影。确切重放帧只计 Received,不再
        // 喂给转换泵或缓存聚合器(原生 Raw 仍按原字节输出)。
        let accepted = self.observer.observe(&facts);
        if accepted && let Some(aggregator) = self.cache_aggregator.as_deref_mut() {
            for event in facts.events.iter().cloned() {
                let _ = aggregator.push(event);
            }
            if let Some(snapshot) = &facts.snapshot {
                aggregator.merge_snapshot(snapshot.format, &snapshot.value);
            }
        }
        if observation_only {
            return Ok(None);
        }
        if !accepted && !self.native_raw && !self.pump.is_passthrough() {
            return Ok(None);
        }
        let source = SourceSpan {
            start: record.offset(),
            end: record.end_offset(),
        };
        if self.native_raw || self.pump.is_passthrough() {
            // 续写拼接(R5):原生 Raw 续写段的新 message_start 是同一逻辑
            // 回复的重复信封,抑制不下发(与重建路径 begin_continuation 同义);
            // 仍计入 Received。索引由上游 prefill 续写保证稳定(门排除工具/推理)。
            if self.native_raw
                && self.observer.in_continuation()
                && facts.events.iter().any(|event| {
                    matches!(
                        event,
                        wr_domain::semantic::model::SemanticEvent::MessageStart { .. }
                    )
                })
            {
                return Ok(None);
            }
            if accepted {
                self.pump.mark_flow(&facts);
            }
            let frames = match self.aggregate_model {
                // R8:仅协议 model 路径;命中才重序列化该记录(声明记录级重写),
                // 未命中或非法 UTF-8 保持原字节。
                Some(model) => match std::str::from_utf8(record.raw_bytes()) {
                    Ok(text) => {
                        let rewritten = match record.representation() {
                            wr_domain::wire::record::Representation::Ndjson => {
                                wr_domain::protocol::rewrite_ndjson_line_model(text, model)
                            }
                            _ => wr_domain::protocol::rewrite_sse_frame_model(text, model),
                        };
                        if rewritten != text && self.trace_items.len() < 200 {
                            // P3-2:修正是可归因的(规则 ID + 协议路径,不含正文)。
                            self.trace_items.push(json!({
                                "kind": "correction",
                                "rule": wr_domain::protocol::correction::R8_MODEL_ALIAS.id,
                                "path": "protocol_model",
                            }));
                        }
                        vec![rewritten.into_bytes()]
                    }
                    Err(_) => vec![record.raw_bytes().to_vec()],
                },
                None => vec![record.raw_bytes().to_vec()],
            };
            return Ok(Some(EmissionBatch {
                frames,
                has_content: wr_domain::semantic::client_stream::facts_have_content(&facts),
                source: Some(source),
            }));
        }
        let encoded = self.pump.consume_facts(&facts);
        Ok(Some(
            EmissionBatch::from_frames(encoded.frames, encoded.has_content).with_source(source),
        ))
    }
}
