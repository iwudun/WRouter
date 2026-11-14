//! 模型指纹库持久化(方案 20261030_01 R11)。
//!
//! 两张表(零迁移策略:只加新表 + `IF NOT EXISTS`):
//! - `model_fingerprint`:条目元数据(一行一个指纹条目);
//! - `model_fingerprint_samples`:原始样本(一行一条模型回答)。
//!
//! **为什么样本单独存**:方案 §3.5 要求「新增/删除条目后全量重拟合」。重拟合
//! 需要原始样本(干扰基与校准都依赖全体样本的联合统计),只存拟合产物无法
//! 重算。样本行是重拟合的唯一真源,拟合产物一律**现算**——库永远不会陈旧。
//!
//! 内置参考库(L1)已下线,条目与样本全部来自用户自建库。

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

use wr_app::ports::{FingerprintStore, PortError};

use super::SqlContext as _;
use super::SqliteStore;

/// 条目行 → JSON 的字段映射。
///
/// 参数多是有意的:这里是「一行 → 一个 JSON 对象」的字段直映射,拆成结构体
/// 反而多一层中转。
#[allow(clippy::too_many_arguments)]
fn entry_to_json(
    id: &str,
    kind: &str,
    provider_id: Option<&str>,
    model_name: &str,
    family: Option<&str>,
    label: &str,
    probe_suite: &str,
    sample_count: i64,
    probe_seed: Option<&str>,
    probe_env: Option<&str>,
    cv_accuracy: Option<f64>,
    created_at: &str,
    updated_at: &str,
    samples: Vec<Value>,
) -> Value {
    json!({
        "id": id,
        "kind": kind,
        "providerId": provider_id,
        "modelName": model_name,
        "family": family,
        "label": label,
        "probeSuite": probe_suite,
        "sampleCount": sample_count,
        "probeSeed": probe_seed,
        "probeEnv": probe_env,
        "cvAccuracy": cv_accuracy,
        "createdAt": created_at,
        "updatedAt": updated_at,
        "samples": samples,
    })
}

/// 读某条目的样本行。
fn load_samples(connection: &Connection, entry_id: &str) -> Result<Vec<Value>, PortError> {
    let mut statement = connection
        .prepare(
            "SELECT challenge_id, condition_id, expected_count, text FROM model_fingerprint_samples \
             WHERE entry_id = ?1 ORDER BY ordinal ASC",
        )
        .context("prepare model_fingerprint_samples")?;
    let rows = statement
        .query_map(params![entry_id], |row| {
            Ok(json!({
                "challengeId": row.get::<_, String>(0)?,
                "conditionId": row.get::<_, String>(1)?,
                "expectedCount": row.get::<_, i64>(2)?,
                "text": row.get::<_, String>(3)?,
            }))
        })
        .context("query model_fingerprint_samples")?;
    let mut samples = Vec::new();
    for row in rows {
        samples.push(row.context("read model_fingerprint_samples")?);
    }
    Ok(samples)
}

/// 读全部条目行(不含样本)。
fn load_entry_rows(
    connection: &Connection,
    kind: Option<&str>,
    provider_id: Option<&str>,
) -> Result<Vec<Value>, PortError> {
    let mut sql = String::from(
        "SELECT id, kind, provider_id, model_name, family, label, probe_suite, sample_count, \
         probe_seed, probe_env, cv_accuracy, created_at, updated_at FROM model_fingerprint",
    );
    let mut filters: Vec<&str> = Vec::new();
    if kind.is_some() {
        filters.push("kind = ?");
    }
    if provider_id.is_some() {
        filters.push("provider_id = ?");
    }
    if !filters.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&filters.join(" AND "));
    }
    sql.push_str(" ORDER BY created_at ASC, id ASC");

    let mut statement = connection.prepare(&sql).context("prepare model_fingerprint")?;
    let mapper = |row: &rusqlite::Row<'_>| -> rusqlite::Result<Value> {
        Ok(json!({
            "id": row.get::<_, String>(0)?,
            "kind": row.get::<_, String>(1)?,
            "providerId": row.get::<_, Option<String>>(2)?,
            "modelName": row.get::<_, String>(3)?,
            "family": row.get::<_, Option<String>>(4)?,
            "label": row.get::<_, String>(5)?,
            "probeSuite": row.get::<_, String>(6)?,
            "sampleCount": row.get::<_, i64>(7)?,
            "probeSeed": row.get::<_, Option<String>>(8)?,
            "probeEnv": row.get::<_, Option<String>>(9)?,
            "cvAccuracy": row.get::<_, Option<f64>>(10)?,
            "createdAt": row.get::<_, String>(11)?,
            "updatedAt": row.get::<_, String>(12)?,
        }))
    };
    let rows = match (kind, provider_id) {
        (Some(kind), Some(provider)) => {
            statement
                .query_map(params![kind, provider], mapper)
                .context("query model_fingerprint")?
        }
        (Some(kind), None) => statement
            .query_map(params![kind], mapper)
            .context("query model_fingerprint")?,
        (None, Some(provider)) => statement
            .query_map(params![provider], mapper)
            .context("query model_fingerprint")?,
        (None, None) => statement
            .query_map([], mapper)
            .context("query model_fingerprint")?,
    };
    let mut entries = Vec::new();
    for row in rows {
        entries.push(row.context("read model_fingerprint")?);
    }
    Ok(entries)
}

/// 给条目补上样本行。
fn attach_samples(connection: &Connection, entries: &mut [Value]) -> Result<(), PortError> {
    for entry in entries.iter_mut() {
        let Some(id) = entry.get("id").and_then(Value::as_str) else {
            continue;
        };
        let samples = load_samples(connection, id)?;
        if let Some(map) = entry.as_object_mut() {
            map.insert("samples".to_owned(), Value::Array(samples));
        }
    }
    Ok(())
}

#[async_trait]
impl FingerprintStore for SqliteStore {
    async fn list_entries(
        &self,
        kind: Option<&str>,
        provider_id: Option<&str>,
    ) -> Result<Vec<Value>, PortError> {
        let kind = kind.map(str::to_owned);
        let provider_id = provider_id.map(str::to_owned);
        self.engine
            .read(move |connection: &Connection| {
                let mut entries = load_entry_rows(connection, kind.as_deref(), provider_id.as_deref())?;
                attach_samples(connection, &mut entries)?;
                Ok(entries)
            })
            .await
    }

    async fn get_entry(&self, id: &str) -> Result<Option<Value>, PortError> {
        let id = id.to_owned();
        self.engine
            .read(move |connection: &Connection| {
                let mut statement = connection
                    .prepare(
                        "SELECT id, kind, provider_id, model_name, family, label, probe_suite, \
                         sample_count, probe_seed, probe_env, cv_accuracy, created_at, updated_at \
                         FROM model_fingerprint WHERE id = ?1",
                    )
                    .context("prepare model_fingerprint by id")?;
                let row = statement
                    .query_row(params![id], |row| {
                        Ok(entry_to_json(
                            &row.get::<_, String>(0)?,
                            &row.get::<_, String>(1)?,
                            row.get::<_, Option<String>>(2)?.as_deref(),
                            &row.get::<_, String>(3)?,
                            row.get::<_, Option<String>>(4)?.as_deref(),
                            &row.get::<_, String>(5)?,
                            &row.get::<_, String>(6)?,
                            row.get::<_, i64>(7)?,
                            row.get::<_, Option<String>>(8)?.as_deref(),
                            row.get::<_, Option<String>>(9)?.as_deref(),
                            row.get::<_, Option<f64>>(10)?,
                            &row.get::<_, String>(11)?,
                            &row.get::<_, String>(12)?,
                            Vec::new(),
                        ))
                    })
                    .optional()
                    .context("query model_fingerprint by id")?;
                let Some(mut entry) = row else {
                    return Ok(None);
                };
                let entry_id = entry
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let samples = load_samples(connection, &entry_id)?;
                if let Some(map) = entry.as_object_mut() {
                    map.insert("samples".to_owned(), Value::Array(samples));
                }
                Ok(Some(entry))
            })
            .await
    }

    async fn upsert_entry(&self, entry: &Value) -> Result<(), PortError> {
        let entry = entry.clone();
        self.engine
            .write(move |connection: &Connection| {
                let id = entry
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| PortError::invalid("fingerprint entry requires an id"))?;
                let kind = entry
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("global")
                    .to_owned();
                let provider_id = entry
                    .get("providerId")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let model_name = entry
                    .get("modelName")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let family = entry
                    .get("family")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let label = entry
                    .get("label")
                    .and_then(Value::as_str)
                    .unwrap_or(&model_name)
                    .to_owned();
                let probe_suite = entry
                    .get("probeSuite")
                    .and_then(Value::as_str)
                    .unwrap_or("wrouter-v1")
                    .to_owned();
                let probe_seed = entry
                    .get("probeSeed")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let probe_env = entry
                    .get("probeEnv")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let cv_accuracy = entry.get("cvAccuracy").and_then(Value::as_f64);
                let samples = entry
                    .get("samples")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let now = super::engine::iso_now();

                // 保留既有 created_at(upsert 语义)。
                let existing_created_at: Option<String> = connection
                    .query_row(
                        "SELECT created_at FROM model_fingerprint WHERE id = ?1",
                        params![id],
                        |row| row.get(0),
                    )
                    .optional()
                    .context("read existing created_at")?;
                let created_at = existing_created_at.unwrap_or_else(|| now.clone());

                connection
                    .execute(
                        "INSERT INTO model_fingerprint \
                         (id, kind, provider_id, model_name, family, label, probe_suite, \
                          sample_count, probe_seed, probe_env, cv_accuracy, created_at, updated_at) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13) \
                         ON CONFLICT(id) DO UPDATE SET \
                         kind = excluded.kind, provider_id = excluded.provider_id, \
                         model_name = excluded.model_name, family = excluded.family, \
                         label = excluded.label, probe_suite = excluded.probe_suite, \
                         sample_count = excluded.sample_count, probe_seed = excluded.probe_seed, \
                         probe_env = excluded.probe_env, cv_accuracy = excluded.cv_accuracy, \
                         updated_at = excluded.updated_at",
                        params![
                            id,
                            kind,
                            provider_id,
                            model_name,
                            family,
                            label,
                            probe_suite,
                            samples.len() as i64,
                            probe_seed,
                            probe_env,
                            cv_accuracy,
                            created_at,
                            now,
                        ],
                    )
                    .context("upsert model_fingerprint")?;

                // 样本整体替换(条目级原子性:同事务)。
                connection
                    .execute(
                        "DELETE FROM model_fingerprint_samples WHERE entry_id = ?1",
                        params![id],
                    )
                    .context("clear model_fingerprint_samples")?;
                for (ordinal, sample) in samples.iter().enumerate() {
                    let challenge_id = sample
                        .get("challengeId")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    let condition_id = sample
                        .get("conditionId")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    let expected_count = sample
                        .get("expectedCount")
                        .and_then(Value::as_i64)
                        .unwrap_or_default();
                    let text = sample
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    connection
                        .execute(
                            "INSERT INTO model_fingerprint_samples \
                             (entry_id, ordinal, challenge_id, condition_id, expected_count, text, created_at) \
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                            params![
                                id,
                                ordinal as i64,
                                challenge_id,
                                condition_id,
                                expected_count,
                                text,
                                now,
                            ],
                        )
                        .context("insert model_fingerprint_samples")?;
                }
                Ok(())
            })
            .await
    }

    async fn delete_entry(&self, id: &str) -> Result<bool, PortError> {
        let id = id.to_owned();
        self.engine
            .write(move |connection: &Connection| {
                let removed = connection
                    .execute(
                        "DELETE FROM model_fingerprint WHERE id = ?1",
                        params![id],
                    )
                    .context("delete model_fingerprint")?;
                connection
                    .execute(
                        "DELETE FROM model_fingerprint_samples WHERE entry_id = ?1",
                        params![id],
                    )
                    .context("delete model_fingerprint_samples")?;
                Ok(removed > 0)
            })
            .await
    }

    async fn all_entries(&self) -> Result<Vec<Value>, PortError> {
        self.list_entries(None, None).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wr_app::ports::KIND_ANCHOR;

    async fn store(name: &str) -> (SqliteStore, std::path::PathBuf) {
        // 目录名带用例名:测试并行跑时不得共用同一个 SQLite 文件。
        let dir = std::env::temp_dir().join(format!("wr-fp-store-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let store = SqliteStore::open(&dir).await.expect("store opens");
        (store, dir)
    }

    fn entry(id: &str, kind: &str, provider: Option<&str>) -> Value {
        json!({
            "id": id,
            "kind": kind,
            "providerId": provider,
            "modelName": "m1",
            "family": "gpt",
            "label": "M1",
            "probeSuite": "wrouter-v1",
            "probeSeed": "seed-1",
            "probeEnv": "environment-01,environment-02",
            "cvAccuracy": 0.96,
            "samples": [
                {"challengeId": "c1", "conditionId": "environment-01", "expectedCount": 300, "text": "1, 2, 3"},
                {"challengeId": "c2", "conditionId": "environment-02", "expectedCount": 300, "text": "4, 5, 6"},
            ],
        })
    }

    #[tokio::test]
    async fn upserts_and_lists_entries_with_samples() {
        let (store, dir) = store("upsert-list").await;
        store.upsert_entry(&entry("e1", "global", None)).await.unwrap();
        let entries = store.list_entries(None, None).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["id"], "e1");
        assert_eq!(entries[0]["sampleCount"], 2);
        assert_eq!(entries[0]["samples"].as_array().unwrap().len(), 2);
        assert_eq!(entries[0]["samples"][0]["challengeId"], "c1");
        store.close().await;
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn filters_by_kind_and_provider() {
        let (store, dir) = store("filter").await;
        store.upsert_entry(&entry("g1", "global", None)).await.unwrap();
        store
            .upsert_entry(&entry("a1", KIND_ANCHOR, Some("p1")))
            .await
            .unwrap();
        store
            .upsert_entry(&entry("a2", KIND_ANCHOR, Some("p2")))
            .await
            .unwrap();
        assert_eq!(store.list_entries(None, None).await.unwrap().len(), 3);
        assert_eq!(store.list_entries(Some("global"), None).await.unwrap().len(), 1);
        assert_eq!(
            store.list_entries(Some(KIND_ANCHOR), None).await.unwrap().len(),
            2
        );
        assert_eq!(
            store
                .list_entries(Some(KIND_ANCHOR), Some("p1"))
                .await
                .unwrap()
                .len(),
            1
        );
        store.close().await;
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn upsert_replaces_samples_and_keeps_created_at() {
        let (store, dir) = store("upsert-replace").await;
        store.upsert_entry(&entry("e1", "global", None)).await.unwrap();
        let first = store.get_entry("e1").await.unwrap().unwrap();
        let created_at = first["createdAt"].as_str().unwrap().to_owned();

        let mut updated = entry("e1", "global", None);
        updated["samples"] = json!([
            {"challengeId": "c9", "conditionId": "environment-09", "expectedCount": 300, "text": "7"}
        ]);
        store.upsert_entry(&updated).await.unwrap();
        let second = store.get_entry("e1").await.unwrap().unwrap();
        assert_eq!(second["sampleCount"], 1);
        assert_eq!(second["samples"].as_array().unwrap().len(), 1);
        assert_eq!(second["createdAt"].as_str().unwrap(), created_at);
        store.close().await;
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn delete_removes_entry_and_samples() {
        let (store, dir) = store("delete").await;
        store.upsert_entry(&entry("e1", "global", None)).await.unwrap();
        assert!(store.delete_entry("e1").await.unwrap());
        assert!(!store.delete_entry("e1").await.unwrap());
        assert!(store.get_entry("e1").await.unwrap().is_none());
        assert!(store.all_entries().await.unwrap().is_empty());
        store.close().await;
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn old_database_without_the_table_still_opens() {
        // 零迁移:先造一个只有 app_config/secrets 的旧库,再打开 store。
        let dir = std::env::temp_dir().join(format!("wr-fp-legacy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.sqlite");
        {
            let connection = Connection::open(&path).unwrap();
            connection
                .execute_batch(
                    "CREATE TABLE app_config (id INTEGER PRIMARY KEY CHECK (id = 1), payload TEXT NOT NULL);
                     CREATE TABLE secrets (name TEXT PRIMARY KEY, value TEXT NOT NULL);",
                )
                .unwrap();
        }
        let store = SqliteStore::open(&dir).await.expect("旧库应可打开");
        assert!(store.all_entries().await.unwrap().is_empty());
        store.close().await;
        std::fs::remove_dir_all(&dir).ok();
    }
}
