//! 节省度量组:RPC `getSavingsStats([period?])`(方案 20260905_03 §4.6
//! S8;Rust 新增,无 Python 镜像)。
//!
//! 时段语义与 `getUsageStats` 同源(today/24h/7d/30d,未知名 = 全表);
//! 聚合在 wr-infra `SavingsEventStore::savings_summary`(三本账:压缩 /
//! 响应缓存 / 上游缓存命中)。

use std::sync::Arc;

use serde_json::Value;

use wr_app::ports::SavingsEventStore as _;
use wr_infra::sqlite::SqliteStore;

/// RPC `getSavingsStats([period?])`:period 仅收字符串(否则 "today",
/// 镜像 getUsageStats 的参数口径)。
pub async fn svc_savings_stats(store: &Arc<SqliteStore>, args: &[Value]) -> Result<Value, String> {
    let period = args
        .first()
        .and_then(Value::as_str)
        .unwrap_or("today")
        .to_owned();
    store
        .savings_summary(&period)
        .await
        .map_err(|e| e.to_string())
}
