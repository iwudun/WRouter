//! 熔断 / provider 健康快照端口(方案 #14 R4.4)。
//!
//! 镜像 Python `application/ports/breaker_health_store.py`:窄端口
//! [`BreakerStateStore`] / [`HealthStore`] 供单一域消费者,合并端口
//! [`BreakerHealthStore`] 进组合端口 `GatewayStore`。

use async_trait::async_trait;

use super::error::PortError;

/// `circuit_breaker_state` 表一行(镜像 domain `CircuitBreakerState.to_dict`)。
///
/// 时间字段是**单调时钟毫秒**;Python 持久化 `time.monotonic()*1000`,
/// Rust 用挂钟 epoch 毫秒(见 `wr-server::gateway::breaker` 模块文档与
/// 14 号文档 R4 完成记录的时钟基偏差说明)。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BreakerStateRow {
    pub consecutive_failures: i64,
    pub opened_until_ms: i64,
    pub last_state_change_ms: i64,
}

/// `provider_health` 表一行(镜像 `ProviderHealthSnapshot`)。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HealthRow {
    pub provider_id: String,
    pub status: String,
    pub available: bool,
    pub latency_ms: i64,
    pub checked_at: String,
    /// 无错误时为 SQL `NULL`(与「错误为空串」不等价)。
    pub error: Option<String>,
}

#[async_trait]
pub trait BreakerStateStore: Send + Sync {
    async fn load_breaker_state(
        &self,
        provider_id: &str,
    ) -> Result<Option<BreakerStateRow>, PortError>;

    /// 全部已持久化熔断状态,按 `provider_id` 索引。
    async fn load_all_breaker_states(&self) -> Result<Vec<(String, BreakerStateRow)>, PortError>;

    async fn save_breaker_state(
        &self,
        provider_id: &str,
        state: &BreakerStateRow,
    ) -> Result<(), PortError>;
}

#[async_trait]
pub trait HealthStore: Send + Sync {
    async fn load_health(&self, provider_id: &str) -> Result<Option<HealthRow>, PortError>;

    async fn load_all_health(&self) -> Result<Vec<(String, HealthRow)>, PortError>;

    /// 落一行健康快照;`provider_id` 为空 → [`PortError::Invalid`]
    /// (镜像 Python 的 `ValueError`)。
    async fn save_health(&self, snapshot: &HealthRow) -> Result<(), PortError>;
}

/// 熔断 + 健康的合并面(组合端口 `GatewayStore` 的成员)。
pub trait BreakerHealthStore: BreakerStateStore + HealthStore + Send + Sync {}

impl<T: BreakerStateStore + HealthStore + Send + Sync> BreakerHealthStore for T {}
