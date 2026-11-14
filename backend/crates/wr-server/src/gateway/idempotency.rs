//! 幂等重放缓存(方案 #14 R4.4;镜像 Python `application/gateway/
//! idempotency.py` 的 `IdempotencyStore` 类)。
//!
//! 两层结构:内存表(含 `pending` 合并)+ `idempotency_entries` 表持久化
//! (只写 `completed` 且状态码可缓存的条目)。语义要点:
//! - **流式响应永不缓存**(只有非流式管线调 `complete`);
//! - 同键不同请求体 → 冲突(上层转 409 `idempotency_key_conflict`);
//! - `pending` 只活在内存:进程死了下一个调用方直接重跑,这正是幂等的
//!   正确语义;
//! - 每次读、每次写都跑一遍 `prune`(过期 + 超过 `max_entries` 的 LRU,
//!   `pending` 条目受保护不被淘汰)。
//!
//! ## 时钟基
//! Python 把 `time.monotonic()*1000` 落库(开机起算),Rust 用 epoch 毫秒。
//! 迁移期双方共读同一数据目录时,[`IdempotencyCache::reload`] 用
//! [`normalize_future_ms`] 把「明显是外来时钟基」的条目按已过期删除 ——
//! 与 Python 单进程重启后单调时钟仍可比较的行为等价,且绝不重放陈旧响应。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value, json};
use wr_app::ports::{GatewayStore, IdempotencyRow};
use wr_infra::config::IdempotencyConfig;
use wr_infra::sqlite::engine::{normalize_future_ms, wall_ms};

/// 一条重放条目(`state` ∈ `pending` | `completed`)。
#[derive(Debug, Clone)]
pub struct IdempotencyEntry {
    pub request_hash: String,
    pub status_code: i64,
    pub payload: Value,
    pub headers: Map<String, Value>,
    /// 过期时刻(epoch 毫秒)。
    pub expires_at_ms: i64,
    pub created_at_ms: i64,
    pub state: EntryState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryState {
    Pending,
    Completed,
}

/// 键被复用但请求体不同。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdempotencyConflict;

#[derive(Default)]
struct CacheState {
    entries: HashMap<String, IdempotencyEntry>,
}

/// 重放缓存。克隆廉价(内部 `Arc<Mutex>`),网关与测试共用一份。
pub struct IdempotencyCache {
    store: Option<Arc<dyn GatewayStore>>,
    config: Arc<Mutex<IdempotencyConfig>>,
    state: Arc<Mutex<CacheState>>,
    clock: Arc<dyn Fn() -> i64 + Send + Sync>,
}

impl std::fmt::Debug for IdempotencyCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self.state.lock().map_or(0, |guard| guard.entries.len());
        f.debug_struct("IdempotencyCache")
            .field("entries", &count)
            .field("persistent", &self.store.is_some())
            .finish()
    }
}

impl IdempotencyCache {
    pub fn new(store: Option<Arc<dyn GatewayStore>>, config: IdempotencyConfig) -> Self {
        Self {
            store,
            config: Arc::new(Mutex::new(config)),
            state: Arc::new(Mutex::new(CacheState::default())),
            clock: Arc::new(wall_ms),
        }
    }

    pub fn with_clock(mut self, clock: impl Fn() -> i64 + Send + Sync + 'static) -> Self {
        self.clock = Arc::new(clock);
        self
    }

    pub fn update_config(&self, config: &IdempotencyConfig) {
        let mut guard = self.config.lock().unwrap_or_else(|e| e.into_inner());
        *guard = config.clone();
    }

    fn config(&self) -> IdempotencyConfig {
        self.config
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn now_ms(&self) -> i64 {
        (self.clock)()
    }

    /// TTL 秒数:`max(1, ttl_ms // 1000)`。
    fn ttl_ms(&self) -> i64 {
        (self.config().ttl_ms / 1000).max(1) * 1000
    }

    /// 查一条并顺带 prune;键冲突返回 `Err(IdempotencyConflict)`,
    /// 未命中/未启用返回 `Ok(None)`。
    pub async fn lookup(
        &self,
        store_key: &str,
        request_hash: &str,
    ) -> Result<Option<IdempotencyEntry>, IdempotencyConflict> {
        if !self.config().enabled {
            return Ok(None);
        }
        self.prune().await;
        let entry = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .get(store_key)
            .cloned();
        let Some(entry) = entry else {
            return Ok(None);
        };
        if entry.request_hash != request_hash {
            return Err(IdempotencyConflict);
        }
        Ok(Some(entry))
    }

    /// 只取 `completed` 且未过期的条目(过期即删)。
    pub async fn get_completed(&self, store_key: &str) -> Option<IdempotencyEntry> {
        let entry = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .get(store_key)
            .cloned();
        let entry = match entry {
            Some(entry) => entry,
            None => return None,
        };
        if entry.state != EntryState::Completed {
            return None;
        }
        if entry.expires_at_ms <= self.now_ms() {
            self.delete(store_key).await;
            return None;
        }
        Some(entry)
    }

    /// 占位一条 in-flight 记录(仅内存)。
    pub async fn put_pending(&self, store_key: &str, request_hash: &str) {
        if !self.config().enabled {
            return;
        }
        let now = self.now_ms();
        let ttl = self.ttl_ms();
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .insert(
                store_key.to_owned(),
                IdempotencyEntry {
                    request_hash: request_hash.to_owned(),
                    status_code: 0,
                    payload: Value::Object(Map::new()),
                    headers: Map::new(),
                    expires_at_ms: now + ttl,
                    created_at_ms: now,
                    state: EntryState::Pending,
                },
            );
        self.prune().await;
    }

    /// 完成一条:进内存,状态码可缓存时落库。
    pub async fn complete(
        &self,
        store_key: &str,
        request_hash: &str,
        status_code: i64,
        payload: &Value,
        headers: Option<Map<String, Value>>,
    ) {
        if !self.config().enabled {
            return;
        }
        let now = self.now_ms();
        let ttl = self.ttl_ms();
        let entry = IdempotencyEntry {
            request_hash: request_hash.to_owned(),
            status_code,
            payload: payload.clone(),
            headers: headers.unwrap_or_default(),
            expires_at_ms: now + ttl,
            created_at_ms: now,
            state: EntryState::Completed,
        };
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .insert(store_key.to_owned(), entry.clone());
        if self.store.is_some() && is_cacheable(status_code, &self.config()) {
            self.persist(store_key, &entry).await;
        }
        self.prune().await;
    }

    /// 过期清理 + `max_entries` LRU(按创建时间升序淘汰 completed)。
    pub async fn prune(&self) {
        if !self.config().enabled {
            return;
        }
        let config = self.config();
        let now = self.now_ms();
        let expired: Vec<String> = {
            let guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
            guard
                .entries
                .iter()
                .filter(|(_, entry)| entry.expires_at_ms <= now)
                .map(|(key, _)| key.clone())
                .collect()
        };
        for key in expired {
            self.delete(&key).await;
        }
        let overflow = {
            let guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.entries.len() as i64 - config.max_entries
        };
        if overflow > 0 {
            let victims: Vec<String> = {
                let guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
                let mut candidates: Vec<(i64, String)> = guard
                    .entries
                    .iter()
                    .filter(|(_, entry)| entry.state == EntryState::Completed)
                    .map(|(key, entry)| (entry.created_at_ms, key.clone()))
                    .collect();
                candidates.sort_by_key(|(created, _)| *created);
                candidates
                    .into_iter()
                    .take(overflow.max(0) as usize)
                    .map(|(_, key)| key)
                    .collect()
            };
            for key in victims {
                self.delete(&key).await;
            }
        }
    }

    /// 从库里重载 completed 条目(过期即删,不进内存)。
    pub async fn reload(&self) {
        let Some(store) = self.store.as_ref() else {
            return;
        };
        let Ok(rows) = store.list_idempotency_entries().await else {
            return;
        };
        let now = self.now_ms();
        // 可信未来上限 = 2× 当前 TTL:超过它的一律视为外来时钟基。
        let horizon = self.ttl_ms().saturating_mul(2).max(1);
        // 先在锁内一次性算出「该删的键」并把保留条目写进内存,不在持锁期间
        // `await` 存储(防死锁 + 缩短临界区);删库动作推迟到释放锁之后。
        let mut to_delete: Vec<String> = Vec::new();
        {
            let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
            for row in rows {
                let expires_at = normalize_future_ms(row.expires_at, now, horizon);
                if expires_at == 0 {
                    to_delete.push(row.store_key.clone());
                    guard.entries.remove(&row.store_key);
                    continue;
                }
                let payload = serde_json::from_str::<Value>(&row.payload).unwrap_or(Value::Null);
                if payload.is_null() {
                    // Python: json.loads 抛错 → continue(整条跳过)。
                    continue;
                }
                let headers = serde_json::from_str::<Value>(&row.headers)
                    .ok()
                    .and_then(|value| value.as_object().cloned())
                    .unwrap_or_default();
                guard.entries.insert(
                    row.store_key.clone(),
                    IdempotencyEntry {
                        request_hash: row.request_hash.clone(),
                        status_code: row.status_code,
                        payload,
                        headers,
                        expires_at_ms: expires_at,
                        created_at_ms: row.created_at,
                        state: EntryState::Completed,
                    },
                );
            }
        }
        // 锁已释放,再异步清理过期条目。
        for key in to_delete {
            let _ = store.delete_idempotency(&key).await;
        }
    }

    pub async fn clear(&self) {
        let keys: Vec<String> = {
            let guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.entries.keys().cloned().collect()
        };
        for key in keys {
            self.delete(&key).await;
        }
    }

    pub fn len(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    async fn delete(&self, store_key: &str) {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .remove(store_key);
        if let Some(store) = self.store.as_ref() {
            let _ = store.delete_idempotency(store_key).await;
        }
    }

    async fn persist(&self, store_key: &str, entry: &IdempotencyEntry) {
        let Some(store) = self.store.as_ref() else {
            return;
        };
        let row = IdempotencyRow {
            store_key: store_key.to_owned(),
            request_hash: entry.request_hash.clone(),
            status_code: entry.status_code,
            payload: wr_domain::json_py::py_json_dumps(&entry.payload),
            headers: wr_domain::json_py::py_json_dumps(&Value::Object(entry.headers.clone())),
            expires_at: entry.expires_at_ms,
            created_at: entry.created_at_ms,
        };
        if let Err(error) = store.save_idempotency(&row).await {
            tracing::warn!(target: "wrouter.gateway.idempotency", %store_key, %error,
                           "idempotency_persist_failed");
        }
    }
}

/// 可缓存状态码:2xx 恒可;4xx 需 `cache_error_responses`(4xx 且 <500)。
pub fn is_cacheable(status_code: i64, config: &IdempotencyConfig) -> bool {
    if (200..300).contains(&status_code) {
        return true;
    }
    config.cache_error_responses && (400..500).contains(&status_code)
}

/// 重放键:`{key}:{protocol}:{sha256(sorted-compact-json)}`。
pub fn idempotency_store_key(key: &str, protocol: &str, payload: &Value) -> String {
    let body = wr_domain::json_py::py_json_dumps_sorted_compact(payload);
    format!(
        "{key}:{protocol}:{}",
        wr_domain::digest::sha256_hex(body.as_bytes())
    )
}

/// 请求指纹:`sha256({"k":protocol,"p":payload} 的排序紧凑 JSON)`。
pub fn request_hash(payload: &Value, protocol: &str) -> String {
    let mut wrapper = Map::new();
    wrapper.insert("p".to_owned(), payload.clone());
    wrapper.insert("k".to_owned(), json!(protocol));
    let body = wr_domain::json_py::py_json_dumps_sorted_compact(&Value::Object(wrapper));
    wr_domain::digest::sha256_hex(body.as_bytes())
}

/// 读 header 里的幂等键(键名来自配置,大小写不敏感)。
pub fn idempotency_header(headers: &[(String, String)], header_name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(header_name))
        .map(|(_, value)| value.clone())
        .filter(|value| !value.is_empty())
}
