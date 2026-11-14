//! R2.8 网关数据面集成测试(wiremock mock 上游)。
//!
//! 覆盖:认证(有效/无效/缺失/禁用/空注册表/管理 token)、非流式
//! 透传、流式透传(relay 重序列化 + finalize)、上游 5xx 错误信封、
//! NO_RETRY 不重试、读超时 502、并发上限 429。
//!
//! 测试装配:临时目录播种 sqlite 配置(provider 指向 wiremock)→
//! `gateway_router` 经 tower oneshot 直测(与 health.rs 同法)。
//!
//! 按分节拆分(20261107_01 W5-4):`common` 放共享装配,其余模块对应
//! 原 `gateway.rs` 的 `// ----` 分节;过滤命令按测试名子串仍然有效。

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use rusqlite::Connection;
use serde_json::{Value, json};
use tower::ServiceExt;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;
use common::*;

mod auth;
mod passthrough;
mod first_byte;
mod terminal;
mod errors;
mod concurrency;
mod routing;
mod credentials;
mod stream_allowlist;
mod aggregate;
mod proxy;
mod fingerprint;
mod continuation;
mod cache;
mod control;
mod native;
mod conversion;
