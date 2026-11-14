//! R4.2 C5 零迁移验收:用 R0.4 固化的**真实旧库夹具**
//! (`testdata/golden/sqlite-snapshot/{schema.sql,rows.json}`)直接读写。
//!
//! 夹具是从一个跑过真实流量的 Python 库导出的(密钥三重替换 + 列名级脱敏)。
//! 这里把它重建为 sqlite 文件,要求 Rust 存储层:
//! 1. 不做任何迁移即可读出全部既有行(表名/列名/键名逐位兼容);
//! 2. 写入后 Python 侧的读路径仍然成立(payload 文本形态、`secrets` 命名空间);
//! 3. `bootstrap` 对旧库只补 `IF NOT EXISTS` 的新表,不动已有行。

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::{Value, json};

use wr_app::ports::{
    BreakerStateStore, ConfigStore, IdempotencyStore, RequestLogStore, SecretStore, UsageStore,
};
use wr_infra::config::AppConfig;
use wr_infra::sqlite::SqliteStore;

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../testdata/golden/sqlite-snapshot")
}

fn load_fixture_rows() -> Value {
    let path = fixture_dir().join("rows.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {} failed: {e}", path.display()));
    serde_json::from_str(&text).expect("rows.json must be valid JSON")
}

/// 从夹具重建一个与 Python 产出同形的库(先表后索引,再按列插行)。
fn build_fixture_db(dir: &Path) -> PathBuf {
    let schema = std::fs::read_to_string(fixture_dir().join("schema.sql")).unwrap();
    let rows = load_fixture_rows();
    let statements: Vec<&str> = schema
        .split(';')
        .map(str::trim)
        .filter(|statement| !statement.is_empty())
        .collect();
    let path = dir.join("config.sqlite");
    let connection = Connection::open(&path).unwrap();
    for pass in [false, true] {
        for statement in &statements {
            let is_index = statement.to_ascii_lowercase().starts_with("create index");
            if is_index != pass {
                continue;
            }
            connection
                .execute_batch(statement)
                .unwrap_or_else(|e| panic!("fixture statement failed: {e}\n{statement}"));
        }
    }
    let table_names: Vec<String> = connection
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    for table in &table_names {
        let Some(items) = rows.get(table).and_then(Value::as_array) else {
            continue;
        };
        for item in items {
            let Some(map) = item.as_object() else {
                continue;
            };
            if map.is_empty() {
                continue;
            }
            let columns: Vec<&String> = map.keys().collect();
            let placeholders = vec!["?"; columns.len()].join(", ");
            let sql = format!(
                "INSERT INTO {table}({}) VALUES({placeholders})",
                columns
                    .iter()
                    .map(|name| name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            let values: Vec<Value> = columns
                .iter()
                .filter_map(|name| map.get(*name))
                .cloned()
                .collect();
            let bound: Vec<String> = values
                .iter()
                .map(|value| match value {
                    Value::Null => String::new(),
                    Value::String(text) => text.clone(),
                    other => other.to_string(),
                })
                .collect();
            let is_null: Vec<bool> = values.iter().map(|value| value.is_null()).collect();
            let mut statement = connection.prepare(&sql).unwrap();
            for (index, (text, null)) in bound.iter().zip(is_null.iter()).enumerate() {
                if *null {
                    statement
                        .raw_bind_parameter(index + 1, rusqlite::types::Null)
                        .unwrap();
                } else {
                    statement
                        .raw_bind_parameter(index + 1, text.as_str())
                        .unwrap();
                }
            }
            statement.raw_execute().unwrap();
        }
    }
    drop(connection);
    path
}

/// 旧库可以原样打开;旧行只保证「可列出/元数据可读」,正文与 attempts
/// 采集按最新拆分格式读取(旧内嵌形态不再并回)。
#[tokio::test]
async fn legacy_snapshot_opens_without_migration() {
    let dir = std::env::temp_dir().join(format!("wr-r4-snap-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    build_fixture_db(&dir);

    // Rust 打开同一个文件:透明补建缺失表,但绝不改已有行。
    let store = SqliteStore::open(&dir).await.unwrap();

    // 配置 payload 可解析(C9/C5 键名)。
    let payload = store.load_payload().await.unwrap();
    let config = AppConfig::from_json(&payload).expect("fixture payload must parse");
    let rows = load_fixture_rows();
    let stored = rows["app_config"][0]["payload"].as_str().unwrap();
    assert_eq!(
        wr_domain::json_py::py_json_dumps(&payload),
        stored,
        "读回来的 payload 必须与库中字节同形"
    );
    assert!(config.version >= 2, "from_json 必须把 version 归一到 >=2");

    // 密钥命名空间可读(脱敏夹具里的名字保持 provider:*:api_key 形态)。
    let secret_names: Vec<String> = rows["secrets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["name"].as_str().unwrap().to_owned())
        .collect();
    assert!(
        secret_names
            .iter()
            .any(|name| name.starts_with("provider:")),
        "夹具应含 provider 级密钥行: {secret_names:?}"
    );
    for name in &secret_names {
        let value = rows["secrets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["name"].as_str() == Some(name.as_str()))
            .unwrap()["value"]
            .as_str()
            .unwrap();
        if let Some(provider_id) = name
            .strip_prefix("provider:")
            .and_then(|rest| rest.strip_suffix(":api_key"))
        {
            assert_eq!(
                store.provider_secret(provider_id).await.unwrap(),
                value,
                "secrets 表读取必须与 Python 命名空间一致"
            );
        }
    }

    // 请求日志行可读且列表按 created_at 倒序。
    let logs = store.list_request_logs(100).await.unwrap();
    assert_eq!(
        logs.len(),
        rows["request_logs"].as_array().unwrap().len(),
        "旧库日志行数必须一致"
    );
    let detail = store
        .get_request_log(logs[0]["request_id"].as_str().unwrap())
        .await
        .unwrap()
        .expect("detail row");
    assert!(detail.get("request_id").is_some(), "详情必须带 request_id");

    // 熔断/幂等旧行同样可读(键类型 int 兼容)。
    let breaker_rows = rows["circuit_breaker_state"].as_array().unwrap();
    if let Some(first) = breaker_rows.first() {
        let provider_id = first["provider_id"].as_str().unwrap();
        let loaded = store
            .load_breaker_state(provider_id)
            .await
            .unwrap()
            .expect("legacy breaker state");
        assert_eq!(
            loaded.consecutive_failures,
            first["consecutive_failures"].as_i64().unwrap()
        );
    }
    let idem_rows = rows["idempotency_entries"].as_array().unwrap();
    if let Some(first) = idem_rows.first() {
        let key = first["store_key"].as_str().unwrap();
        let loaded = store
            .load_idempotency(key)
            .await
            .unwrap()
            .expect("legacy entry");
        assert_eq!(loaded.status_code, first["status_code"].as_i64().unwrap());
        assert_eq!(loaded.payload, first["payload"].as_str().unwrap());
    }

    // 写路径落在同一个库上仍成立(零迁移后读写皆可)。
    store
        .append_request_log(
            "rust-written",
            &json!({"protocol": "openai", "model": "m-test", "provider": "p1"}),
            None,
        )
        .await
        .unwrap();
    assert!(
        store
            .list_request_logs(200)
            .await
            .unwrap()
            .iter()
            .any(|row| row["request_id"] == json!("rust-written"))
    );
    let status = store.runtime_status().await.unwrap();
    assert_eq!(
        status["requestLogCount"].as_i64().unwrap(),
        rows["request_logs"].as_array().unwrap().len() as i64 + 1
    );
    store.close().await;
    std::fs::remove_dir_all(&dir).ok();
}

/// 侧表正文行(最新拆分格式)通过详情读接口并回 API 字段名。
#[tokio::test]
async fn split_body_rows_merge_back_under_api_names() {
    let dir = std::env::temp_dir().join(format!("wr-r4-snap-bodies-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    build_fixture_db(&dir);
    let store = SqliteStore::open(&dir).await.unwrap();
    let rows = load_fixture_rows();
    let bodies = rows["request_log_bodies"].as_array().unwrap();
    let Some(first) = bodies.first() else {
        store.close().await;
        std::fs::remove_dir_all(&dir).ok();
        return;
    };
    let request_id = first["request_id"].as_str().unwrap();
    let detail = store
        .get_request_log(request_id)
        .await
        .unwrap()
        .expect("detail");
    for column in ["request_body", "response_body"] {
        if let Some(text) = first[column].as_str() {
            let legacy = match column {
                "request_body" => "request_body_text",
                _ => "response_body_text",
            };
            assert_eq!(detail[legacy], json!(text), "{column} 必须并回 {legacy}");
        }
    }
    store.close().await;
    std::fs::remove_dir_all(&dir).ok();
}
