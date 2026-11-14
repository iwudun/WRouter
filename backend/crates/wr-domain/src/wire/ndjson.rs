//! 无损 NDJSON 分帧(方案 20261106_01 §4.2、F-07)。
//!
//! NDJSON 以 **LF** 分行,可选 CRLF;一行一条记录。这里不套用 SSE 的
//! `data:` 语法,也不把 `data: {...}` 误当成 SSE 事件。空行保留为
//! `blank` 记录;非 JSON 行(如 `[DONE]`)保留原文并标记解析错误,
//! 不静默丢弃。EOF 最后一行可以不帶换行,`terminated = false` 但视图
//! 仍可按完整 JSON 行解析。

use std::sync::Arc;

use crate::wire::error::{WireError, WireLimits};
use crate::wire::json::{JsonView, parse_json_view};
use crate::wire::record::{RawRecord, RecordSource, RecordView, Representation};

/// 一行 NDJSON 的解释。
#[derive(Debug, Clone)]
pub struct NdjsonView {
    /// 空行(仅分隔符)。
    pub blank: bool,
    /// JSON 解析视图;空行为 `None`。语法错误时仍是 `Some`,用
    /// [`JsonView::error`] 区分。
    pub json: Option<Arc<JsonView>>,
}

impl NdjsonView {
    /// 是否为 `[DONE]` 哨兵(按去空白后的原文判断)。
    pub fn is_done_sentinel(&self, content: &[u8]) -> bool {
        std::str::from_utf8(content)
            .map(|text| text.trim() == "[DONE]")
            .unwrap_or(false)
    }
}

#[derive(Debug)]
pub struct NdjsonFramer {
    limits: WireLimits,
    source: RecordSource,
    buffer: Vec<u8>,
    scan: usize,
    first_byte_abs: u64,
    record_started_at_ms: i64,
    total_bytes: u64,
    failed: Option<WireError>,
    finished: bool,
}

impl NdjsonFramer {
    pub fn new(limits: WireLimits, source: RecordSource) -> Self {
        Self {
            limits,
            source,
            buffer: Vec::new(),
            scan: 0,
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

    /// EOF:剩余不带换行的末行作为 `terminated = false` 的记录返回。
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
            let view = parse_line(&self.buffer, &self.limits);
            records.push(RawRecord::from_parts(
                raw,
                0..self.buffer.len(),
                self.first_byte_abs,
                started_at,
                Representation::Ndjson,
                self.source.clone(),
                Arc::new(RecordView::Ndjson(Box::new(view))),
                false,
            ));
        }
        Ok(records)
    }

    fn scan_records(&mut self) -> Result<Vec<RawRecord>, WireError> {
        let mut records = Vec::new();
        while let Some(lf) = self.buffer[self.scan..]
            .iter()
            .position(|byte| *byte == b'\n')
        {
            let lf = self.scan + lf;
            let (content_end, term_end) = if lf > 0 && self.buffer[lf - 1] == b'\r' {
                (lf - 1, lf + 1)
            } else {
                (lf, lf + 1)
            };
            if term_end > self.limits.max_record_bytes {
                return Err(WireError::RecordTooLarge {
                    limit: self.limits.max_record_bytes,
                    observed: term_end,
                });
            }
            let raw: Arc<[u8]> = Arc::from(&self.buffer[..term_end]);
            let view = parse_line(&self.buffer[..content_end], &self.limits);
            records.push(RawRecord::from_parts(
                raw,
                0..content_end,
                self.first_byte_abs,
                self.record_started_at_ms,
                Representation::Ndjson,
                self.source.clone(),
                Arc::new(RecordView::Ndjson(Box::new(view))),
                true,
            ));
            self.buffer.drain(..term_end);
            self.first_byte_abs += term_end as u64;
            self.scan = 0;
        }
        if self.buffer.len() > self.limits.max_record_bytes {
            return Err(WireError::RecordTooLarge {
                limit: self.limits.max_record_bytes,
                observed: self.buffer.len(),
            });
        }
        self.scan = self.buffer.len();
        Ok(records)
    }

    fn fail(&mut self, error: WireError) -> WireError {
        self.failed = Some(error.clone());
        error
    }
}

fn parse_line(content: &[u8], limits: &WireLimits) -> NdjsonView {
    if content.is_empty() {
        return NdjsonView {
            blank: true,
            json: None,
        };
    }
    NdjsonView {
        blank: false,
        json: Some(parse_json_view(content, limits)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::record::reassemble_raw;

    fn framer() -> NdjsonFramer {
        NdjsonFramer::new(WireLimits::default(), RecordSource::upstream("gemini"))
    }

    #[test]
    fn splits_lines_and_keeps_terminators() {
        let mut framer = framer();
        let input = b"{\"a\":1}\n{\"b\":2}\r\n";
        let records = framer.feed(input, 7).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(reassemble_raw(&records), input);
        assert_eq!(records[0].content(), b"{\"a\":1}");
        assert_eq!(records[0].terminator(), b"\n");
        assert_eq!(records[1].content(), b"{\"b\":2}");
        assert_eq!(records[1].terminator(), b"\r\n");
        assert!(
            records[0]
                .ndjson()
                .unwrap()
                .json
                .as_ref()
                .unwrap()
                .is_valid()
        );
    }

    #[test]
    fn does_not_treat_sse_data_line_as_event() {
        let mut framer = framer();
        let records = framer.feed(b"data: {\"a\":1}\n", 0).unwrap();
        let view = records[0].ndjson().unwrap();
        assert!(!view.blank);
        assert!(view.json.as_ref().unwrap().error().is_some());
    }

    #[test]
    fn blank_line_is_retained() {
        let mut framer = framer();
        let records = framer.feed(b"\n", 0).unwrap();
        assert_eq!(records.len(), 1);
        assert!(records[0].ndjson().unwrap().blank);
        assert_eq!(records[0].raw_bytes(), b"\n");
    }

    #[test]
    fn eof_without_newline_is_a_complete_json_line() {
        let mut framer = framer();
        assert!(framer.feed(b"{\"a\":1}", 0).unwrap().is_empty());
        let tail = framer.finish(3).unwrap();
        assert_eq!(tail.len(), 1);
        assert!(!tail[0].is_terminated());
        assert!(tail[0].ndjson().unwrap().json.as_ref().unwrap().is_valid());
    }

    #[test]
    fn crlf_split_across_chunks() {
        let input = b"{\"a\":1}\r\n{\"b\":2}\r\n";
        let whole = {
            let mut f = framer();
            let mut out = f.feed(input, 0).unwrap();
            out.extend(f.finish(0).unwrap());
            out
        };
        for split in 0..input.len() {
            let mut f = framer();
            let mut out = f.feed(&input[..split], 0).unwrap();
            out.extend(f.feed(&input[split..], 0).unwrap());
            out.extend(f.finish(0).unwrap());
            assert_eq!(reassemble_raw(&out), input);
            assert_eq!(out.len(), whole.len(), "split at {split}");
            for (left, right) in whole.iter().zip(out.iter()) {
                assert_eq!(left.content(), right.content(), "split at {split}");
            }
        }
    }

    #[test]
    fn record_budget_is_sticky() {
        let mut f = NdjsonFramer::new(
            WireLimits::default().with_max_record_bytes(4),
            RecordSource::upstream("x"),
        );
        assert!(matches!(
            f.feed(b"{\"abcdef\":1}", 0),
            Err(WireError::RecordTooLarge { .. })
        ));
        assert!(matches!(
            f.feed(b"\n", 0),
            Err(WireError::RecordTooLarge { .. })
        ));
    }
}
