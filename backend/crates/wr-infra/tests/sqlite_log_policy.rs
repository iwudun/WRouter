//! C3 日志数据策略 + 用量入账的存储层测试(方案 #14 R4.5/R4.6)。
//!
//! 每条策略一个命名测试:
//! - 元数据永久 / 正文按期过期(`prune_logs` 分档);
//! - 正文与元数据同戳写入、无捕获时清侧表旧行;
//! - 旧格式大 payload 行迁移幂等;
//! - 用量只统计真正调用过供应商的行(被拒行留作审计)。

use std::path::PathBuf;

use serde_json::{Value, json};

use wr_app::ports::{LogBodies, PruneStats, RequestLogStore, UsageStore};
use wr_infra::sqlite::SqliteStore;

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wr-r4-log-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn store(name: &str) -> (SqliteStore, PathBuf) {
    let dir = temp_dir(name);
    let store = SqliteStore::open(&dir).await.unwrap();
    (store, dir)
}

/// 用指定 `created_at` 插一行(保留窗口测试需要历史时间戳)。
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
                .map_err(|e| wr_app::ports::PortError::storage(e.to_string()))?;
            Ok(())
        })
        .await
        .unwrap();
}

fn days_ago(days: i64) -> String {
    (chrono::Utc::now() - chrono::Duration::days(days))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}

/// 主行 + 侧表行共用同一条 `datetime('now')`,两表时间戳必须一致。
#[tokio::test]
async fn request_log_splits_metadata_and_bodies_with_shared_timestamp() {
    let (store, dir) = store("split").await;
    let bodies = LogBodies {
        request_body: Some(r#"{"model":"m1"}"#.to_owned()),
        request_body_size: Some(14),
        request_body_truncated: false,
        response_body: Some(r#"{"id":"c1"}"#.to_owned()),
        response_body_size: Some(11),
        response_body_truncated: true,
        response_headers: Some(
            json!({"content-type": "application/json"})
                .as_object()
                .unwrap()
                .clone(),
        ),
        request_headers: Some(json!({"accept": "application/json"}).as_object().unwrap().clone()),
        trace_items: vec![json!({"kind":"attempt","ordinal":0}), json!({"kind":"sse_event","ordinal":1})],
    };
    store
        .append_request_log(
            "r1",
            &json!({"protocol": "openai", "model": "m1"}),
            Some(&bodies),
        )
        .await
        .unwrap();
    let pair: (String, String) = store
        .engine()
        .read(|conn| {
            let main = conn.query_row(
                "SELECT created_at FROM request_logs WHERE request_id='r1'",
                [],
                |row| row.get::<_, String>(0),
            );
            let side = conn.query_row(
                "SELECT created_at FROM request_log_bodies WHERE request_id='r1'",
                [],
                |row| row.get::<_, String>(0),
            );
            Ok((main.unwrap(), side.unwrap()))
        })
        .await
        .unwrap();
    assert_eq!(pair.0, pair.1, "两行必须共用同一时间戳(跨秒也一致)");
    assert!(!pair.0.contains('T'), "created_at 是 datetime('now') 形态");
    let detail = store.get_request_log("r1").await.unwrap().unwrap();
    assert_eq!(detail["request_body_text"], json!(r#"{"model":"m1"}"#));
    assert_eq!(detail["response_body_text"], json!(r#"{"id":"c1"}"#));
    assert_eq!(detail["request_body_size_bytes"], json!(14));
    assert_eq!(detail["request_body_truncated"], json!(false));
    assert_eq!(detail["response_body_size_bytes"], json!(11));
    assert_eq!(detail["response_body_truncated"], json!(true));
    assert_eq!(
        detail["response_headers"]["content-type"],
        json!("application/json")
    );
    assert_eq!(detail["request_headers"]["accept"], json!("application/json"));
    let trace = store.get_request_log_trace("r1", 0, 1, None).await.unwrap();
    assert_eq!(trace.total, 2);
    assert_eq!(trace.items[0]["kind"], json!("attempt"));
    assert_eq!(trace.next_offset, Some(1));
    let trace_last = store
        .get_request_log_trace("r1", 1, 10, None)
        .await
        .unwrap();
    assert_eq!(trace_last.items[0]["kind"], json!("sse_event"));
    assert_eq!(trace_last.next_offset, None);
    // D4:按 kind 过滤,两类数据各自独立分页。
    let only_attempts = store
        .get_request_log_trace("r1", 0, 10, Some("attempt"))
        .await
        .unwrap();
    assert_eq!(only_attempts.total, 1);
    assert_eq!(only_attempts.items.len(), 1);
    assert_eq!(only_attempts.items[0]["kind"], json!("attempt"));
    let only_events = store
        .get_request_log_trace("r1", 0, 10, Some("sse_event"))
        .await
        .unwrap();
    assert_eq!(only_events.total, 1);
    assert_eq!(only_events.items[0]["kind"], json!("sse_event"));
    let none = store
        .get_request_log_trace("r1", 0, 10, Some("nope"))
        .await
        .unwrap();
    assert_eq!(none.total, 0);
    assert!(none.items.is_empty());
    // 主行 payload 不含正文(列表/用量查询因此不解析大对象)。
    let listed = store.list_request_logs(10).await.unwrap();
    assert!(
        listed[0].get("request_body_text").is_none(),
        "主行不得内联正文"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// D9:trace 子表在同一 request id 重写时必须整批替换(不累积旧事件)。
#[tokio::test]
async fn reused_request_id_replaces_trace_items() {
    let (store, dir) = store("trace-replace").await;
    let first = LogBodies {
        trace_items: vec![json!({"kind": "sse_event", "payload": "old"})],
        request_headers: Some(json!({"accept": "text/event-stream"}).as_object().unwrap().clone()),
        ..Default::default()
    };
    store
        .append_request_log("dup", &json!({"protocol": "openai"}), Some(&first))
        .await
        .unwrap();
    let second = LogBodies {
        trace_items: vec![json!({"kind": "attempt", "payload": "new"})],
        ..Default::default()
    };
    store
        .append_request_log("dup", &json!({"protocol": "openai"}), Some(&second))
        .await
        .unwrap();
    let page = store.get_request_log_trace("dup", 0, 10, None).await.unwrap();
    assert_eq!(page.total, 1, "旧 trace 必须被替换而不是累积");
    assert_eq!(page.items[0]["payload"], json!("new"));
    // 未随本次写入提供请求头 → 旧头必须清掉。
    let detail = store.get_request_log("dup").await.unwrap().unwrap();
    assert!(
        detail.get("request_headers").is_none(),
        "重写未提供 headers 时必须清除旧请求头"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// 未捕获正文时必须清掉同 id 的旧侧表行(request_id 复用不得翻出历史正文)。
#[tokio::test]
async fn uncaptured_reused_request_id_drops_stale_bodies() {
    let (store, dir) = store("stale").await;
    let bodies = LogBodies {
        response_body: Some("old secret body".to_owned()),
        response_body_size: Some(15),
        ..Default::default()
    };
    store
        .append_request_log("dup", &json!({"protocol": "openai"}), Some(&bodies))
        .await
        .unwrap();
    store
        .append_request_log("dup", &json!({"protocol": "openai"}), None)
        .await
        .unwrap();
    let detail = store.get_request_log("dup").await.unwrap().unwrap();
    assert!(
        detail.get("response_body_text").is_none(),
        "无捕获的重复写入必须连带删除旧正文"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// C3:正文按短期窗口过期,**元数据在默认(0=永久)下全部留下**。
#[tokio::test]
async fn body_pruning_expires_rows_but_metadata_stays_forever() {
    let (store, dir) = store("prune").await;
    let old = days_ago(8);
    let fresh = days_ago(1);
    for (id, created_at) in [("old", &old), ("fresh", &fresh)] {
        insert_log_at(&store, id, created_at, json!({"protocol": "openai"})).await;
        // 侧表行直接带历史时间戳写入(append_request_log 会用当前时间覆盖)。
        let id_owned = id.to_owned();
        let created_at = created_at.clone();
        store
            .engine()
            .write(move |conn| {
                conn.execute(
                    "INSERT OR REPLACE INTO request_log_bodies(request_id, created_at, \
                     response_body, response_body_size) VALUES(?1, ?2, ?3, ?4)",
                    rusqlite::params![id_owned, created_at, format!("body-of-{id_owned}"), 11],
                )
                .map_err(|e| wr_app::ports::PortError::storage(e.to_string()))?;
                Ok(())
            })
            .await
            .unwrap();
    }
    let stats: PruneStats = store.prune_logs(7, 0).await.unwrap();
    assert_eq!(stats.bodies_deleted, 1, "只有 8 天前的正文过期");
    assert_eq!(
        stats.logs_deleted, 0,
        "log_retention_days=0 → 元数据永久保留"
    );
    assert_eq!(store.list_request_logs(100).await.unwrap().len(), 2);
    let remaining: i64 = store
        .engine()
        .read(|conn| {
            conn.query_row("SELECT COUNT(*) FROM request_log_bodies", [], |r| r.get(0))
                .map_err(|e| wr_app::ports::PortError::storage(e.to_string()))
        })
        .await
        .unwrap();
    assert_eq!(remaining, 1);
    assert!(stats.bytes_deleted > 0, "bytesDeleted 用于驱动 VACUUM 判定");
    std::fs::remove_dir_all(&dir).ok();
}

/// C3:元数据档也可开启,且过期元数据行的正文一并删除(不分档)。
#[tokio::test]
async fn metadata_retention_tier_removes_their_bodies_too() {
    let (store, dir) = store("metadata-tier").await;
    insert_log_at(&store, "old", &days_ago(31), json!({"protocol": "openai"})).await;
    insert_log_at(&store, "new", &days_ago(2), json!({"protocol": "openai"})).await;
    let side = LogBodies {
        response_body: Some("x".to_owned()),
        ..Default::default()
    };
    for id in ["old", "new"] {
        store
            .append_request_log(id, &json!({"protocol": "openai"}), Some(&side))
            .await
            .unwrap();
    }
    // append_request_log 会把 created_at 刷成当前时间,这里把 old 那一组
    // (主行 + 侧表行)一起回写成 31 天前,才落在 30 天元数据档之外。
    let created = days_ago(31);
    store
        .engine()
        .write(move |conn| {
            conn.execute(
                "UPDATE request_logs SET created_at=?1 WHERE request_id='old'",
                rusqlite::params![created],
            )
            .map_err(|e| wr_app::ports::PortError::storage(e.to_string()))?;
            conn.execute(
                "UPDATE request_log_bodies SET created_at=?1 WHERE request_id='old'",
                rusqlite::params![created],
            )
            .map_err(|e| wr_app::ports::PortError::storage(e.to_string()))?;
            Ok(())
        })
        .await
        .unwrap();
    let stats = store.prune_logs(0, 30).await.unwrap();
    assert_eq!(stats.logs_deleted, 1);
    assert_eq!(
        stats.bodies_deleted, 0,
        "body 档关闭时不计 bodiesDeleted 计数"
    );
    let ids: Vec<String> = store
        .list_request_logs(100)
        .await
        .unwrap()
        .into_iter()
        .map(|row| row["request_id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ids, vec!["new".to_owned()]);
    let side_count: i64 = store
        .engine()
        .read(|conn| {
            conn.query_row("SELECT COUNT(*) FROM request_log_bodies", [], |r| r.get(0))
                .map_err(|e| wr_app::ports::PortError::storage(e.to_string()))
        })
        .await
        .unwrap();
    assert_eq!(side_count, 1, "过期元数据行的正文同步删除");
    std::fs::remove_dir_all(&dir).ok();
}

/// W3-8:旧行内嵌正文不再读取;元数据可读、正文按「不可用」降级,
/// 不崩溃、不改写历史行。
#[tokio::test]
async fn legacy_inline_body_rows_degrade_without_crash() {
    let (store, dir) = store("legacy-degrade").await;
    let legacy = json!({
        "protocol": "openai",
        "model": "m1",
        "request_body_text": "{\"a\": 1}",
        "response_body_text": "y".repeat(40 * 1024),
        "response_headers": {"content-type": "application/json"},
        "response_body_size_bytes": 40960,
    });
    store
        .append_request_log("legacy", &legacy, None)
        .await
        .unwrap();
    let detail = store.get_request_log("legacy").await.unwrap().unwrap();
    assert_eq!(detail["model"], json!("m1"));
    assert!(
        detail.get("response_body_text").is_none(),
        "旧内嵌正文不再并回"
    );
    assert!(detail.get("request_body_text").is_none());
    assert_eq!(
        detail["request_body_capture_status"],
        json!("unavailable")
    );
    assert_eq!(
        detail["response_body_capture_status"],
        json!("unavailable")
    );
    let list = store.list_request_logs(10).await.unwrap();
    let row = list
        .iter()
        .find(|row| row["request_id"] == json!("legacy"))
        .expect("旧行仍可列出");
    assert!(row.get("response_body_text").is_none());
    std::fs::remove_dir_all(&dir).ok();
}

/// 用量入账:只有真正调用过供应商的行参与统计,被拒行留作审计。
#[tokio::test]
async fn usage_counts_only_requests_that_reached_a_provider() {
    let (store, dir) = store("usage").await;
    store
        .append_request_log(
            "ok",
            &json!({
                "protocol": "openai", "model": "m1", "provider": "p1", "status": 200,
                "stream": true, "duration_ms": 20.0,
                "input_tokens": 100, "output_tokens": 40, "total_tokens": 140,
                "cache_read_tokens": 20, "cache_write_tokens": 0, "cost_usd": 0.002,
                "api_key_id": "k1",
            }),
            None,
        )
        .await
        .unwrap();
    store
        .append_request_log(
            "rejected",
            &json!({"protocol": "openai", "model": "m1", "status": 401, "duration_ms": 1.0}),
            None,
        )
        .await
        .unwrap();
    store
        .append_request_log(
            "bad",
            &json!({"protocol": "openai", "model": "m1", "provider": "p1", "status": 503,
                    "input_tokens": 5, "output_tokens": 0, "total_tokens": 5}),
            None,
        )
        .await
        .unwrap();

    let summary = store.usage_summary(Some("24h"), &json!({})).await.unwrap();
    let totals = &summary["totals"];
    assert_eq!(totals["requestCount"], json!(2), "被拒的 401 行不入账");
    assert_eq!(totals["errorCount"], json!(1));
    assert_eq!(totals["inputTokens"], json!(105));
    assert_eq!(totals["outputTokens"], json!(40));
    assert_eq!(totals["totalTokens"], json!(145));
    assert_eq!(
        totals["cacheTokens"],
        json!(20),
        "cacheTokens 只累计 cache_read"
    );
    assert_eq!(totals["successRate"], json!(0.5));
    assert_eq!(totals["cacheRatio"], json!(0.1905));
    assert_eq!(summary["recentRequests"].as_array().unwrap().len(), 2);
    // models 维度带 provider 归属;标签走 应用商/模型名(需配置目录)。
    let models = summary["models"].as_array().unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0]["requestCount"], json!(2));
    // 时间轴补零:24h/小时桶 → 至少 24 个点。
    assert!(
        summary["series"].as_array().unwrap().len() >= 24,
        "空桶必须补零,否则折线图被拉成直线"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// 模型筛选:完整展示 label("供应商/模型名")与落库裸名都必须命中。
#[tokio::test]
async fn usage_model_filter_matches_display_label_and_bare_name() {
    let (store, dir) = store("usage-filter").await;
    for (id, model, provider) in [("a", "m1", "p1"), ("b", "m2", "p1"), ("c", "m1", "p2")] {
        store
            .append_request_log(
                id,
                &json!({
                    "protocol": "openai", "model": model, "provider": provider, "status": 200,
                    "stream": false, "duration_ms": 10.0,
                    "input_tokens": 10, "output_tokens": 5, "total_tokens": 15,
                }),
                None,
            )
            .await
            .unwrap();
    }

    // 展示 label(空目录下 label = provider/model)精确命中跨供应商同名模型。
    let by_label = store
        .usage_summary(Some("24h"), &json!({"model": "p1/m1"}))
        .await
        .unwrap();
    assert_eq!(
        by_label["totals"]["requestCount"],
        json!(1),
        "label 筛选只留 p1/m1"
    );
    assert_eq!(by_label["models"].as_array().unwrap().len(), 1);
    assert_eq!(by_label["models"][0]["label"], json!("p1/m1"));

    // 裸名兼容旧行为:命中 p1/m1 与 p2/m1 两行。
    let by_bare = store
        .usage_summary(Some("24h"), &json!({"model": "m1"}))
        .await
        .unwrap();
    assert_eq!(
        by_bare["totals"]["requestCount"],
        json!(2),
        "裸名筛选命中两个供应商"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// 20261104_01 阶段三 · B-1a/B-1b:调用方维度。
/// - `clientModels` 是 (client, model, provider) 三元组维度行,但每行都带**纯**
///   `client`(前端据此归并成「谁在用我的网关」);label 是 "{client} / {模型展示名}"。
/// - `client` 过滤(totals / clientModels / recentRequests 同时收窄)。
/// - B-1b:`client` 是**归一 id**(`claude-cli/2.1.123` → `Claude Code`,
///   `curl/8.5.0` → `curl`),不是原始 UA。
#[tokio::test]
async fn usage_client_dimension_is_reported_and_filterable() {
    let (store, dir) = store("usage-client").await;
    let row = |client: &str, model: &str| {
        json!({
            "protocol": "openai", "model": model, "provider": "p1", "status": 200,
            "stream": false, "duration_ms": 10.0,
            "input_tokens": 10, "output_tokens": 5, "total_tokens": 15,
            "client": client,
        })
    };
    store
        .append_request_log("a", &row("claude-cli/2.1.123", "m1"), None)
        .await
        .unwrap();
    store
        .append_request_log("b", &row("claude-cli/2.1.123", "m2"), None)
        .await
        .unwrap();
    store
        .append_request_log("c", &row("curl/8.5.0", "m1"), None)
        .await
        .unwrap();

    let all = store.usage_summary(Some("24h"), &json!({})).await.unwrap();
    assert_eq!(all["totals"]["requestCount"], json!(3));
    let client_models = all["clientModels"].as_array().unwrap();
    // 三元组维度:同 client 不同 model 各占一行。
    assert_eq!(client_models.len(), 3);
    let clients: Vec<&str> = client_models
        .iter()
        .map(|row| row["client"].as_str().unwrap())
        .collect();
    assert_eq!(
        clients
            .iter()
            .filter(|client| **client == "Claude Code")
            .count(),
        2
    );
    assert_eq!(
        clients.iter().filter(|client| **client == "curl").count(),
        1
    );
    // label 是 "{client} / {供应商/模型名}"(空目录下 label = provider/model)。
    let label = client_models
        .iter()
        .find(|row| row["client"] == json!("Claude Code"))
        .map(|row| row["label"].as_str().unwrap())
        .unwrap();
    assert!(
        label.starts_with("Claude Code / "),
        "label 形态必须是 {{client}} / {{模型展示名}},实得 {label}"
    );

    // 按 client 过滤:只留该调用方(totals / clientModels / recentRequests 同步收窄)。
    let filtered = store
        .usage_summary(Some("24h"), &json!({"client": "Claude Code"}))
        .await
        .unwrap();
    assert_eq!(filtered["totals"]["requestCount"], json!(2));
    let filtered_models = filtered["clientModels"].as_array().unwrap();
    assert_eq!(filtered_models.len(), 2);
    for row in filtered_models {
        assert_eq!(row["client"], json!("Claude Code"));
    }
    assert_eq!(
        filtered["recentRequests"].as_array().unwrap().len(),
        2,
        "recent 行也随调用方收窄"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// 20261104_01 阶段三 · B-1b:同一客户端**换版本号**必须合成一条。
///
/// 这是 B-1b 的核心诉求:归一前 `ZCode/3.14.5 …` 与 `ZCode/3.15.3 …` 是
/// 两个维度行(客户端用量卡里显示成两个客户端),归一后必须是同一个
/// `ZCode`,且用量累加到同一行。
#[tokio::test]
async fn usage_client_dimension_merges_versions_of_the_same_client() {
    let (store, dir) = store("usage-client-versions").await;
    let row = |client: &str, model: &str| {
        json!({
            "protocol": "openai", "model": model, "provider": "p1", "status": 200,
            "stream": false, "duration_ms": 10.0,
            "input_tokens": 10, "output_tokens": 5, "total_tokens": 15,
            "client": client,
        })
    };
    store
        .append_request_log(
            "a",
            &row("ZCode/3.14.5 ai-sdk/provider-utils/4.0.39", "m1"),
            None,
        )
        .await
        .unwrap();
    store
        .append_request_log(
            "b",
            &row("ZCode/3.15.3 ai-sdk/provider-utils/4.0.39", "m1"),
            None,
        )
        .await
        .unwrap();
    store
        .append_request_log(
            "c",
            &row("ZCode/3.16.3 ai/6.0.193 ai-sdk/provider-utils/4.0.39", "m1"),
            None,
        )
        .await
        .unwrap();

    let all = store.usage_summary(Some("24h"), &json!({})).await.unwrap();
    let client_models = all["clientModels"].as_array().unwrap();
    // 三个版本号 → 同一个客户端维度行(不是三行)。
    assert_eq!(
        client_models.len(),
        1,
        "同一客户端的多个版本必须合成一行,实得 {client_models:?}"
    );
    let client_row = &client_models[0];
    assert_eq!(client_row["client"], json!("ZCode"));
    assert_eq!(client_row["requestCount"], json!(3));
    assert_eq!(client_row["totalTokens"], json!(45));

    // 按归一 id 过滤:三次调用全部命中。
    let filtered = store
        .usage_summary(Some("24h"), &json!({"client": "ZCode"}))
        .await
        .unwrap();
    assert_eq!(filtered["totals"]["requestCount"], json!(3));
    // 拿**原始 UA** 来查也要命中(归一幂等 → 两侧归一后相等)。
    let by_raw_ua = store
        .usage_summary(
            Some("24h"),
            &json!({"client": "ZCode/3.14.5 ai-sdk/provider-utils/4.0.39"}),
        )
        .await
        .unwrap();
    assert_eq!(
        by_raw_ua["totals"]["requestCount"],
        json!(3),
        "按原始 UA 过滤也必须命中同一批行(归一幂等)"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// 20261104_01 阶段三 · B-1b:**历史行**也要归一。
///
/// 归一落在**写入侧**(`dims_from_item`),所以归一落地之前写进
/// `usage_rollup.client` 列的是**原始 UA**。读侧(`load_rollup_rows` /
/// `load_hist_rows`)必须再归一一次,否则老数据仍按版本号裂成多条 ——
/// 这正是「归一必须幂等」的意义所在(写侧存 id、历史行存 UA,读侧一个
/// 函数收敛)。
#[tokio::test]
async fn usage_client_dimension_normalizes_legacy_rollup_rows() {
    let (store, dir) = store("usage-client-legacy").await;
    // 直接写两条**归一落地前**的卷积行(client 列里是原始 UA,两个版本号)。
    store
        .engine()
        .write(|connection| {
            let bucket = chrono::Utc::now().timestamp() / 3600 * 3600;
            for (client, count) in [
                ("ZCode/3.14.5 ai-sdk/provider-utils/4.0.39", 3i64),
                ("ZCode/3.15.3 ai-sdk/provider-utils/4.0.39", 5i64),
            ] {
                connection
                    .execute(
                        "INSERT INTO usage_rollup(bucket_epoch, model, upstream_model, \
                         requested_model, provider, client, api_key_id, api_key_name, \
                         request_count, input_tokens, output_tokens, total_tokens) \
                         VALUES(?1, 'm1', '', '', 'p1', ?2, 'k1', '', ?3, 10, 5, 15)",
                        rusqlite::params![bucket, client, count],
                    )
                    .map_err(|e| wr_app::ports::PortError::storage(e.to_string()))?;
            }
            Ok(())
        })
        .await
        .unwrap();

    let all = store.usage_summary(Some("24h"), &json!({})).await.unwrap();
    let client_models = all["clientModels"].as_array().unwrap();
    assert_eq!(
        client_models.len(),
        1,
        "历史行的原始 UA 必须在读侧归一后合成一行,实得 {client_models:?}"
    );
    assert_eq!(client_models[0]["client"], json!("ZCode"));
    assert_eq!(
        client_models[0]["requestCount"],
        json!(8),
        "两行的请求数必须累加到同一行"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// 20261104_01 阶段三 · B-1a/B-1b:调用方过滤在**卷积段**同样生效。
/// 原始行删掉(只剩 `usage_rollup`)后仍按 client 维度筛出同一批行 ——
/// 卷积的 `client` 列是主键的一部分,与原始行同口径(B-1b 起两路都存
/// **归一 id**;历史行存的是原始 UA,读侧再归一一次收敛)。
#[tokio::test]
async fn usage_client_filter_also_applies_to_rollup_segment() {
    let (store, dir) = store("usage-client-rollup").await;
    let row = |client: &str, model: &str| {
        json!({
            "protocol": "openai", "model": model, "provider": "p1", "status": 200,
            "stream": false, "duration_ms": 10.0,
            "input_tokens": 10, "output_tokens": 5, "total_tokens": 15,
            "client": client,
        })
    };
    store
        .append_request_log("a", &row("claude-cli/2.1.123", "m1"), None)
        .await
        .unwrap();
    store
        .append_request_log("b", &row("curl/8.5.0", "m1"), None)
        .await
        .unwrap();
    // 清掉原始行 → 只剩卷积段(读侧走 merge_rollup_rows)。
    store
        .engine()
        .write(|connection| {
            connection
                .execute("DELETE FROM request_logs", [])
                .map_err(|e| wr_app::ports::PortError::storage(e.to_string()))?;
            Ok(())
        })
        .await
        .unwrap();

    let all = store.usage_summary(Some("24h"), &json!({})).await.unwrap();
    assert_eq!(all["totals"]["requestCount"], json!(2), "卷积段两行都要在");
    let filtered = store
        .usage_summary(Some("24h"), &json!({"client": "curl"}))
        .await
        .unwrap();
    assert_eq!(
        filtered["totals"]["requestCount"],
        json!(1),
        "卷积段也必须按调用方筛"
    );
    let filtered_models = filtered["clientModels"].as_array().unwrap();
    assert_eq!(filtered_models.len(), 1);
    assert_eq!(filtered_models[0]["client"], json!("curl"));
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn last_used_by_api_key_reports_newest_per_key() {
    let (store, dir) = store("lastused").await;
    insert_log_at(&store, "a", &days_ago(3), json!({"api_key_id": "k1"})).await;
    insert_log_at(&store, "b", &days_ago(1), json!({"api_key_id": "k1"})).await;
    insert_log_at(&store, "c", &days_ago(2), json!({"api_key_id": "k2"})).await;
    insert_log_at(&store, "d", &days_ago(2), json!({"api_key_id": ""})).await;
    insert_log_at(&store, "e", &days_ago(2), json!({"protocol": "openai"})).await;
    let latest = store.last_used_by_api_key().await.unwrap();
    assert_eq!(
        latest.as_object().unwrap().len(),
        2,
        "空/缺 api_key_id 不参与"
    );
    assert!(
        latest["k1"].as_str().unwrap() > latest["k2"].as_str().unwrap(),
        "k1 取最近一条"
    );
    std::fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// 20261016_01 B-6/C-6:批量落库与用量小时卷积
// ---------------------------------------------------------------------------

/// 批量落库:多行一个事务,trace 逐条保留(prepared statement 复用)。
#[tokio::test]
async fn append_batch_writes_rows_and_trace_items() {
    let (store, dir) = store("append-batch").await;
    let rows: Vec<wr_app::ports::PendingRequestLog> = (0..3)
        .map(|i| wr_app::ports::PendingRequestLog {
            request_id: format!("b{i}"),
            payload: json!({
                "protocol": "openai", "model": "m1", "provider": "p1",
                "status": 200, "duration_ms": 10.0,
            }),
            bodies: Some(LogBodies {
                trace_items: vec![
                    json!({"kind": "attempt", "ordinal": 0, "i": i}),
                    json!({"kind": "sse_event", "ordinal": 1, "i": i}),
                ],
                ..Default::default()
            }),
        })
        .collect();
    store.append_request_log_batch(&rows).await.unwrap();
    for i in 0..3 {
        let row = store
            .get_request_log(&format!("b{i}"))
            .await
            .unwrap()
            .expect("批量行必须落库");
        assert_eq!(row["request_id"], json!(format!("b{i}")));
    }
    let page = store
        .get_request_log_trace("b1", 0, 10, None)
        .await
        .unwrap();
    assert_eq!(page.total, 2, "trace 两条都要在");
    assert_eq!(page.items[0]["i"], json!(1), "trace 归属正确的行");
    store.close().await;
    std::fs::remove_dir_all(&dir).ok();
}

/// 用量小时卷积与现场全量聚合**逐字段对账**(C-6 验收):
/// 同一批行,卷积生效与清空卷积(强制走原始行)的 usage_summary
/// 必须一致(totals/models/分组维度/recent/非空 series 桶);
/// REPLACE 同 id 行扣回旧增量后仍一致。
#[tokio::test]
async fn usage_rollup_matches_raw_aggregation() {
    let (store, dir) = store("usage-rollup").await;
    store
        .append_request_log(
            "ok",
            &json!({
                "protocol": "openai", "model": "prov/m1", "provider": "p1",
                "status": 200, "stream": true, "duration_ms": 10_000.0,
                "first_byte_ms": 1_000.0,
                "input_tokens": 100, "output_tokens": 900, "total_tokens": 1000,
                "cache_read_tokens": 20, "cache_write_tokens": 0, "cost_usd": 0.02,
                "api_key_id": "k1", "api_key_name": "golden", "client": "claude-code",
            }),
            None,
        )
        .await
        .unwrap();
    store
        .append_request_log(
            "ok2",
            &json!({
                "protocol": "anthropic", "model": "prov/m2", "provider": "p2",
                "status": 503, "stream": false, "duration_ms": 300.0,
                "input_tokens": 5, "output_tokens": 0, "total_tokens": 5,
                "api_key_id": "k2",
            }),
            None,
        )
        .await
        .unwrap();
    store
        .append_request_log(
            "rejected",
            &json!({"protocol": "openai", "model": "m1", "status": 401, "duration_ms": 1.0}),
            None,
        )
        .await
        .unwrap();

    // REPLACE 同 id:旧增量必须被扣回(否则卷积比现场多一行)。
    store
        .append_request_log(
            "ok2",
            &json!({
                "protocol": "anthropic", "model": "prov/m2", "provider": "p2",
                "status": 200, "stream": false, "duration_ms": 800.0,
                "input_tokens": 6, "output_tokens": 2, "total_tokens": 8,
                "api_key_id": "k2",
            }),
            None,
        )
        .await
        .unwrap();

    let comparable = |summary: &Value| {
        let mut value = summary.clone();
        if let Some(map) = value.as_object_mut() {
            map.remove("generatedAt");
            // 补零空桶依赖调用时刻,不参与对账;只比非空桶。
            if let Some(series) = map.get_mut("series").and_then(Value::as_array_mut) {
                series.retain(|bucket| bucket["requestCount"] != json!(0));
            }
            if let Some(series) = map.get_mut("modelSeries").and_then(Value::as_array_mut) {
                for entry in series {
                    if let Some(points) = entry.get_mut("points").and_then(Value::as_array_mut) {
                        points.retain(|bucket| bucket["requestCount"] != json!(0));
                    }
                }
            }
        }
        value
    };

    let with_rollup = store.usage_summary(Some("24h"), &json!({})).await.unwrap();
    // 清空卷积 → 强制全量现场聚合。
    store
        .engine()
        .write(|connection| {
            connection
                .execute("DELETE FROM usage_rollup", [])
                .map_err(|e| wr_app::ports::PortError::storage(e.to_string()))?;
            Ok(())
        })
        .await
        .unwrap();
    let raw = store.usage_summary(Some("24h"), &json!({})).await.unwrap();
    assert_eq!(
        comparable(&with_rollup),
        comparable(&raw),
        "卷积路径必须与现场聚合逐字段一致"
    );
    assert_eq!(with_rollup["totals"]["requestCount"], json!(2), "被拒行不入账");
    assert_eq!(
        with_rollup["totals"]["errorCount"],
        json!(0),
        "REPLACE 后 503 旧行的错误计数被扣回"
    );
    assert_eq!(
        with_rollup["totals"]["outputTokens"],
        json!(902),
        "REPLACE 后以新 payload 计(900 + 2)"
    );
    // 被拒行不进 recent;recent 按最新 200 行候选现取。
    assert_eq!(with_rollup["recentRequests"].as_array().unwrap().len(), 2);
    store.close().await;
    std::fs::remove_dir_all(&dir).ok();
}

/// 20261104_01 阶段一 · 计量口径修正的端到端验收(A-2 / A-3 / B-3)。
///
/// 覆盖三件事:
/// 1. **A-2**:桶级 `avgDurationMs` / `cacheRatio` 不再恒 0(活跃度日历的
///    「当日总结」弹窗读的就是这两个字段)。
/// 2. **A-3**:OpenAI Chat 隐藏思考(reasoning_tokens 存在且无可测思考流)
///    从吞吐分子剥离;Gemini 上游**不扣**(output 本就排除 thoughts)。
/// 3. **B-3**:失败行(502,带 first_byte_ms、无 token)不进首字延迟均值。
///
/// 同时验证**写侧卷积与读侧现场聚合逐字段一致** —— 新口径同时落在两侧,
/// 不能只改一侧(否则同一桶里两半的数字口径不同)。
#[tokio::test]
async fn stage_one_metrics_reflect_new_definitions() {
    let (store, dir) = store("stage-one-metrics").await;
    // OpenAI Chat 上游:900 输出含 300 隐藏思考 → 可见 600;答案流 600ms。
    store
        .append_request_log(
            "ok-openai",
            &json!({
                "protocol": "openai", "model": "prov/m1", "provider": "p1",
                "status": 200, "stream": true, "duration_ms": 1_000.0,
                "first_byte_ms": 400.0,
                "answer_end_ms": 1_000.0, "first_answer_byte_ms": 400.0,
                "flow_ms": 600.0, "answer_flow_ms": 600.0,
                "input_tokens": 1_000, "output_tokens": 900, "total_tokens": 1_900,
                "cache_read_tokens": 250, "cache_write_tokens": 0, "cost_usd": 0.02,
                "reasoning_tokens": 300, "provider_protocol": "openai",
                "api_key_id": "k1", "api_key_name": "golden",
            }),
            None,
        )
        .await
        .unwrap();
    // Gemini 上游:900 答案 + 300 思考(并列,不含在 output 里)→ 分子仍是 900。
    store
        .append_request_log(
            "ok-gemini",
            &json!({
                "protocol": "openai", "model": "prov/m2", "provider": "p2",
                "status": 200, "stream": true, "duration_ms": 3_000.0,
                "first_byte_ms": 400.0,
                "answer_end_ms": 3_000.0, "first_answer_byte_ms": 400.0,
                "flow_ms": 2_600.0, "answer_flow_ms": 2_600.0,
                "input_tokens": 1_000, "output_tokens": 900, "total_tokens": 2_200,
                "cache_read_tokens": 0, "cache_write_tokens": 0, "cost_usd": 0.02,
                "reasoning_tokens": 300, "provider_protocol": "gemini",
                "api_key_id": "k1", "api_key_name": "golden",
            }),
            None,
        )
        .await
        .unwrap();
    // 502 中断:带 first_byte_ms(8s)、无 token → 不进首字延迟均值。
    store
        .append_request_log(
            "failed-502",
            &json!({
                "protocol": "openai", "model": "prov/m1", "provider": "p1",
                "status": 502, "stream": true, "duration_ms": 8_000.0,
                "first_byte_ms": 8_000.0,
                "input_tokens": 0, "output_tokens": 0, "total_tokens": 0,
                "api_key_id": "k1", "api_key_name": "golden",
            }),
            None,
        )
        .await
        .unwrap();

    let summary = store.usage_summary(Some("24h"), &json!({})).await.unwrap();

    // --- A-2:桶级字段取真值 ---
    let bucket = summary["series"]
        .as_array()
        .unwrap()
        .iter()
        .find(|bucket| bucket["requestCount"] != json!(0))
        .expect("至少一个非空桶");
    assert!(
        bucket["avgDurationMs"].as_f64().unwrap() > 0.0,
        "桶级 avgDurationMs 不得恒 0:{bucket}"
    );
    assert!(
        bucket["cacheRatio"].as_f64().unwrap() > 0.0,
        "桶级 cacheRatio 不得恒 0:{bucket}"
    );

    // --- A-3:隐藏思考剥离与全输出分流 ---
    // OpenAI Chat 可见 600 / 0.6s = 1000;Gemini 全输出 900 / 2.6s ≈ 346.15。
    // 合计分子 1500 / 3.2s = 468.75 tok/s。
    let totals_rate = summary["totals"]["avgDecodeTps"].as_f64().unwrap();
    assert!(
        (totals_rate - 468.75).abs() < 1.0,
        "吞吐分子应按隐藏思考/全输出分流,实得 {totals_rate}"
    );

    // --- B-3:失败行的 8s 首字不得进均值 ---
    // 只计两条成功行(各 400ms)→ 400ms;若失败行被计入则为 (8000+800)/3 ≈ 2933ms。
    let avg_first_byte = summary["totals"]["avgFirstByteMs"].as_f64().unwrap();
    assert!(
        (avg_first_byte - 400.0).abs() < 1.0,
        "失败行不得进首字延迟均值,实得 {avg_first_byte}"
    );

    // --- 写侧卷积 vs 读侧现场聚合:新口径必须两侧一致 ---
    let with_rollup = summary.clone();
    store
        .engine()
        .write(|connection| {
            connection
                .execute("DELETE FROM usage_rollup", [])
                .map_err(|e| wr_app::ports::PortError::storage(e.to_string()))?;
            Ok(())
        })
        .await
        .unwrap();
    let raw = store.usage_summary(Some("24h"), &json!({})).await.unwrap();
    let comparable = |summary: &Value| {
        let mut value = summary.clone();
        if let Some(map) = value.as_object_mut() {
            map.remove("generatedAt");
            // 补零空桶依赖调用时刻,不参与对账;只比非空桶。
            if let Some(series) = map.get_mut("series").and_then(Value::as_array_mut) {
                series.retain(|bucket| bucket["requestCount"] != json!(0));
            }
            if let Some(series) = map.get_mut("modelSeries").and_then(Value::as_array_mut) {
                for entry in series {
                    if let Some(points) = entry.get_mut("points").and_then(Value::as_array_mut) {
                        points.retain(|bucket| bucket["requestCount"] != json!(0));
                    }
                }
            }
            // 分组数组的**次序**在两条读路径上本就不同(卷积按
            // `bucket_epoch DESC, rowid DESC`、现场按 `created_at DESC`),
            // 与本次口径无关 → 按 key 排序后比对,只比数值。
            for key in ["models", "clientModels", "providerModels"] {
                if let Some(groups) = map.get_mut(key).and_then(Value::as_array_mut) {
                    groups.sort_by(|left, right| {
                        left["key"]
                            .as_str()
                            .unwrap_or_default()
                            .cmp(right["key"].as_str().unwrap_or_default())
                    });
                }
            }
            if let Some(series) = map.get_mut("modelSeries").and_then(Value::as_array_mut) {
                series.sort_by(|left, right| {
                    left["model"]
                        .as_str()
                        .unwrap_or_default()
                        .cmp(right["model"].as_str().unwrap_or_default())
                });
            }
        }
        value
    };
    assert_eq!(
        comparable(&with_rollup),
        comparable(&raw),
        "卷积路径必须与现场聚合逐字段一致: {:?}",
        first_diff(&comparable(&with_rollup), &comparable(&raw), "")
    );

    store.close().await;
    std::fs::remove_dir_all(&dir).ok();
}

/// 临时调试:找出两棵 JSON 的第一处不同(路径 + 两侧取值)。
fn first_diff(left: &Value, right: &Value, path: &str) -> String {
    match (left, right) {
        (Value::Object(l), Value::Object(r)) => {
            for (key, lv) in l {
                let child = format!("{path}.{key}");
                match r.get(key) {
                    Some(rv) => {
                        let diff = first_diff(lv, rv, &child);
                        if !diff.is_empty() {
                            return diff;
                        }
                    }
                    None => return format!("{child}: 只在左侧存在"),
                }
            }
            for key in r.keys() {
                if !l.contains_key(key) {
                    return format!("{path}.{key}: 只在右侧存在");
                }
            }
            String::new()
        }
        (Value::Array(l), Value::Array(r)) => {
            for (index, lv) in l.iter().enumerate() {
                let child = format!("{path}[{index}]");
                match r.get(index) {
                    Some(rv) => {
                        let diff = first_diff(lv, rv, &child);
                        if !diff.is_empty() {
                            return diff;
                        }
                    }
                    None => return format!("{child}: 右侧数组更短"),
                }
            }
            if l.len() != r.len() {
                return format!("{path}: 数组长度 {} vs {}", l.len(), r.len());
            }
            String::new()
        }
        (l, r) if l == r => String::new(),
        (l, r) => format!("{path}: {l} vs {r}"),
    }
}
