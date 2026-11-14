//! P1 验收测试(方案 20261102_01 §4):管理面静态托管 + `/api/wr/session`。
//!
//! 覆盖:
//! - 静态命中(index.html / assets 资源,MIME 正确);
//! - SPA fallback(未命中路由回落 index.html);
//! - **API 404 不被 SPA 吞掉**(`/api/*` 仍是 JSON 404,R7);
//! - `serve_ui` 关闭时行为与改造前完全一致(回归);
//! - 路径穿越防护(`..` / 百分号编码均不能读到前端目录外的文件);
//! - `/api/wr/session` 的 loopback 放行与非 loopback 403(D4 / R5);
//! - `into_make_service_with_connect_info` 接线真的把 peer addr 送到 handler。
//!
//! 静态托管用手写零依赖实现(`admin::static_assets`),故测试直接
//! 打真实 HTTP(裸 TCP + 手写请求行),不引第三方 HTTP 客户端。

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;
use wr_server::settings::Settings;

const TEST_TOKEN: &str = "test-ui-token";

/// 构造前端目录:`index.html` + 两个 hash 化资源 + 目录外的"机密"文件。
fn write_frontend(tag: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("wr-static-ui-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let frontend = base.join("dist");
    std::fs::create_dir_all(frontend.join("assets")).unwrap();
    std::fs::write(
        frontend.join("index.html"),
        "<!doctype html><title>WRouter</title><div id=root></div>",
    )
    .unwrap();
    std::fs::write(frontend.join("assets/app-abc123.js"), "console.log(1)").unwrap();
    std::fs::write(frontend.join("assets/app-abc123.css"), "body{}").unwrap();
    // 目录外的机密文件:用于验证路径穿越被挡住。
    std::fs::write(base.join("secret.txt"), "TOP-SECRET-CONTENT").unwrap();
    frontend
}

fn test_settings(tag: &str, serve_ui: bool, frontend_dir: PathBuf) -> Settings {
    let data_dir =
        std::env::temp_dir().join(format!("wr-static-ui-data-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);
    let mut settings = Settings::from_env();
    settings.data_dir = data_dir.clone();
    settings.config_dir = data_dir.join("config");
    settings.auth_token = TEST_TOKEN.to_owned();
    settings.frontend_dir = frontend_dir;
    settings.serve_ui = serve_ui;
    settings
}

async fn build_app(
    settings: Settings,
) -> (
    axum::Router,
    Arc<wr_infra::sqlite::SqliteStore>,
    Arc<wr_server::gateway::GatewayDeps>,
) {
    let settings = Arc::new(settings);
    let source = Arc::new(wr_infra::sqlite_config::SqliteConfigSource::new(
        &settings.data_dir,
    ));
    let store = Arc::new(
        wr_infra::sqlite::SqliteStore::open(&settings.data_dir)
            .await
            .unwrap(),
    );
    let deps = Arc::new(wr_server::gateway::GatewayDeps::new(
        settings.clone(),
        source,
        store.clone(),
    ));
    (
        wr_server::management_router(settings, deps.clone(), store.clone()),
        store,
        deps,
    )
}

/// tower oneshot 发一个 GET,返回 (状态码, 原始 body 文本, content-type)。
async fn get_text(app: axum::Router, uri: &str) -> (StatusCode, String, String) {
    let request = Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        String::from_utf8_lossy(&bytes).to_string(),
        content_type,
    )
}

// ---------------------------------------------------------------------------
// 静态托管
// ---------------------------------------------------------------------------

#[tokio::test]
async fn static_ui_serves_index_and_hashed_assets() {
    let frontend = write_frontend("hit");
    let (app, _store, _deps) = build_app(test_settings("hit", true, frontend)).await;

    let (status, body, content_type) = get_text(app.clone(), "/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<div id=root></div>"));
    assert!(content_type.starts_with("text/html"));

    let (status, body, content_type) = get_text(app.clone(), "/assets/app-abc123.js").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "console.log(1)");
    assert!(content_type.starts_with("text/javascript"));
}

#[tokio::test]
async fn static_ui_spa_fallback_returns_index_for_unknown_route() {
    let frontend = write_frontend("spa");
    let (app, _store, _deps) = build_app(test_settings("spa", true, frontend)).await;

    // 前端路由(非 /api/)未命中 → 回落 index.html
    let (status, body, content_type) = get_text(app, "/settings/network").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<!doctype html>"));
    assert!(content_type.starts_with("text/html"));
}

#[tokio::test]
async fn static_ui_keeps_api_404_as_json() {
    let frontend = write_frontend("api404");
    let (app, _store, _deps) = build_app(test_settings("api404", true, frontend)).await;

    // R7:SPA fallback 不得吞掉 API 404。
    let (status, body, _) = get_text(app.clone(), "/api/v1/nonexistent").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let json: Value = serde_json::from_str(&body).expect("API 404 必须仍是 JSON");
    assert_eq!(json["detail"], "Not Found");

    // 已注册的 API 路由也不受影响(鉴权仍生效)。
    let (status, body, _) = get_text(app, "/api/v1/status").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body.contains("Invalid WRouter management token"));
}

#[tokio::test]
async fn static_ui_disabled_keeps_pre_change_behavior() {
    let frontend = write_frontend("off");
    let (app, _store, _deps) = build_app(test_settings("off", false, frontend)).await;

    // 未开启 serve_ui → 静态路由整体不挂载,首页仍是 JSON 404。
    let (status, body, _) = get_text(app.clone(), "/").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let json: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["detail"], "Not Found");

    // session 端点同样不挂载(缺省行为与改造前完全一致)。
    let (status, body, _) = get_text(app, "/api/wr/session").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let json: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["detail"], "Not Found");
}

#[tokio::test]
async fn static_ui_blocks_path_traversal() {
    let frontend = write_frontend("traversal");
    let (app, _store, _deps) = build_app(test_settings("traversal", true, frontend)).await;

    // `..` 直写
    let (status, body, _) = get_text(app.clone(), "/../secret.txt").await;
    assert!(!body.contains("TOP-SECRET-CONTENT"), "路径穿越泄露: {body}");
    assert_eq!(status, StatusCode::OK); // 回落 index.html

    // 百分号编码的 `..`
    let (status, body, _) = get_text(app, "/%2e%2e/secret.txt").await;
    assert!(!body.contains("TOP-SECRET-CONTENT"), "编码路径穿越泄露: {body}");
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn static_ui_missing_frontend_dir_returns_404_without_panic() {
    let missing = std::env::temp_dir().join(format!("wr-static-ui-absent-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&missing);
    let (app, _store, _deps) = build_app(test_settings("absent", true, missing)).await;

    let (status, body, _) = get_text(app, "/").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let json: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["detail"], "Not Found");
}

// ---------------------------------------------------------------------------
// /api/wr/session
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn session_endpoint_serves_token_over_real_loopback_socket() {
    // 真起一个监听端口,验证 `into_make_service_with_connect_info` 真的把
    // peer addr 送到了 handler(而不是只在单测里直调 handler)。
    let frontend = write_frontend("session");
    let settings = Arc::new(test_settings("session", true, frontend));
    let source = Arc::new(wr_infra::sqlite_config::SqliteConfigSource::new(
        &settings.data_dir,
    ));
    let store = Arc::new(
        wr_infra::sqlite::SqliteStore::open(&settings.data_dir)
            .await
            .unwrap(),
    );
    let deps = Arc::new(wr_server::gateway::GatewayDeps::new(
        settings.clone(),
        source,
        store.clone(),
    ));
    let router = wr_server::management_router(settings, deps, store);

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    );
    let handle = tokio::spawn(async move {
        let _ = server.await;
    });

    // 阻塞式裸 TCP 客户端丢进 spawn_blocking:不占住 runtime 线程,
    // 否则 server 任务无法被调度(读超时)。
    let raw = tokio::task::spawn_blocking(move || raw_get(addr, "/api/wr/session"))
        .await
        .unwrap();
    assert!(raw.starts_with("HTTP/1.1 200"), "unexpected response: {raw}");
    let body = raw.split("\r\n\r\n").nth(1).unwrap_or_default().to_string();
    let json: Value = serde_json::from_str(body.trim()).expect("session 响应必须是 JSON");
    assert_eq!(json["token"], TEST_TOKEN);

    handle.abort();
}

#[tokio::test]
async fn session_endpoint_forbids_non_loopback_peer() {
    let frontend = write_frontend("nonloop");
    let settings = test_settings("nonloop", true, frontend);
    let state = build_admin_state(settings).await;

    // 非回环来源(D4:本版本不放开远程管理)→ 403。
    let response = wr_server::admin::status::session_endpoint(
        State(state.clone()),
        ConnectInfo(SocketAddr::from(([192, 168, 1, 10], 54321))),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // 回环来源 → 200 + 令牌。
    let response = wr_server::admin::status::session_endpoint(
        State(state),
        ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 54321))),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
}

/// 构造 `AdminState`(供直调 handler 用)。
async fn build_admin_state(settings: Settings) -> wr_server::admin::AdminState {
    let settings = Arc::new(settings);
    let source = Arc::new(wr_infra::sqlite_config::SqliteConfigSource::new(
        &settings.data_dir,
    ));
    let store = Arc::new(
        wr_infra::sqlite::SqliteStore::open(&settings.data_dir)
            .await
            .unwrap(),
    );
    let deps = Arc::new(wr_server::gateway::GatewayDeps::new(
        settings.clone(),
        source,
        store.clone(),
    ));
    wr_server::admin::AdminState {
        settings,
        deps,
        store,
        capabilities: Value::Object(Default::default()),
        // 20261102_01 P6:服务控制通道(测试里用独立一条即可)。
        control: wr_server::service_control::ServiceControl::new(),
    }
}

/// 裸 TCP 发一个 HTTP/1.1 GET 并读回完整响应(零第三方 HTTP 客户端)。
fn raw_get(addr: SocketAddr, path: &str) -> String {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nAccept: */*\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).unwrap();
    stream.flush().unwrap();
    let mut buf = String::new();
    stream.read_to_string(&mut buf).unwrap();
    buf
}
