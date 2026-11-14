//! R1.3 验收测试:双面 /health 路由形状与 Python 版一致。
//!
//! 用 tower oneshot 直测 Router(R1.9:集成/e2e 测试统一放
//! `wr-server/tests/`;R2 的黄金样本 parity 也挂在这里)。

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;
use wr_server::settings::Settings;

/// 构造带网关数据面依赖的 gateway router(配置源指向空临时目录 =
/// 默认空配置)。本文件只测免认证的 /health 与 404,不触达数据面认证。
async fn test_gateway_router() -> axum::Router {
    let settings = Arc::new(test_settings());
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
        store,
    ));
    wr_server::gateway_router(settings, deps)
}

fn test_settings() -> Settings {
    Settings {
        app_name: "WRouter".to_owned(),
        version: wr_server::VERSION.to_owned(),
        host: "127.0.0.1".to_owned(),
        gateway_port: 3460,
        web_port: 3462,
        proxy_port: 3463,
        data_dir: std::env::temp_dir().join("wr-server-test-data"),
        config_dir: std::env::temp_dir().join("wr-server-test-config"),
        auth_token: "test-token".to_owned(),
        parent_pid: None,
        ready_file: None,
        body_retention_days: 7,
        log_retention_days: 0,
        body_capture_max_bytes: wr_domain::observability::RAW_BODY_MAX_BYTES,
        max_body_bytes: wr_server::settings::DEFAULT_MAX_BODY_BYTES,
        capabilities_file: std::path::PathBuf::from("contracts/wrouter-capabilities.json"),
        frontend_dir: std::path::PathBuf::from("frontend/dist"),
        // 20261102_01 P1-5:管理面静态托管开关(测试默认关闭)。
        serve_ui: false,
        // 20261102_01 P6:托管方式(测试按独立进程语义)。
        managed_by: wr_server::service_control::ManagedBy::Process,
    }
}

#[tokio::test]
async fn gateway_health_matches_python_shape() {
    // Python: {"ok": True, "name": "WRouter", "version": ..., "port": 3460}
    let app = test_gateway_router().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["ok"], Value::Bool(true));
    assert_eq!(json["name"], "WRouter");
    assert_eq!(json["version"], wr_server::VERSION);
    assert_eq!(json["port"], 3460);
}

#[tokio::test]
async fn management_health_matches_python_shape() {
    // Python: {"ok": True, "name": "WRouter", "version": ..., "apiVersion": "v1", "gatewayPort": ..., "webPort": ...}
    let settings = Arc::new(test_settings());
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
    let app = wr_server::management_router(settings, deps, store);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["ok"], Value::Bool(true));
    assert_eq!(json["apiVersion"], "v1");
    assert_eq!(json["gatewayPort"], 3460);
    assert_eq!(json["webPort"], 3462);
}

#[tokio::test]
async fn unknown_route_is_404() {
    let app = test_gateway_router().await;
    let resp = app
        .oneshot(Request::builder().uri("/nope").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[test]
fn version_falls_back_to_dev_placeholder() {
    // 未注入 WR_VERSION 的裸 cargo build/test 必须给出可识别的占位。
    if std::env::var_os("WR_VERSION").is_none() {
        assert_eq!(wr_server::VERSION, "0.0.0-dev");
    }
}
