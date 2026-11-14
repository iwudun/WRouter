//! 请求日志存储端口(方案 #14 R4.4/R4.5)。
//!
//! 镜像 Python `application/ports/request_log_store.py`。落库形态是
//! 「主行元数据 + 侧表正文」的 A9.7 拆分:`request_logs(request_id,
//! created_at, payload)` 主行只带元数据(约 2 KB),正文与响应头进
//! `request_log_bodies` 侧表,两行共用同一个 `SELECT datetime('now')`
//! 时间戳,使正文层的独立保留清理无需 join。
//!
//! C3 日志数据策略(每条一个命名测试):
//! - 元数据永久保留、正文默认 7 天过期 → [`RequestLogStore::prune_logs`];
//! - 正文默认不捕获 → 捕获门在 `wr-domain::observability::log_policy`;
//! - `callLogs` 开关只门控 request_logs,不影响用量统计。

use async_trait::async_trait;
use serde_json::{Map, Value};

use super::error::PortError;

/// 正文捕获写入侧表的一行(镜像 `append_request_log(bodies=…)` 的 dict)。
///
/// `None` 字段落 SQL `NULL`;`*_truncated` 落 `1/0` 整数。
#[derive(Debug, Clone, Default)]
pub struct LogBodies {
    pub request_body: Option<String>,
    pub request_body_size: Option<i64>,
    pub request_body_truncated: bool,
    pub response_body: Option<String>,
    pub response_body_size: Option<i64>,
    pub response_body_truncated: bool,
    /// 响应头对象;`None` 落 SQL `NULL`,否则按默认 `json.dumps` 形态存文本。
    pub response_headers: Option<Map<String, Value>>,
    /// 请求头的脱敏快照。
    pub request_headers: Option<Map<String, Value>>,
    /// 有序、受限的已捕获事件/attempt 明细(JSON)。
    pub trace_items: Vec<Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestLogTracePage {
    pub items: Vec<Value>,
    pub next_offset: Option<i64>,
    pub total: i64,
}

/// 一轮保留清理的计数(镜像 `prune_logs` 返回的 dict)。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PruneStats {
    pub bodies_deleted: i64,
    pub logs_deleted: i64,
    /// 被删正文的字符量(SQLite `length()` 按字符计),驱动 compact 判定。
    pub bytes_deleted: i64,
}

/// 批量落库的一行(20261016_01 B-6):主行 + 可选侧表,由实现放进
/// **单个事务**写入。
#[derive(Debug, Clone)]
pub struct PendingRequestLog {
    pub request_id: String,
    pub payload: Value,
    pub bodies: Option<LogBodies>,
}

#[async_trait]
pub trait RequestLogStore: Send + Sync {
    /// 落一行请求日志。`bodies = None`(未开启捕获)时会 **删除** 同
    /// `request_id` 的旧侧表行,防止 id 复用翻出历史正文。
    async fn append_request_log(
        &self,
        request_id: &str,
        payload: &Value,
        bodies: Option<&LogBodies>,
    ) -> Result<(), PortError>;

    /// 批量落库(20261016_01 B-6):多行一个事务,替代逐行单事务 ——
    /// 日志主表/正文表/trace 表与其它 SQLite 写共享单写线程,批量化
    /// 直接摊薄每行的事务与 fsync 开销。默认实现逐行退化为
    /// [`RequestLogStore::append_request_log`]。
    async fn append_request_log_batch(&self, batch: &[PendingRequestLog]) -> Result<(), PortError> {
        for row in batch {
            self.append_request_log(&row.request_id, &row.payload, row.bodies.as_ref())
                .await?;
        }
        Ok(())
    }

    /// 最新 `limit` 行(钳到 `[1, 1000]`),按 `created_at` 倒序;
    /// 每行 = `{id, request_id, created_at, **payload}`。
    async fn list_request_logs(&self, limit: i64) -> Result<Vec<Value>, PortError>;

    /// 详情读(左连侧表,把正文并回 `request_body_text` /
    /// `response_body_text` / `response_headers` 字段)。
    async fn get_request_log(&self, request_id: &str) -> Result<Option<Value>, PortError>;

    async fn get_request_log_by_rowid(&self, rowid: i64) -> Result<Option<Value>, PortError>;

    /// 按 request id 分页读取详情 trace,不进入主列表查询。
    ///
    /// `kind` 为 `Some` 时只返回该类型(如 `sse_event` / `attempt`),
    /// 让 attempts 与 chunks 两类数据各自独立翻页(D4)。
    async fn get_request_log_trace(
        &self,
        request_id: &str,
        offset: i64,
        limit: i64,
        kind: Option<&str>,
    ) -> Result<RequestLogTracePage, PortError>;

    /// 调用日志分页查询(镜像 `query_request_logs(filter)`)。
    ///
    /// `in_flight` 是调用方(网关在途注册表)预先投影好的「等待中/处理中」
    /// 行(camelCase,与投影输出同形):实现必须把它们并到结果**最前**,
    /// 与正式行共用同一套过滤/计数/分页(Rust 新增参数,无 Python 镜像)。
    async fn query_request_logs(&self, filter: &Value, in_flight: &[Value]) -> Result<Value, PortError>;

    /// 分档过期:`body_retention_days` / `log_retention_days` 为 0 表示
    /// 该档永久保留。
    async fn prune_logs(
        &self,
        body_retention_days: i64,
        log_retention_days: i64,
    ) -> Result<PruneStats, PortError>;

    /// 归还空闲页:`PRAGMA wal_checkpoint(TRUNCATE)` + `VACUUM`。
    async fn compact(&self) -> Result<(), PortError>;
}
