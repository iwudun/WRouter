use super::*;
use serde_json::json;

fn row(item: serde_json::Value) -> UsageRow {
    UsageRow {
        created: None,
        item: item.as_object().cloned().unwrap(),
    }
}

/// 非流式(无 first_byte_ms)不进吞吐:不得用整段耗时冒充解码时长。
#[test]
fn non_streaming_rows_excluded_from_throughput() {
    let mut acc = Accumulator::default();
    acc.add(
        &row(json!({"output_tokens": 500, "duration_ms": 30_000.0})),
        false,
    );
    assert_eq!(acc.avg_decode_tps(), 0.0, "非流式不参与吞吐");
    // 输出 token 总量照常累计(只影响吞吐分子集合,不影响用量统计)。
    assert_eq!(acc.output_tokens, 500);
}

/// 缓冲突发样本作废:解码窗口 < 10% 总时长且速率畸高。
#[test]
fn buffered_burst_samples_dropped() {
    let mut acc = Accumulator::default();
    // duration 1000ms,首字 990ms → 窗口 10ms(<10% 且 50tok/10ms=5000tok/s)。
    acc.add(
        &row(json!({
            "output_tokens": 50, "duration_ms": 1_000.0, "first_byte_ms": 990.0
        })),
        false,
    );
    assert_eq!(acc.avg_decode_tps(), 0.0, "缓冲突发作废");
}

/// 旧行(无 answer_end_ms):解码窗口口径仍可逐行计算,但归属
/// `legacy_decode_window`,不进入统计页的新口径均值。
#[test]
fn legacy_streaming_rows_report_window_rate_but_are_excluded_from_average() {
    let item = json!({
        "output_tokens": 900, "duration_ms": 10_000.0, "first_byte_ms": 1_000.0
    });
    let sample = decode_tps_sample(item.as_object().unwrap(), false).expect("旧行可测");
    assert_eq!(sample.source, DecodeSource::LegacyDecodeWindow);
    assert!((sample.tokens as f64 / (sample.ms / 1000.0) - 100.0).abs() < 1e-6);

    let mut acc = Accumulator::default();
    acc.add(&row(item), false);
    assert_eq!(acc.avg_decode_tps(), 0.0, "旧口径不参与新口径均值");
}

/// 混合样本:非流式行不产出解码样本,不稀释新口径流式速率。
#[test]
fn mixed_rows_do_not_dilute_streaming_rate() {
    let mut acc = Accumulator::default();
    acc.add(
        &row(json!({
            "output_tokens": 900,
            "duration_ms": 10_000.0,
            "first_byte_ms": 1_000.0,
            "answer_end_ms": 10_000.0,
            "first_answer_byte_ms": 1_000.0,
            "flow_ms": 9_000.0,
            "answer_flow_ms": 9_000.0
        })),
        false,
    );
    acc.add(
        &row(json!({"output_tokens": 500, "duration_ms": 30_000.0})),
        false,
    );
    assert!(
        (acc.avg_decode_tps() - 100.0).abs() < 1e-6,
        "非流式行不得稀释"
    );
}

// ---------------------------------------------------------------------------
// 20261108_01 §7:条件吞吐口径(新行)与旧行断代。
// ---------------------------------------------------------------------------

/// 思考内容以文本流可见的 Chat 上游:分子分母同集合 —— 全输出 token ÷
/// 全内容 flow,不做拆分。
#[test]
fn chat_visible_reasoning_uses_full_output_flow() {
    let mut acc = Accumulator::default();
    acc.add(
        &row(json!({
            "output_tokens": 500,
            "reasoning_tokens": 100,
            "provider_protocol": "openai",
            "reasoning_decode_ms": 3_000.0,
            "duration_ms": 20_000.0,
            "first_byte_ms": 500.0,
            "answer_end_ms": 5_000.0,
            "first_answer_byte_ms": 1_000.0,
            "answer_flow_ms": 1_000.0,
            "flow_ms": 4_000.0
        })),
        false,
    );
    // 500 tok / 4s = 125 tok/s:可见思考与 flow 同集合。
    assert!(
        (acc.avg_decode_tps() - 125.0).abs() < 1e-6,
        "可见思考全输出口径:{}",
        acc.avg_decode_tps()
    );
}

/// Chat 上游思考不可测(无 reasoning_decode_ms):剥离 reasoning_tokens,
/// 分母改用答案流。
#[test]
fn chat_hidden_reasoning_uses_visible_tokens() {
    let mut acc = Accumulator::default();
    acc.add(
        &row(json!({
            "output_tokens": 500,
            "reasoning_tokens": 100,
            "provider_protocol": "openai",
            "duration_ms": 20_000.0,
            "first_byte_ms": 500.0,
            "answer_end_ms": 5_000.0,
            "first_answer_byte_ms": 1_000.0,
            "answer_flow_ms": 4_000.0,
            "flow_ms": 4_000.0
        })),
        false,
    );
    // 可见 400 tok / 4s = 100 tok/s。
    assert!(
        (acc.avg_decode_tps() - 100.0).abs() < 1e-6,
        "隐藏思考答案口径:{}",
        acc.avg_decode_tps()
    );
}

/// 新行突发守卫:内容在生成窗口 1/4 以内倒完 → 样本作废。
#[test]
fn full_flow_held_burst_dropped() {
    let mut acc = Accumulator::default();
    acc.add(
        &row(json!({
            "output_tokens": 500,
            "first_byte_ms": 1_000.0,
            "answer_end_ms": 5_000.0,
            "first_answer_byte_ms": 4_500.0,
            "answer_flow_ms": 500.0,
            "flow_ms": 500.0
        })),
        false,
    );
    assert_eq!(acc.avg_decode_tps(), 0.0, "批量投递不作吞吐样本");
}

/// 新行的 flow 不可测(0 / 缺 flow / 缺首字)时**不回落**旧口径 ——
/// 否则同一请求在列表与统计会拿到不同分母。
#[test]
fn flow_unmeasurable_does_not_fall_back_to_decode_window() {
    let mut acc = Accumulator::default();
    acc.add(
        &row(json!({
            "output_tokens": 500,
            "duration_ms": 10_000.0,
            "first_byte_ms": 1_000.0,
            "answer_end_ms": 5_000.0,
            "first_answer_byte_ms": 1_000.0,
            "answer_flow_ms": 4_000.0,
            "flow_ms": 0.0
        })),
        false,
    );
    assert_eq!(acc.avg_decode_tps(), 0.0);
    acc.add(
        &row(json!({
            "output_tokens": 500,
            // 新行断代标记存在,但 flow 缺失 → 不回落旧解码窗口。
            "answer_end_ms": 5_000.0,
            "first_answer_byte_ms": 1_000.0,
            "answer_flow_ms": 4_000.0,
            "duration_ms": 10_000.0,
            "first_byte_ms": 1_000.0
        })),
        false,
    );
    assert_eq!(acc.avg_decode_tps(), 0.0);
    acc.add(
        &row(json!({
            "output_tokens": 500,
            "duration_ms": 10_000.0,
            "answer_end_ms": 5_000.0,
            "answer_flow_ms": 4_000.0,
            "flow_ms": 4_000.0
            // first_byte_ms 缺失 → 生成窗口不可测。
        })),
        false,
    );
    assert_eq!(acc.avg_decode_tps(), 0.0);
}

/// 旧行(无 answer_end_ms)沿用解码窗口口径与全输出分子(逐行仍可见),
/// 但聚合均值只统计新口径样本。
#[test]
fn legacy_rows_keep_decode_window_with_full_output() {
    let item = json!({
        "output_tokens": 500,
        "reasoning_tokens": 100,
        "provider_protocol": "openai",
        "duration_ms": 10_000.0,
        "first_byte_ms": 1_000.0
    });
    let sample = decode_tps_sample(item.as_object().unwrap(), false).expect("旧行可测");
    assert_eq!(sample.tokens, 500);
    assert_eq!(sample.ms, 9_000.0);
    assert_eq!(sample.source, DecodeSource::LegacyDecodeWindow);
    let mut acc = Accumulator::default();
    acc.add(&row(item), false);
    assert_eq!(acc.avg_decode_tps(), 0.0);
}

/// 单行吞吐与聚合同源:同一个样本函数供列表逐行(`call_log_entry`)与
/// 统计(`usage_sums_from_item`)使用。
#[test]
fn decode_tps_sample_shared_between_row_and_aggregate() {
    let item = json!({
        "output_tokens": 240,
        "first_byte_ms": 200.0,
        "answer_end_ms": 3_200.0,
        "answer_flow_ms": 2_000.0,
        "flow_ms": 2_000.0
    });
    let item = item.as_object().unwrap();
    let sample = decode_tps_sample(item, false).expect("样本应可测");
    assert_eq!(sample.tokens, 240);
    assert_eq!(sample.ms, 2_000.0);
    assert_eq!(sample.source, DecodeSource::FullFlow);
    let sums = usage_sums_from_item(item, false);
    assert_eq!(sums.decode_tokens, 240);
    assert!((sums.decode_ms_sum - 2_000.0).abs() < 1e-9);
    assert_eq!(sums.decode_source, Some(DecodeSource::FullFlow));
}

/// 物理上限:1-2ms 的窗口即使「flow ≈ window」也测不出解码速度 ——
/// 296 token / 2ms = 148000 tok/s,必须作废(线上 Ling 批量倒内容的复现)。
#[test]
fn implausible_rate_dropped() {
    let mut acc = Accumulator::default();
    acc.add(
        &row(json!({
            "output_tokens": 296,
            "first_byte_ms": 4_348.0,
            "answer_end_ms": 4_350.0,
            "first_answer_byte_ms": 4_348.0,
            "answer_flow_ms": 2.0,
            "flow_ms": 2.0
        })),
        false,
    );
    assert_eq!(acc.avg_decode_tps(), 0.0, "畸高速率不产出样本");
}

/// Ling 只发 reasoning_content、completion_tokens 含思考且不报
/// `reasoning_tokens`;工具调用轮次 content 为空。全输出口径不依赖
/// token 分解,仍能给出可测样本。
#[test]
fn reasoning_without_token_breakdown_uses_full_output() {
    let mut acc = Accumulator::default();
    acc.add(
        &row(json!({
            "output_tokens": 296,
            "provider_protocol": "openai",
            "reasoning_observed": true,
            "first_byte_ms": 1_000.0,
            "answer_end_ms": 3_000.0,
            "answer_flow_ms": 100.0,
            "flow_ms": 2_000.0
        })),
        false,
    );
    // 296 tok / 2s = 148 tok/s:思考既在分子也在分母。
    assert!((acc.avg_decode_tps() - 148.0).abs() < 1e-9);

    acc.add(
        &row(json!({
            "output_tokens": 296,
            "reasoning_tokens": 96,
            "provider_protocol": "openai",
            "reasoning_observed": true,
            "reasoning_decode_ms": 500.0,
            "first_byte_ms": 1_000.0,
            "answer_end_ms": 3_000.0,
            "answer_flow_ms": 1_500.0,
            "flow_ms": 2_000.0
        })),
        false,
    );
    // 思考以文本流可见时,是否上报 reasoning_tokens 不改变口径,均值仍为 148。
    assert!((acc.avg_decode_tps() - 148.0).abs() < 1e-9);
}

/// 线上 Ling 行回归:104 个 reasoning 块 3.2s + 答案单点 1ms,
/// output 296 / flow 3226.25ms ≈ 91.74 tok/s,不再是 296000。
#[test]
fn ling_reasoning_row_reproduces_plausible_rate() {
    let mut acc = Accumulator::default();
    acc.add(
        &row(json!({
            "output_tokens": 296,
            "provider_protocol": "openai",
            "reasoning_observed": true,
            "first_byte_ms": 943.12,
            "answer_end_ms": 4_350.45,
            "first_answer_byte_ms": 4_350.35,
            "answer_flow_ms": 1.0,
            "flow_ms": 3_226.25
        })),
        false,
    );
    let expected = (296.0_f64 / 3.22625 * 100.0).round() / 100.0;
    assert!(
        (acc.avg_decode_tps() - expected).abs() < 1e-9,
        "实际 {}",
        acc.avg_decode_tps()
    );
}

// ---------------------------------------------------------------------------
// AgentRouter/Responses 隐藏思考:分子必须与 answer_flow 同集合。
// ---------------------------------------------------------------------------

/// 线上 180739ea:1622 全输出含 1216 隐藏思考,可见答案 406 /
/// 5.38375s ≈ 75.41 tok/s;修前 1622/5.38375 ≈ 301 是虚高。
#[test]
fn responses_hidden_reasoning_uses_visible_tokens_and_answer_flow() {
    let mut acc = Accumulator::default();
    acc.add(
        &row(json!({
            "output_tokens": 1622,
            "reasoning_tokens": 1216,
            "provider_protocol": "openai_responses",
            "duration_ms": 32_673.28,
            "first_byte_ms": 27_351.48,
            "answer_end_ms": 32_673.28,
            "first_answer_byte_ms": 27_351.42,
            "flow_ms": 5_383.75,
            "answer_flow_ms": 5_383.75
        })),
        false,
    );
    let expected = (406.0_f64 / 5.38375 * 100.0).round() / 100.0;
    assert!(
        (acc.avg_decode_tps() - expected).abs() < 1e-9,
        "实际 {}",
        acc.avg_decode_tps()
    );
}

/// 列表逐行投影与聚合同源:180739ea 的 `decodeTps` 应直接拿到
/// `(406, 5383.75)`,而不是全输出的 1622。
#[test]
fn responses_hidden_reasoning_row_projection_uses_visible_tokens() {
    let item = json!({
        "output_tokens": 1622,
        "reasoning_tokens": 1216,
        "provider_protocol": "openai_responses",
        "duration_ms": 32_673.28,
        "first_byte_ms": 27_351.48,
        "answer_end_ms": 32_673.28,
        "first_answer_byte_ms": 27_351.42,
        "flow_ms": 5_383.75,
        "answer_flow_ms": 5_383.75
    });
    let sample = decode_tps_sample(item.as_object().unwrap(), false).expect("隐藏思考行应可测");
    assert_eq!(sample.tokens, 406);
    assert_eq!(sample.ms, 5_383.75);
    assert_eq!(sample.source, DecodeSource::AnswerFlow);
}

/// 线上 cacc247a:可见 40 / 192.5ms;窗口 < 1s 且首内容前等待 8.3s
/// → 长思考后的短工具调用,按 HeldWindow 作废,不再伪造 207.79。
#[test]
fn responses_short_sample_after_long_wait_dropped() {
    let mut acc = Accumulator::default();
    acc.add(
        &row(json!({
            "output_tokens": 268,
            "reasoning_tokens": 228,
            "provider_protocol": "openai_responses",
            "duration_ms": 8_755.69,
            "first_byte_ms": 8_315.71,
            "answer_end_ms": 8_755.69,
            "first_answer_byte_ms": 8_315.75,
            "flow_ms": 192.5,
            "answer_flow_ms": 192.5
        })),
        false,
    );
    assert_eq!(acc.avg_decode_tps(), 0.0, "短样本不作样本");
}

/// 线上 d04a424f(suggestion engine 返回 NO_SUGGESTION):可见 11 /
/// 33.75ms,窗口 < MinDecodeMs(100ms)→ 计时粒度不足,作废。
#[test]
fn responses_sub_100ms_sample_dropped() {
    let mut acc = Accumulator::default();
    acc.add(
        &row(json!({
            "output_tokens": 185,
            "reasoning_tokens": 174,
            "provider_protocol": "openai_responses",
            "duration_ms": 6_540.87,
            "first_byte_ms": 2_797.66,
            "answer_end_ms": 6_540.87,
            "first_answer_byte_ms": 6_438.88,
            "flow_ms": 33.75,
            "answer_flow_ms": 33.75
        })),
        false,
    );
    assert_eq!(acc.avg_decode_tps(), 0.0, "100ms 以下不作样本");
}

/// 线上 6b09af71:可见 32 / 138.75ms;窗口虽过 100ms,但 < 1s 且
/// 首内容前等待 14.3s > 4×窗口 → 长思考后的短答案,按 HeldWindow 作废。
#[test]
fn responses_short_answer_after_long_wait_dropped() {
    let mut acc = Accumulator::default();
    acc.add(
        &row(json!({
            "output_tokens": 403,
            "reasoning_tokens": 371,
            "provider_protocol": "openai_responses",
            "duration_ms": 14_704.75,
            "first_byte_ms": 5_988.76,
            "answer_end_ms": 14_704.75,
            "first_answer_byte_ms": 14_306.65,
            "flow_ms": 138.75,
            "answer_flow_ms": 138.75
        })),
        false,
    );
    assert_eq!(acc.avg_decode_tps(), 0.0, "短答案长等待不作样本");
}

/// 线上 d18ae939:11 个可见 token / 65ms,虽然答案口径约 169 tok/s,
/// 但内容在 272ms 生成窗口的 1/4 内倒完 → 仍按批量投递作废。
#[test]
fn responses_hidden_reasoning_short_answer_still_dropped() {
    let mut acc = Accumulator::default();
    acc.add(
        &row(json!({
            "output_tokens": 131,
            "reasoning_tokens": 120,
            "provider_protocol": "openai_responses",
            "duration_ms": 5_345.71,
            "first_byte_ms": 5_073.45,
            "answer_end_ms": 5_345.71,
            "first_answer_byte_ms": 5_073.38,
            "flow_ms": 65.0,
            "answer_flow_ms": 65.0
        })),
        false,
    );
    assert_eq!(acc.avg_decode_tps(), 0.0, "短答案批量投递不作样本");
}

/// Responses 无 reasoning_tokens(63f8d6d6 同型):维持全输出 / flow,
/// 1.25ms 的批量投递仍由物理上限作废。
#[test]
fn responses_without_reasoning_tokens_keep_full_output() {
    let mut acc = Accumulator::default();
    acc.add(
        &row(json!({
            "output_tokens": 50,
            "reasoning_tokens": 0,
            "provider_protocol": "openai_responses",
            "duration_ms": 4_025.03,
            "first_byte_ms": 3_813.47,
            "answer_end_ms": 4_025.03,
            "first_answer_byte_ms": 3_813.58,
            "flow_ms": 1.25,
            "answer_flow_ms": 1.25
        })),
        false,
    );
    assert_eq!(acc.avg_decode_tps(), 0.0, "不可测样本仍作废");
}

/// 对照行 7b292d66:68 token / 1.4s = 48.57,无思考,正常产出。
#[test]
fn responses_without_reasoning_tokens_reports_plausible_rate() {
    let mut acc = Accumulator::default();
    acc.add(
        &row(json!({
            "output_tokens": 68,
            "provider_protocol": "openai_responses",
            "duration_ms": 3_916.07,
            "first_byte_ms": 2_724.75,
            "answer_end_ms": 3_916.07,
            "first_answer_byte_ms": 2_724.70,
            "flow_ms": 1_400.0,
            "answer_flow_ms": 1_400.0
        })),
        false,
    );
    let expected = (68.0_f64 / 1.4 * 100.0).round() / 100.0;
    assert!((acc.avg_decode_tps() - expected).abs() < 1e-9);
}

/// 隐藏思考但无可见 token(异常上报 reasoning ≥ output)→ 无样本。
#[test]
fn hidden_reasoning_with_no_visible_tokens_dropped() {
    let mut acc = Accumulator::default();
    acc.add(
        &row(json!({
            "output_tokens": 100,
            "reasoning_tokens": 150,
            "provider_protocol": "openai_responses",
            "duration_ms": 10_000.0,
            "first_byte_ms": 1_000.0,
            "answer_end_ms": 5_000.0,
            "first_answer_byte_ms": 1_000.0,
            "flow_ms": 4_000.0,
            "answer_flow_ms": 4_000.0
        })),
        false,
    );
    assert_eq!(acc.avg_decode_tps(), 0.0);
}

/// 隐藏思考行缺 answer_flow_ms(纯思考/数据不完整)→ 无样本,不得回退
/// flow_ms —— 后者可能包含思考时段,与已剥离的分子不同集合。
#[test]
fn hidden_reasoning_without_answer_flow_dropped() {
    let mut acc = Accumulator::default();
    acc.add(
        &row(json!({
            "output_tokens": 500,
            "reasoning_tokens": 100,
            "provider_protocol": "openai_responses",
            "duration_ms": 10_000.0,
            "first_byte_ms": 1_000.0,
            "answer_end_ms": 5_000.0,
            "first_answer_byte_ms": 1_000.0,
            "flow_ms": 4_000.0
        })),
        false,
    );
    assert_eq!(acc.avg_decode_tps(), 0.0);
}

/// Responses 即使流出了 reasoning summary,usage 的 reasoning_tokens 仍是
/// 隐藏思考总量;summary 文本不等于全部思考 → 仍走可见答案口径。
#[test]
fn responses_reasoning_summary_still_uses_visible_tokens() {
    let mut acc = Accumulator::default();
    acc.add(
        &row(json!({
            "output_tokens": 900,
            "reasoning_tokens": 300,
            "provider_protocol": "openai_responses",
            "reasoning_decode_ms": 2_000.0,
            "duration_ms": 10_000.0,
            "first_byte_ms": 1_000.0,
            "answer_end_ms": 10_000.0,
            "first_answer_byte_ms": 1_000.0,
            "flow_ms": 5_000.0,
            "answer_flow_ms": 5_000.0
        })),
        false,
    );
    // 可见 600 / 5s = 120,不得用 900 / 5 = 180。
    assert!((acc.avg_decode_tps() - 120.0).abs() < 1e-9);
}

/// Gemini 的 output 不含 thoughts:即使上报 reasoning_tokens 也不得剥离。
#[test]
fn gemini_reasoning_tokens_are_not_stripped() {
    let mut acc = Accumulator::default();
    acc.add(
        &row(json!({
            "output_tokens": 900,
            "reasoning_tokens": 300,
            "provider_protocol": "gemini",
            "duration_ms": 10_000.0,
            "first_byte_ms": 1_000.0,
            "answer_end_ms": 9_000.0,
            "first_answer_byte_ms": 1_000.0,
            "flow_ms": 8_000.0,
            "answer_flow_ms": 8_000.0
        })),
        false,
    );
    // 900 / 8s = 112.5,不得变成 600 / 8 = 75。
    assert!((acc.avg_decode_tps() - 112.5).abs() < 1e-9);
}

// ---------------------------------------------------------------------------
// 20261108_01 §13:Chat 可见性、旧口径来源标记、错误行门控、输出速度。
// ---------------------------------------------------------------------------

/// Chat 思考完整可见(可见字节 ≥ 上报 token):即使有 reasoning_decode_ms,
/// 也维持全输出 / flow —— 思考既在分子也在分母。
#[test]
fn chat_complete_reasoning_content_keeps_full_output() {
    let item = json!({
        "output_tokens": 500,
        "reasoning_tokens": 100,
        "provider_protocol": "openai",
        "reasoning_decode_ms": 3_000.0,
        "reasoning_content_bytes": 420,
        "first_byte_ms": 500.0,
        "answer_end_ms": 5_000.0,
        "first_answer_byte_ms": 1_000.0,
        "answer_flow_ms": 1_000.0,
        "flow_ms": 4_000.0
    });
    let sample = decode_tps_sample(item.as_object().unwrap(), false).expect("应可测");
    assert_eq!(sample.tokens, 500, "可见思考不剥离");
    assert_eq!(sample.ms, 4_000.0);
    assert_eq!(sample.source, DecodeSource::FullFlow);
}

/// Chat 思考只有摘要/单块(可测时长存在,但可见字节不足):按隐藏思考
/// 剥离 reasoning_tokens,分母改用答案流。
#[test]
fn chat_summarized_reasoning_content_is_stripped() {
    let item = json!({
        "output_tokens": 500,
        "reasoning_tokens": 100,
        "provider_protocol": "openai",
        "reasoning_decode_ms": 3_000.0,
        "reasoning_content_bytes": 39,
        "first_byte_ms": 500.0,
        "answer_end_ms": 5_000.0,
        "first_answer_byte_ms": 1_000.0,
        "answer_flow_ms": 4_000.0,
        "flow_ms": 4_000.0
    });
    let sample = decode_tps_sample(item.as_object().unwrap(), false).expect("应可测");
    assert_eq!(sample.tokens, 400);
    assert_eq!(sample.ms, 4_000.0);
    assert_eq!(sample.source, DecodeSource::AnswerFlow);
}

/// 缺 `reasoning_content_bytes` 的旧 Chat 行:维持旧规则 —— 有可测思考
/// 时长就不剥离(保守,不新增误剥离)。
#[test]
fn chat_without_visible_bytes_keeps_legacy_rule() {
    let item = json!({
        "output_tokens": 500,
        "reasoning_tokens": 100,
        "provider_protocol": "openai",
        "reasoning_decode_ms": 3_000.0,
        "first_byte_ms": 500.0,
        "answer_end_ms": 5_000.0,
        "first_answer_byte_ms": 1_000.0,
        "answer_flow_ms": 1_000.0,
        "flow_ms": 4_000.0
    });
    let sample = decode_tps_sample(item.as_object().unwrap(), false).expect("应可测");
    assert_eq!(sample.tokens, 500);
    assert_eq!(sample.source, DecodeSource::FullFlow);
}

/// 旧行(无 answer_end_ms)带 legacy 来源标记;统计聚合不把它混入新口径。
#[test]
fn legacy_rows_report_legacy_decode_source_and_are_excluded_from_average() {
    let legacy = json!({
        "output_tokens": 500,
        "duration_ms": 10_000.0,
        "first_byte_ms": 1_000.0
    });
    let sample = decode_tps_sample(legacy.as_object().unwrap(), false).expect("旧行可测");
    assert_eq!(sample.source, DecodeSource::LegacyDecodeWindow);

    let mut acc = Accumulator::default();
    acc.add(&row(legacy), false);
    assert_eq!(acc.avg_decode_tps(), 0.0, "旧口径不参与新口径均值");
}

/// 失败/取消行:解码速度与输出速度都不产出(499 已交付部分输出也一样)。
#[test]
fn error_rows_do_not_report_speed() {
    let item = json!({
        "status": 499,
        "stream_end_reason": "client_disconnected",
        "output_tokens": 400,
        "duration_ms": 5_000.0,
        "first_byte_ms": 1_000.0,
        "answer_end_ms": 5_000.0,
        "first_answer_byte_ms": 1_000.0,
        "answer_flow_ms": 2_000.0,
        "flow_ms": 2_000.0
    });
    let item = item.as_object().unwrap();
    assert!(decode_tps_sample(item, true).is_none());
    assert!(output_tps_sample(item, true).is_none());
    // 成功行不受影响。
    assert!(decode_tps_sample(item, false).is_some());
    assert!(output_tps_sample(item, false).is_some());
}

/// 输出速度:全部上报输出 token ÷ 请求总耗时,不套内容流守卫。
#[test]
fn output_speed_uses_total_duration_without_flow_guards() {
    // 短样本(解码速度会被 MinDecodeMs 作废)仍有输出速度。
    let item = json!({
        "output_tokens": 11,
        "duration_ms": 6_540.87,
        "first_byte_ms": 2_797.66,
        "answer_end_ms": 6_540.87,
        "first_answer_byte_ms": 6_438.88,
        "answer_flow_ms": 33.75,
        "flow_ms": 33.75
    });
    let item = item.as_object().unwrap();
    assert!(decode_tps_sample(item, false).is_none());
    let output = output_tps_sample(item, false).expect("输出速度应可测");
    let expected = 11.0 / 6.54087;
    assert!((output - expected).abs() < 1e-9);
}

/// 输出速度对缺失/零值不产出。
#[test]
fn output_speed_requires_positive_tokens_and_duration() {
    for item in [
        json!({"output_tokens": 0, "duration_ms": 1_000.0}),
        json!({"output_tokens": 10, "duration_ms": 0.0}),
        json!({"output_tokens": 10}),
    ] {
        assert!(
            output_tps_sample(item.as_object().unwrap(), false).is_none(),
            "{item}"
        );
    }
}

/// 20261108_01 §13 验收:线上真实行的解码/输出双指标(两位小数)。
#[test]
fn online_rows_match_stage_d_acceptance_values() {
    let cases = vec![
        (
            json!({
                "output_tokens": 403, "reasoning_tokens": 371,
                "provider_protocol": "openai_responses",
                "duration_ms": 14_704.75, "first_byte_ms": 5_988.76,
                "answer_end_ms": 14_704.75, "first_answer_byte_ms": 14_306.65,
                "flow_ms": 138.75, "answer_flow_ms": 138.75
            }),
            None,
            27.41,
        ),
        (
            json!({
                "output_tokens": 185, "reasoning_tokens": 174,
                "provider_protocol": "openai_responses",
                "duration_ms": 6_540.87, "first_byte_ms": 2_797.66,
                "answer_end_ms": 6_540.87, "first_answer_byte_ms": 6_438.88,
                "flow_ms": 33.75, "answer_flow_ms": 33.75
            }),
            None,
            28.28,
        ),
        (
            json!({
                "output_tokens": 1622, "reasoning_tokens": 1216,
                "provider_protocol": "openai_responses",
                "duration_ms": 32_673.28, "first_byte_ms": 27_351.48,
                "answer_end_ms": 32_673.28, "first_answer_byte_ms": 27_351.42,
                "flow_ms": 5_383.75, "answer_flow_ms": 5_383.75
            }),
            Some(75.41),
            49.64,
        ),
        (
            json!({
                "output_tokens": 1266, "reasoning_tokens": 1034,
                "provider_protocol": "openai_responses",
                "duration_ms": 26_579.14, "first_byte_ms": 23_863.47,
                "answer_end_ms": 26_579.14, "first_answer_byte_ms": 23_863.38,
                "flow_ms": 2_475.0, "answer_flow_ms": 2_475.0
            }),
            Some(93.74),
            47.63,
        ),
        (
            json!({
                "output_tokens": 1314, "reasoning_tokens": 1158,
                "provider_protocol": "openai_responses",
                "duration_ms": 32_106.01, "first_byte_ms": 29_210.01,
                "answer_end_ms": 32_106.01, "first_answer_byte_ms": 29_209.95,
                "flow_ms": 2_701.25, "answer_flow_ms": 2_701.25
            }),
            Some(57.75),
            40.93,
        ),
        (
            json!({
                "output_tokens": 268, "reasoning_tokens": 228,
                "provider_protocol": "openai_responses",
                "duration_ms": 8_755.69, "first_byte_ms": 8_315.71,
                "answer_end_ms": 8_755.69, "first_answer_byte_ms": 8_315.75,
                "flow_ms": 192.5, "answer_flow_ms": 192.5
            }),
            None,
            30.61,
        ),
        (
            json!({
                "output_tokens": 296,
                "provider_protocol": "openai", "reasoning_observed": true,
                "reasoning_decode_ms": 3_217.5,
                "duration_ms": 4_350.45, "first_byte_ms": 943.12,
                "answer_end_ms": 4_350.45, "first_answer_byte_ms": 4_350.35,
                "flow_ms": 3_226.25, "answer_flow_ms": 1.0
            }),
            Some(91.75),
            68.04,
        ),
    ];
    for (item, expected_decode, expected_output) in cases {
        let item = item.as_object().unwrap();
        let decode = decode_tps_sample(item, false)
            .map(|sample| round_even(sample.tokens as f64 / (sample.ms / 1000.0), 2));
        let output = output_tps_sample(item, false).map(|tps| round_even(tps, 2));
        assert_eq!(decode, expected_decode, "解码速度: {item:?}");
        assert_eq!(output, Some(expected_output), "输出速度: {item:?}");
    }
}
