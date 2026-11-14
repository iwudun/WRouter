//! 应用层端口(方案 #14 D14 / R4)。
//!
//! 逐模块镜像 Python `application/ports/`(同名文件、同名方法),把
//! 持久化面从应用层解耦:`wr-app` 声明 trait,`wr-infra` 给出 SQLite
//! 实现,`wr-server` 组合根装配注入。
//!
//! 端口约定(方案 #14 R1.1 决策,全仓一致):
//! - async 方法一律 `#[async_trait]`(dyn 对象安全,装配注入与测试替身需要);
//! - `Send + Sync` 超界:网关在 tokio 多线程运行时下并发持有 `Arc<dyn …>`;
//! - 载荷以 `serde_json::Value` 传递,镜像 Python 侧的 `dict` 入参/返回。
//!   这是与 Python 的一处**有意的层内差异**:Python 的 `ConfigStore` 端口
//!   收发 `AppConfig` 实体,而 Rust 侧 `AppConfig` 住在 `wr-infra`(R1.4
//!   落点),端口若引用它就要 `wr-app → wr-infra`,破坏 D14 依赖方向。
//!   因此端口收发 config payload JSON,`AppConfig` 的解析/序列化留在装配层。
//! - 方法名与 Python 保持 snake_case 一致,便于评审对照。

pub mod breaker_health_store;
pub mod config_store;
pub mod error;
pub mod fingerprint_store;
pub mod gateway_store;
pub mod idempotency_store;
pub mod request_log_store;
pub mod response_cache_store;
pub mod savings_store;
pub mod secret_store;
pub mod usage_store;

pub use breaker_health_store::{
    BreakerHealthStore, BreakerStateRow, BreakerStateStore, HealthRow, HealthStore,
};
pub use config_store::ConfigStore;
pub use error::PortError;
pub use fingerprint_store::{FingerprintStore, KIND_ANCHOR, KIND_GLOBAL};
pub use gateway_store::GatewayStore;
pub use idempotency_store::{IdempotencyRow, IdempotencyStore};
pub use request_log_store::{
    LogBodies, PendingRequestLog, PruneStats, RequestLogStore, RequestLogTracePage,
};
pub use savings_store::{SavingsEventInput, SavingsEventStore};
pub use response_cache_store::{ResponseCacheDiskRow, ResponseCacheStore};
pub use secret_store::{AccountSnapshotInput, AccountUpsert, SecretStore};
pub use usage_store::UsageStore;
