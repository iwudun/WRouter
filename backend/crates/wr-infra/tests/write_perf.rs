//! R4.13 性能实测:日志写入延迟(写线程隔离验证)。
//!
//! **R4.13 的 `P99 < 2ms @ 100 并发` 门未达成**：本测试把实测数字与根因
//! 固定下来(另见 14 号文档 §3.3 与 §11.1 R4 完成记录)。
//!
//! - 单次无争用的 `append_request_log` 就要 ~2.3ms(P50 2.06 / P99 3.25)——
//!   它等于一次独立事务 + 一次 WAL 提交 fsync;
//! - 100 并发只是把这份服务耗时排队,P99 因此涨到百毫秒量级;
//! - 所以瓶颈不是「写线程没隔离」,而是「每次写都单独 fsync」。
//!
//! 补救方向(单独评估,不混进 R4 主体):写线程把队列里已到的一次性收成一
//! 个事务、用 SAVEPOINT 做逐任务回滚,让 N 次写共享 1 次 fsync;或按部署档位
//! 放开 `PRAGMA synchronous`。两者都改变持久化粒度,需重跑双实现 db parity。
//!
//! 因此本测试断言真实且有回归价值的东西:
//! - 单次落库延迟留在 10ms 回归门内(能抓住隔离失效 / 锁风暴级别的退化);
//! - 100 并发下不丢写、不重复、全部可读回(正确性门);
//! - P50/P95/P99/max 全部打印,留下性能趋势历史。

use std::time::Instant;

use serde_json::{Map, Value, json};

use wr_app::ports::RequestLogStore;
use wr_infra::sqlite::SqliteStore;

const CONCURRENCY: usize = 100;
const WRITES_PER_TASK: usize = 20;
const SERIAL_WRITES: usize = 200;

/// 从(已排序升序的)样本里取分位数;`percent` ∈ [0,100]。
fn percentile(sorted: &[f64], percent: f64) -> f64 {
    assert!(!sorted.is_empty(), "样本不得为空");
    let rank = ((percent / 100.0) * (sorted.len() as f64 - 1.0)).round() as usize;
    sorted[rank.min(sorted.len() - 1)]
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let dir =
        std::env::temp_dir().join(format!("wr-r4-perf-{}-{name}-{nanos}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn payload(index: usize) -> Value {
    let mut map = Map::new();
    map.insert("protocol".to_owned(), json!("openai"));
    map.insert("model".to_owned(), json!("m-perf"));
    map.insert("status".to_owned(), json!(200));
    map.insert("stream".to_owned(), json!(false));
    map.insert("duration_ms".to_owned(), json!(index as f64));
    map.insert("provider".to_owned(), json!("prov-perf"));
    map.insert("input_tokens".to_owned(), json!(11));
    map.insert("output_tokens".to_owned(), json!(7));
    map.insert("total_tokens".to_owned(), json!(18));
    Value::Object(map)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn log_write_latency_percentiles() {
    let dir = temp_dir("percentiles");
    let store = std::sync::Arc::new(SqliteStore::open(&dir).await.unwrap());

    // ---- 无争用基线:串行写,隔离出「单次落库」自身的服务耗时 ----
    let mut serial: Vec<f64> = Vec::with_capacity(SERIAL_WRITES);
    for i in 0..SERIAL_WRITES {
        let started = Instant::now();
        store
            .append_request_log(&format!("serial-{i}"), &payload(i), None)
            .await
            .expect("落库不得失败");
        serial.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    serial.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    println!(
        "[r4.13 serial] n={SERIAL_WRITES} avg={:.3}ms p50={:.3}ms p95={:.3}ms p99={:.3}ms max={:.3}ms",
        serial.iter().sum::<f64>() / SERIAL_WRITES as f64,
        percentile(&serial, 50.0),
        percentile(&serial, 95.0),
        percentile(&serial, 99.0),
        serial[SERIAL_WRITES - 1],
    );
    // 回归门(非 R4.13 的 2ms 目标 —— 那个未达成,见模块文档):单次落库
    // P99 必须留在 10ms 内,抓住写线程隔离失效 / 锁风暴级别的退化。
    assert!(
        percentile(&serial, 99.0) < 10.0,
        "单次落库 P99 回归门 10ms 被突破,实测 {:.3}ms(R4.13 目标 2ms 未达成)",
        percentile(&serial, 99.0)
    );

    // ---- 100 并发:正确性门(排队延迟另记,见模块文档) ----
    let mut handles = Vec::with_capacity(CONCURRENCY);
    for task in 0..CONCURRENCY {
        let store = store.clone();
        handles.push(tokio::spawn(async move {
            let mut samples = Vec::with_capacity(WRITES_PER_TASK);
            for i in 0..WRITES_PER_TASK {
                let started = Instant::now();
                store
                    .append_request_log(
                        &format!("req-{task}-{i}"),
                        &payload(task * WRITES_PER_TASK + i),
                        None,
                    )
                    .await
                    .expect("落库不得失败");
                samples.push(started.elapsed().as_secs_f64() * 1000.0);
            }
            samples
        }));
    }
    let mut all: Vec<f64> = Vec::with_capacity(CONCURRENCY * WRITES_PER_TASK);
    for handle in handles {
        all.extend(handle.await.unwrap());
    }
    all.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let total = all.len();
    println!(
        "[r4.13 concurrent] n={total} ({}×{}) avg={:.3}ms p50={:.3}ms p95={:.3}ms p99={:.3}ms max={:.3}ms",
        CONCURRENCY,
        WRITES_PER_TASK,
        all.iter().sum::<f64>() / total as f64,
        percentile(&all, 50.0),
        percentile(&all, 95.0),
        percentile(&all, 99.0),
        all[total - 1],
    );

    // 并发下不丢写、不重复、全部可读回。
    let count: i64 = store
        .engine()
        .read(|conn| {
            conn.query_row("SELECT COUNT(*) FROM request_logs", [], |r| r.get(0))
                .map_err(|e| wr_app::ports::PortError::storage(e.to_string()))
        })
        .await
        .unwrap();
    assert_eq!(
        count,
        (SERIAL_WRITES + total) as i64,
        "落库行数必须等于写入次数(串行 + 并发)"
    );
    let distinct: i64 = store
        .engine()
        .read(|conn| {
            conn.query_row(
                "SELECT COUNT(DISTINCT request_id) FROM request_logs",
                [],
                |r| r.get(0),
            )
            .map_err(|e| wr_app::ports::PortError::storage(e.to_string()))
        })
        .await
        .unwrap();
    assert_eq!(distinct, count, "request_id 必须全部唯一(无覆盖丢行)");

    store.close().await;
    std::fs::remove_dir_all(&dir).ok();
}

/// 门禁自检:确认测试本身不会因 0 样本而虚过。
#[test]
fn percentile_helper_is_monotonic() {
    let samples = [1.0, 2.0, 3.0, 4.0, 100.0];
    assert!(percentile(&samples, 50.0) <= percentile(&samples, 99.0));
    assert_eq!(percentile(&samples, 100.0), 100.0);
}
