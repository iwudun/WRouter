use super::*;
use serde_json::json;

/// TTFT 分桶:三段步长(10ms / 100ms / 1s);桶上界必须覆盖真实值
/// (分位数取上界 = 保守口径)。
#[test]
fn ttft_buckets_cover_value_with_upper_bound() {
    assert_eq!(ttft_bucket(0.0), 0);
    assert_eq!(ttft_bucket(9.9), 0);
    assert_eq!(ttft_bucket(10.0), 1);
    assert_eq!(ttft_bucket(999.0), 99);
    assert_eq!(ttft_bucket(1_000.0), 100);
    assert_eq!(ttft_bucket(9_999.0), 189);
    assert_eq!(ttft_bucket(10_000.0), 190);
    for value in [
        0.0, 9.9, 10.0, 123.0, 999.0, 1_000.0, 5_432.0, 9_999.0, 10_000.0, 54_321.0,
    ] {
        assert!(
            ttft_bucket_upper(ttft_bucket(value)) >= value,
            "TTFT 桶上界必须覆盖真实值:{value}"
        );
    }
}

/// 解码速度分桶同上(1 / 5 / 25 / 100 四段步长)。
#[test]
fn decode_tps_buckets_cover_value_with_upper_bound() {
    assert_eq!(decode_tps_bucket(0.0), 0);
    assert_eq!(decode_tps_bucket(99.9), 99);
    assert_eq!(decode_tps_bucket(100.0), 100);
    assert_eq!(decode_tps_bucket(499.0), 179);
    assert_eq!(decode_tps_bucket(500.0), 180);
    assert_eq!(decode_tps_bucket(2_000.0), 250);
    for value in [
        0.5, 42.0, 99.9, 100.0, 333.0, 500.0, 1_234.0, 2_000.0, 7_777.0,
    ] {
        assert!(
            decode_tps_bucket_upper(decode_tps_bucket(value)) >= value,
            "吞吐桶上界必须覆盖真实值:{value}"
        );
    }
}

/// 分位数:取所在桶的**上界**;无该指标的样本 → None。
#[test]
fn histogram_quantile_reads_bucket_upper_bound() {
    let mut hist = Histogram::default();
    hist.add_metric(METRIC_TTFT, 10, 50);
    hist.add_metric(METRIC_TTFT, 20, 50);
    // 100 个样本:一半在 100–110ms,一半在 200–210ms。
    assert_eq!(hist.quantile(METRIC_TTFT, 0.50), Some(110.0));
    assert_eq!(hist.quantile(METRIC_TTFT, 0.95), Some(210.0));
    assert_eq!(hist.quantile(METRIC_DECODE_TPS, 0.50), None);
}

/// 阶段四 C-7:实测 flow 优先于速率阈值启发式。
#[test]
fn burst_sample_prefers_measured_flow() {
    // flow 远小于解码窗口 → 内容在窗口的 1/8 内倒出来 → 作废。
    assert!(burst_sample(1_000, 8_000.0, 8_000.0, Some(1_000.0)));
    // flow ≈ 解码窗口(均匀投递)→ 不作废,哪怕推算速率畸高。
    assert!(!burst_sample(100_000, 8_000.0, 8_000.0, Some(8_000.0)));
    // 没有 flow → 沿用速率阈值启发式(旧行口径不变)。
    assert!(burst_sample(100_000, 100.0, 8_000.0, None));
    assert!(!burst_sample(100, 8_000.0, 8_000.0, None));
}

/// 样本抽取与解码分子同口径;失败行两个速度样本都不进直方图。
#[test]
fn histogram_samples_follow_existing_gates() {
    let item = json!({
        "output_tokens": 900,
        "duration_ms": 10_000.0,
        "first_byte_ms": 1_000.0,
        "answer_end_ms": 10_000.0,
        "first_answer_byte_ms": 1_000.0,
        "flow_ms": 9_000.0,
        "answer_flow_ms": 9_000.0,
        "status": 200
    })
    .as_object()
    .cloned()
    .unwrap();
    let samples = histogram_samples_for_item(&item, false);
    assert_eq!(samples.len(), 2, "TTFT + 解码速度各一个样本");
    assert_eq!(samples[0].metric, METRIC_TTFT);
    assert_eq!(samples[0].bucket, ttft_bucket(1_000.0));
    assert_eq!(samples[0].source, "");
    assert_eq!(samples[1].metric, METRIC_DECODE_TPS);
    assert_eq!(samples[1].source, "full_flow");
    // 失败行:TTFT 与解码速度都不计(20261108_01 §13 错误行门控)。
    let failed = histogram_samples_for_item(&item, true);
    assert!(failed.is_empty());
}
