//! 熔断 / 健康快照存储实现(方案 #14 R4.4 / R4.8 的持久化面)。
//!
//! 镜像 Python `infrastructure/persistence/sqlite/breaker_health_store.py`。
//!
//! `circuit_breaker_state.updated_at` 由 SQL 侧 `datetime('now')` 生成
//! (与 Python 一致:写入时刻来自数据库时钟,不是进程时钟)。

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension, params};

use wr_app::ports::{
    BreakerHealthStore, BreakerStateRow, BreakerStateStore, HealthRow, HealthStore, PortError,
};

use super::SqlContext as _;
use super::SqliteStore;

fn breaker_row(
    consecutive_failures: i64,
    opened_until_ms: i64,
    last_state_change_ms: i64,
) -> BreakerStateRow {
    BreakerStateRow {
        consecutive_failures,
        opened_until_ms,
        last_state_change_ms,
    }
}

fn health_row(
    provider_id: String,
    status: String,
    available: i64,
    latency_ms: i64,
    checked_at: String,
    error: Option<String>,
) -> HealthRow {
    HealthRow {
        provider_id,
        status,
        available: available != 0,
        latency_ms,
        checked_at,
        error,
    }
}

#[async_trait]
impl BreakerStateStore for SqliteStore {
    async fn load_breaker_state(
        &self,
        provider_id: &str,
    ) -> Result<Option<BreakerStateRow>, PortError> {
        let provider_id = provider_id.to_owned();
        self.engine
            .read(move |connection: &Connection| {
                connection
                    .query_row(
                        "SELECT consecutive_failures, opened_until_ms, last_state_change_ms \
                         FROM circuit_breaker_state WHERE provider_id=?1",
                        params![provider_id],
                        |row| Ok(breaker_row(row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .optional()
                    .context("load breaker state")
            })
            .await
    }

    async fn load_all_breaker_states(&self) -> Result<Vec<(String, BreakerStateRow)>, PortError> {
        self.engine
            .read(|connection| {
                let mut statement = connection
                    .prepare(
                        "SELECT provider_id, consecutive_failures, opened_until_ms, \
                         last_state_change_ms FROM circuit_breaker_state",
                    )
                    .context("prepare breaker states")?;
                let rows = statement
                    .query_map([], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            breaker_row(row.get(1)?, row.get(2)?, row.get(3)?),
                        ))
                    })
                    .context("query breaker states")?;
                let mut loaded = Vec::new();
                for row in rows {
                    loaded.push(row.context("row breaker state")?);
                }
                Ok(loaded)
            })
            .await
    }

    async fn save_breaker_state(
        &self,
        provider_id: &str,
        state: &BreakerStateRow,
    ) -> Result<(), PortError> {
        let provider_id = provider_id.to_owned();
        let state = *state;
        self.engine
            .write(move |connection| {
                connection
                    .execute(
                        "INSERT INTO circuit_breaker_state(provider_id, consecutive_failures, \
                         opened_until_ms, last_state_change_ms, updated_at) \
                         VALUES(?1, ?2, ?3, ?4, datetime('now')) \
                         ON CONFLICT(provider_id) DO UPDATE SET \
                         consecutive_failures=excluded.consecutive_failures, \
                         opened_until_ms=excluded.opened_until_ms, \
                         last_state_change_ms=excluded.last_state_change_ms, \
                         updated_at=excluded.updated_at",
                        params![
                            provider_id,
                            state.consecutive_failures,
                            state.opened_until_ms,
                            state.last_state_change_ms
                        ],
                    )
                    .context("save breaker state")?;
                Ok(())
            })
            .await
    }
}

#[async_trait]
impl HealthStore for SqliteStore {
    async fn load_health(&self, provider_id: &str) -> Result<Option<HealthRow>, PortError> {
        let provider_id = provider_id.to_owned();
        self.engine
            .read(move |connection| {
                connection
                    .query_row(
                        "SELECT provider_id, status, available, latency_ms, checked_at, error \
                         FROM provider_health WHERE provider_id=?1",
                        params![provider_id],
                        |row| {
                            Ok(health_row(
                                row.get(0)?,
                                row.get(1)?,
                                row.get(2)?,
                                row.get(3)?,
                                row.get(4)?,
                                row.get(5)?,
                            ))
                        },
                    )
                    .optional()
                    .context("load health")
            })
            .await
    }

    async fn load_all_health(&self) -> Result<Vec<(String, HealthRow)>, PortError> {
        self.engine
            .read(|connection| {
                let mut statement = connection
                    .prepare(
                        "SELECT provider_id, status, available, latency_ms, checked_at, error \
                         FROM provider_health",
                    )
                    .context("prepare health")?;
                let rows = statement
                    .query_map([], |row| {
                        let provider_id: String = row.get(0)?;
                        Ok((
                            provider_id.clone(),
                            health_row(
                                provider_id,
                                row.get(1)?,
                                row.get(2)?,
                                row.get(3)?,
                                row.get(4)?,
                                row.get(5)?,
                            ),
                        ))
                    })
                    .context("query health")?;
                let mut loaded = Vec::new();
                for row in rows {
                    loaded.push(row.context("row health")?);
                }
                Ok(loaded)
            })
            .await
    }

    /// `provider_id` 与 `provider` 两个键都接受(Python 快照 dict 的键是
    /// `provider`,端口 docstring 写 `provider_id`);都取不到 → Invalid。
    async fn save_health(&self, snapshot: &HealthRow) -> Result<(), PortError> {
        if snapshot.provider_id.is_empty() {
            return Err(PortError::invalid(
                "provider_health snapshot requires a provider_id",
            ));
        }
        let snapshot = snapshot.clone();
        self.engine
            .write(move |connection| {
                connection
                    .execute(
                        "INSERT INTO provider_health(provider_id, status, available, latency_ms, \
                         checked_at, error) VALUES(?1, ?2, ?3, ?4, ?5, ?6) \
                         ON CONFLICT(provider_id) DO UPDATE SET status=excluded.status, \
                         available=excluded.available, latency_ms=excluded.latency_ms, \
                         checked_at=excluded.checked_at, error=excluded.error",
                        params![
                            snapshot.provider_id,
                            snapshot.status,
                            i64::from(snapshot.available),
                            snapshot.latency_ms,
                            snapshot.checked_at,
                            snapshot.error
                        ],
                    )
                    .context("save health")?;
                Ok(())
            })
            .await
    }
}

/// 编译期保证:`BreakerHealthStore` 合并面确实由上面两个 impl 合成
/// (blanket impl 生效,否则这一行就不过)。
const _: fn() -> bool = || {
    fn assert_store<T: BreakerHealthStore>() {}
    assert_store::<SqliteStore>();
    true
};
