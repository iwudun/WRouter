//! 响应缓存磁盘层(20260905_03 §4.4 S6;Rust 新增,无 Python 镜像)。
//!
//! 内存 LRU(见 wr-server `gateway::response_cache`)之外的二层:
//! `response_cache` 表按 `cache_key` 主键存取,TTL 独立(默认 1h,容量
//! 1000 条,由调用方传配置)。读路径 SQLite 命中由调用方晋升内存。
//! 端口在 wr-app `ports::ResponseCacheStore`,并纳入 `GatewayStore`
//! 组合端口(gateway 侧 `deps.store` 直接调用)。

use async_trait::async_trait;
use rusqlite::{OptionalExtension, params};
use serde_json::Value;

use wr_app::ports::{PortError, ResponseCacheDiskRow, ResponseCacheStore};

use super::SqlContext as _;
use super::SqliteStore;

#[async_trait]
impl ResponseCacheStore for SqliteStore {
    /// 读缓存条目(含 TTL 门:过期即视为未命中,行由 put 路径惰性清理)。
    async fn response_cache_get(
        &self,
        cache_key: &str,
        now_ms: i64,
    ) -> Result<Option<ResponseCacheDiskRow>, PortError> {
        let cache_key = cache_key.to_owned();
        self.engine
            .read(move |connection| {
                connection
                    .query_row(
                        "SELECT payload, cost_usd, model, created_at_ms FROM response_cache \
                         WHERE cache_key = ?1 AND expires_at_ms > ?2",
                        params![cache_key, now_ms],
                        |row| {
                            let payload_text: String = row.get(0)?;
                            let payload = serde_json::from_str(&payload_text)
                                .map_err(|e| {
                                    rusqlite::Error::FromSqlConversionFailure(
                                        0,
                                        rusqlite::types::Type::Text,
                                        e.into(),
                                    )
                                })?;
                            Ok(ResponseCacheDiskRow {
                                payload,
                                cost_usd: row.get(1)?,
                                model: row.get(2)?,
                                created_at_ms: row.get(3)?,
                            })
                        },
                    )
                    .optional()
                    .context("read response_cache")
            })
            .await
    }

    /// 写缓存条目:INSERT OR REPLACE + 惰性清理(过期行)+ 容量裁剪
    /// (超容量时保留 created_at_ms 最新的 max_entries 条)。
    async fn response_cache_put(
        &self,
        cache_key: &str,
        payload: &Value,
        cost_usd: f64,
        model: &str,
        ttl_secs: i64,
        max_entries: usize,
        now_ms: i64,
    ) -> Result<(), PortError> {
        let cache_key = cache_key.to_owned();
        let payload_text = serde_json::to_string(payload)
            .map_err(|e| PortError::storage(format!("serialize response cache failed: {e}")))?;
        let model = model.to_owned();
        let expires_at_ms = now_ms.saturating_add(ttl_secs.max(0) * 1000);
        self.engine
            .write(move |connection| {
                connection
                    .execute(
                        "INSERT INTO response_cache(cache_key, payload, cost_usd, model, \
                         created_at_ms, expires_at_ms) VALUES(?1, ?2, ?3, ?4, ?5, ?6) \
                         ON CONFLICT(cache_key) DO UPDATE SET \
                         payload=excluded.payload, cost_usd=excluded.cost_usd, \
                         model=excluded.model, created_at_ms=excluded.created_at_ms, \
                         expires_at_ms=excluded.expires_at_ms",
                        params![cache_key, payload_text, cost_usd, model, now_ms, expires_at_ms],
                    )
                    .context("write response_cache")?;
                // 惰性清过期。
                connection
                    .execute(
                        "DELETE FROM response_cache WHERE expires_at_ms <= ?1",
                        params![now_ms],
                    )
                    .context("prune expired response_cache")?;
                // 容量裁剪:保留最新 max_entries 条。
                connection
                    .execute(
                        "DELETE FROM response_cache WHERE cache_key NOT IN (\
                         SELECT cache_key FROM response_cache \
                         ORDER BY created_at_ms DESC, cache_key LIMIT ?1)",
                        params![max_entries as i64],
                    )
                    .context("trim response_cache")?;
                Ok(())
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    async fn open_temp(name: &str) -> SqliteStore {
        let dir = std::env::temp_dir().join(format!(
            "wr-s6-response-cache-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        SqliteStore::open(&dir)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn put_get_roundtrip_and_ttl() {
        let store = open_temp("roundtrip").await;
        let payload = json!({"id": "msg_1", "content": "hello"});
        store
            .response_cache_put("k1", &payload, 0.012, "m1", 3_600, 100, 10_000)
            .await
            .unwrap();
        // TTL 内命中。
        let row = store.response_cache_get("k1", 11_000).await.unwrap();
        assert_eq!(
            row,
            Some(ResponseCacheDiskRow {
                payload: payload.clone(),
                cost_usd: 0.012,
                model: "m1".to_owned(),
                created_at_ms: 10_000,
            })
        );
        // 过期即 miss。
        assert_eq!(store.response_cache_get("k1", 10_000 + 3_600_000).await.unwrap(), None);
    }

    #[tokio::test]
    async fn put_is_upsert_and_capacity_trims_oldest() {
        let store = open_temp("capacity").await;
        for i in 0..5 {
            store
                .response_cache_put(
                    &format!("k{i}"),
                    &json!({"i": i}),
                    0.0,
                    "m",
                    3_600_000,
                    3,
                    1_000 + i,
                )
                .await
                .unwrap();
        }
        // 容量 3:最旧的 k0/k1 被裁。
        assert!(store.response_cache_get("k0", 2_000).await.unwrap().is_none());
        assert!(store.response_cache_get("k1", 2_000).await.unwrap().is_none());
        assert!(store.response_cache_get("k2", 2_000).await.unwrap().is_some());
        assert!(store.response_cache_get("k4", 2_000).await.unwrap().is_some());
        // 同键 upsert:覆盖且不占新容量位。
        store
            .response_cache_put("k4", &json!({"i": "new"}), 0.0, "m", 3_600_000, 3, 9_000)
            .await
            .unwrap();
        let row = store.response_cache_get("k4", 9_500).await.unwrap().unwrap();
        assert_eq!(row.payload, json!({"i": "new"}));
    }

    #[tokio::test]
    async fn put_prunes_expired_rows() {
        let store = open_temp("prune").await;
        store
            .response_cache_put("a", &json!(1), 0.0, "m", 100, 100, 1_000)
            .await
            .unwrap();
        store
            .response_cache_put("b", &json!(2), 0.0, "m", 100, 100, 5_000)
            .await
            .unwrap();
        // 此时 a(1_000+100_000)未过期;推进到 b 过期后,put 触发惰性清理。
        store
            .response_cache_put("c", &json!(3), 0.0, "m", 100, 100, 5_000 + 100_000)
            .await
            .unwrap();
        assert!(store.response_cache_get("a", 106_000).await.unwrap().is_none());
        assert!(store.response_cache_get("b", 106_000).await.unwrap().is_none());
        assert!(store.response_cache_get("c", 106_000).await.unwrap().is_some());
    }
}
