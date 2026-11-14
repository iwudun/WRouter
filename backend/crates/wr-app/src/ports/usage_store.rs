//! 用量统计端口(方案 #14 R4.6)。
//!
//! 镜像 Python `application/ports/usage_store.py`。**没有独立的用量表**:
//! 用量全部从 `request_logs.payload` 派生(单写者 + 只增事实),因此本端口
//! 的「写」就是 request_logs 落库(`RequestLogStore::append_request_log`),
//! 「读」是按范围/维度聚合。
//!
//! 有效请求判定与 Python 一致:只统计 `provider` 非空的行,被拒请求
//! (认证失败/模型限制/限流/策略拒绝/幂等冲突/无供应商)保留供审计但不入账。

use async_trait::async_trait;
use serde_json::Value;

use super::error::PortError;

#[async_trait]
pub trait UsageStore: Send + Sync {
    /// 用量聚合快照(`UsageStatsSnapshot` 形态):`range` ∈
    /// `today|24h|7d|30d|all`(其余按 `86400` 秒桶),`filter` 支持
    /// `credential` / `provider` / `model` 三维收窄。
    /// `range` 也可写自定义窗口 `YYYY-MM-DD..YYYY-MM-DD`(本地日,两端都含),
    /// 此时按该窗口取数(其余按 `86400` 秒桶)。
    async fn usage_summary(&self, range: Option<&str>, filter: &Value) -> Result<Value, PortError>;

    /// 轻量运行态(store 形态 + 配置/密钥/日志计数),供 RPC `getStatus`。
    async fn runtime_status(&self) -> Result<Value, PortError>;

    /// 每个 `api_key_id` 最近一次请求时间(ISO),支撑路由密钥列表的
    /// 「最后使用时间」列;受日志保留窗口约束。
    async fn last_used_by_api_key(&self) -> Result<Value, PortError>;
}
