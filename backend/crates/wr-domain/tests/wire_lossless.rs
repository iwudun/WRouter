//! P1 无损基座验收:C2/C3 基础层、C8/C13 适用子集。
//!
//! 样本位置:`tests/fixtures/wire/`。这些样本只验证分帧/记录模型,
//! 不经过生产转发链路,也不宣称全链 ByteExact 已完成。

use wr_domain::wire::RecordFramer;
use wr_domain::wire::error::{WireError, WireLimits};
use wr_domain::wire::ndjson::NdjsonFramer;
use wr_domain::wire::record::{RawRecord, RecordSource, Representation, reassemble_raw};
use wr_domain::wire::sse::SseFramer;

const SSE_MIXED: &[u8] = include_bytes!("fixtures/wire/sse_mixed.bin");
const NDJSON_MIXED: &[u8] = include_bytes!("fixtures/wire/ndjson_mixed.txt");
const JSON_DUPLICATE: &[u8] = include_bytes!("fixtures/wire/json_duplicate.json");

fn sse_source() -> RecordSource {
    RecordSource::upstream("openai_chat")
}

fn collect_sse_with_chunks(input: &[u8], sizes: &[usize]) -> Vec<RawRecord> {
    let mut framer = SseFramer::new(WireLimits::default(), sse_source());
    let mut records = Vec::new();
    let mut position = 0;
    let mut index = 0;
    while position < input.len() {
        let size = sizes[index % sizes.len()]
            .max(1)
            .min(input.len() - position);
        records.extend(
            framer
                .feed(&input[position..position + size], 1_000)
                .expect("fixture must stay within budget"),
        );
        position += size;
        index += 1;
    }
    records.extend(framer.finish(1_000).unwrap());
    records
}

fn assert_same_sse_records(left: &[RawRecord], right: &[RawRecord], context: &str) {
    assert_eq!(left.len(), right.len(), "{context}: record count");
    for (index, (a, b)) in left.iter().zip(right.iter()).enumerate() {
        assert_eq!(
            a.raw_bytes(),
            b.raw_bytes(),
            "{context}: record {index} raw"
        );
        assert_eq!(a.sse(), b.sse(), "{context}: record {index} view");
        assert_eq!(
            a.is_terminated(),
            b.is_terminated(),
            "{context}: record {index} termination"
        );
        assert_eq!(a.offset(), b.offset(), "{context}: record {index} offset");
    }
}

#[test]
fn sse_fixture_reassembles_byte_exactly_and_covers_input() {
    let records = collect_sse_with_chunks(SSE_MIXED, &[usize::MAX]);
    assert_eq!(reassemble_raw(&records), SSE_MIXED);

    let mut expected_offset = 0u64;
    for record in &records {
        assert_eq!(record.offset(), expected_offset);
        assert_eq!(
            record.content().len() + record.terminator().len(),
            record.byte_len()
        );
        expected_offset += record.byte_len() as u64;
    }
    assert_eq!(expected_offset, SSE_MIXED.len() as u64);
}

#[test]
fn sse_arbitrary_chunkings_are_invariant() {
    let whole = collect_sse_with_chunks(SSE_MIXED, &[usize::MAX]);
    for size in 1..=SSE_MIXED.len() {
        let chunked = collect_sse_with_chunks(SSE_MIXED, &[size]);
        assert_same_sse_records(&whole, &chunked, &format!("fixed chunk {size}"));
        assert_eq!(reassemble_raw(&chunked), SSE_MIXED);
    }
    for sizes in [
        &[1usize, 2, 3, 5, 7, 11][..],
        &[13, 1, 64, 2][..],
        &[2, 2, 2, 3, 1][..],
    ] {
        let chunked = collect_sse_with_chunks(SSE_MIXED, sizes);
        assert_same_sse_records(&whole, &chunked, &format!("pattern {sizes:?}"));
        assert_eq!(reassemble_raw(&chunked), SSE_MIXED);
    }
}

fn next_random(state: &mut u64) -> u64 {
    let mut value = *state;
    value ^= value << 13;
    value ^= value >> 7;
    value ^= value << 17;
    *state = value;
    value
}

#[test]
fn sse_random_chunkings_are_invariant() {
    let whole = collect_sse_with_chunks(SSE_MIXED, &[usize::MAX]);
    let mut state = 0x5DEE_CE66_D1CE_B00F_u64;
    for round in 0..200 {
        let mut framer = SseFramer::new(WireLimits::default(), sse_source());
        let mut records = Vec::new();
        let mut position = 0;
        while position < SSE_MIXED.len() {
            let size = (next_random(&mut state) % 10) as usize;
            let end = (position + size).min(SSE_MIXED.len());
            records.extend(framer.feed(&SSE_MIXED[position..end], 1_000).unwrap());
            position = end;
        }
        records.extend(framer.finish(1_000).unwrap());
        assert_same_sse_records(&whole, &records, &format!("random round {round}"));
        assert_eq!(reassemble_raw(&records), SSE_MIXED);
    }
}

#[test]
fn ndjson_random_chunkings_are_invariant() {
    let mut state = 0x1234_5678_9ABC_DEF0_u64;
    let whole = {
        let mut framer = NdjsonFramer::new(WireLimits::default(), RecordSource::upstream("gemini"));
        let mut records = framer.feed(NDJSON_MIXED, 0).unwrap();
        records.extend(framer.finish(0).unwrap());
        records
    };
    for round in 0..200 {
        let mut framer = NdjsonFramer::new(WireLimits::default(), RecordSource::upstream("gemini"));
        let mut records = Vec::new();
        let mut position = 0;
        while position < NDJSON_MIXED.len() {
            let size = (next_random(&mut state) % 7) as usize;
            let end = (position + size).min(NDJSON_MIXED.len());
            records.extend(framer.feed(&NDJSON_MIXED[position..end], 0).unwrap());
            position = end;
        }
        records.extend(framer.finish(0).unwrap());
        assert_eq!(reassemble_raw(&records), NDJSON_MIXED, "round {round}");
        assert_eq!(records.len(), whole.len(), "round {round}");
        for (index, (left, right)) in whole.iter().zip(records.iter()).enumerate() {
            assert_eq!(
                left.content(),
                right.content(),
                "round {round} record {index}"
            );
            assert_eq!(
                left.ndjson().unwrap().blank,
                right.ndjson().unwrap().blank,
                "round {round} record {index}"
            );
        }
    }
}

#[test]
fn sse_fixture_preserves_comments_unknown_fields_and_identity() {
    let records = collect_sse_with_chunks(SSE_MIXED, &[5]);
    assert_eq!(records.len(), 4);

    let ping = records[0].sse().unwrap();
    assert!(ping.had_bom);
    assert_eq!(ping.event.as_deref(), Some("vendor.ping"));
    assert_eq!(ping.id.as_deref(), Some("42"));
    assert_eq!(ping.retry_ms, Some(1500));
    assert_eq!(ping.data, " one\ntwo");
    assert_eq!(ping.comments.len(), 1);
    assert_eq!(
        ping.fields
            .iter()
            .find(|field| field.name == "x-unknown")
            .map(|field| field.value.as_str()),
        Some("a:b")
    );
    assert!(ping.dispatched);
    assert_eq!(records[0].representation(), Representation::Sse);

    let message = records[1].sse().unwrap();
    assert_eq!(message.event.as_deref(), Some("message"));
    assert_eq!(message.json(), Some(serde_json::json!({"ok": true})));

    let done = records[2].sse().unwrap();
    assert!(done.is_done_sentinel());

    let tail = records[3].sse().unwrap();
    assert!(tail.incomplete);
    assert!(!tail.dispatched);
    assert!(!records[3].is_terminated());
    assert_eq!(records[3].content(), b"data: tail-without-blank");
}

#[test]
fn sse_framer_is_object_safe_and_sticky_on_budget() {
    let mut framer: Box<dyn RecordFramer> =
        Box::new(SseFramer::new(WireLimits::default(), sse_source()));
    assert_eq!(framer.representation(), Representation::Sse);
    let records = framer.feed(b"data: 1\n\n", 1).unwrap();
    assert_eq!(records.len(), 1);
    assert!(framer.finish(1).unwrap().is_empty());

    let mut framer = SseFramer::new(WireLimits::default().with_max_total_bytes(4), sse_source());
    assert!(matches!(
        framer.feed(b"data:", 0),
        Err(WireError::TotalBytesExceeded { .. })
    ));
    assert!(matches!(
        framer.feed(b"x", 0),
        Err(WireError::TotalBytesExceeded { .. })
    ));
}

#[test]
fn ndjson_fixture_is_not_sse_and_keeps_tail() {
    let mut framer = NdjsonFramer::new(WireLimits::default(), RecordSource::upstream("gemini"));
    let records = framer.feed(NDJSON_MIXED, 7).unwrap();
    let tail = framer.finish(7).unwrap();
    let mut all = records;
    all.extend(tail);

    assert_eq!(reassemble_raw(&all), NDJSON_MIXED);
    assert_eq!(all.len(), 4);
    assert!(all.iter().all(|record| record.sse().is_none()));
    assert_eq!(all[0].representation(), Representation::Ndjson);
    assert!(all[1].ndjson().unwrap().blank);
    assert!(
        all[2]
            .ndjson()
            .unwrap()
            .json
            .as_ref()
            .unwrap()
            .structure_exact_safe()
    );
    assert!(!all[3].is_terminated());
    assert!(all[3].ndjson().unwrap().is_done_sentinel(all[3].content()));
    assert!(
        all[3]
            .ndjson()
            .unwrap()
            .json
            .as_ref()
            .unwrap()
            .error()
            .is_some()
    );
}

#[test]
fn ndjson_arbitrary_chunkings_are_invariant() {
    let run = |sizes: &[usize]| {
        let mut framer = NdjsonFramer::new(WireLimits::default(), RecordSource::upstream("gemini"));
        let mut records = Vec::new();
        let mut position = 0;
        let mut index = 0;
        while position < NDJSON_MIXED.len() {
            let size = sizes[index % sizes.len()]
                .max(1)
                .min(NDJSON_MIXED.len() - position);
            records.extend(
                framer
                    .feed(&NDJSON_MIXED[position..position + size], 7)
                    .unwrap(),
            );
            position += size;
            index += 1;
        }
        records.extend(framer.finish(7).unwrap());
        records
    };
    let whole = run(&[usize::MAX]);
    for size in 1..=NDJSON_MIXED.len() {
        let chunked = run(&[size]);
        assert_eq!(reassemble_raw(&chunked), NDJSON_MIXED);
        assert_eq!(whole.len(), chunked.len(), "chunk {size}");
        for (a, b) in whole.iter().zip(chunked.iter()) {
            assert_eq!(a.content(), b.content(), "chunk {size}");
            assert_eq!(
                a.ndjson().unwrap().blank,
                b.ndjson().unwrap().blank,
                "chunk {size}"
            );
        }
    }
}

#[test]
fn json_view_reports_duplicates_numbers_and_raw_length() {
    let view = wr_domain::wire::json::parse_json_view(JSON_DUPLICATE, &WireLimits::default());
    assert_eq!(view.byte_len(), JSON_DUPLICATE.len());
    assert!(view.is_valid());
    assert!(!view.structure_exact_safe());
    assert_eq!(view.duplicate_keys().len(), 1);
    assert_eq!(view.duplicate_keys()[0].path, "$.model");
    assert_eq!(view.number_risks().len(), 2);
    assert_eq!(view.number_risks()[0].path, "$.n");
    assert_eq!(view.number_risks()[1].path, "$.e");

    let value = view.value().unwrap();
    assert_eq!(value["model"], serde_json::json!("b"));
    assert_eq!(value["nested"]["model"], serde_json::json!("keep"));
}

#[test]
fn repeated_parses_have_one_json_interpretation() {
    let first = wr_domain::wire::json::parse_json_view(JSON_DUPLICATE, &WireLimits::default());
    let second = wr_domain::wire::json::parse_json_view(JSON_DUPLICATE, &WireLimits::default());
    assert_eq!(first.value().unwrap(), second.value().unwrap());
    assert_eq!(
        first.duplicate_keys().len(),
        1,
        "重复键不能因消费者不同而消失"
    );
}
