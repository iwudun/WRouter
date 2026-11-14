//! 用量/节省统计的**时间窗**:自定义日区间(`from..to`)与「全部」。
//!
//! 覆盖:
//! - 自定义窗口只计入窗口内的行(窗口外的原始行不入账);
//! - 自定义窗口的补零时间轴不越出窗口末尾;
//! - 卷积行(`usage_rollup`)同样受窗口上界约束(窗口内的计入、之后的排除);
//! - 「全部」= 全表;
//! - 形如自定义却解析不出来的 range → **空窗口**(不是全表);
//! - 节省三本账跟随同一个自定义窗口。
//!
//! 窗口口径:自定义范围是**本地日历日**(两端都含),与 `today` 同口径;
//! 库里 `created_at` 存 UTC —— 本地正午必然落在本地日之内,故用本地正午
//! 造点可避开时区/DST 边界。

use std::path::PathBuf;

use chrono::{DateTime, Duration, Local, NaiveDate, TimeZone, Utc};
use serde_json::{Value, json};

use wr_app::ports::{PortError, SavingsEventStore, UsageStore};
use wr_infra::sqlite::SqliteStore;

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wr-usage-window-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn store(name: &str) -> (SqliteStore, PathBuf) {
    let dir = temp_dir(name);
    let store = SqliteStore::open(&dir).await.unwrap();
    (store, dir)
}

/// 本地日历第 n 天(0=今天)的日期键 `YYYY-MM-DD`。
fn local_day_key(offset_days: i64) -> String {
    (Local::now().date_naive() + Duration::days(offset_days))
        .format("%Y-%m-%d")
        .to_string()
}

/// 某本地日的正午 → UTC epoch。
fn local_noon_epoch(day_key: &str) -> i64 {
    let date = NaiveDate::parse_from_str(day_key, "%Y-%m-%d").unwrap();
    let naive = date.and_hms_opt(12, 0, 0).unwrap();
    Local
        .from_local_datetime(&naive)
        .earliest()
        .unwrap()
        .with_timezone(&Utc)
        .timestamp()
}

/// 某本地日的正午 → 库里 `created_at` 的 UTC 形态。
fn local_noon_utc(day_key: &str) -> String {
    Utc.timestamp_opt(local_noon_epoch(day_key), 0)
        .single()
        .unwrap()
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}

/// 某本地日的**次日**零点 → UTC epoch(与后端窗口上界同口径)。
fn local_next_midnight_epoch(day_key: &str) -> i64 {
    let date = NaiveDate::parse_from_str(day_key, "%Y-%m-%d").unwrap();
    let naive = (date + Duration::days(1)).and_hms_opt(0, 0, 0).unwrap();
    Local
        .from_local_datetime(&naive)
        .earliest()
        .unwrap()
        .with_timezone(&Utc)
        .timestamp()
}

/// 一条计入用量的请求日志 payload(只有真调用过供应商的行才入账)。
fn usage_payload(input_tokens: i64) -> Value {
    json!({
        "protocol": "openai", "model": "m1", "provider": "p1", "status": 200,
        "stream": true, "duration_ms": 20.0,
        "input_tokens": input_tokens, "output_tokens": 10, "total_tokens": input_tokens + 10,
        "cache_read_tokens": 0, "cache_write_tokens": 0, "cost_usd": 0.002,
        "api_key_id": "k1",
    })
}

/// 用指定 `created_at` 插一行原始请求日志(绕过 `append_request_log`,
/// 这样时间戳完全可控,且不会连带写卷积行 —— 原始行路径单独可测)。
async fn insert_log_at(store: &SqliteStore, request_id: &str, created_at: &str, payload: Value) {
    let text = wr_domain::json_py::py_json_dumps(&payload);
    let request_id = request_id.to_owned();
    let created_at = created_at.to_owned();
    store
        .engine()
        .write(move |connection| {
            connection
                .execute(
                    "INSERT OR REPLACE INTO request_logs(request_id, created_at, payload) \
                     VALUES(?1, ?2, ?3)",
                    rusqlite::params![request_id, created_at, text],
                )
                .map_err(|e| PortError::storage(e.to_string()))?;
            Ok(())
        })
        .await
        .unwrap();
}

/// 插一行卷积(小时桶)行 —— 用于验证窗口上界对卷积段同样生效。
async fn insert_rollup(store: &SqliteStore, bucket_epoch: i64, request_count: i64) {
    store
        .engine()
        .write(move |connection| {
            connection
                .execute(
                    "INSERT OR REPLACE INTO usage_rollup(bucket_epoch, model, upstream_model, \
                     requested_model, provider, client, api_key_id, api_key_name, request_count, \
                     error_count, input_tokens, output_tokens, total_tokens, cache_tokens, cost_usd) \
                     VALUES(?1, 'm1', '', 'm1', 'p1', 'c1', 'k1', '', ?2, 0, ?2, 0, ?2, 0, 0.0)",
                    rusqlite::params![bucket_epoch, request_count],
                )
                .map_err(|e| PortError::storage(e.to_string()))?;
            Ok(())
        })
        .await
        .unwrap();
}

/// 插一行节省事件(压缩账)。
async fn insert_savings_event(
    store: &SqliteStore,
    request_id: &str,
    created_at: &str,
    tokens_saved: i64,
) {
    let request_id = request_id.to_owned();
    let created_at = created_at.to_owned();
    store
        .engine()
        .write(move |connection| {
            connection
                .execute(
                    "INSERT INTO savings_events(request_id, created_at, kind, tokens_saved, \
                     estimated_usd_saved, engine_breakdown) \
                     VALUES(?1, ?2, 'compression', ?3, 0.0, '[]')",
                    rusqlite::params![request_id, created_at, tokens_saved],
                )
                .map_err(|e| PortError::storage(e.to_string()))?;
            Ok(())
        })
        .await
        .unwrap();
}

/// 自定义窗口只计入窗口内的行。
#[tokio::test]
async fn custom_window_counts_only_rows_inside_it() {
    let (store, dir) = store("raw").await;
    let day = local_day_key(-2);
    insert_log_at(&store, "in", &local_noon_utc(&day), usage_payload(100)).await;
    insert_log_at(
        &store,
        "before",
        &local_noon_utc(&local_day_key(-3)),
        usage_payload(900),
    )
    .await;
    insert_log_at(
        &store,
        "after",
        &local_noon_utc(&local_day_key(-1)),
        usage_payload(900),
    )
    .await;

    let window = format!("{day}..{day}");
    let summary = store
        .usage_summary(Some(&window), &json!({}))
        .await
        .unwrap();
    assert_eq!(
        summary["totals"]["requestCount"],
        json!(1),
        "窗口外的行不得入账"
    );
    assert_eq!(
        summary["totals"]["inputTokens"],
        json!(100),
        "只该计入窗口内那一行"
    );
    assert_eq!(summary["range"], json!(window));
    std::fs::remove_dir_all(&dir).ok();
}

/// 自定义窗口的补零时间轴不越出窗口末尾(否则图表会拖出一条不属于该窗口的尾巴)。
#[tokio::test]
async fn custom_window_series_stops_at_window_end() {
    let (store, dir) = store("series").await;
    let day = local_day_key(-2);
    insert_log_at(&store, "in", &local_noon_utc(&day), usage_payload(100)).await;

    let window_end = local_next_midnight_epoch(&day);
    let window = format!("{day}..{day}");
    let summary = store
        .usage_summary(Some(&window), &json!({}))
        .await
        .unwrap();
    let series = summary["series"].as_array().unwrap();
    assert!(!series.is_empty(), "窗口内有点时序列不能为空");
    for point in series {
        let bucket = point["bucket"].as_str().unwrap();
        let epoch = DateTime::parse_from_rfc3339(bucket).unwrap().timestamp();
        assert!(
            epoch < window_end,
            "补零不得越过窗口末尾:{bucket} >= {window_end}"
        );
    }
    std::fs::remove_dir_all(&dir).ok();
}

/// 卷积行也受窗口上界约束:窗口内的计入、窗口之后的排除。
#[tokio::test]
async fn custom_window_excludes_rollup_rows_after_it() {
    let (store, dir) = store("rollup").await;
    let day = local_day_key(-2);
    // 卷积段起点落在窗口内(窗口第一天早上 6 点)。
    let inside = local_noon_epoch(&day) - 6 * 3600;
    insert_rollup(&store, inside, 1).await;
    // 另一行落在窗口之后(次日本地正午)—— 不得入账。
    insert_rollup(&store, local_noon_epoch(&local_day_key(-1)), 7).await;

    let window = format!("{day}..{day}");
    let summary = store
        .usage_summary(Some(&window), &json!({}))
        .await
        .unwrap();
    assert_eq!(
        summary["totals"]["requestCount"],
        json!(1),
        "窗口之后的卷积行不得入账"
    );
    assert_eq!(summary["totals"]["inputTokens"], json!(1));
    std::fs::remove_dir_all(&dir).ok();
}

/// 「全部」= 全表(不限时间)。
#[tokio::test]
async fn all_range_counts_the_whole_table() {
    let (store, dir) = store("all").await;
    insert_log_at(
        &store,
        "d3",
        &local_noon_utc(&local_day_key(-3)),
        usage_payload(10),
    )
    .await;
    insert_log_at(
        &store,
        "d2",
        &local_noon_utc(&local_day_key(-2)),
        usage_payload(10),
    )
    .await;
    insert_log_at(
        &store,
        "d1",
        &local_noon_utc(&local_day_key(-1)),
        usage_payload(10),
    )
    .await;

    let summary = store.usage_summary(Some("all"), &json!({})).await.unwrap();
    assert_eq!(summary["totals"]["requestCount"], json!(3), "全部 = 全表");
    assert_eq!(summary["range"], json!("all"));
    std::fs::remove_dir_all(&dir).ok();
}

/// 形如自定义却解析不出来的 range → 空窗口,**不是**全表。
#[tokio::test]
async fn malformed_custom_range_is_empty_not_full_table() {
    let (store, dir) = store("malformed").await;
    insert_log_at(
        &store,
        "row",
        &local_noon_utc(&local_day_key(-1)),
        usage_payload(10),
    )
    .await;

    let summary = store
        .usage_summary(Some("2026-13-45..not-a-date"), &json!({}))
        .await
        .unwrap();
    assert_eq!(
        summary["totals"]["requestCount"],
        json!(0),
        "解析失败的自定义窗口应是空窗口,不能静默变成全量"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// 节省三本账跟随同一个自定义窗口。
#[tokio::test]
async fn savings_summary_follows_the_custom_window() {
    let (store, dir) = store("savings").await;
    let day = local_day_key(-2);
    insert_savings_event(&store, "in", &local_noon_utc(&day), 1000).await;
    insert_savings_event(&store, "out", &local_noon_utc(&local_day_key(-1)), 5000).await;

    let window = format!("{day}..{day}");
    let summary = store.savings_summary(&window).await.unwrap();
    assert_eq!(
        summary["compression"]["requests"],
        json!(1),
        "窗口之外的节省事件不入账"
    );
    assert_eq!(summary["compression"]["tokensSaved"], json!(1000));
    assert_eq!(summary["period"], json!(window));
    std::fs::remove_dir_all(&dir).ok();
}
