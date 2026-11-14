//! R4 存储层兼容与策略测试(方案 #14 R4.2/R4.3,约束 C2/C5)。
//!
//! 分三个测试文件,各自对应一组约束:
//! - 本文件:`sqlite_snapshot_compat.rs` 之外,专测 **C5 键名持久化** 与
//!   **C2 api_key 三态**、metadata/onboarding、runtime_status;
//! - `sqlite_snapshot_compat.rs`:R0.4 真实旧库夹具的直读写零迁移;
//! - `sqlite_log_policy.rs`:C3 日志数据策略 + 用量入账。

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use wr_app::ports::{ConfigStore, RequestLogStore, SecretStore, UsageStore};
use wr_infra::config::AppConfig;
use wr_infra::sqlite::SqliteStore;

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wr-r4-storage-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn store(name: &str) -> (SqliteStore, PathBuf) {
    let dir = temp_dir(name);
    let store = SqliteStore::open(&dir).await.unwrap();
    (store, dir)
}

fn provider(id: &str, api_key: Value) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("id".to_owned(), json!(id));
    map.insert("name".to_owned(), json!(id));
    map.insert("api_base_url".to_owned(), json!("http://127.0.0.1:1/v1"));
    map.insert("protocol".to_owned(), json!("openai"));
    map.insert("models".to_owned(), json!(["m-test"]));
    map.insert("api_key".to_owned(), api_key);
    Value::Object(map)
}

fn payload_with(providers: Vec<Value>) -> Value {
    let config = AppConfig {
        providers: providers
            .iter()
            .filter_map(Value::as_object)
            .cloned()
            .map(|raw| wr_infra::config::ProviderRecord { raw })
            .collect(),
        ..AppConfig::default()
    };
    config.to_dict(true)
}

/// C5:`app_config` payload 的键名必须保持持久化形式(`Providers` /
/// `APIKEYS` 大写),Rust 写的行 Python 能直接读。
#[tokio::test]
async fn sqlite_persisted_key_names_are_capitalized() {
    let (store, dir) = store("keynames").await;
    store
        .save_payload(&payload_with(vec![provider("p1", json!("k1"))]))
        .await
        .unwrap();
    let raw = store.load_payload().await.unwrap();
    let map = raw.as_object().unwrap();
    assert!(map.contains_key("Providers"), "C5 大写 P");
    assert!(map.contains_key("APIKEYS"), "C5 大写 APIKEYS");
    assert!(map.contains_key("version"));
    assert!(map.contains_key("preferredProvider"));
    assert!(map.contains_key("Router"));
    // 真实密钥绝不落在主 payload 里(落 secrets 表)。
    assert_eq!(map["Providers"][0]["api_key"], json!(""));
    let text = std::fs::read_to_string(store.path()).is_ok();
    assert!(text, "库文件应存在");
    std::fs::remove_dir_all(&dir).ok();
}

/// C2 之一:payload 里 `api_key` 缺失 → 已存密钥不动。
/// (read-modify-write 链路只带部分 provider 字段,不能顺手抹密钥)
#[tokio::test]
async fn apikey_threestate_absent_keeps_existing_secret() {
    let (store, dir) = store("absent").await;
    store.upsert_provider_secret("p1", "keep-me").await.unwrap();
    let mut payload = payload_with(vec![provider("p1", json!("fresh"))]);
    payload["Providers"][0]
        .as_object_mut()
        .unwrap()
        .remove("api_key");
    store.save_payload(&payload).await.unwrap();
    assert_eq!(
        store.provider_secret("p1").await.unwrap(),
        "keep-me",
        "键缺失必须不动已存密钥"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// C2 之二:空串 `api_key` → 已存密钥不动(2026-08 修的那次事故的根因)。
#[tokio::test]
async fn apikey_threestate_empty_keeps_existing_secret() {
    let (store, dir) = store("empty").await;
    store.upsert_provider_secret("p1", "keep-me").await.unwrap();
    store
        .save_payload(&payload_with(vec![provider("p1", json!(""))]))
        .await
        .unwrap();
    assert_eq!(
        store.provider_secret("p1").await.unwrap(),
        "keep-me",
        "空串不得清空密钥"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// C2 之三:显式 `null` → 删除已存密钥。
#[tokio::test]
async fn apikey_threestate_null_clears_existing_secret() {
    let (store, dir) = store("null").await;
    store.upsert_provider_secret("p1", "bye").await.unwrap();
    store
        .save_payload(&payload_with(vec![provider("p1", Value::Null)]))
        .await
        .unwrap();
    assert_eq!(store.provider_secret("p1").await.unwrap(), "");
    std::fs::remove_dir_all(&dir).ok();
}

/// C2 之四:非空值 → upsert;`"***"`(前端掩码回传)→ 不动。
#[tokio::test]
async fn apikey_threestate_value_upserts_and_mask_is_noop() {
    let (store, dir) = store("upsert").await;
    store
        .save_payload(&payload_with(vec![provider("p1", json!("first"))]))
        .await
        .unwrap();
    assert_eq!(store.provider_secret("p1").await.unwrap(), "first");
    store
        .save_payload(&payload_with(vec![provider("p1", json!("***"))]))
        .await
        .unwrap();
    assert_eq!(
        store.provider_secret("p1").await.unwrap(),
        "first",
        "掩码值不得覆盖真密钥"
    );
    store
        .save_payload(&payload_with(vec![provider("p1", json!("second"))]))
        .await
        .unwrap();
    assert_eq!(store.provider_secret("p1").await.unwrap(), "second");
    std::fs::remove_dir_all(&dir).ok();
}

/// 落库 payload 的 `api_key` 被搬到对象末尾(Python `pop` 再赋值的副作用),
/// 且恒为空串 —— R4.12 的 db 文本 diff 依赖这个形态。
#[tokio::test]
async fn save_payload_blanks_api_key_at_end_of_object() {
    let (store, dir) = store("order").await;
    store
        .save_payload(&payload_with(vec![provider("p1", json!("k1"))]))
        .await
        .unwrap();
    let raw = store.load_payload().await.unwrap();
    let keys: Vec<String> = raw["Providers"][0]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(keys.last().unwrap(), "api_key");
    assert_eq!(raw["Providers"][0]["api_key"], json!(""));
    std::fs::remove_dir_all(&dir).ok();
}

/// A5 多账号:保存时同步 `provider_accounts` 行 + 账号级密钥,
/// 且 `created_at` 首次创建后不再被后续保存改写。
#[tokio::test]
async fn save_payload_upserts_account_pool_and_keeps_created_at() {
    let (store, dir) = store("accounts").await;
    let mut provider = provider("p1", json!(""));
    provider["accounts"] = json!([
        {"id": "a1", "label": "primary", "api_key": "secret-a", "priority": 2, "weight": 3,
         "account_config": {"region": "eu"}}
    ]);
    store
        .save_payload(&payload_with(vec![provider.clone()]))
        .await
        .unwrap();
    let accounts = store.list_accounts("p1").await.unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0]["id"], json!("a1"));
    assert_eq!(accounts[0]["label"], json!("primary"));
    assert_eq!(accounts[0]["priority"], json!(2));
    assert_eq!(accounts[0]["weight"], json!(3));
    assert_eq!(accounts[0]["account_config"]["region"], json!("eu"));
    assert_eq!(store.account_secret("p1", "a1").await.unwrap(), "secret-a");
    let created = first_created_at(&dir).await;
    store
        .save_payload(&payload_with(vec![provider]))
        .await
        .unwrap();
    assert_eq!(
        first_created_at(&dir).await,
        created,
        "created_at 必须保住首次值(COALESCE(NULLIF(...)))"
    );
    std::fs::remove_dir_all(&dir).ok();
}

async fn first_created_at(dir: &Path) -> String {
    let store = SqliteStore::open(dir).await.unwrap();
    store
        .engine()
        .read(|conn| {
            conn.query_row(
                "SELECT created_at FROM provider_accounts WHERE account_id='a1'",
                [],
                |row| row.get::<_, String>(0),
            )
            .map_err(|e| wr_app::ports::PortError::storage(e.to_string()))
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn metadata_and_onboarding_roundtrip() {
    let (store, dir) = store("metadata").await;
    assert!(!store.get_onboarding_finished().await.unwrap());
    store.set_onboarding_finished(true).await.unwrap();
    assert!(store.get_onboarding_finished().await.unwrap());
    store.set_metadata("greeting", "hi").await.unwrap();
    assert_eq!(
        store.get_metadata("greeting").await.unwrap().as_deref(),
        Some("hi")
    );
    assert_eq!(store.get_metadata("missing").await.unwrap(), None);
    // Python 的判定是 str(value).lower() in ("true","1")。
    store
        .set_metadata("onboarding_finished", "1")
        .await
        .unwrap();
    assert!(store.get_onboarding_finished().await.unwrap());
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn runtime_status_counts_match_python_shape() {
    let (store, dir) = store("status").await;
    store
        .save_payload(&payload_with(vec![provider("p1", json!("k1"))]))
        .await
        .unwrap();
    store
        .append_request_log("r1", &json!({"protocol": "openai"}), None)
        .await
        .unwrap();
    let status = store.runtime_status().await.unwrap();
    assert_eq!(status["store"], json!("sqlite"));
    assert_eq!(status["configCount"], json!(1));
    assert_eq!(status["providerSecretCount"], json!(1));
    assert_eq!(status["requestLogCount"], json!(1));
    assert_eq!(status["path"], json!(store.path().display().to_string()));
    std::fs::remove_dir_all(&dir).ok();
}

/// 调用日志合并查询(Rust 新增;无 Python 镜像):
/// - 在途行(pending/processing)排在落库行之前,共用过滤/计数/分页;
/// - 落库行状态由 ok + end_reason 推导:client_disconnected → canceled,
///   ok → completed,其余 → failed;
/// - statusCounts 为五档 + all;status 筛选在五档值上生效。
#[tokio::test]
async fn call_log_query_merges_in_flight_and_derives_status() {
    let (store, dir) = store("calllog-status").await;
    // payload 里不含 request_id(主键由 append_request_log 的首参决定),
    // 故闭包不收 request_id —— 避免同一个 ID 在调用处写两遍而写岔。
    let row = |status: i64, end_reason: &str| {
        json!({
            "protocol": "openai",
            "model": "m1",
            "status": status,
            "stream": false,
            "duration_ms": 12.0,
            "provider_attempts": [],
            "stream_end_reason": end_reason,
            "reasoning_effort": "high",
        })
    };
    store
        .append_request_log("r-ok", &row(200, "completed"), None)
        .await
        .unwrap();
    store
        .append_request_log("r-fail", &row(502, "upstream_error"), None)
        .await
        .unwrap();
    store
        .append_request_log("r-cancel", &row(499, "client_disconnected"), None)
        .await
        .unwrap();

    let in_flight = vec![
        json!({
            "id": -1, "requestId": "in-1", "createdAt": "2026-09-26T00:00:02Z",
            "modelLabel": "p1/m1", "baseModel": "p1/m1", "status": "pending",
            "ok": false, "statusCode": 0, "isStream": false,
        }),
        json!({
            "id": -2, "requestId": "in-2", "createdAt": "2026-09-26T00:00:01Z",
            "modelLabel": "p1/m1", "baseModel": "p1/m1", "status": "processing",
            "ok": false, "statusCode": 0, "isStream": true,
        }),
    ];
    let page = store
        .query_request_logs(&json!({}), &in_flight)
        .await
        .unwrap();

    let items = page["items"].as_array().unwrap();
    assert_eq!(items.len(), 5, "2 在途 + 3 落库");
    // 在途行在最前:新登记(id=-1)更靠前。
    assert_eq!(items[0]["requestId"], json!("in-1"));
    assert_eq!(items[0]["status"], json!("pending"));
    assert_eq!(items[1]["status"], json!("processing"));
    // 落库行状态推导:canceled 优先于 ok 判定(499)。
    assert_eq!(items[2]["requestId"], json!("r-cancel"));
    assert_eq!(items[2]["status"], json!("canceled"));
    assert_eq!(items[3]["status"], json!("failed"));
    assert_eq!(items[4]["status"], json!("completed"));
    // 投影携带思考强度。
    assert_eq!(items[4]["reasoningEffort"], json!("high"));
    let counts = &page["statusCounts"];
    assert_eq!(counts["all"], json!(5));
    assert_eq!(counts["pending"], json!(1));
    assert_eq!(counts["processing"], json!(1));
    assert_eq!(counts["completed"], json!(1));
    assert_eq!(counts["failed"], json!(1));
    assert_eq!(counts["canceled"], json!(1));

    // status 筛选落在五档值上(pending 只中在途行)。
    let pending_page = store
        .query_request_logs(&json!({"status": "pending"}), &in_flight)
        .await
        .unwrap();
    let pending_items = pending_page["items"].as_array().unwrap();
    assert_eq!(pending_items.len(), 1);
    assert_eq!(pending_items[0]["requestId"], json!("in-1"));
    assert_eq!(pending_page["total"], json!(1));
    std::fs::remove_dir_all(&dir).ok();
}

/// 20261104_01 阶段三 · B-1a:调用日志条目带 `client`(调用方标识),且能按它
/// 自由文本检索 —— payload 里一直有 `client` 键,投影此前**不带** → 前端
/// 契约 `RequestLogEntry.client` 恒为 undefined。
#[tokio::test]
async fn call_log_entry_exposes_client_and_is_searchable_by_it() {
    let (store, dir) = store("calllog-client").await;
    // payload 里不含 request_id(主键由 append_request_log 的首参决定)。
    let row = |client: &str| {
        json!({
            "protocol": "openai",
            "model": "m1",
            "provider": "p1",
            "status": 200,
            "stream": false,
            "duration_ms": 12.0,
            "provider_attempts": [],
            "stream_end_reason": "completed",
            "client": client,
        })
    };
    store
        .append_request_log("r-cc", &row("claude-cli/2.1.123"), None)
        .await
        .unwrap();
    store
        .append_request_log("r-curl", &row("curl/8.5.0"), None)
        .await
        .unwrap();

    let page = store.query_request_logs(&json!({}), &[]).await.unwrap();
    let items = page["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    let client_of = |request_id: &str| -> Value {
        items
            .iter()
            .find(|item| item["requestId"] == json!(request_id))
            .map(|item| item["client"].clone())
            .unwrap_or(Value::Null)
    };
    assert_eq!(client_of("r-cc"), json!("claude-cli/2.1.123"));
    assert_eq!(client_of("r-curl"), json!("curl/8.5.0"));

    // 自由文本检索(filter 键是 `query`)命中 client 片段。
    let searched = store
        .query_request_logs(&json!({"query": "claude-cli"}), &[])
        .await
        .unwrap();
    let searched_items = searched["items"].as_array().unwrap();
    assert_eq!(searched_items.len(), 1);
    assert_eq!(searched_items[0]["requestId"], json!("r-cc"));
    std::fs::remove_dir_all(&dir).ok();
}

/// 2026-10-08 重试记录丰富化:结构化 `retry_attempts` 键优先,旧行
/// (只有 provider_attempts)回落合成(delayMs 恒 0)。
#[tokio::test]
async fn call_log_retry_attempts_rich_over_legacy() {
    let (store, dir) = store("calllog-retry").await;
    // 新行:网关落库的结构化明细(等待/状态/错误/耗时)。
    let rich = json!({
            "protocol": "openai",
            "model": "m1",
            "status": 200,
            "stream": false,
            "duration_ms": 3200.0,
            "provider_attempts": ["p1", "p1", "p1"],
            "retry_attempts": [
                {"provider": "p1", "model": "m-test", "account": null, "status": 0,
                 "error": "upstream read timed out", "duration_ms": 50.0,
                 "wait_ms": 1000.0, "wait_unit": "ms", "wait_source": "backoff",
                 "started_at": "2026-10-08T00:00:00Z"},
                {"provider": "p1", "model": "m-test", "account": "acct-1", "status": 429,
                 "error": "rate limited", "duration_ms": 120.5,
                 "wait_ms": 30000.0, "wait_unit": "ms", "wait_source": "retry-after",
                 "started_at": "2026-10-08T00:00:01Z"},
                {"provider": "p1", "model": "m-test", "account": "acct-1", "status": 200,
                 "error": null, "duration_ms": 900.0,
                 "wait_ms": 0.0, "wait_unit": "ms", "wait_source": null,
                 "started_at": "2026-10-08T00:00:31Z",
                 "attempt_id": "p1#3",
                 "upstream_request": "{\"model\":\"m-test\"}",
                 "upstream_request_headers": {"content-type": "application/json", "authorization": "***"},
                 "upstream_response": "{\"id\":\"c1\"}",
                 "upstream_headers": {"content-type": "application/json"},
                 "conversion": {"strategy": "semantic", "diagnostics": []},
                 "upstream_response_size": 12,
                 "upstream_response_truncated": false}
            ],
            "stream_end_reason": "completed",
    });
    store
        .append_request_log("r-rich", &rich, None)
        .await
        .unwrap();
    // 旧行:只有 provider_attempts(历史形态)。
    let legacy = json!({
            "protocol": "openai",
            "model": "m1",
            "status": 502,
            "stream": false,
            "duration_ms": 10.0,
            "provider_attempts": ["p1", "p2"],
            "stream_end_reason": "no_provider",
    });
    store
        .append_request_log("r-legacy", &legacy, None)
        .await
        .unwrap();

    let page = store.query_request_logs(&json!({}), &[]).await.unwrap();
    let items = page["items"].as_array().unwrap();
    let rich_row = items
        .iter()
        .find(|row| row["requestId"] == json!("r-rich"))
        .unwrap();
    let attempts = rich_row["retryAttempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 3);
    assert_eq!(attempts[0]["attempt"], json!(1));
    assert_eq!(attempts[0]["delayMs"], json!(1000.0), "退避等待入表");
    assert_eq!(attempts[0]["statusCode"], json!(0), "传输错误状态记 0");
    assert_eq!(attempts[0]["error"], json!("upstream read timed out"));
    assert_eq!(attempts[0]["durationMs"], json!(50.0));
    assert_eq!(attempts[0]["waitSource"], json!("backoff"));
    assert_eq!(attempts[0]["final"], json!(false));
    assert_eq!(
        attempts[1]["delayMs"],
        json!(30000.0),
        "Retry-After 等待入表"
    );
    assert_eq!(attempts[1]["waitSource"], json!("retry-after"));
    assert_eq!(attempts[1]["statusCode"], json!(429));
    assert_eq!(attempts[1]["account"], json!("acct-1"));
    assert_eq!(attempts[2]["final"], json!(true));
    assert_eq!(attempts[2]["delayMs"], json!(0.0));
    assert_eq!(attempts[2]["statusCode"], json!(200));
    assert_eq!(attempts[2]["provider"], json!("p1"));
    assert_eq!(attempts[2]["model"], json!("m-test"));
    assert_eq!(attempts[2]["startedAt"], json!("2026-10-08T00:00:31Z"));
    // D3/D4:稳定 attempt id 透出到调用日志行。20261017_02 批5:列表行的
    // 采集正文拆侧表按需读取 —— 条目只带 captured 标记,不带正文键。
    assert_eq!(attempts[2]["attemptId"], json!("p1#3"));
    assert_eq!(attempts[2]["captured"], json!(true), "瘦身影带 captured 标记");
    assert_eq!(attempts[2]["upstreamRequest"], json!(null), "列表行不带采集正文");
    assert_eq!(attempts[2]["upstreamRequestHeaders"], json!(null), "请求头同样拆侧表");
    assert_eq!(attempts[2]["upstreamResponse"], json!(null));
    assert_eq!(attempts[2]["upstreamResponseSize"], json!(12));
    assert_eq!(attempts[2]["upstreamResponseTruncated"], json!(false));
    // 未采集的 attempt 不得凭空出现采集键/标记。
    assert_eq!(attempts[0]["attemptId"], json!(null));
    assert_eq!(attempts[0]["upstreamRequest"], json!(null));
    assert_eq!(attempts[0]["upstreamResponse"], json!(null));
    assert_eq!(attempts[0]["captured"], json!(null));

    // 详情读取(get_request_log)按需并回:正文四件套完整可见(双源兼容
    // 的「新格式」腿;批5 后正文存 request_log_attempts 侧表)。
    let detail = store
        .get_request_log("r-rich")
        .await
        .unwrap()
        .expect("detail row exists");
    let detail_attempts = detail["retryAttempts"].as_array().unwrap();
    // 并回 = 侧表原始 attempt 对象整项替换(captured 是列表瘦身影的标记,
    // 详情用正文键存在性判断,不需要它)。
    assert_eq!(
        detail_attempts[2]["upstreamRequest"],
        json!("{\"model\":\"m-test\"}")
    );
    assert_eq!(detail_attempts[2]["upstreamResponse"], json!("{\"id\":\"c1\"}"));
    assert_eq!(
        detail_attempts[2]["upstreamHeaders"]["content-type"],
        json!("application/json")
    );
    // 上游请求头并回可见(网关侧已打码认证键;此处断言透传不丢键)。
    assert_eq!(
        detail_attempts[2]["upstreamRequestHeaders"]["content-type"],
        json!("application/json")
    );
    assert_eq!(
        detail_attempts[2]["upstreamRequestHeaders"]["authorization"],
        json!("***")
    );
    assert_eq!(detail_attempts[2]["conversion"]["strategy"], json!("semantic"));

    // 旧行回落合成:status 列仍是 provider id 串,delayMs 恒 0。
    let legacy_row = items
        .iter()
        .find(|row| row["requestId"] == json!("r-legacy"))
        .unwrap();
    let legacy_attempts = legacy_row["retryAttempts"].as_array().unwrap();
    assert_eq!(legacy_attempts.len(), 2);
    assert_eq!(legacy_attempts[0]["status"], json!("p1"));
    assert_eq!(legacy_attempts[0]["delayMs"], json!(0));
    assert_eq!(
        legacy_attempts[0]["statusCode"],
        json!(null),
        "合成行无状态码"
    );
    assert_eq!(legacy_attempts[1]["final"], json!(true));
    std::fs::remove_dir_all(&dir).ok();
}

/// 2026-10-20:历史行 `wait_ms` 实为秒(旧写入 bug),无 `wait_unit`
/// 标记 —— 读取侧必须换算成毫秒再投影,否则展示缩小 1000 倍。
#[tokio::test]
async fn call_log_retry_attempts_legacy_wait_seconds_convert_to_ms() {
    let (store, dir) = store("calllog-legacy-wait").await;
    let row = json!({
        "protocol": "openai",
        "model": "m1",
        "status": 200,
        "stream": true,
        "duration_ms": 3300.0,
        "provider_attempts": ["p1", "p1"],
        "retry_attempts": [
            {"provider": "p1", "status": 0, "error": "timeout", "duration_ms": 50.0,
             "wait_ms": 1.4325796, "wait_source": "backoff",
             "started_at": "2026-10-19T00:00:00Z"},
            {"provider": "p1", "status": 200, "duration_ms": 900.0,
             "wait_ms": 0.0, "wait_source": null,
             "started_at": "2026-10-19T00:00:02Z"}
        ]
    });
    store
        .append_request_log("r-legacy-wait", &row, None)
        .await
        .unwrap();
    let page = store.query_request_logs(&json!({}), &[]).await.unwrap();
    let item = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["requestId"] == json!("r-legacy-wait"))
        .unwrap();
    let attempts = item["retryAttempts"].as_array().unwrap();
    assert_eq!(
        attempts[0]["delayMs"],
        json!(1432.58),
        "历史秒值应换算成毫秒并保留两位小数"
    );
    assert_eq!(attempts[1]["delayMs"], json!(0.0));
    std::fs::remove_dir_all(&dir).ok();
}

/// 20261017_02 批1:投影缓存增量化 —— 新行落库后查询走增量合并,
/// 列表内容/顺序/总数与全量重建完全一致(增量对行为透明)。
#[tokio::test]
async fn call_log_projection_increments_on_new_rows() {
    let (store, dir) = store("calllog-increment").await;
    let row = |model: &str| {
        json!({
            "protocol": "openai",
            "model": model,
            "status": 200,
            "stream": false,
            "duration_ms": 5.0,
            "provider_attempts": [],
            "stream_end_reason": "completed",
        })
    };
    store
        .append_request_log("r-1", &row("m1"), None)
        .await
        .unwrap();
    let first = store.query_request_logs(&json!({}), &[]).await.unwrap();
    assert_eq!(first["total"], json!(1));
    assert_eq!(first["items"][0]["requestId"], json!("r-1"));

    // 落第二行 → version 推进 → 增量合并:新行在最前,旧行保留,
    // totalRows(全表计数)随增量推进。
    store
        .append_request_log("r-2", &row("m2"), None)
        .await
        .unwrap();
    let second = store.query_request_logs(&json!({}), &[]).await.unwrap();
    let items = second["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "增量合并不丢旧行");
    assert_eq!(items[0]["requestId"], json!("r-2"), "新行在前");
    assert_eq!(items[1]["requestId"], json!("r-1"));
    assert_eq!(second["total"], json!(2));
    // 增量行同样进入筛选下拉项。
    let models = second["options"]["models"].as_array().unwrap();
    assert!(models.len() >= 2, "增量行的模型进入下拉项:{models:?}");

    // 第三行继续增量,顺序稳定。
    store
        .append_request_log("r-3", &row("m1"), None)
        .await
        .unwrap();
    let third = store.query_request_logs(&json!({}), &[]).await.unwrap();
    let items = third["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(items[0]["requestId"], json!("r-3"));
    assert_eq!(third["total"], json!(3));
    std::fs::remove_dir_all(&dir).ok();
}

/// 20261017_02 批1:INSERT OR REPLACE 改写历史行(同 request_id 重落)
/// 时,增量「只加不删」会让缓存残留旧副本 —— 新行 request_id 命中缓存
/// 已知集合必须回退全量重建,最终列表无重复且内容为改写后的新值。
#[tokio::test]
async fn call_log_projection_replace_falls_back_to_full() {
    let (store, dir) = store("calllog-replace").await;
    let row = |model: &str, created: &str| {
        json!({
            "protocol": "openai",
            "model": model,
            "status": 200,
            "stream": false,
            "duration_ms": 5.0,
            "provider_attempts": [],
            "stream_end_reason": "completed",
            "completed_at": created,
        })
    };
    // 两次 append 让投影缓存建立(第二次触发一次增量也无妨)。
    store
        .append_request_log("r-old", &row("m1", "2026-10-17T00:00:00Z"), None)
        .await
        .unwrap();
    store
        .query_request_logs(&json!({}), &[])
        .await
        .unwrap();
    // REPLACE 同 request_id:行内容/rowid 更新,投影必须反映新值。
    store
        .append_request_log("r-old", &row("m1-rewritten", "2026-10-17T00:00:00Z"), None)
        .await
        .unwrap();
    let page = store.query_request_logs(&json!({}), &[]).await.unwrap();
    let items = page["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "REPLACE 不产生重复行");
    assert_eq!(
        items[0]["baseModel"],
        json!("m1-rewritten"),
        "读到改写后的值"
    );
    // created_at 相同时按 rowid 降序兜底有序,不触发乱序回退路径之外
    // 的行为;列表查询正常返回。
    assert_eq!(page["total"], json!(1));
    std::fs::remove_dir_all(&dir).ok();
}

/// 阶段四 C-8:写入侧分桶 → 读侧 P50 / P95 的往返(直方图表)。
#[tokio::test]
async fn usage_histogram_roundtrip_yields_quantiles() {
    let (store, dir) = store("hist").await;
    // 10 条流式成功行:首字 100ms / 200ms 各 5 条;输出 900 token、
    // 答案完成 10s,内容跨度 9.9s / 9.8s(均匀流式 → 非突发)。
    for index in 0..10 {
        let first_byte = if index < 5 { 100.0 } else { 200.0 };
        let flow = if index < 5 { 9_900.0 } else { 9_800.0 };
        store
            .append_request_log(
                &format!("r{index}"),
                &json!({
                    "protocol": "openai",
                    "model": "m1",
                    "provider": "p1",
                    "status": 200,
                    "stream": true,
                    "duration_ms": 10_000.0,
                    "first_byte_ms": first_byte,
                    "answer_end_ms": 10_000.0,
                    "first_answer_byte_ms": first_byte,
                    "flow_ms": flow,
                    "answer_flow_ms": flow,
                    "input_tokens": 100,
                    "output_tokens": 900,
                    "total_tokens": 1000,
                }),
                None,
            )
            .await
            .unwrap();
    }
    let summary = store
        .usage_summary(Some("today"), &json!({}))
        .await
        .unwrap();
    let totals = &summary["totals"];
    // TTFT:5 条落在 100–110ms 桶,5 条落在 200–210ms 桶 → P50 取上界
    // 110ms、P95 取上界 210ms(保守口径)。
    assert_eq!(totals["p50FirstByteMs"].as_f64(), Some(110.0));
    assert_eq!(totals["p95FirstByteMs"].as_f64(), Some(210.0));
    // 解码速度:首字 100ms 的 5 条 → 900/9.9s ≈ 90.9 tok/s(桶 [90,91));
    // 首字 200ms 的 5 条 → 900/9.8s ≈ 91.8 tok/s(桶 [91,92))。
    // P50 落在第一个桶(上界 91),P95 落在第二个桶(上界 92)。
    assert_eq!(totals["p50DecodeTps"].as_f64(), Some(91.0));
    assert_eq!(totals["p95DecodeTps"].as_f64(), Some(92.0));
    // 均值口径不受影响(分位数是新增列,不改既有聚合)。
    assert_eq!(totals["requestCount"].as_i64(), Some(10));
    std::fs::remove_dir_all(&dir).ok();
}

/// 20261108_01 §13:调用日志逐行输出解码速度 / 输出速度 / 解码来源;
/// 失败/取消行两者均为 null。
#[tokio::test]
async fn call_log_entry_reports_decode_and_output_speeds() {
    let (store, dir) = store("calllog-speeds").await;
    let base = |status: u16, end_reason: &str| {
        json!({
            "protocol": "openai",
            "model": "m1",
            "provider": "p1",
            "status": status,
            "stream": true,
            "stream_end_reason": end_reason,
            "duration_ms": 10_000.0,
            "first_byte_ms": 1_000.0,
            "answer_end_ms": 10_000.0,
            "first_answer_byte_ms": 1_000.0,
            "flow_ms": 9_000.0,
            "answer_flow_ms": 9_000.0,
            "input_tokens": 100,
            "output_tokens": 900,
            "total_tokens": 1_000,
        })
    };
    store
        .append_request_log("ok", &base(200, "completed"), None)
        .await
        .unwrap();
    store
        .append_request_log("canceled", &base(499, "client_disconnected"), None)
        .await
        .unwrap();

    let page = store.query_request_logs(&json!({}), &[]).await.unwrap();
    let items = page["items"].as_array().unwrap();
    let entry = |request_id: &str| {
        items
            .iter()
            .find(|item| item["requestId"] == json!(request_id))
            .unwrap()
    };
    let ok = entry("ok");
    assert_eq!(ok["decodeTps"], json!(100.0), "900 tok / 9s");
    assert_eq!(ok["outputTps"], json!(90.0), "900 tok / 10s");
    assert_eq!(ok["decodeSource"], json!("full_flow"));
    assert_eq!(ok["status"], json!("completed"));

    let canceled = entry("canceled");
    assert_eq!(canceled["decodeTps"], json!(null), "取消行不产出解码速度");
    assert_eq!(canceled["outputTps"], json!(null), "取消行不产出输出速度");
    assert_eq!(canceled["decodeSource"], json!(null));
    assert_eq!(canceled["status"], json!("canceled"));
    std::fs::remove_dir_all(&dir).ok();
}
