//! 幂等存储端口(方案 #14 R4.4)。
//!
//! 镜像 Python `application/ports/idempotency_store.py`(抽象
//! `idempotency_entries` 表:load / save / delete / list / prune)。
//!
//! 命名澄清(与 Python 同名冲突,原样继承):应用层的
//! `application/gateway/idempotency.py::IdempotencyStore` 是**重放缓存**
//! (内存 + 本端口持久化),住在 `wr-server::gateway::idempotency`;
//! 本文件是**持久化端口**。
//!
//! `payload` / `headers` 两列在本端口里是**已序列化的 JSON 文本**
//! (与 Python 端口一致:重放缓存在上层做 `json.dumps`,端口只做字节落库)。

use async_trait::async_trait;

use super::error::PortError;

/// `idempotency_entries` 表一行。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IdempotencyRow {
    pub store_key: String,
    pub request_hash: String,
    pub status_code: i64,
    /// 响应体 JSON 文本(原样存,读出后上层 `json.loads`)。
    pub payload: String,
    /// 响应头 JSON 文本。
    pub headers: String,
    /// 过期时刻(单调时钟毫秒)。
    pub expires_at: i64,
    pub created_at: i64,
}

#[async_trait]
pub trait IdempotencyStore: Send + Sync {
    async fn load_idempotency(&self, store_key: &str) -> Result<Option<IdempotencyRow>, PortError>;

    /// upsert 一条(镜像 `ON CONFLICT(store_key) DO UPDATE` 全列覆盖)。
    async fn save_idempotency(&self, entry: &IdempotencyRow) -> Result<(), PortError>;

    async fn delete_idempotency(&self, store_key: &str) -> Result<(), PortError>;

    /// 全量条目(LRU 淘汰用)。
    async fn list_idempotency_entries(&self) -> Result<Vec<IdempotencyRow>, PortError>;

    /// 删除 `expires_at <= now_ms` 的过期条目,返回删除数。
    async fn prune_idempotency(&self, now_ms: i64) -> Result<i64, PortError>;
}
