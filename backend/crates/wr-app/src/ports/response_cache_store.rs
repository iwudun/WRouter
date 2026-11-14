//! 响应缓存端口(20260905_03 §4.4 S6;Rust 新增,无 Python 镜像)。
//!
//! 网关响应缓存的磁盘层读写;内存 LRU 在 wr-server
//! `gateway::response_cache`(进程内,不走端口)。读路径磁盘命中由调用方
//! 晋升内存;写路径仅成功响应。

use async_trait::async_trait;
use serde_json::Value;

use super::PortError;

/// 磁盘命中的解码产物(响应缓存条目主体)。
#[derive(Debug, Clone, PartialEq)]
pub struct ResponseCacheDiskRow {
    pub payload: Value,
    pub cost_usd: f64,
    pub model: String,
    /// 原始成功响应被缓存的时刻(epoch ms);命中方据此报告缓存年龄,
    /// 不把重放伪装成新的上游生成。
    pub created_at_ms: i64,
}

#[async_trait]
pub trait ResponseCacheStore: Send + Sync {
    /// 读缓存条目(含 TTL 门:过期即视为未命中)。
    async fn response_cache_get(
        &self,
        cache_key: &str,
        now_ms: i64,
    ) -> Result<Option<ResponseCacheDiskRow>, PortError>;

    /// 写缓存条目:upsert + 惰性清过期 + 容量裁剪(超容量保留最新)。
    async fn response_cache_put(
        &self,
        cache_key: &str,
        payload: &Value,
        cost_usd: f64,
        model: &str,
        ttl_secs: i64,
        max_entries: usize,
        now_ms: i64,
    ) -> Result<(), PortError>;
}
