//! 可观测性领域模型(镜像 Python `domain/observability/`;方案 #14 R4.5)。
//!
//! - [`log_policy`]:C3 日志数据策略的门与正文/预览工具;
//! - [`request_log`]:`request_logs.payload` 的字段拼装器;
//! - [`snapshots`]:provider 健康与熔断的只读快照实体。
//!
//! **与 Python 的一处有意差异**:Python 的 `UsageRecord` / `BillingSnapshot`
//! / `RequestRecord` 三个实体在网关里**没有生产写入点**(payload 由
//! `application/gateway/request_log.py::_record_request` 手工拼装)。Rust
//! 不复制这份死代码,而是把那条真实路径移植成 [`request_log::RequestLogPayload`],
//! 字段顺序与其逐一锁定(R4.12 db 文本 diff 依赖)。

pub mod log_policy;
pub mod request_log;
pub mod snapshots;

pub use log_policy::{
    CapturedBody, PREVIEW_MAX_CHARS, RAW_BODY_MAX_BYTES, SENSITIVE_FIELDS, body_retention_days,
    build_preview, call_logs_enabled, capture_bodies_enabled, capture_json, capture_text,
    redact_for_preview,
};
pub use request_log::{RequestLogPayload, RequestTiming, timing_from_attempts};
pub use snapshots::{CircuitBreakerSnapshot, ProviderHealthSnapshot};
