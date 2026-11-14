//! 幂等存储实现(方案 #14 R4.4)。
//!
//! 镜像 Python `infrastructure/persistence/sqlite/idempotency_store.py`:
//! `idempotency_entries` 五件套 load / save / delete / list / prune。
//!
//! 时钟基差异(见 14 号文档 R4 完成记录):Python 侧持久化的是
//! `int(time.monotonic()*1000)`(本机开机起算),Rust 侧统一用 epoch 毫秒。
//! 两套数值不可直接比较,所以本 store **只按字面存取**,归一化放在
//! `wr-server::gateway::idempotency` 的重放缓存里(它才知道自己的时钟基)。

use async_trait::async_trait;
use rusqlite::{OptionalExtension, params};

use wr_app::ports::{IdempotencyRow, IdempotencyStore, PortError};

use super::SqlContext as _;
use super::SqliteStore;

const SELECT_COLUMNS: &str = "store_key, request_hash, status_code, payload, headers, \
                              expires_at, created_at";

fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<IdempotencyRow> {
    Ok(IdempotencyRow {
        store_key: row.get(0)?,
        request_hash: row.get(1)?,
        status_code: row.get(2)?,
        payload: row.get(3)?,
        headers: row.get(4)?,
        expires_at: row.get(5)?,
        created_at: row.get(6)?,
    })
}

#[async_trait]
impl IdempotencyStore for SqliteStore {
    async fn load_idempotency(&self, store_key: &str) -> Result<Option<IdempotencyRow>, PortError> {
        let store_key = store_key.to_owned();
        self.engine
            .read(move |connection| {
                let sql =
                    format!("SELECT {SELECT_COLUMNS} FROM idempotency_entries WHERE store_key=?1");
                connection
                    .query_row(&sql, params![store_key], row_to_entry)
                    .optional()
                    .context("load idempotency")
            })
            .await
    }

    async fn save_idempotency(&self, entry: &IdempotencyRow) -> Result<(), PortError> {
        let entry = entry.clone();
        self.engine
            .write(move |connection| {
                connection
                    .execute(
                        "INSERT INTO idempotency_entries(store_key, request_hash, status_code, \
                         payload, headers, expires_at, created_at) \
                         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7) \
                         ON CONFLICT(store_key) DO UPDATE SET \
                         request_hash=excluded.request_hash, status_code=excluded.status_code, \
                         payload=excluded.payload, headers=excluded.headers, \
                         expires_at=excluded.expires_at, created_at=excluded.created_at",
                        params![
                            entry.store_key,
                            entry.request_hash,
                            entry.status_code,
                            entry.payload,
                            entry.headers,
                            entry.expires_at,
                            entry.created_at
                        ],
                    )
                    .context("save idempotency")?;
                Ok(())
            })
            .await
    }

    async fn delete_idempotency(&self, store_key: &str) -> Result<(), PortError> {
        let store_key = store_key.to_owned();
        self.engine
            .write(move |connection| {
                connection
                    .execute(
                        "DELETE FROM idempotency_entries WHERE store_key=?1",
                        params![store_key],
                    )
                    .context("delete idempotency")?;
                Ok(())
            })
            .await
    }

    async fn list_idempotency_entries(&self) -> Result<Vec<IdempotencyRow>, PortError> {
        self.engine
            .read(|connection| {
                let sql = format!("SELECT {SELECT_COLUMNS} FROM idempotency_entries");
                let mut statement = connection.prepare(&sql).context("prepare idempotency")?;
                let rows = statement
                    .query_map([], row_to_entry)
                    .context("query idempotency")?;
                let mut loaded = Vec::new();
                for row in rows {
                    loaded.push(row.context("row idempotency")?);
                }
                Ok(loaded)
            })
            .await
    }

    /// `expires_at <= now_ms` 视为过期(与 Python 的 `<=` 一致)。
    async fn prune_idempotency(&self, now_ms: i64) -> Result<i64, PortError> {
        self.engine
            .write(move |connection| {
                let deleted = connection
                    .execute(
                        "DELETE FROM idempotency_entries WHERE expires_at <= ?1",
                        params![now_ms],
                    )
                    .context("prune idempotency")?;
                Ok(deleted as i64)
            })
            .await
    }
}
