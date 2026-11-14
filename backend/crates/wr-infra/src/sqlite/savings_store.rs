//! 节省事件存储与汇总(20260905_03 §4.6 S8;Rust 新增,无 Python 镜像)。
//!
//! `savings_events`:每请求一行,**仅记录发生了节省行为的**:
//! - kind = `compression`:压缩管线有引擎实际生效(估算 tokens_saved +
//!   engine_breakdown JSON;真实 usage 回填 actual_tokens_saved 可空,
//!   v1 暂记 NULL —— 上游只报压缩后 prompt tokens,未压缩基线不可知,
//!   校准策略见方案 §7.5「节省金额是估算」);
//! - kind = `response_cache`:响应缓存命中(tokens = 被免掉的用量,
//!   金额 = 存储时的原始成本);
//! - kind = `prefix_hit`:保留字面,v1 不写(上游缓存命中节省由
//!   request_logs 的 cache_read_tokens 折算,见 summary 的 upstreamCache,
//!   避免双账)。
//!
//! 汇总 [`SqliteStore::savings_summary`]:三本账一次出 —— 压缩账(events)、
//! 响应缓存账(events)、上游缓存账(request_logs.cache_read_tokens ×
//! 供应商 input 单价,口径镜像 usage_store 的目录读取)。

use std::collections::HashMap;

use async_trait::async_trait;
use chrono::Utc;
use rusqlite::{params};
use serde_json::{Value, json};

use wr_app::ports::{ConfigStore as _, PortError, SavingsEventInput, SavingsEventStore};

use super::SqlContext as _;
use super::SqliteStore;
use super::usage_store::{int_or, load_rows, range_bounds, str_or_empty};

fn now_created_at() -> String {
    Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

#[async_trait]
impl SavingsEventStore for SqliteStore {
    /// 追加一条节省事件(请求路径调用;失败由调用方决定是否容忍 ——
    /// 度量非关键路径,forward 侧静默降级)。
    async fn record_savings_event(&self, event: &SavingsEventInput) -> Result<(), PortError> {
        let created_at = now_created_at();
        let request_id = event.request_id.clone();
        let kind = event.kind.to_owned();
        let tokens_saved = event.tokens_saved;
        let estimated_usd_saved = event.estimated_usd_saved;
        let engine_breakdown = event
            .engine_breakdown
            .as_ref()
            .map(Value::to_string)
            .unwrap_or_default();
        self.engine
            .write(move |connection| {
                connection
                    .execute(
                        "INSERT INTO savings_events(request_id, created_at, kind, tokens_saved, \
                         estimated_usd_saved, actual_tokens_saved, engine_breakdown, skip_reason) \
                         VALUES(?1, ?2, ?3, ?4, ?5, NULL, ?6, NULL)",
                        params![request_id, created_at, kind, tokens_saved, estimated_usd_saved, engine_breakdown],
                    )
                    .context("insert savings_event")?;
                Ok(())
            })
            .await
    }

    /// 三本账汇总(时段语义与 usage_summary 同源:today/24h/7d/30d/all,
    /// 自定义窗口 `from..to`,未知名 = 全表)。输出 camelCase(前端直读)。
    async fn savings_summary(&self, period: &str) -> Result<Value, PortError> {
        let range_key = if period.is_empty() { "today" } else { period };
        let (min_moment, max_moment) = range_bounds(range_key);
        let bind = min_moment.map(|moment| moment.format("%Y-%m-%d %H:%M:%S").to_string());
        let hi_bind = max_moment.map(|moment| moment.format("%Y-%m-%d %H:%M:%S").to_string());

        // --- 账一/账二:savings_events -----------------------------------
        let events_bind = bind.clone();
        let events_hi = hi_bind.clone();
        let events_rows = self
            .engine
            .read(move |connection| {
                let mut statement = connection
                    .prepare(
                        "SELECT kind, tokens_saved, engine_breakdown \
                         FROM savings_events WHERE (?1 IS NULL OR created_at >= ?1) \
                         AND (?2 IS NULL OR created_at < ?2) \
                         ORDER BY rowid",
                    )
                    .context("prepare savings_events")?;
                let rows = statement
                    .query_map(params![events_bind, events_hi], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            // 旧行/外部写入可能为 NULL;按空串处理,不丢账。
                            row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                        ))
                    })
                    .context("query savings_events")?;
                let mut out = Vec::new();
                for row in rows {
                    out.push(row.context("read savings_events row")?);
                }
                Ok(out)
            })
            .await?;

        // 压缩账。
        let mut compression_requests = 0i64;
        let mut compression_tokens = 0i64;
        // engine_id → (applied, chars_saved)
        let mut engines: Vec<(String, i64, i64)> = Vec::new();
        let mut skips: Vec<(String, i64)> = Vec::new();
        // 响应缓存账。
        let mut cache_hits = 0i64;
        let mut cache_tokens = 0i64;

        for (kind, tokens_saved, breakdown_text) in events_rows {
            match kind.as_str() {
                "compression" => {
                    compression_requests += 1;
                    compression_tokens += tokens_saved;
                    // 引擎明细聚合(breakdown 解析失败按无明细处理,不丢账)。
                    let Ok(breakdown) = serde_json::from_str::<Value>(&breakdown_text) else {
                        continue;
                    };
                    if let Some(list) = breakdown.get("engines").and_then(Value::as_array) {
                        for engine in list {
                            let id = str_or_empty(engine.get("id"));
                            if id.is_empty() {
                                continue;
                            }
                            let changed = engine
                                .get("changed")
                                .and_then(Value::as_bool)
                                .unwrap_or(false);
                            // 引擎账只收**实际生效**的(跳过的进 skips,避免
                            // applied=0 的哑行刷屏)。
                            if changed {
                                let before = int_or(engine.get("charsBefore"), 0);
                                let after = int_or(engine.get("charsAfter"), 0);
                                match engines.iter_mut().find(|(name, _, _)| *name == id) {
                                    Some((_, applied, chars)) => {
                                        *applied += 1;
                                        *chars += before.saturating_sub(after);
                                    }
                                    None => engines.push((
                                        id,
                                        1,
                                        before.saturating_sub(after),
                                    )),
                                }
                            }
                            if let Some(reason) =
                                engine.get("skipReason").and_then(Value::as_str)
                            {
                                match skips.iter_mut().find(|(name, _)| name == reason) {
                                    Some((_, count)) => *count += 1,
                                    None => skips.push((reason.to_owned(), 1)),
                                }
                            }
                        }
                    }
                }
                "response_cache" => {
                    cache_hits += 1;
                    cache_tokens += tokens_saved;
                }
                _ => {}
            }
        }
        engines.sort_by(|left, right| left.0.cmp(&right.0));
        skips.sort_by(|left, right| left.0.cmp(&right.0));

        // --- 账三:上游缓存命中(request_logs.cache_read_tokens × 单价)---
        let prices = self.input_price_by_provider().await;
        let log_rows = self
            .engine
            .read(move |connection| load_rows(connection, bind.as_deref(), hi_bind.as_deref()))
            .await?;
        let mut upstream_cache_tokens = 0i64;
        let mut upstream_cache_usd = 0f64;
        for row in &log_rows {
            if let (Some(min), Some(moment)) = (min_moment, row.created)
                && moment < min
            {
                continue;
            }
            // 自定义窗口上界(不含):窗口之后的行不入账。
            if let (Some(max), Some(moment)) = (max_moment, row.created)
                && moment >= max
            {
                continue;
            }
            let read = int_or(row.item.get("cache_read_tokens"), 0);
            if read <= 0 {
                continue;
            }
            let provider = str_or_empty(row.item.get("provider"));
            let price = prices.get(provider.as_str()).copied().unwrap_or(0.0);
            upstream_cache_tokens += read;
            upstream_cache_usd += read as f64 / 1_000_000.0 * price;
        }

        Ok(json!({
            "period": range_key,
            "compression": {
                "requests": compression_requests,
                "tokensSaved": compression_tokens,
                "engines": engines.iter().map(|(id, applied, chars)| json!({
                    "id": id, "applied": applied, "charsSaved": chars,
                })).collect::<Vec<_>>(),
                "skips": skips.iter().map(|(reason, count)| json!({
                    "reason": reason, "count": count,
                })).collect::<Vec<_>>(),
            },
            "responseCache": {
                "hits": cache_hits,
                "tokensSaved": cache_tokens,
            },
            "upstreamCache": {
                "cacheReadTokens": upstream_cache_tokens,
                "estimatedUsdSaved": round6(upstream_cache_usd),
            },
        }))
    }

}

impl SqliteStore {
    /// provider_id → input_per_million 单价(每次重读配置,镜像
    /// usage_summary 的目录读取立场;读失败 = 空表,金额退化为 0)。
    async fn input_price_by_provider(&self) -> HashMap<String, f64> {
        let mut prices = HashMap::new();
        let Ok(payload) = self.load_payload().await else {
            return prices;
        };
        let Some(Value::Array(providers)) = payload.get("Providers") else {
            return prices;
        };
        for provider in providers {
            let Some(map) = provider.as_object() else {
                continue;
            };
            let id = map
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if id.is_empty() {
                continue;
            }
            let price = map
                .get("billing")
                .and_then(|billing| billing.get("input_per_million"))
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            prices.insert(id, price);
        }
        prices
    }
}

fn round6(value: f64) -> f64 {
    (value * 1_000_000.0).round() / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::Path;

    async fn open_temp(name: &str) -> SqliteStore {
        let dir = std::env::temp_dir().join(format!("wr-s8-savings-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        SqliteStore::open(&Path::new(&dir)).await.unwrap()
    }

    #[tokio::test]
    async fn events_roundtrip_and_summary_accounts() {
        let store = open_temp("summary").await;
        store
            .record_savings_event(&SavingsEventInput {
                request_id: "r1".to_owned(),
                kind: "compression",
                tokens_saved: 1_000,
                estimated_usd_saved: 0.003,
                engine_breakdown: Some(json!({
                    "engines": [
                        {"id": "whitespace-lite", "changed": true, "skipReason": null, "charsBefore": 4000, "charsAfter": 1000},
                        {"id": "tool-output-trim", "changed": false, "skipReason": "fidelity", "charsBefore": 0, "charsAfter": 0},
                    ],
                })),
            })
            .await
            .unwrap();
        store
            .record_savings_event(&SavingsEventInput {
                request_id: "r2".to_owned(),
                kind: "compression",
                tokens_saved: 500,
                estimated_usd_saved: 0.0015,
                engine_breakdown: None,
            })
            .await
            .unwrap();
        store
            .record_savings_event(&SavingsEventInput {
                request_id: "r3".to_owned(),
                kind: "response_cache",
                tokens_saved: 2_500,
                estimated_usd_saved: 0.01,
                engine_breakdown: None,
            })
            .await
            .unwrap();

        let summary = store.savings_summary("").await.unwrap();
        assert_eq!(summary["compression"]["requests"], json!(2));
        assert_eq!(summary["compression"]["tokensSaved"], json!(1500));
        let engines = summary["compression"]["engines"].as_array().unwrap();
        assert_eq!(engines.len(), 1);
        assert_eq!(engines[0]["id"], json!("whitespace-lite"));
        assert_eq!(engines[0]["applied"], json!(1));
        assert_eq!(engines[0]["charsSaved"], json!(3000));
        let skips = summary["compression"]["skips"].as_array().unwrap();
        assert_eq!(skips[0]["reason"], json!("fidelity"));
        assert_eq!(summary["responseCache"]["hits"], json!(1));
        assert_eq!(summary["responseCache"]["tokensSaved"], json!(2500));
        assert_eq!(summary["upstreamCache"]["cacheReadTokens"], json!(0));
        // 时段过滤:7d 覆盖刚刚写入的事件;不存在的时段名 = 全表,同样非空。
        let summary = store.savings_summary("7d").await.unwrap();
        assert_eq!(summary["compression"]["requests"], json!(2));
    }

    #[tokio::test]
    async fn empty_summary_shape_is_stable() {
        let store = open_temp("empty").await;
        let summary = store.savings_summary("24h").await.unwrap();
        assert_eq!(summary["period"], json!("24h"));
        assert_eq!(summary["compression"]["requests"], json!(0));
        assert_eq!(summary["compression"]["engines"].as_array().unwrap().len(), 0);
        assert_eq!(summary["responseCache"]["hits"], json!(0));
        assert_eq!(summary["upstreamCache"]["estimatedUsdSaved"], json!(0.0));
    }

    /// 旧行/外部写入的 `engine_breakdown` 可能是 NULL(dev 造数脚本即如此);
    /// all 窗口会扫到它们,读取层必须按空明细处理,不能 500。
    #[tokio::test]
    async fn null_engine_breakdown_rows_do_not_fail_summary() {
        let store = open_temp("null-breakdown").await;
        store
            .engine
            .write(|connection| {
                connection
                    .execute(
                        "INSERT INTO savings_events(request_id, created_at, kind, tokens_saved, \
                         estimated_usd_saved, actual_tokens_saved, engine_breakdown, skip_reason) \
                         VALUES(?1, ?2, ?3, ?4, 0, NULL, NULL, NULL)",
                        params!["r-null", "2026-09-20 09:30:00", "compression", 12i64],
                    )
                    .context("insert null breakdown")?;
                Ok(())
            })
            .await
            .unwrap();
        let summary = store.savings_summary("all").await.unwrap();
        assert_eq!(summary["compression"]["requests"], json!(1));
        assert_eq!(summary["compression"]["tokensSaved"], json!(12));
        assert_eq!(summary["compression"]["engines"].as_array().unwrap().len(), 0);
    }
}
