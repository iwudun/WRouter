//! 无损 SSE 分帧(方案 20261106_01 §4.1、P1-3/P1-4)。
//!
//! 与旧 [`crate::sse`] 的关键差异:
//! - 以字节而非规范化后的 UTF-8 字符串分帧;CR、CRLF、LF 及跨 chunk 的
//!   CRLF 都按原样保留;
//! - 注释、`id:`、`retry:`、未知字段和非应用事件记录全部保留在原文与
//!   视图中,不丢弃;
//! - `raw` 包含记录分隔符,ByteExact 可直接重组;
//! - EOF 残片作为 `terminated = false` 的独立记录返回,`dispatched`
//!   恒为 false,不伪装成完整事件;
//! - 多行 `data:` 只按规范移除一个前导空格,再以 LF 连接;
//! - 无效 UTF-8 不阻断分帧,视图标记 `utf8_valid = false`,原文权威。
//!
//! 依据 WHATWG Server-sent events §9.2.5–9.2.6。

use std::ops::Range;
use std::sync::Arc;

use serde_json::Value;

use crate::wire::error::{WireError, WireLimits};
use crate::wire::record::{RawRecord, RecordSource, RecordView, Representation};

/// 一条 SSE 字段行(含注释以外的所有行)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseFieldView {
    /// 字段名;无冒号行按规范把整行视为字段名、值为空。
    pub name: String,
    /// 字段值(仅移除一个前导空格)。
    pub value: String,
    /// 整行在记录内容中的范围(含行终止符)。
    pub span: Range<usize>,
}

/// SSE 记录解析视图。
#[derive(Debug, Clone, PartialEq)]
pub struct SseView {
    /// 最后一次 `event:` 字段;未携带为 `None`。
    pub event: Option<String>,
    /// 最后一次合法 `id:` 字段(含 NUL 的值被忽略)。
    pub id: Option<String>,
    /// 最后一次合法 `retry:` 字段(纯 ASCII 数字)。
    pub retry_ms: Option<u64>,
    /// 按规范拼接后的 data(不含结尾 LF)。
    pub data: String,
    /// `data:` 行数。
    pub data_lines: usize,
    /// 所有非注释字段(含未知字段)。
    pub fields: Vec<SseFieldView>,
    /// 注释行在内容中的范围。
    pub comments: Vec<Range<usize>>,
    /// 是否在空行处分派了应用事件(必须同时有 data 且记录完整)。
    pub dispatched: bool,
    /// 是否剥离了流首 BOM(原文仍保留)。
    pub had_bom: bool,
    /// 字段名/值/data 是否全部为合法 UTF-8。无效时视图使用替换字符,
    /// 原始字节仍是权威。
    pub utf8_valid: bool,
    /// EOF 残片(无空行终止)。
    pub incomplete: bool,
}

impl SseView {
    pub fn is_done_sentinel(&self) -> bool {
        self.data.trim() == "[DONE]"
    }

    /// 尽力 JSON 解析(与原 `SseEvent::json` 语义一致):
    /// 空串/`[DONE]`/非 JSON 返回 `None`;顶层非对象包成 `{"value": ...}`。
    pub fn json(&self) -> Option<Value> {
        let text = self.data.trim();
        if text.is_empty() || text == "[DONE]" {
            return None;
        }
        match serde_json::from_str::<Value>(text) {
            Ok(value @ Value::Object(_)) => Some(value),
            Ok(other) => Some(serde_json::json!({ "value": other })),
            Err(_) => None,
        }
    }

    /// `event:` 值,兼容旧调用习惯。
    pub fn event_name(&self) -> Option<&str> {
        self.event.as_deref()
    }
}

fn parse_sse_content(content: &[u8], record_offset: u64, incomplete: bool) -> SseView {
    let had_bom = record_offset == 0 && content.starts_with(&[0xEF, 0xBB, 0xBF]);
    let start = if had_bom { 3 } else { 0 };

    let mut event = None;
    let mut id = None;
    let mut retry_ms = None;
    let mut data_lines: Vec<String> = Vec::new();
    let mut fields = Vec::new();
    let mut comments = Vec::new();
    let mut utf8_valid = true;

    let mut pos = start;
    while pos < content.len() {
        let line_start = pos;
        let (line_end, term_end) = next_line_ending(content, pos);
        let line = &content[line_start..line_end];

        if line.first() == Some(&b':') {
            comments.push(line_start..term_end);
            pos = term_end;
            continue;
        }

        let (name_raw, value_raw) = match line.iter().position(|byte| *byte == b':') {
            Some(colon) => (&line[..colon], &line[colon + 1..]),
            None => (line, &[][..]),
        };
        let value_raw = value_raw.strip_prefix(b" ").unwrap_or(value_raw);

        let name = match std::str::from_utf8(name_raw) {
            Ok(name) => name.to_owned(),
            Err(_) => {
                utf8_valid = false;
                String::from_utf8_lossy(name_raw).into_owned()
            }
        };
        let value = match std::str::from_utf8(value_raw) {
            Ok(value) => value.to_owned(),
            Err(_) => {
                utf8_valid = false;
                String::from_utf8_lossy(value_raw).into_owned()
            }
        };

        match name_raw {
            b"data" => data_lines.push(value.clone()),
            b"event" => event = Some(value.clone()),
            b"id" => {
                if !value_raw.contains(&0) {
                    id = Some(value.clone());
                }
            }
            b"retry" if !value_raw.is_empty() && value_raw.iter().all(u8::is_ascii_digit) => {
                retry_ms = value.parse::<u64>().ok();
            }
            _ => {}
        }

        fields.push(SseFieldView {
            name,
            value,
            span: line_start..term_end,
        });
        pos = term_end;
    }

    let data = data_lines.join("\n");
    SseView {
        event,
        id,
        retry_ms,
        data,
        data_lines: data_lines.len(),
        fields,
        comments,
        dispatched: !incomplete && !data_lines.is_empty(),
        had_bom,
        utf8_valid,
        incomplete,
    }
}

/// 返回 `(行内容结束, 行终止符结束)`;行终止符为 CRLF/CR/LF。
/// 调用方保证 `pos < content.len()`。
fn next_line_ending(content: &[u8], pos: usize) -> (usize, usize) {
    let mut cursor = pos;
    while cursor < content.len() {
        match content[cursor] {
            b'\n' => return (cursor, cursor + 1),
            b'\r' => {
                let term_end = if content.get(cursor + 1) == Some(&b'\n') {
                    cursor + 2
                } else {
                    cursor + 1
                };
                return (cursor, term_end);
            }
            _ => cursor += 1,
        }
    }
    (content.len(), content.len())
}

/// 增量 SSE framer。`feed` 可接收任意字节切分;记录边界只由空行决定。
#[derive(Debug)]
pub struct SseFramer {
    limits: WireLimits,
    source: RecordSource,
    buffer: Vec<u8>,
    /// 下一条待检查字节(相对 buffer)。
    scan: usize,
    /// 当前行起点(相对 buffer)。
    line_start: usize,
    /// buffer[0] 在输入流中的绝对偏移。
    first_byte_abs: u64,
    /// 当前记录第一块数据的观察时间(由调用方时钟注入)。
    record_started_at_ms: i64,
    total_bytes: u64,
    failed: Option<WireError>,
    finished: bool,
}

impl SseFramer {
    pub fn new(limits: WireLimits, source: RecordSource) -> Self {
        Self {
            limits,
            source,
            buffer: Vec::new(),
            scan: 0,
            line_start: 0,
            first_byte_abs: 0,
            record_started_at_ms: 0,
            total_bytes: 0,
            failed: None,
            finished: false,
        }
    }

    pub fn feed(&mut self, chunk: &[u8], received_at_ms: i64) -> Result<Vec<RawRecord>, WireError> {
        if let Some(error) = &self.failed {
            return Err(error.clone());
        }
        if self.finished {
            return Err(WireError::FramerFinished);
        }
        if chunk.is_empty() {
            return Ok(Vec::new());
        }

        self.total_bytes = self.total_bytes.saturating_add(chunk.len() as u64);
        if let Some(limit) = self.limits.max_total_bytes
            && self.total_bytes > limit
        {
            return Err(self.fail(WireError::TotalBytesExceeded {
                limit,
                observed: self.total_bytes,
            }));
        }
        if self.buffer.is_empty() {
            self.record_started_at_ms = received_at_ms;
        }
        self.buffer.extend_from_slice(chunk);
        match self.scan_records() {
            Ok(records) => Ok(records),
            Err(error) => Err(self.fail(error)),
        }
    }

    /// 上游 EOF:先产出空行终止的完整记录,再把剩余残片作为一条
    /// `terminated = false` 的记录返回;残片永不 `dispatched`。
    pub fn finish(&mut self, _received_at_ms: i64) -> Result<Vec<RawRecord>, WireError> {
        if let Some(error) = &self.failed {
            return Err(error.clone());
        }
        if self.finished {
            return Ok(Vec::new());
        }
        self.finished = true;

        let mut records = match self.scan_records() {
            Ok(records) => records,
            Err(error) => return Err(self.fail(error)),
        };

        if !self.buffer.is_empty() {
            if self.buffer.len() > self.limits.max_tail_bytes {
                return Err(self.fail(WireError::TailTooLarge {
                    limit: self.limits.max_tail_bytes,
                    observed: self.buffer.len(),
                }));
            }
            if self.buffer.len() > self.limits.max_record_bytes {
                return Err(self.fail(WireError::RecordTooLarge {
                    limit: self.limits.max_record_bytes,
                    observed: self.buffer.len(),
                }));
            }
            let started_at = self.record_started_at_ms;
            let raw: Arc<[u8]> = Arc::from(self.buffer.as_slice());
            let content_len = self.buffer.len();
            let view = parse_sse_content(&self.buffer, self.first_byte_abs, true);
            records.push(RawRecord::from_parts(
                raw,
                0..content_len,
                self.first_byte_abs,
                started_at,
                Representation::Sse,
                self.source.clone(),
                Arc::new(RecordView::Sse(Box::new(view))),
                false,
            ));
            self.first_byte_abs += self.buffer.len() as u64;
            self.buffer.clear();
        }
        Ok(records)
    }

    fn scan_records(&mut self) -> Result<Vec<RawRecord>, WireError> {
        let mut records = Vec::new();
        while let Some((content_end, term_end)) = self.next_blank_line() {
            if term_end > self.limits.max_record_bytes {
                return Err(WireError::RecordTooLarge {
                    limit: self.limits.max_record_bytes,
                    observed: term_end,
                });
            }
            let started_at = self.record_started_at_ms;
            let raw: Arc<[u8]> = Arc::from(&self.buffer[..term_end]);
            let view = parse_sse_content(&self.buffer[..content_end], self.first_byte_abs, false);
            records.push(RawRecord::from_parts(
                raw,
                0..content_end,
                self.first_byte_abs,
                started_at,
                Representation::Sse,
                self.source.clone(),
                Arc::new(RecordView::Sse(Box::new(view))),
                true,
            ));
            self.buffer.drain(..term_end);
            self.first_byte_abs += term_end as u64;
            self.scan = 0;
            self.line_start = 0;
            self.record_started_at_ms = started_at;
        }
        if self.buffer.len() > self.limits.max_record_bytes {
            return Err(WireError::RecordTooLarge {
                limit: self.limits.max_record_bytes,
                observed: self.buffer.len(),
            });
        }
        Ok(records)
    }

    /// 查找下一个空行分隔符,返回 `(内容结束, 整条记录结束)`。
    fn next_blank_line(&mut self) -> Option<(usize, usize)> {
        let len = self.buffer.len();
        while self.scan < len {
            let byte = self.buffer[self.scan];
            let (line_end, term_end) = match byte {
                b'\n' => (self.scan, self.scan + 1),
                b'\r' => {
                    if self.scan + 1 >= len {
                        // 可能是跨 chunk 的 CRLF,等待下一块。
                        return None;
                    }
                    if self.buffer[self.scan + 1] == b'\n' {
                        (self.scan, self.scan + 2)
                    } else {
                        (self.scan, self.scan + 1)
                    }
                }
                _ => {
                    self.scan += 1;
                    continue;
                }
            };
            if line_end == self.line_start {
                return Some((self.line_start, term_end));
            }
            self.line_start = term_end;
            self.scan = term_end;
        }
        None
    }

    fn fail(&mut self, error: WireError) -> WireError {
        self.failed = Some(error.clone());
        error
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::record::reassemble_raw;

    fn framer() -> SseFramer {
        SseFramer::new(WireLimits::default(), RecordSource::upstream("openai_chat"))
    }

    #[test]
    fn preserves_comments_fields_and_terminators() {
        let mut framer = framer();
        let input = b": keepalive\r\nevent: ping\r\nid: 7\r\nretry: 1500\r\nx-unknown: v\r\ndata: {\"a\":1}\r\n\r\n";
        let records = framer.feed(input, 10).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(reassemble_raw(&records), input);
        let view = records[0].sse().unwrap();
        assert_eq!(view.event.as_deref(), Some("ping"));
        assert_eq!(view.id.as_deref(), Some("7"));
        assert_eq!(view.retry_ms, Some(1500));
        assert_eq!(view.data, "{\"a\":1}");
        assert!(view.dispatched);
        assert_eq!(view.comments.len(), 1);
        assert_eq!(view.fields.len(), 5);
        assert_eq!(records[0].terminator(), b"\r\n");
    }

    #[test]
    fn multiline_data_strips_one_space_only() {
        let mut framer = framer();
        let records = framer.feed(b"data:  two\ndata:\n\n", 0).unwrap();
        let view = records[0].sse().unwrap();
        assert_eq!(view.data, " two\n");
        assert_eq!(view.data_lines, 2);
    }

    #[test]
    fn empty_data_field_dispatches_empty_event() {
        let mut framer = framer();
        let records = framer.feed(b"data:\n\n", 0).unwrap();
        let view = records[0].sse().unwrap();
        assert!(view.dispatched);
        assert_eq!(view.data, "");
    }

    #[test]
    fn comment_only_record_is_retained_but_not_dispatched() {
        let mut framer = framer();
        let records = framer.feed(b": ping\n\n", 0).unwrap();
        assert_eq!(records.len(), 1);
        assert!(!records[0].sse().unwrap().dispatched);
        assert_eq!(reassemble_raw(&records), b": ping\n\n");
    }

    #[test]
    fn crlf_split_across_chunks_is_identical() {
        let input = b"event: a\r\ndata: 1\r\n\r\n";
        let whole = {
            let mut f = framer();
            let mut records = f.feed(input, 0).unwrap();
            records.extend(f.finish(0).unwrap());
            records
        };
        for split in 0..input.len() {
            let mut f = framer();
            let mut records = f.feed(&input[..split], 0).unwrap();
            records.extend(f.feed(&input[split..], 0).unwrap());
            records.extend(f.finish(0).unwrap());
            assert_eq!(
                whole.len(),
                records.len(),
                "split at {split} produced different record count"
            );
            for (left, right) in whole.iter().zip(records.iter()) {
                assert_eq!(left.raw_bytes(), right.raw_bytes(), "split at {split}");
                assert_eq!(left.sse(), right.sse(), "split at {split}");
            }
        }
    }

    #[test]
    fn lone_cr_and_mixed_line_endings_are_terminators() {
        let mut framer = framer();
        let input = b"event: a\rdata: 1\r\rdata: 2\n\ndata: 3\r\n\r\n";
        let mut records = framer.feed(input, 0).unwrap();
        records.extend(framer.finish(0).unwrap());
        assert_eq!(records.len(), 3);
        assert_eq!(reassemble_raw(&records), input);
        assert_eq!(records[0].sse().unwrap().data, "1");
        assert_eq!(records[1].sse().unwrap().data, "2");
        assert_eq!(records[2].sse().unwrap().data, "3");
    }

    #[test]
    fn utf8_multibyte_split_across_chunks_is_not_corrupted() {
        let input = "data: 你好\n\n".as_bytes();
        for split in 0..=input.len() {
            let mut framer = framer();
            let mut records = framer.feed(&input[..split], 0).unwrap();
            records.extend(framer.feed(&input[split..], 0).unwrap());
            records.extend(framer.finish(0).unwrap());
            assert_eq!(records.len(), 1, "split at {split}");
            let view = records[0].sse().unwrap();
            assert!(view.utf8_valid, "split at {split}");
            assert_eq!(view.data, "你好", "split at {split}");
        }
    }

    #[test]
    fn eof_tail_is_not_dispatched() {
        let mut framer = framer();
        assert!(framer.feed(b"data: partial\n", 0).unwrap().is_empty());
        let tail = framer.finish(5).unwrap();
        assert_eq!(tail.len(), 1);
        let view = tail[0].sse().unwrap();
        assert!(!view.dispatched);
        assert!(view.incomplete);
        assert!(!tail[0].is_terminated());
        assert_eq!(tail[0].raw_bytes(), b"data: partial\n");
    }

    #[test]
    fn bom_is_preserved_but_not_part_of_event_name() {
        let mut framer = framer();
        let mut records = framer.feed(b"\xEF\xBB\xBFdata: 1\n\n", 0).unwrap();
        records.extend(framer.finish(0).unwrap());
        let view = records[0].sse().unwrap();
        assert!(view.had_bom);
        assert_eq!(view.data, "1");
        assert_eq!(records[0].raw_bytes(), b"\xEF\xBB\xBFdata: 1\n\n");
    }

    #[test]
    fn invalid_utf8_is_flagged_not_dropped() {
        let mut framer = framer();
        let records = framer.feed(b"data: \xFF\n\n", 0).unwrap();
        let view = records[0].sse().unwrap();
        assert!(!view.utf8_valid);
        assert!(!view.dispatched || view.data.contains('\u{FFFD}'));
        assert_eq!(records[0].raw_bytes(), b"data: \xFF\n\n");
    }

    #[test]
    fn record_budget_is_sticky() {
        let limits = WireLimits::default().with_max_record_bytes(8);
        let mut framer = SseFramer::new(limits, RecordSource::upstream("x"));
        assert!(matches!(
            framer.feed(b"data: 12345", 0),
            Err(WireError::RecordTooLarge { .. })
        ));
        assert!(matches!(
            framer.feed(b"\n\n", 0),
            Err(WireError::RecordTooLarge { .. })
        ));
    }

    #[test]
    fn offsets_are_absolute() {
        let mut framer = framer();
        let first = framer.feed(b"data: 1\n\n", 0).unwrap();
        let second = framer.feed(b"data: 2\n\n", 0).unwrap();
        assert_eq!(first[0].offset(), 0);
        assert_eq!(second[0].offset(), 9);
        assert_eq!(second[0].end_offset(), 18);
    }
}
