//! WRouter 应用层(方案 #14 D14)。
//!
//! 镜像 Python `application/` 分层:应用服务 + 端口(traits)。
//! 依赖方向:`wr-app → wr-domain`。`wr-infra` 实现本层声明的端口,
//! `wr-server` 在装配时构造具体实现并注入。
//!
//! 端口 trait 约定(方案 #14 R1.1 决策,全仓一致):所有 async 端口
//! trait 使用 `async-trait` 宏,保证 dyn 对象安全以支持装配注入与
//! 测试替身;不使用裸 RPITIT。
//!
//! R4 落地 [`ports`]:六个持久化端口(`ConfigStore`、`SecretStore`、
//! `RequestLogStore`、`BreakerHealthStore`、`IdempotencyStore`、
//! `UsageStore`)与组合端口 [`ports::GatewayStore`](超界合并 + blanket
//! impl),逐文件对应 Python `application/ports/*.py`。
//!
//! R5 追加:管理面事件发布端口(`/events` SSE 事件源)。

pub mod ports;
