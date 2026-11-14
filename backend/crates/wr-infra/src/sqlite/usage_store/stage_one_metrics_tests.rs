use super::*;
use serde_json::json;

fn payload(value: serde_json::Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap()
}

fn sums(value: serde_json::Value, is_error: bool) -> UsageSums {
    usage_sums_from_item(&payload(value), is_error)
}

// --- A-2 桶级 avgDurationMs / cacheRatio 不再恒 0 ----------------------

/// 桶级耗时与缓存命中率取真实值(修前恒为 int 0)。
#[test]
fn bucket_rows_report_real_avg_duration_and_cache_ratio() {
    let mut bucket = SeriesRow::new("2026-11-04T00:00:00+00:00");
    bucket.total.add_sums(
        1,
        &sums(
            json!({
                "output_tokens": 100,
                "input_tokens": 1_000,
                "cache_read_tokens": 250,
                "duration_ms": 2_000.0,
            }),
            false,
        ),
    );
    let json = bucket.to_json();
    assert!(
        json["avgDurationMs"].as_f64().unwrap() > 0.0,
        "桶级 avgDurationMs 不得恒 0: {json}"
    );
    assert!(
        json["cacheRatio"].as_f64().unwrap() > 0.0,
        "桶级 cacheRatio 不得恒 0: {json}"
    );
    // 与窗口级 totals 同一口径:250 / 1000 = 0.25。
    assert!((json["cacheRatio"].as_f64().unwrap() - 0.25).abs() < 1e-9);
    assert!((json["avgDurationMs"].as_f64().unwrap() - 2_000.0).abs() < 1e-9);
}

/// 补零桶(request_count = 0)不得除零 / 出 NaN。
#[test]
fn empty_bucket_avg_duration_is_zero_not_nan() {
    let bucket = SeriesRow::new("2026-11-04T00:00:00+00:00");
    let json = bucket.to_json();
    assert_eq!(json["avgDurationMs"].as_f64().unwrap(), 0.0);
    assert_eq!(json["cacheRatio"].as_f64().unwrap(), 0.0);
}

// --- 旧行解码分子 = 全输出 token(20261108_01 §7 断代) ------------------

/// 旧行(无 answer_end):沿用解码窗口口径,分子用全输出。新行的隐藏
/// 思考条件剥离见 `speed_tests`。
#[test]
fn legacy_openai_rows_use_full_output() {
    let sums = sums(
        json!({
            "output_tokens": 900,
            "reasoning_tokens": 300,
            "duration_ms": 10_000.0,
            "first_byte_ms": 1_000.0,
            "provider_protocol": "openai",
        }),
        false,
    );
    assert_eq!(sums.decode_tokens, 900, "全输出口径:分子与分母都含思考");
    assert_eq!(sums.output_tokens, 900, "落库的 output_tokens 不受口径影响");
}

/// Responses 旧行同上(线格式不同,断代口径一致)。
#[test]
fn legacy_openai_responses_rows_use_full_output() {
    let sums = sums(
        json!({
            "output_tokens": 900,
            "reasoning_tokens": 300,
            "duration_ms": 10_000.0,
            "first_byte_ms": 1_000.0,
            "provider_protocol": "openai_responses",
        }),
        false,
    );
    assert_eq!(sums.decode_tokens, 900);
}

/// Gemini 旧行:output 不含 thoughts,解码窗口口径仍取全输出。
#[test]
fn legacy_gemini_rows_use_full_output() {
    let sums = sums(
        json!({
            "output_tokens": 900,
            "reasoning_tokens": 300,
            "duration_ms": 10_000.0,
            "first_byte_ms": 1_000.0,
            "provider_protocol": "gemini",
        }),
        false,
    );
    assert_eq!(sums.decode_tokens, 900);
}

/// Anthropic 旧行:思考计入 output 且无独立键 → 分子维持原值。
#[test]
fn legacy_anthropic_numerator_unchanged() {
    let sums = sums(
        json!({
            "output_tokens": 900,
            "duration_ms": 10_000.0,
            "first_byte_ms": 1_000.0,
            "provider_protocol": "anthropic",
        }),
        false,
    );
    assert_eq!(sums.decode_tokens, 900, "Anthropic 维持原口径");
}

/// 旧行缺失 `provider_protocol`:保守维持全输出,不做条件剥离。
#[test]
fn legacy_rows_without_provider_protocol_use_full_output() {
    let sums = sums(
        json!({
            "output_tokens": 900,
            "reasoning_tokens": 300,
            "duration_ms": 10_000.0,
            "first_byte_ms": 1_000.0,
        }),
        false,
    );
    assert_eq!(sums.decode_tokens, 900);
}

/// 旧行推理 token 超过 output 的异常上报:全输出口径不受影响,不得为负。
#[test]
fn legacy_reasoning_above_output_keeps_full_numerator() {
    let sums = sums(
        json!({
            "output_tokens": 100,
            "reasoning_tokens": 150,
            "duration_ms": 10_000.0,
            "first_byte_ms": 1_000.0,
            "provider_protocol": "openai",
        }),
        false,
    );
    assert_eq!(sums.decode_tokens, 100);
}

// --- B-3 失败 / 中断请求不进 TTFT 均值 ---------------------------------

/// 502(带 first_byte_ms、无 token)不计入首字延迟;成功请求照计。
#[test]
fn failed_rows_are_excluded_from_first_byte_average() {
    let failed = sums(
        json!({
            "status": 502,
            "first_byte_ms": 8_000.0,
            "duration_ms": 8_000.0,
        }),
        true,
    );
    assert_eq!(failed.first_byte_count, 0, "失败行不进 TTFT 样本集");
    assert_eq!(failed.first_byte_ms_sum, 0.0, "失败行的首字不进分子");

    let ok = sums(
        json!({
            "status": 200,
            "first_byte_ms": 1_000.0,
            "duration_ms": 10_000.0,
        }),
        false,
    );
    assert_eq!(ok.first_byte_count, 1);
    assert_eq!(ok.first_byte_ms_sum, 1_000.0);
}

/// 499(客户端断开)同样排除 —— 它也带 first_byte_ms。
#[test]
fn client_disconnect_rows_are_excluded_from_first_byte_average() {
    let sums = sums(
        json!({
            "status": 499,
            "first_byte_ms": 5_000.0,
            "duration_ms": 6_000.0,
        }),
        true,
    );
    assert_eq!(sums.first_byte_count, 0);
}

/// 分子与分母必须同时门限:只门限分母会让均值更偏(分母变小、分子不变)。
#[test]
fn first_byte_numerator_and_denominator_are_gated_together() {
    let mut acc = Accumulator::default();
    acc.add(
        &UsageRow {
            created: None,
            item: payload(json!({
                "status": 502,
                "first_byte_ms": 8_000.0,
                "duration_ms": 8_000.0,
            })),
        },
        true,
    );
    acc.add(
        &UsageRow {
            created: None,
            item: payload(json!({
                "status": 200,
                "first_byte_ms": 1_000.0,
                "duration_ms": 10_000.0,
            })),
        },
        false,
    );
    // 只计成功那条:1000 / 1。
    assert!((acc.avg_first_byte_ms() - 1_000.0).abs() < 1e-9);
}
