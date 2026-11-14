//! 网关集成测试共享装配:临时配置、app 构建与请求 helper。

use super::*;

pub(super) fn seed_config(data_dir: &std::path::Path, providers: Value, api_keys: Value, model_groups: Value) {
    let payload = json!({
        "version": 2,
        "Providers": providers,
        "APIKEYS": api_keys,
        "modelGroups": model_groups,
    });
    write_config_payload(data_dir, &payload);
}

/// 阶段 D:带 `observability` 段的播种入口(captureBodies opt-in)。
pub(super) fn seed_config_with_observability(
    data_dir: &std::path::Path,
    providers: Value,
    api_keys: Value,
    observability: Value,
) {
    let payload = json!({
        "version": 2,
        "Providers": providers,
        "APIKEYS": api_keys,
        "modelGroups": json!([]),
        "observability": observability,
    });
    write_config_payload(data_dir, &payload);
}

/// 20260906_01 B4:带 `proxy` 段(命名预设 + 全局默认)的播种入口。
pub(super) fn seed_config_with_proxy(
    data_dir: &std::path::Path,
    providers: Value,
    api_keys: Value,
    proxy: Value,
) {
    let payload = json!({
        "version": 2,
        "Providers": providers,
        "APIKEYS": api_keys,
        "proxy": proxy,
    });
    write_config_payload(data_dir, &payload);
}

/// 2026-11-05(B-5):带 `Router.fallback`(全局失败回退)的播种入口。
/// model-chain 形态的链项是 `Provider/模型` 形态的**内部选择器**。
pub(super) fn seed_config_with_fallback(
    data_dir: &std::path::Path,
    providers: Value,
    api_keys: Value,
    fallback: Value,
) {
    let payload = json!({
        "version": 2,
        "Providers": providers,
        "APIKEYS": api_keys,
        "Router": {"fallback": fallback},
    });
    write_config_payload(data_dir, &payload);
}

pub(super) fn write_config_payload(data_dir: &std::path::Path, payload: &Value) {
    let conn = Connection::open(data_dir.join("config.sqlite")).unwrap();
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS app_config (id INTEGER PRIMARY KEY CHECK (id = 1), payload TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS secrets (name TEXT PRIMARY KEY, value TEXT NOT NULL);",
    )
    .unwrap();
    conn.execute("DELETE FROM app_config", []).unwrap();
    conn.execute("DELETE FROM secrets", []).unwrap();
    conn.execute(
        "INSERT INTO app_config(id, payload) VALUES(1, ?1)",
        [payload.to_string()],
    )
    .unwrap();
}

pub(super) fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "wr-gateway-tests-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub(super) fn provider_entry(base_url: &str, extra: Value) -> Value {
    let mut entry = json!({
        "id": "prov-test",
        "name": "prov-test",
        "api_base_url": base_url,
        "protocol": "openai",
        "models": ["m-test"],
        "max_retries": 0,
    });
    if let (Some(target), Some(obj)) = (entry.as_object_mut(), extra.as_object()) {
        for (key, value) in obj {
            target.insert(key.clone(), value.clone());
        }
    }
    entry
}

pub(super) async fn build_app(
    tag: &str,
    providers: Value,
    api_keys: Value,
) -> (axum::Router, std::path::PathBuf) {
    build_app_with_groups(tag, providers, api_keys, json!([])).await
}

/// 20260905_04:带聚合定义(modelGroups)的装配入口。
pub(super) async fn build_app_with_groups(
    tag: &str,
    providers: Value,
    api_keys: Value,
    model_groups: Value,
) -> (axum::Router, std::path::PathBuf) {
    let data_dir = temp_dir(tag);
    seed_config(&data_dir, providers, api_keys, model_groups);
    let mut settings = test_settings();
    settings.data_dir = data_dir.clone();
    let settings = Arc::new(settings);
    let source = Arc::new(wr_infra::sqlite_config::SqliteConfigSource::new(
        &settings.data_dir,
    ));
    let store = Arc::new(
        wr_infra::sqlite::SqliteStore::open(&data_dir)
            .await
            .unwrap(),
    );
    let deps = Arc::new(wr_server::gateway::GatewayDeps::new(
        settings.clone(),
        source,
        store,
    ));
    let app = wr_server::gateway_router(settings, deps);
    (app, data_dir)
}

/// 带 `savings` 段的播种入口(P5 响应缓存闭环)。
pub(super) fn seed_config_with_savings(
    data_dir: &std::path::Path,
    providers: Value,
    api_keys: Value,
    savings: Value,
) {
    let payload = json!({
        "version": 2,
        "Providers": providers,
        "APIKEYS": api_keys,
        "savings": savings,
    });
    write_config_payload(data_dir, &payload);
}

/// P5:带 `savings.responseCache` 的装配入口(缓存命中语义测试)。
pub(super) async fn build_app_with_savings(
    tag: &str,
    providers: Value,
    api_keys: Value,
    savings: Value,
) -> (axum::Router, std::path::PathBuf) {
    let data_dir = temp_dir(tag);
    seed_config_with_savings(&data_dir, providers, api_keys, savings);
    let mut settings = test_settings();
    settings.data_dir = data_dir.clone();
    let settings = Arc::new(settings);
    let source = Arc::new(wr_infra::sqlite_config::SqliteConfigSource::new(
        &settings.data_dir,
    ));
    let store = Arc::new(
        wr_infra::sqlite::SqliteStore::open(&data_dir)
            .await
            .unwrap(),
    );
    let deps = Arc::new(wr_server::gateway::GatewayDeps::new(
        settings.clone(),
        source,
        store,
    ));
    let app = wr_server::gateway_router(settings, deps);
    (app, data_dir)
}

/// P5-5:自定义请求体上限的装配入口(资源上界测试)。
pub(super) async fn build_app_with_body_limit(
    tag: &str,
    providers: Value,
    api_keys: Value,
    max_body_bytes: usize,
) -> (axum::Router, std::path::PathBuf) {
    let data_dir = temp_dir(tag);
    seed_config(&data_dir, providers, api_keys, json!([]));
    let mut settings = test_settings();
    settings.data_dir = data_dir.clone();
    settings.max_body_bytes = max_body_bytes;
    let settings = Arc::new(settings);
    let source = Arc::new(wr_infra::sqlite_config::SqliteConfigSource::new(
        &settings.data_dir,
    ));
    let store = Arc::new(
        wr_infra::sqlite::SqliteStore::open(&data_dir)
            .await
            .unwrap(),
    );
    let deps = Arc::new(wr_server::gateway::GatewayDeps::new(
        settings.clone(),
        source,
        store,
    ));
    let app = wr_server::gateway_router(settings, deps);
    (app, data_dir)
}

/// 2026-11-05(B-5):带全局失败回退(`Router.fallback`)的装配入口。
pub(super) async fn build_app_with_fallback(
    tag: &str,
    providers: Value,
    api_keys: Value,
    fallback: Value,
) -> (axum::Router, std::path::PathBuf) {
    let data_dir = temp_dir(tag);
    seed_config_with_fallback(&data_dir, providers, api_keys, fallback);
    let mut settings = test_settings();
    settings.data_dir = data_dir.clone();
    let settings = Arc::new(settings);
    let source = Arc::new(wr_infra::sqlite_config::SqliteConfigSource::new(
        &settings.data_dir,
    ));
    let store = Arc::new(
        wr_infra::sqlite::SqliteStore::open(&data_dir)
            .await
            .unwrap(),
    );
    let deps = Arc::new(wr_server::gateway::GatewayDeps::new(
        settings.clone(),
        source,
        store,
    ));
    let app = wr_server::gateway_router(settings, deps);
    (app, data_dir)
}

pub(super) fn test_settings() -> wr_server::settings::Settings {
    wr_server::settings::Settings {
        app_name: "WRouter".to_owned(),
        version: wr_server::VERSION.to_owned(),
        host: "127.0.0.1".to_owned(),
        gateway_port: 3460,
        web_port: 3462,
        proxy_port: 3463,
        data_dir: std::env::temp_dir().join("wr-gateway-tests-settings"),
        config_dir: std::env::temp_dir().join("wr-gateway-tests-config"),
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

/// dev-open 移除后数据面必须带凭据。非认证测试统一经 `send_json` 自动注入
/// internal token(`test_settings` 的 `auth_token="test-token"`,恒放行);
/// 认证专项测试改用 `send_json_no_auth` 精确控制请求头(含“无凭据→401”)。
pub(super) const INTERNAL_AUTH: (&str, &str) = ("authorization", "Bearer test-token");

pub(super) fn has_auth_header(headers: &[(&str, &str)]) -> bool {
    headers.iter().any(|(name, _)| {
        let lowered = name.to_ascii_lowercase();
        lowered == "authorization" || lowered == "x-api-key" || lowered == "api-key"
    })
}

/// 无显式认证头时自动注入 internal token,再转发到 [`send_json_no_auth`]。
pub(super) async fn send_json(
    app: axum::Router,
    path: &str,
    body: Value,
    headers: &[(&str, &str)],
) -> (StatusCode, Value, axum::http::HeaderMap) {
    let mut effective = headers.to_vec();
    if !has_auth_header(headers) {
        effective.push(INTERNAL_AUTH);
    }
    send_json_no_auth(app, path, body, &effective).await
}

/// 原样发送(不注入凭据)——认证专项测试用。
pub(super) async fn send_json_no_auth(
    app: axum::Router,
    path: &str,
    body: Value,
    headers: &[(&str, &str)],
) -> (StatusCode, Value, axum::http::HeaderMap) {
    let mut builder = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = app
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let response_headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let parsed = serde_json::from_slice::<Value>(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    (status, parsed, response_headers)
}

pub(super) fn ok_chat() -> Value {
    json!({
        "id": "up-ok",
        "object": "chat.completion",
        "model": "m-test",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "hello"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
    })
}

// ---------------------------------------------------------------------------
// 跨分节共享的流式/聚合装配 helper(原分段内定义,按引用上移)
// ---------------------------------------------------------------------------

/// 上游 mock 的流式响应体(一条内容 delta + [DONE])。
pub(super) fn ok_sse() -> &'static str {
    concat!(
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m-test\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"hi\"},\"finish_reason\":null}]}\n\n",
        "data: [DONE]\n\n",
    )
}

/// 发一条流式 chat 请求,返回 (HTTP 状态, SSE 正文)。
pub(super) async fn send_stream(app: axum::Router, model: &str, api_key: &str) -> (StatusCode, String) {
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .header("x-api-key", api_key)
        .body(Body::from(
            json!({"model": model, "messages": [], "stream": true}).to_string(),
        ))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

/// 原样发送流式请求体,返回状态/头/正文(P5 缓存头断言用)。
pub(super) async fn send_stream_body(
    app: axum::Router,
    body: Value,
) -> (StatusCode, axum::http::HeaderMap, String) {
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .header(INTERNAL_AUTH.0, INTERNAL_AUTH.1)
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

/// 提交前错误现在是真实 HTTP 状态 + JSON 错误体。
pub(super) fn error_body(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|error| panic!("错误体不是 JSON({error}): {text}"))
}

/// 内部身份发流式请求(聚合/控制链测试用)。
pub(super) async fn send_stream_internal(app: axum::Router, model: &str) -> (StatusCode, String) {
    send_stream_internal_path(app, "/v1/chat/completions", model).await
}

pub(super) async fn send_stream_internal_path(
    app: axum::Router,
    path: &str,
    model: &str,
) -> (StatusCode, String) {
    let request = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .header(INTERNAL_AUTH.0, INTERNAL_AUTH.1)
        .body(Body::from(
            json!({"model": model, "messages": [], "stream": true}).to_string(),
        ))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

pub(super) fn group_provider(id: &str, base_url: &str, models: &[&str]) -> Value {
    json!({
        "id": id,
        "name": id,
        "api_base_url": base_url,
        "protocol": "openai",
        "models": models,
        "max_retries": 0,
    })
}

pub(super) fn ok_chat_model(model: &str) -> Value {
    json!({
        "id": "up-ok",
        "object": "chat.completion",
        "model": model,
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "hello"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
    })
}
