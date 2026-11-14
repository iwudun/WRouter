//! 管理面(3462):RPC 分发 + REST + /events SSE(方案 #14 R5)。
//!
//! 镜像 Python `interfaces/http/app.py::create_app` 与 `handlers/*`:
//! - 免认证:`GET /api/v1/health`、`GET /api/v1/capabilities`;
//! - 其余全部要求 `x-wrouter-web-auth` 头(401 detail 镜像);
//! - CORS 中间件挂在**整个 app 外层**(Python 同构):allow_origins=["null"]
//!   + localhost/127.0.0.1/[::1] 正则,methods/headers 通配,max_age=600;
//! - 未知路径 → 404 `{"detail":"Not Found"}`(不经过 auth,FastAPI 路由
//!   匹配失败先于依赖注入)。
//!
//! RPC 分发见 [`config::rpc_dispatch`];管理面用量查询直接经
//! `AdminState.store`(Arc<SqliteStore>,含 UsageStore 端口)。

pub mod account_expression;
mod account_snapshot;
pub mod config;
pub mod connectivity_probe;
pub mod credential_stats;
// 2026-11-01:模型指纹按职责拆三文件 —— 主模块(四个库 RPC) + 探针执行 +
// 导入导出(此前 1700 行单文件混四职责)。
pub mod model_fingerprint;
pub mod model_fingerprint_probe;
pub mod model_fingerprint_transfer;
pub mod savings;
pub mod service_control;
pub mod speedtest;
pub mod static_assets;
pub mod status;
pub mod usage;

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode, Uri};
use axum::middleware::{Next, from_fn, from_fn_with_state};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde_json::Value;

use crate::gateway::SharedGatewayDeps;
use crate::settings::Settings;

/// 管理面共享状态。
#[derive(Clone)]
pub struct AdminState {
    pub settings: Arc<Settings>,
    pub deps: SharedGatewayDeps,
    /// 直接持有 SqliteStore:管理面的用量/请求日志查询走 UsageStore/
    /// RequestLogStore 端口(后者也在 GatewayStore 里,前者不在)。
    pub store: std::sync::Arc<wr_infra::sqlite::SqliteStore>,
    /// 启动时加载一次的 capabilities(ports 由 live settings 覆盖)。
    pub capabilities: Value,
    /// 20261102_01 P6:服务控制通道(网关面启停 + 整进程停机请求)。
    pub control: crate::service_control::ServiceControl,
}

/// 装配管理面路由表。
pub fn admin_router(state: AdminState) -> Router {
    // 20261102_01 P1-2:静态托管与 session 端点**整体**由 `serve_ui` 开关控制
    // ——缺省关闭时路由表与改造前逐条一致(回归验收项)。
    let serve_ui = state.settings.serve_ui;

    // 免认证:health / capabilities
    let public = Router::new()
        .route("/api/v1/health", get(status::management_health))
        .route("/api/v1/capabilities", get(status::get_capabilities));

    // 受认证保护:RPC + REST + SSE
    let protected = Router::new()
        .route("/api/wr/rpc", post(config::rpc_dispatch))
        .route("/api/wr/events", get(status::events_endpoint))
        // config
        .route(
            "/api/v1/config",
            get(config::rest_get_config).put(config::rest_put_config),
        )
        .route("/api/v1/config/export", get(config::rest_export_config))
        .route("/api/v1/config/import", post(config::rest_import_config))
        // status
        .route("/api/v1/status", get(status::rest_status))
        // metrics
        .route("/api/v1/metrics", get(status::rest_metrics))
        // providers
        .route("/api/v1/providers", get(status::rest_providers_list))
        .route(
            "/api/v1/providers/{provider_id}",
            put(status::rest_upsert_provider).delete(status::rest_delete_provider),
        )
        .route("/api/v1/providers/probe", post(status::rest_probe_provider))
        .route(
            "/api/v1/client-fingerprints",
            get(status::rest_client_fingerprints),
        )
        .route(
            "/api/v1/providers/health",
            get(status::rest_provider_health),
        )
        .route(
            "/api/v1/providers/health/check",
            post(status::rest_probe_all_providers),
        )
        // logs / usage
        .route("/api/v1/request-logs", get(usage::rest_request_logs))
        .route("/api/v1/usage", get(usage::rest_usage))
        .layer(from_fn_with_state(state.clone(), auth_middleware));

    let mut router = public.merge(protected);
    if serve_ui {
        // 20261102_01 P1-2/P1-3:`/api/wr/session` 进免认证组(不进
        // protected),浏览器 / Tauri 首屏据此取令牌。
        router = router
            .route("/api/wr/session", get(status::session_endpoint))
            .fallback(ui_fallback);
    } else {
        router = router.fallback(not_found);
    }

    router
        .layer(from_fn(cors_middleware))
        .with_state(state)
}

/// 404 JSON 响应体(镜像 Python FastAPI 的 `{"detail":"Not Found"}`)。
pub(super) fn not_found_response() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({"detail": "Not Found"})),
    )
        .into_response()
}

/// 404 路由处理器(axum 的 Handler 必须返回 Future,故用 async 包装)。
async fn not_found() -> Response {
    not_found_response()
}

/// SPA fallback(20261102_01 P1-2):未命中路由的请求分流。
///
/// - `/api/*` 前缀 → 保持 JSON 404(R7 风险:API 404 不被 SPA 吞成 HTML);
/// - 其余路径且方法为 GET/HEAD → 交静态托管(命中文件或回落 index.html);
/// - 其余方法 → 保持 JSON 404。
async fn ui_fallback(State(state): State<AdminState>, uri: Uri, method: Method) -> Response {
    let path = uri.path();
    if path == "/api" || path.starts_with("/api/") {
        return not_found_response();
    }
    if method != Method::GET && method != Method::HEAD {
        return not_found_response();
    }
    static_assets::serve_static(&state, path).await
}

/// 认证中间件(镜像 app.py `require_management_auth`)。
async fn auth_middleware(State(state): State<AdminState>, req: Request, next: Next) -> Response {
    let token = req
        .headers()
        .get("x-wrouter-web-auth")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    if token != state.settings.auth_token {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"detail": "Invalid WRouter management token"})),
        )
            .into_response();
    }
    next.run(req).await
}

/// CORS 中间件(手写,镜像 Python CORSMiddleware 的可观察行为;
/// 挂在 app 最外层,预检 OPTIONS 在 auth 之前短路)。
async fn cors_middleware(req: Request, next: Next) -> Response {
    let origin = req
        .headers()
        .get(axum::http::header::ORIGIN)
        .and_then(|h| h.to_str().ok())
        .map(str::to_owned);

    let Some(origin) = origin else {
        return next.run(req).await;
    };
    if !is_allowed_origin(&origin) {
        return next.run(req).await;
    }

    if req.method() == Method::OPTIONS {
        // 预检:不进路由,直接 200 + CORS 头。
        let mut resp = StatusCode::OK.into_response();
        insert_cors_headers(&origin, &mut resp);
        return resp;
    }

    let mut resp = next.run(req).await;
    resp.headers_mut().insert(
        axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_str(&origin).unwrap_or(HeaderValue::from_static("null")),
    );
    resp
}

fn is_allowed_origin(origin: &str) -> bool {
    if origin == "null" {
        return true;
    }
    // 镜像 ^https?://(localhost|127\.0\.0\.1|\[::1\])(:\d+)?$
    let rest = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"));
    let Some(host_port) = rest else {
        return false;
    };
    let host = host_port.split(':').next().unwrap_or("");
    let port_part = host_port.split_once(':').map(|(_, p)| p).unwrap_or("");
    let host_ok = matches!(host, "localhost" | "127.0.0.1" | "[::1]");
    let port_ok = port_part.is_empty() || port_part.chars().all(|c| c.is_ascii_digit());
    host_ok && port_ok
}

fn insert_cors_headers(origin: &str, resp: &mut Response) {
    let headers = resp.headers_mut();
    headers.insert(
        axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_str(origin).unwrap_or(HeaderValue::from_static("null")),
    );
    headers.insert(
        axum::http::header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        axum::http::header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        axum::http::header::ACCESS_CONTROL_MAX_AGE,
        HeaderValue::from_static("600"),
    );
}
