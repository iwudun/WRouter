//! 节省事件端口(20260905_03 §4.6 S8;Rust 新增,无 Python 镜像)。
//!
//! `savings_events` 的写入(网关请求路径,fire-and-forget)与三本账汇总
//! (管理面 `getSavingsStats` RPC)。聚合口径见 wr-infra 实现。

use async_trait::async_trait;
use serde_json::Value;

use super::PortError;

/// 单条节省事件写入载荷。
#[derive(Debug, Clone)]
pub struct SavingsEventInput {
    pub request_id: String,
    /// `compression` | `response_cache`(`prefix_hit` 保留字面,v1 不写)。
    pub kind: &'static str,
    pub tokens_saved: i64,
    pub estimated_usd_saved: f64,
    /// 压缩专用:engine_breakdown(CompressionReport.to_value())。
    pub engine_breakdown: Option<Value>,
}

#[async_trait]
pub trait SavingsEventStore: Send + Sync {
    /// 追加一条节省事件。
    async fn record_savings_event(&self, event: &SavingsEventInput) -> Result<(), PortError>;

    /// 三本账汇总(时段:today/24h/7d/30d/all,自定义窗口
    /// `YYYY-MM-DD..YYYY-MM-DD`,未知名 = 全表)。
    async fn savings_summary(&self, period: &str) -> Result<Value, PortError>;
}
