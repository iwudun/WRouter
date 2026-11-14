//! 网关响应缓存 · 内存层(20260905_03 §4.4 S6;Rust 新增,无 Python 镜像)。
//!
//! 两层存储的内存侧:LRU(容量/字节上限随全局 `savings.responseCache`
//! 配置,TTL 默认 30min);磁盘层在 wr-infra `response_cache_store`。
//! 读路径:内存 → 磁盘(命中晋升内存,见 forward 的 `response_cache_lookup`)。
//!
//! 键与门控在 wr-domain `gateway::response_cache_key`(显式 temp==0 才有
//! 键;protocol/model/messages/top_p/tools 参与键);进程内存态,重启清空,
//! 与粘性表/冷却态同一立场(方案 §8)。

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use serde_json::Value;

use wr_infra::config::SavingsResponseCacheConfig;

/// 内存缓存条目(解码后的客户端协议响应 JSON + 原始成本,命中时用于
/// `x-wr-cost-saved` 与节省度量)。
#[derive(Debug, Clone, PartialEq)]
pub struct CachedResponse {
    pub payload: Value,
    pub cost_usd: f64,
    pub model: String,
    pub created_at_ms: i64,
}

impl CachedResponse {
    fn body_bytes(&self) -> usize {
        self.payload.to_string().len()
    }
}

struct Inner {
    map: HashMap<String, (CachedResponse, i64)>,
    order: VecDeque<String>,
    bytes: usize,
    max_entries: usize,
    max_bytes: usize,
}

/// 内存响应缓存(进程内;`&self` + 内部锁)。
pub struct ResponseCache {
    inner: Mutex<Inner>,
}

impl Default for ResponseCache {
    fn default() -> Self {
        // 默认上限与 SavingsResponseCacheConfig 默认值一致(64 条 / 5MB);
        // 运行时由 GatewayDeps::sync_runtime_config 随配置刷新。
        Self::new(64, 5 * 1024 * 1024)
    }
}

impl ResponseCache {
    pub fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                map: HashMap::new(),
                order: VecDeque::new(),
                bytes: 0,
                max_entries: max_entries.max(1),
                max_bytes: max_bytes.max(1),
            }),
        }
    }

    /// 配置同步:刷新容量上限(只影响此后 put 的淘汰判定)。
    pub fn set_limits(&self, max_entries: usize, max_bytes: usize) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.max_entries = max_entries.max(1);
        inner.max_bytes = max_bytes.max(1);
    }

    /// 查缓存;过期即删。`now_ms` epoch 毫秒(与粘性表同时钟口径)。
    pub fn get(&self, key: &str, now_ms: i64) -> Option<CachedResponse> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let expired = matches!(inner.map.get(key), Some((_, until)) if *until <= now_ms);
        if expired {
            if let Some((entry, _)) = inner.map.remove(key) {
                inner.bytes = inner.bytes.saturating_sub(entry.body_bytes());
            }
            return None;
        }
        inner.map.get(key).map(|(entry, _)| entry.clone())
    }

    /// 写缓存(成功响应专用写入路径):upsert + 惰性清过期 + 容量淘汰
    /// (条数超 `max_entries` 或字节超 `max_bytes` 时按插入序淘汰最旧)。
    pub fn put(&self, key: &str, entry: CachedResponse, ttl_ms: i64, now_ms: i64) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((old, _)) = inner.map.remove(key) {
            inner.bytes = inner.bytes.saturating_sub(old.body_bytes());
            inner.order.retain(|k| k != key);
        }
        inner.bytes += entry.body_bytes();
        inner.map.insert(
            key.to_owned(),
            (entry, now_ms.saturating_add(ttl_ms.max(1))),
        );
        inner.order.push_back(key.to_owned());
        while (inner.map.len() > inner.max_entries || inner.bytes > inner.max_bytes)
            && inner.map.len() > 1
        {
            let Some(oldest) = inner.order.pop_front() else {
                break;
            };
            if let Some((entry, _)) = inner.map.remove(&oldest) {
                inner.bytes = inner.bytes.saturating_sub(entry.body_bytes());
            }
        }
    }

    /// 条目数:仅供同文件单测断言容量淘汰行为,生产路径不用(淘汰走
    /// `inner.map.len()` 直接判定)—— 故只编进测试目标,避免 dev 态
    /// `cargo build` 报 dead_code 噪音。
    #[cfg(test)]
    fn len(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .map
            .len()
    }
}

/// 内存上限随配置刷新(在 `GatewayDeps::sync_runtime_config` 调用)。
pub fn sync_limits(cache: &ResponseCache, config: &SavingsResponseCacheConfig) {
    cache.set_limits(config.memory_max_entries, config.memory_max_bytes);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(id: &str) -> CachedResponse {
        CachedResponse {
            payload: json!({"id": id}),
            cost_usd: 0.01,
            model: "m".to_owned(),
            created_at_ms: 10_000,
        }
    }

    #[test]
    fn put_get_roundtrip_and_ttl_expiry() {
        let cache = ResponseCache::new(10, 1 << 20);
        cache.put("k1", entry("msg_1"), 1_000, 10_000);
        assert_eq!(cache.get("k1", 10_500), Some(entry("msg_1")));
        assert_eq!(cache.get("k1", 11_000), None, "过期即删");
        assert_eq!(cache.get("k1", 11_500), None);
    }

    #[test]
    fn put_upserts_and_does_not_double_count_bytes() {
        let cache = ResponseCache::new(10, 1 << 20);
        cache.put("k", entry("a"), 1_000_000, 1);
        let bytes_after_first = cache.inner.lock().unwrap().bytes;
        cache.put("k", entry("b"), 1_000_000, 2);
        assert_eq!(cache.get("k", 3).unwrap().payload, json!({"id": "b"}));
        assert_eq!(
            cache.inner.lock().unwrap().bytes,
            bytes_after_first,
            "同键 upsert 字节量不重复累计"
        );
    }

    #[test]
    fn capacity_evicts_oldest_by_entries_then_bytes() {
        // 条数上限 2。
        let cache = ResponseCache::new(2, 1 << 20);
        cache.put("a", entry("a"), 1_000_000, 1);
        cache.put("b", entry("b"), 1_000_000, 2);
        cache.put("c", entry("c"), 1_000_000, 3);
        assert_eq!(cache.get("a", 4), None);
        assert_eq!(cache.len(), 2);
        // 字节上限:两个大条目只留一个。
        let big = || CachedResponse {
            payload: json!({"pad": "x".repeat(1_000)}),
            cost_usd: 0.0,
            model: "m".to_owned(),
            created_at_ms: 10_000,
        };
        let cache = ResponseCache::new(10, 1_500);
        cache.put("a", big(), 1_000_000, 1);
        cache.put("b", big(), 1_000_000, 2);
        assert_eq!(cache.len(), 1, "字节超限淘汰最旧");
    }
}
