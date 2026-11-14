//! 组合持久化端口(方案 #14 R4.4)。
//!
//! 镜像 Python `application/ports/gateway_store.py`:网关编排器合法地
//! 触及六个持久化域(config / secrets+账号池 / 请求日志 / 熔断+健康 /
//! 幂等 / 用量),用组合端口保持其构造面紧凑,同时仍然反转依赖
//! (应用层依赖端口,不依赖 `SqliteStore`)。
//!
//! Rust 呈现方式(计划所列「supertrait 或组合结构」二选一的**supertrait**):
//! 本 trait 无自有方法,只做超界合并 + blanket impl,因此任何实现了全部
//! 成员端口的具体 store 自动满足 `GatewayStore`,且 `Arc<dyn GatewayStore>`
//! 可直接调用各成员端口方法(vtable 含全部超界方法)。

use super::{
    BreakerHealthStore, ConfigStore, IdempotencyStore, RequestLogStore, ResponseCacheStore,
    SavingsEventStore, SecretStore,
};

/// `GatewayApplication` 实际消费的持久化面之并集。
///
/// 成员与 Python / 方案 #14 R4.4 的列举一致:config + secrets + 请求日志
/// + 熔断健康 + 幂等;20260905_03 S6 增补响应缓存(新域,无 Python 镜像)。
///
/// **`UsageStore` 不在组合端口内**——Python 侧用量消费者拿的是具体 store
/// (用量聚合与请求日志同库同连接,没有独立的消费者群),Rust 沿用同一划分,
/// 避免把六个域强行绑给每个网关协作者。
pub trait GatewayStore:
    ConfigStore
    + SecretStore
    + RequestLogStore
    + BreakerHealthStore
    + IdempotencyStore
    + ResponseCacheStore
    + SavingsEventStore
{
}

impl<
        T: ConfigStore
            + SecretStore
            + RequestLogStore
            + BreakerHealthStore
            + IdempotencyStore
            + ResponseCacheStore
            + SavingsEventStore,
    > GatewayStore for T
{
}
