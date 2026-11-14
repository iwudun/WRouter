//! Secret store 实现(方案 #14 R4.3 / R4.7 持久化面)。
//!
//! 镜像 Python `infrastructure/persistence/sqlite/secret_store.py`:
//! `secrets` 命名空间(`provider:{id}:api_key` 与
//! `provider:{id}:account:{aid}:api_key`)+ A5 多账号池
//! `provider_accounts` + 快照缓存 `account_snapshots`。
//!
//! 读取契约与 Python 一致:密钥缺失返回**空串**而不是 `None`
//! (`-> str` 语义);列表方法**永不含密钥值**(C8 脱敏的存储侧保证)。

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value};

use wr_app::ports::{AccountSnapshotInput, AccountUpsert, PortError, SecretStore};

use super::SqliteStore;
use super::engine::{iso_now, wall_ms};
use super::object;
use crate::sqlite::SqlContext as _;

fn account_secret_name(provider_id: &str, account_id: &str) -> String {
    format!("provider:{provider_id}:account:{account_id}:api_key")
}

fn provider_secret_name(provider_id: &str) -> String {
    format!("provider:{provider_id}:api_key")
}

const UPSERT_SECRET_SQL: &str = "INSERT INTO secrets(name, value) VALUES(?1, ?2) \
                                 ON CONFLICT(name) DO UPDATE SET value=excluded.value";

/// `provider_accounts` upsert(与 `save_payload` 的账号路径共用同一条 SQL,
/// 保持 `created_at` 的 `COALESCE(NULLIF(...))` 语义一致)。
const UPSERT_ACCOUNT_SQL: &str = "INSERT INTO provider_accounts(provider_id, account_id, \
     label, enabled, priority, weight, account_config, created_at, updated_at) \
     VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, \
     COALESCE(NULLIF((SELECT created_at FROM provider_accounts WHERE provider_id=?8 \
     AND account_id=?9), ''), ?10), ?11) \
     ON CONFLICT(provider_id, account_id) DO UPDATE SET \
     label=excluded.label, enabled=excluded.enabled, priority=excluded.priority, \
     weight=excluded.weight, account_config=excluded.account_config, \
     updated_at=excluded.updated_at";

fn read_secret(connection: &Connection, name: &str) -> Result<String, PortError> {
    let value: Option<String> = connection
        .query_row(
            "SELECT value FROM secrets WHERE name=?1",
            params![name],
            |row| row.get(0),
        )
        .optional()
        .context("read secret")?;
    Ok(value.unwrap_or_default())
}

fn write_account_row(
    connection: &Connection,
    input: &AccountUpsert<'_>,
    now: &str,
) -> Result<(), PortError> {
    let account_config = match input.account_config {
        Some(value) => value.clone(),
        None => Value::Object(Map::new()),
    };
    connection
        .execute(
            UPSERT_ACCOUNT_SQL,
            params![
                input.provider_id,
                input.account_id,
                input.label,
                i64::from(input.enabled),
                input.priority,
                input.weight,
                wr_domain::json_py::py_json_dumps(&account_config),
                input.provider_id,
                input.account_id,
                now,
                now,
            ],
        )
        .context("upsert provider_accounts")?;
    Ok(())
}

#[async_trait]
impl SecretStore for SqliteStore {
    /// Python `if api_key:` —— 空串直接什么都不写(不是删除)。
    async fn upsert_provider_secret(
        &self,
        provider_id: &str,
        api_key: &str,
    ) -> Result<(), PortError> {
        if api_key.is_empty() {
            return Ok(());
        }
        let name = provider_secret_name(provider_id);
        let api_key = api_key.to_owned();
        self.engine
            .write(move |connection| {
                connection
                    .execute(UPSERT_SECRET_SQL, params![name, api_key])
                    .context("upsert provider secret")?;
                Ok(())
            })
            .await
    }

    async fn provider_secret(&self, provider_id: &str) -> Result<String, PortError> {
        let name = provider_secret_name(provider_id);
        self.engine
            .read(move |connection| read_secret(connection, &name))
            .await
    }

    async fn delete_provider_secret(&self, provider_id: &str) -> Result<(), PortError> {
        let name = provider_secret_name(provider_id);
        self.engine
            .write(move |connection| {
                connection
                    .execute("DELETE FROM secrets WHERE name=?1", params![name])
                    .context("delete provider secret")?;
                Ok(())
            })
            .await
    }

    async fn upsert_account(&self, input: &AccountUpsert<'_>) -> Result<(), PortError> {
        if input.provider_id.is_empty() || input.account_id.is_empty() {
            // 镜像 Python ValueError(端口契约违反,不是存储故障)。
            return Err(PortError::invalid(
                "upsert_account requires non-empty provider_id and account_id",
            ));
        }
        let owned = AccountUpsertOwned::from(input);
        self.engine
            .write(move |connection| {
                let now = iso_now();
                if !owned.api_key.is_empty() {
                    let name = account_secret_name(&owned.provider_id, &owned.account_id);
                    connection
                        .execute(UPSERT_SECRET_SQL, params![name, owned.api_key])
                        .context("upsert account secret")?;
                }
                write_account_row(connection, &owned.as_ref(), &now)
            })
            .await
    }

    /// 账号行按 `priority ASC, account_id ASC`;`account_config` 反序列化
    /// 失败落 `{}`(镜像 Python 的 try/except)。
    async fn list_accounts(&self, provider_id: &str) -> Result<Vec<Value>, PortError> {
        let provider_id = provider_id.to_owned();
        self.engine
            .read(move |connection| {
                let mut statement = connection
                    .prepare(
                        "SELECT provider_id, account_id, label, enabled, priority, weight, \
                         account_config FROM provider_accounts WHERE provider_id=?1 \
                         ORDER BY priority ASC, account_id ASC",
                    )
                    .context("prepare list_accounts")?;
                let rows = statement
                    .query_map(params![provider_id], |row| {
                        Ok((
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, i64>(4)?,
                            row.get::<_, i64>(5)?,
                            row.get::<_, Option<String>>(6)?,
                        ))
                    })
                    .context("query list_accounts")?;
                let mut accounts = Vec::new();
                for row in rows {
                    let (id, label, enabled, priority, weight, config_text) =
                        row.context("row list_accounts")?;
                    let config = match config_text {
                        Some(text) => {
                            serde_json::from_str(&text).unwrap_or(Value::Object(Map::new()))
                        }
                        None => Value::Object(Map::new()),
                    };
                    accounts.push(object(vec![
                        ("id", Value::String(id)),
                        ("label", Value::String(label)),
                        ("enabled", Value::Bool(enabled != 0)),
                        ("priority", Value::from(priority)),
                        ("weight", Value::from(weight)),
                        ("account_config", config),
                    ]));
                }
                Ok(accounts)
            })
            .await
    }

    async fn account_secret(
        &self,
        provider_id: &str,
        account_id: &str,
    ) -> Result<String, PortError> {
        let name = account_secret_name(provider_id, account_id);
        self.engine
            .read(move |connection| read_secret(connection, &name))
            .await
    }

    async fn delete_account(&self, provider_id: &str, account_id: &str) -> Result<(), PortError> {
        let provider_id = provider_id.to_owned();
        let account_id = account_id.to_owned();
        self.engine
            .write(move |connection| {
                connection
                    .execute(
                        "DELETE FROM provider_accounts WHERE provider_id=?1 AND account_id=?2",
                        params![provider_id, account_id],
                    )
                    .context("delete account row")?;
                let name = account_secret_name(&provider_id, &account_id);
                connection
                    .execute("DELETE FROM secrets WHERE name=?1", params![name])
                    .context("delete account secret")?;
                Ok(())
            })
            .await
    }

    async fn delete_provider_accounts(&self, provider_id: &str) -> Result<(), PortError> {
        let provider_id = provider_id.to_owned();
        let pattern = format!("provider:{provider_id}:account:%");
        self.engine
            .write(move |connection| {
                connection
                    .execute(
                        "DELETE FROM provider_accounts WHERE provider_id=?1",
                        params![provider_id],
                    )
                    .context("delete provider accounts")?;
                connection
                    .execute("DELETE FROM secrets WHERE name LIKE ?1", params![pattern])
                    .context("delete provider account secrets")?;
                Ok(())
            })
            .await
    }

    async fn upsert_account_snapshot(
        &self,
        input: &AccountSnapshotInput<'_>,
    ) -> Result<(), PortError> {
        let provider_id = input.provider_id.to_owned();
        let account_id = if input.account_id.is_empty() {
            String::new()
        } else {
            input.account_id.to_owned()
        };
        let status = input.status.to_owned();
        let source = input.source.to_owned();
        let payload = wr_domain::json_py::py_json_dumps(&Value::Object(input.payload.clone()));
        let expires_at = wall_ms() + input.ttl_ms;
        let fetched_at = iso_now();
        self.engine
            .write(move |connection| {
                connection
                    .execute(
                        "INSERT INTO account_snapshots(provider_id, account_id, status, source, \
                         payload, fetched_at, expires_at) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7) \
                         ON CONFLICT(provider_id, account_id) DO UPDATE SET \
                         status=excluded.status, source=excluded.source, \
                         payload=excluded.payload, fetched_at=excluded.fetched_at, \
                         expires_at=excluded.expires_at",
                        params![
                            provider_id,
                            account_id,
                            status,
                            source,
                            payload,
                            fetched_at,
                            expires_at
                        ],
                    )
                    .context("upsert account snapshot")?;
                Ok(())
            })
            .await
    }

    /// 过期判定留给调用方(Python 契约如此)。
    async fn get_account_snapshot(
        &self,
        provider_id: &str,
        account_id: &str,
    ) -> Result<Option<Value>, PortError> {
        let provider_id = provider_id.to_owned();
        let account_id = if account_id.is_empty() {
            String::new()
        } else {
            account_id.to_owned()
        };
        self.engine
            .read(move |connection| {
                let row = connection
                    .query_row(
                        "SELECT status, source, payload, fetched_at, expires_at \
                         FROM account_snapshots WHERE provider_id=?1 AND account_id=?2",
                        params![provider_id, account_id],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, Option<String>>(2)?,
                                row.get::<_, String>(3)?,
                                row.get::<_, Option<i64>>(4)?,
                            ))
                        },
                    )
                    .optional()
                    .context("read account snapshot")?;
                let Some((status, source, payload_text, fetched_at, expires_at)) = row else {
                    return Ok(None);
                };
                let payload = match payload_text {
                    Some(text) => serde_json::from_str(&text).unwrap_or(Value::Object(Map::new())),
                    None => Value::Object(Map::new()),
                };
                Ok(Some(object(vec![
                    ("status", Value::String(status)),
                    ("source", Value::String(source)),
                    ("payload", payload),
                    ("fetched_at", Value::String(fetched_at)),
                    ("expires_at", Value::from(expires_at.unwrap_or(0))),
                ])))
            })
            .await
    }
}

/// `AccountUpsert` 的 owned 版本(闭包要 move 进写线程)。
struct AccountUpsertOwned {
    provider_id: String,
    account_id: String,
    api_key: String,
    label: String,
    enabled: bool,
    priority: i64,
    weight: i64,
    account_config: Value,
}

impl<'a> From<&AccountUpsert<'a>> for AccountUpsertOwned {
    fn from(input: &AccountUpsert<'a>) -> Self {
        Self {
            provider_id: input.provider_id.to_owned(),
            account_id: input.account_id.to_owned(),
            api_key: input.api_key.to_owned(),
            label: input.label.to_owned(),
            enabled: input.enabled,
            priority: input.priority,
            weight: input.weight,
            account_config: input
                .account_config
                .cloned()
                .unwrap_or(Value::Object(Map::new())),
        }
    }
}

impl AccountUpsertOwned {
    /// 借出成 `AccountUpsert` 以复用同一条 upsert SQL。
    fn as_ref(&self) -> AccountUpsert<'_> {
        AccountUpsert {
            provider_id: &self.provider_id,
            account_id: &self.account_id,
            api_key: &self.api_key,
            label: &self.label,
            enabled: self.enabled,
            priority: self.priority,
            weight: self.weight,
            account_config: Some(&self.account_config),
        }
    }
}
