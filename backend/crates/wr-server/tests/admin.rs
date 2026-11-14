//! R5 管理面验收测试(14a §6):认证/CORS/RPC 信封/api_key 三态/事件发布。
//!
//! 用 tower oneshot 直测 `management_router`;三态语义走真实 SqliteStore
//! (镜像 Python `test_sqlite_store.py` 的三态断言,落在管理面端到端链路)。

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use wr_app::ports::SecretStore as _;
use wr_server::settings::Settings;

fn test_settings(tag: &str) -> Settings {
    let data_dir =
        std::env::temp_dir().join(format!("wr-admin-tests-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);
    let mut settings = Settings::from_env();
    settings.data_dir = data_dir.clone();
    settings.config_dir = data_dir.join("config");
    settings.auth_token = "test-admin-token".to_owned();
    settings
}

async fn build_app(
    tag: &str,
) -> (
    axum::Router,
    Arc<wr_infra::sqlite::SqliteStore>,
    Arc<wr_server::gateway::GatewayDeps>,
) {
    let settings = Arc::new(test_settings(tag));
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

async fn send_json(
    app: axum::Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header("x-wrouter-web-auth", token);
    }
    let request = builder
        .header("content-type", "application/json")
        .body(Body::from(body.map(|v| v.to_string()).unwrap_or_default()))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

fn rpc_body(method: &str, args: Value) -> Value {
    json!({"method": method, "args": args})
}

// ---------------------------------------------------------------------------
// 认证与豁免(R5.10)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn management_auth_missing_header_401() {
    let (app, _store, _deps) = build_app("auth-missing").await;
    let (status, body) = send_json(app, "GET", "/api/v1/status", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["detail"], "Invalid WRouter management token");
}

#[tokio::test]
async fn management_auth_bad_token_401() {
    let (app, _store, _deps) = build_app("auth-bad").await;
    let (status, body) = send_json(app, "GET", "/api/v1/status", Some("wrong"), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["detail"], "Invalid WRouter management token");
}

#[tokio::test]
async fn management_auth_exempts_health_and_capabilities() {
    let (app, _store, _deps) = build_app("auth-exempt").await;
    let (status, health) = send_json(app.clone(), "GET", "/api/v1/health", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(health["ok"], Value::Bool(true));
    assert_eq!(health["gatewayPort"], 3460);
    let (status, caps) = send_json(app, "GET", "/api/v1/capabilities", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(caps["schema"], "wrouter.capabilities");
    // ports 由 live settings 覆盖。
    assert_eq!(caps["ports"]["management"], 3462);
}

#[tokio::test]
async fn management_auth_ok_with_token() {
    let (app, _store, _deps) = build_app("auth-ok").await;
    let (status, body) =
        send_json(app, "GET", "/api/v1/status", Some("test-admin-token"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], Value::Bool(true));
    assert_eq!(body["ports"]["gateway"], 3460);
}

// ---------------------------------------------------------------------------
// CORS(R5.10)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cors_null_origin_allowed() {
    let (app, _store, _deps) = build_app("cors-null").await;
    let request = Request::builder()
        .method("GET")
        .uri("/api/v1/health")
        .header("origin", "null")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .unwrap(),
        "null"
    );
}

#[tokio::test]
async fn cors_preflight_options_200_with_max_age() {
    let (app, _store, _deps) = build_app("cors-preflight").await;
    let request = Request::builder()
        .method("OPTIONS")
        .uri("/api/wr/rpc")
        .header("origin", "http://localhost:5173")
        .header("access-control-request-method", "POST")
        .header(
            "access-control-request-headers",
            "x-wrouter-web-auth,content-type",
        )
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .unwrap(),
        "http://localhost:5173"
    );
    assert_eq!(
        response.headers().get("access-control-max-age").unwrap(),
        "600"
    );
}

#[tokio::test]
async fn cors_foreign_origin_no_headers() {
    let (app, _store, _deps) = build_app("cors-foreign").await;
    let request = Request::builder()
        .method("GET")
        .uri("/api/v1/health")
        .header("origin", "https://evil.example.com")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response
            .headers()
            .get("access-control-allow-origin")
            .is_none()
    );
}

// ---------------------------------------------------------------------------
// RPC 信封与 501(R5.9 形状断言)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rpc_unknown_method_501_shape() {
    let (app, _store, _deps) = build_app("rpc-unknown").await;
    let (status, body) = send_json(
        app,
        "POST",
        "/api/wr/rpc",
        Some("test-admin-token"),
        Some(rpc_body("noSuchMethod", json!([]))),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(body["detail"]["ok"], Value::Bool(false));
    assert_eq!(body["detail"]["error"]["code"], "NOT_IMPLEMENTED");
    assert_eq!(body["detail"]["error"]["method"], "noSuchMethod");
}

#[tokio::test]
async fn rpc_desktop_methods_use_custom_501_message() {
    let (app, _store, _deps) = build_app("rpc-desktop").await;
    let (status, body) = send_json(
        app,
        "POST",
        "/api/wr/rpc",
        Some("test-admin-token"),
        Some(rpc_body("updateCheck", json!([]))),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    assert!(
        body["detail"]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no update channel")
    );
}

#[tokio::test]
async fn rpc_get_config_returns_ok_envelope() {
    let (app, _store, _deps) = build_app("rpc-getcfg").await;
    let (status, body) = send_json(
        app,
        "POST",
        "/api/wr/rpc",
        Some("test-admin-token"),
        Some(rpc_body("getConfig", json!([]))),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], Value::Bool(true));
    // to_dict 键序与键名契约(C9)。
    assert!(body["value"].get("version").is_some());
    assert!(body["value"].get("Providers").is_some());
    assert!(body["value"].get("APIKEYS").is_some());
    assert!(body["value"].get("preferredProvider").is_some());
    // 20261101_02 T3:routingRules 键不再写出(Router 仅存 fallback)。
    assert!(body["value"].get("routingRules").is_none());
    assert!(body["value"].get("Router").is_some());
}

// ---------------------------------------------------------------------------
// api_key 三态(C2,R5.2 命名测试)
// ---------------------------------------------------------------------------

// 20261101_02 T3-B:设置页「成本节省」分区经 saveConfig 写全局回退与
// savings 开关 —— 往返(getConfig 回读)必须保真(Router 仅存 fallback)。
#[tokio::test]
async fn save_config_roundtrips_fallback_and_savings_switches() {
    let (app, _store, _deps) = build_app("fallback-roundtrip").await;
    let args = json!([{
        "version": 2,
        "Providers": [{
            "id": "p1", "name": "P1",
            "api_base_url": "https://api.test.com", "protocol": "openai",
        }],
        "Router": {"fallback": {"mode": "model-chain", "retryCount": 0, "models": ["p1/m-test"]}},
        "savings": {
            "compression": {"mode": "auto", "thresholdTokens": 16000},
            "responseCache": {"enabled": true},
            "sticky": {"enabled": true, "ttlSecs": 600},
            "prefixPin": {"enabled": true},
        },
    }]);
    let (status, body) = send_json(
        app,
        "POST",
        "/api/wr/rpc",
        Some("test-admin-token"),
        Some(rpc_body("saveConfig", args)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let value = &body["value"];
    // Router.fallback 往返(Router.rules 不再存在)。
    assert_eq!(value["Router"]["fallback"]["mode"], json!("model-chain"));
    assert_eq!(value["Router"]["fallback"]["models"], json!(["p1/m-test"]));
    assert!(value["Router"].get("rules").is_none());
    // savings 全局开关往返。
    assert_eq!(value["savings"]["compression"]["mode"], json!("auto"));
    assert_eq!(value["savings"]["responseCache"]["enabled"], json!(true));
    assert_eq!(value["savings"]["sticky"]["enabled"], json!(true));
    assert_eq!(value["savings"]["sticky"]["ttlSecs"], json!(600));
    assert_eq!(value["savings"]["prefixPin"]["enabled"], json!(true));
    assert!(value.get("routingRules").is_none());
}

/// 构造含一个合法 provider 的 saveConfig args。
fn save_args(api_key: Value) -> Value {
    json!([{
        "version": 2,
        "preferredProvider": "p1",
        "Providers": [{
            "id": "p1",
            "name": "P1",
            "api_base_url": "https://api.test.com",
            "protocol": "openai",
            "api_key": api_key,
        }],
    }])
}

#[tokio::test]
async fn apikey_threestate_missing_keeps_secret() {
    let (app, store, _deps) = build_app("ts-missing").await;
    store
        .upsert_provider_secret("p1", "sk-live-key")
        .await
        .unwrap();
    let (status, _) = send_json(
        app,
        "POST",
        "/api/wr/rpc",
        Some("test-admin-token"),
        Some(rpc_body("saveConfig", json!([{"Providers": [{"id": "p1", "name": "P1", "api_base_url": "https://api.test.com", "protocol": "openai"}]}]))),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // 键缺失 → 不动 secrets。
    assert_eq!(store.provider_secret("p1").await.unwrap(), "sk-live-key");
}

#[tokio::test]
async fn apikey_threestate_empty_string_keeps_secret() {
    let (app, store, _deps) = build_app("ts-empty").await;
    store
        .upsert_provider_secret("p1", "sk-live-key")
        .await
        .unwrap();
    let (status, _) = send_json(
        app,
        "POST",
        "/api/wr/rpc",
        Some("test-admin-token"),
        Some(rpc_body("saveConfig", save_args(json!("")))),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(store.provider_secret("p1").await.unwrap(), "sk-live-key");
}

#[tokio::test]
async fn apikey_threestate_mask_stars_keeps_secret() {
    let (app, store, _deps) = build_app("ts-stars").await;
    store
        .upsert_provider_secret("p1", "sk-live-key")
        .await
        .unwrap();
    let (status, _) = send_json(
        app,
        "POST",
        "/api/wr/rpc",
        Some("test-admin-token"),
        Some(rpc_body("saveConfig", save_args(json!("***")))),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(store.provider_secret("p1").await.unwrap(), "sk-live-key");
}

#[tokio::test]
async fn apikey_threestate_null_clears_secret() {
    let (app, store, _deps) = build_app("ts-null").await;
    store
        .upsert_provider_secret("p1", "sk-live-key")
        .await
        .unwrap();
    let (status, _) = send_json(
        app,
        "POST",
        "/api/wr/rpc",
        Some("test-admin-token"),
        Some(rpc_body("saveConfig", save_args(Value::Null))),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // null = 显式清空。
    assert_eq!(store.provider_secret("p1").await.unwrap_or_default(), "");
}

#[tokio::test]
async fn apikey_threestate_new_key_upserts_secret() {
    let (app, store, _deps) = build_app("ts-new").await;
    let (status, body) = send_json(
        app,
        "POST",
        "/api/wr/rpc",
        Some("test-admin-token"),
        Some(rpc_body("saveConfig", save_args(json!("sk-fresh")))),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(store.provider_secret("p1").await.unwrap(), "sk-fresh");
    // 保存后回读载荷注水:getConfig 返回明文(镜像 to_dict(True))。
    assert_eq!(body["value"]["Providers"][0]["api_key"], "sk-fresh");
}

// ---------------------------------------------------------------------------
// config-changed 事件(R5.2)+ saveApiKeys(R5.2)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rpc_save_config_publishes_config_changed_event() {
    let (app, _store, deps) = build_app("evt-save").await;
    let mut subscriber = deps.events.subscribe();
    let (status, _) = send_json(
        app,
        "POST",
        "/api/wr/rpc",
        Some("test-admin-token"),
        Some(rpc_body("saveConfig", save_args(json!("sk-evt")))),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let event = subscriber.next().await.expect("saveConfig 应发布事件");
    assert_eq!(
        event.kind,
        wr_server::gateway::events::EventKind::ConfigChanged
    );
    assert_eq!(event.data["scope"], json!("config"));
}

#[tokio::test]
async fn save_api_keys_dedup_last_wins_and_persists() {
    let (app, _store, _deps) = build_app("sak-dedup").await;
    let keys = json!([
        {"id": "k1", "key": "sk-old", "name": "old"},
        {"id": "k1", "key": "sk-new", "name": "new"},
        {"key": "sk-noid"},
        {"key": ""},
    ]);
    let (status, body) = send_json(
        app,
        "POST",
        "/api/wr/rpc",
        Some("test-admin-token"),
        // Python 契约:args = [keys 列表](外层再包一层)。
        Some(rpc_body("saveApiKeys", json!([keys]))),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let apikeys = body["value"]["APIKEYS"].as_array().unwrap();
    // k1 末位胜;无 id 保留;空 key 丢弃。
    assert_eq!(apikeys.len(), 2);
    assert_eq!(apikeys[0]["key"], "sk-new");
    assert_eq!(apikeys[1]["key"], "sk-noid");
}

// ---------------------------------------------------------------------------
// providers CRUD(R5.3 REST 面)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn providers_upsert_delete_round_trip() {
    let (app, store, _deps) = build_app("prov-crud").await;
    // upsert(新 provider,带明文密钥)。
    let (status, body) = send_json(
        app.clone(),
        "PUT",
        "/api/v1/providers/p1",
        Some("test-admin-token"),
        Some(json!({"id": "p1", "name": "P1", "api_base_url": "https://api.test.com", "protocol": "openai", "api_key": "sk-crud"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // 响应为实体形状(掩码),键集镜像 Python Provider.to_dict(False)。
    assert_eq!(body["api_key"], "***");
    assert_eq!(body["protocol"], "openai");
    assert!(body.get("base_url").is_some());
    // 密钥落 secrets。
    assert_eq!(store.provider_secret("p1").await.unwrap(), "sk-crud");
    // 回填语义:已有密钥时空串不覆盖(改名不改密钥)。
    let (status, body) = send_json(
        app.clone(),
        "PUT",
        "/api/v1/providers/p1",
        Some("test-admin-token"),
        Some(json!({"id": "p1", "name": "P1-renamed", "api_base_url": "https://api.test.com", "protocol": "openai", "api_key": ""})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], "P1-renamed");
    assert_eq!(store.provider_secret("p1").await.unwrap(), "sk-crud");
    // delete → 200 + 级联清密钥;再 delete → 404。
    let (status, body) = send_json(
        app.clone(),
        "DELETE",
        "/api/v1/providers/p1",
        Some("test-admin-token"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], Value::Bool(true));
    assert_eq!(store.provider_secret("p1").await.unwrap_or_default(), "");
    let (status, body) = send_json(
        app,
        "DELETE",
        "/api/v1/providers/p1",
        Some("test-admin-token"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["detail"], "Provider not found");
}

#[tokio::test]
async fn providers_list_masks_secrets() {
    let (app, store, _deps) = build_app("prov-list").await;
    store
        .upsert_provider_secret("p1", "sk-hidden")
        .await
        .unwrap();
    // 先写一个 provider 进配置(空串 api_key 不动已有密钥)。
    let (status, _) = send_json(
        app.clone(),
        "POST",
        "/api/wr/rpc",
        Some("test-admin-token"),
        Some(rpc_body("saveConfig", save_args(json!("")))),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = send_json(
        app,
        "GET",
        "/api/v1/providers",
        Some("test-admin-token"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let items = body.as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], "p1");
    // 注水发生在 to_dict 之前,故掩码为 ***(非空密钥被掩)。
    assert_eq!(items[0]["api_key"], "***");
}
