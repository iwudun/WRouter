//! Config store 实现(方案 #14 R4.2/R4.3;约束 C2/C5/C9)。
//!
//! 镜像 Python `infrastructure/persistence/sqlite/config_store.py`。
//! 关键是 `save_config` 的 **C2 api_key 三态**(2026-08 修过的 bug:
//! 早先把空串也当「清空」,导致服务端 read-modify-write 链路
//! (`save_api_keys` / `set_provider` / `delete_provider` / 导入)一次保存
//! 就抹掉全部 provider 密钥):
//!
//! | `api_key` 取值 | 语义 | 动作 |
//! |---|---|---|
//! | 键缺失 | 调用方没打算改密钥 | 不动 `secrets` |
//! | 空串 `""` | 同上(payload 落库形态恒为空串) | 不动 `secrets` |
//! | `null` | **显式清空** | `DELETE FROM secrets` |
//! | `"***"` | 前端掩码回传 | 不动 |
//! | 其它非空 | 全量写入 | upsert |
//!
//! 多账号路径(`provider:{pid}:account:{aid}:api_key`)同套三态语义,
//! 并额外接受 `apiKey` 别名(仅当 `api_key` 缺失时)。
//!
//! **payload 键序副作用**必须保持:Python 用 `pop("api_key")` 取走再
//! `["api_key"] = ""` 放回,因此落库文本里 `api_key` 恒在该 provider
//! 对象的**末尾**(accounts 同理)。R4.12 db 文本 diff 依赖这一形态。

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value};

use wr_app::ports::{ConfigStore, PortError};

use super::SqliteStore;
use super::engine::iso_now;
use crate::python_semantics::{py_bool, py_int, py_str};
use crate::sqlite::SqlContext as _;

/// `api_key` 的三种到达形态:Python 用 `_MISSING` 哨兵区分「缺失」与
/// 「显式 `null`」,Rust 用同构枚举表达。持有 owned 值,因为判定之后
/// 还要从同一个 map 里 `remove` 该键。
#[derive(Debug, PartialEq, Eq)]
enum KeyState {
    Missing,
    Null,
    Present(Value),
}

fn key_state(map: &Map<String, Value>, name: &str) -> KeyState {
    match map.get(name) {
        None => KeyState::Missing,
        Some(Value::Null) => KeyState::Null,
        Some(value) => KeyState::Present(value.clone()),
    }
}

/// Python `str(value or "")`:假值归空串,其余 `str()` 化。
fn text_or_empty(value: Option<&Value>) -> String {
    match value {
        Some(value) if py_bool(value) => py_str(value).unwrap_or_default(),
        _ => String::new(),
    }
}

/// Python `str(raw_key or "")`。
fn key_text(value: &Value) -> String {
    if !py_bool(value) {
        return String::new();
    }
    py_str(value).unwrap_or_default()
}

impl SqliteStore {
    /// `save_config` 的完整写事务(端口实现与 R4.12 parity 测试共用)。
    pub(crate) fn save_payload_on(
        connection: &Connection,
        payload: &Value,
    ) -> Result<(), PortError> {
        let Some(root) = payload.as_object() else {
            return Err(PortError::invalid("config payload 必须是对象"));
        };
        let mut working = root.clone();
        let providers: Vec<Value> = match working.get("Providers") {
            Some(Value::Array(items)) => items.clone(),
            _ => Vec::new(),
        };
        let now = iso_now();
        let mut rewritten = Vec::with_capacity(providers.len());
        for provider_value in providers {
            let Value::Object(mut provider) = provider_value else {
                // Python 在这里会 AttributeError(to_dict 恒产出 dict,
                // 真实链路不可能走到);端口面按非法入参报错。
                return Err(PortError::invalid("Providers 每一项必须是对象"));
            };
            // Python: `str(provider.get("id") or "")` —— provider 级不 strip。
            let provider_id = text_or_empty(provider.get("id"));
            let raw_key = key_state(&provider, "api_key");
            provider.remove("api_key");
            if raw_key != KeyState::Missing && !provider_id.is_empty() {
                apply_secret(
                    connection,
                    &format!("provider:{provider_id}:api_key"),
                    &raw_key,
                )?;
            }
            provider.insert("api_key".to_owned(), Value::String(String::new()));

            let accounts: Vec<Value> = match provider.get("accounts") {
                Some(Value::Array(items)) => items.clone(),
                _ => Vec::new(),
            };
            if !accounts.is_empty() && !provider_id.is_empty() {
                let mut rewritten_accounts = Vec::with_capacity(accounts.len());
                for account_value in accounts {
                    let Value::Object(mut account) = account_value else {
                        // Python: `if not isinstance(acc, dict): continue`。
                        rewritten_accounts.push(account_value.clone());
                        continue;
                    };
                    // Python: `str(acc.get("id") or "").strip()` —— 账号级 strip。
                    let account_id = text_or_empty(account.get("id")).trim().to_owned();
                    if account_id.is_empty() {
                        // 空 account_id 在 pop **之前** continue:账号对象原样
                        // 留在 payload(api_key 未被取走)。
                        rewritten_accounts.push(Value::Object(account));
                        continue;
                    }
                    let raw_account_key = key_state(&account, "api_key");
                    account.remove("api_key");
                    apply_secret(
                        connection,
                        &format!("provider:{provider_id}:account:{account_id}:api_key"),
                        &raw_account_key,
                    )?;
                    account.insert("api_key".to_owned(), Value::String(String::new()));
                    upsert_account_row(connection, &provider_id, &account_id, &account, &now)?;
                    rewritten_accounts.push(Value::Object(account));
                }
                provider.insert("accounts".to_owned(), Value::Array(rewritten_accounts));
            }
            rewritten.push(Value::Object(provider));
        }
        working.insert("Providers".to_owned(), Value::Array(rewritten));
        let text = wr_domain::json_py::py_json_dumps(&Value::Object(working));
        connection
            .execute("UPDATE app_config SET payload=?1 WHERE id=1", params![text])
            .context("update app_config")?;
        Ok(())
    }
}

/// secrets 三态写入(`provider:{...}:api_key` 与账号级共用同一 SQL 形态)。
fn apply_secret(connection: &Connection, name: &str, raw_key: &KeyState) -> Result<(), PortError> {
    match raw_key {
        KeyState::Null => {
            connection
                .execute("DELETE FROM secrets WHERE name=?1", params![name])
                .context("delete secret")?;
        }
        KeyState::Present(value) => {
            let key = key_text(value);
            if !key.is_empty() && key != "***" {
                connection
                    .execute(
                        "INSERT INTO secrets(name, value) VALUES(?1, ?2) \
                         ON CONFLICT(name) DO UPDATE SET value=excluded.value",
                        params![name, key],
                    )
                    .context("upsert secret")?;
            }
        }
        KeyState::Missing => {}
    }
    Ok(())
}

/// `provider_accounts` 行 upsert:`created_at` 用
/// `COALESCE(NULLIF(既有值, ''), now)` 保住首次创建时间(镜像 Python)。
fn upsert_account_row(
    connection: &Connection,
    provider_id: &str,
    account_id: &str,
    account: &Map<String, Value>,
    now: &str,
) -> Result<(), PortError> {
    let label = text_or_empty(account.get("label"));
    let enabled = match account.get("enabled") {
        Some(value) => py_bool(value),
        None => true,
    };
    // Python: int(acc.get("priority") or 0) / int(acc.get("weight") or 1)。
    let priority = match account.get("priority") {
        Some(value) if py_bool(value) => py_int(value).unwrap_or(0),
        _ => 0,
    };
    let weight = match account.get("weight") {
        Some(value) if py_bool(value) => py_int(value).unwrap_or(1),
        _ => 1,
    };
    let account_config = match account.get("account_config") {
        Some(value) if py_bool(value) => value.clone(),
        _ => Value::Object(Map::new()),
    };
    connection
        .execute(
            "INSERT INTO provider_accounts(provider_id, account_id, label, enabled, priority, \
             weight, account_config, created_at, updated_at) \
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, \
             COALESCE(NULLIF((SELECT created_at FROM provider_accounts WHERE provider_id=?8 \
             AND account_id=?9), ''), ?10), ?11) \
             ON CONFLICT(provider_id, account_id) DO UPDATE SET \
             label=excluded.label, enabled=excluded.enabled, priority=excluded.priority, \
             weight=excluded.weight, account_config=excluded.account_config, \
             updated_at=excluded.updated_at",
            params![
                provider_id,
                account_id,
                label,
                i64::from(enabled),
                priority,
                weight,
                wr_domain::json_py::py_json_dumps(&account_config),
                provider_id,
                account_id,
                now,
                now,
            ],
        )
        .context("upsert provider_accounts")?;
    Ok(())
}

#[async_trait]
impl ConfigStore for SqliteStore {
    async fn load_payload(&self) -> Result<Value, PortError> {
        self.engine
            .read(|connection| {
                let text: Option<String> = connection
                    .query_row("SELECT payload FROM app_config WHERE id=1", [], |row| {
                        row.get(0)
                    })
                    .optional()
                    .context("read app_config")?;
                match text {
                    Some(text) => serde_json::from_str(&text).map_err(|e| {
                        PortError::storage(format!("app_config payload 非法 JSON: {e}"))
                    }),
                    None => Ok(Value::Object(Map::new())),
                }
            })
            .await
    }

    async fn save_payload(&self, payload: &Value) -> Result<(), PortError> {
        let payload = payload.clone();
        self.engine
            .write(move |connection| SqliteStore::save_payload_on(connection, &payload))
            .await?;
        // 20261017_02 批1:配置变更显式失效调用日志投影 —— 投影条目的
        // modelLabel / 协议标签固化自 provider 目录,缓存 version 只跟
        // request_logs 走,感知不到配置变化(旧实现靠 5s TTL 兜底,
        // TTL 放宽后必须显式失效)。
        self.invalidate_call_log_cache();
        Ok(())
    }

    async fn get_metadata(&self, key: &str) -> Result<Option<String>, PortError> {
        let key = key.to_owned();
        self.engine
            .read(move |connection| {
                connection
                    .query_row(
                        "SELECT value FROM metadata WHERE key=?1",
                        params![key],
                        |row| row.get(0),
                    )
                    .optional()
                    .context("read metadata")
            })
            .await
    }

    async fn set_metadata(&self, key: &str, value: &str) -> Result<(), PortError> {
        let key = key.to_owned();
        let value = value.to_owned();
        self.engine
            .write(move |connection| {
                connection
                    .execute(
                        "INSERT INTO metadata(key, value) VALUES(?1, ?2) \
                         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                        params![key, value],
                    )
                    .context("upsert metadata")?;
                Ok(())
            })
            .await
    }

    /// Python `str(row[0]).lower() in ("true", "1")`;缺行 → `false`。
    async fn get_onboarding_finished(&self) -> Result<bool, PortError> {
        let value = self.get_metadata("onboarding_finished").await?;
        Ok(matches!(
            value.map(|raw| raw.to_lowercase()).as_deref(),
            Some("true") | Some("1")
        ))
    }

    async fn set_onboarding_finished(&self, finished: bool) -> Result<(), PortError> {
        self.set_metadata(
            "onboarding_finished",
            if finished { "true" } else { "false" },
        )
        .await
    }
}
