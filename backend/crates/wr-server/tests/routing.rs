//! R3 路由引擎与协议适配集成测试(wiremock mock 上游)。
//!
//! 覆盖 14 号文档 R3 验收标准的显式命名断言:
//! - `streaming_no_inplace_retry`(C4:已发首字节即不原地重试);
//! - `retry_chain_midway_success` / `candidate_chain_switch` /
//!   `breaker_trigger_skips_provider` / `breaker_ignores_501`(R3.13 新增场景);
//! - 路由规则端到端(改写 body.model + x-wr-route-* 头);
//! - model-chain 执行计划(首环无 provider → 推进次环);
//! - 跨协议转换(openai↔anthropic 非流式/流式);
//! - 访问控制 deny(403 policy_denied)。

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use rusqlite::Connection;
use serde_json::{Value, json};
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ---------------------------------------------------------------------------
// 装配助手
// ---------------------------------------------------------------------------

fn seed_payload(data_dir: &std::path::Path, payload: Value) {
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

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::path::PathBuf::from(
        std::env::var("TMPDIR")
            .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned()),
    )
    .join(format!(
        "wr-routing-tests-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn build_app_with(tag: &str, payload: Value) -> (axum::Router, std::path::PathBuf) {
    let data_dir = temp_dir(tag);
    seed_payload(&data_dir, payload);
    let mut settings = wr_server::settings::Settings {
        app_name: "WRouter".to_owned(),
        version: wr_server::VERSION.to_owned(),
        host: "127.0.0.1".to_owned(),
        gateway_port: 3460,
        web_port: 3462,
        proxy_port: 3463,
        data_dir: std::env::temp_dir().join("wr-routing-tests-settings"),
        config_dir: std::env::temp_dir().join("wr-routing-tests-config"),
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
    };
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

fn provider_entry(id: &str, base_url: &str, extra: Value) -> Value {
    let mut entry = json!({
        "id": id,
        "name": id,
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

fn base_payload(providers: Vec<Value>) -> Value {
    json!({"version": 2, "Providers": providers, "APIKEYS": []})
}

fn ok_chat() -> Value {
    json!({
        "id": "up-ok", "object": "chat.completion", "created": 1, "model": "m-test",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "hello"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3},
    })
}

/// dev-open 移除后数据面请求必须带凭据。测试统一用 internal token
/// (`test_settings` 的 `auth_token="test-token"`,恒放行);调用方已
/// 显式带认证头时不覆盖。
const INTERNAL_AUTH: (&str, &str) = ("authorization", "Bearer test-token");

fn has_auth_header(headers: &[(&str, &str)]) -> bool {
    headers.iter().any(|(name, _)| {
        let lowered = name.to_ascii_lowercase();
        lowered == "authorization" || lowered == "x-api-key" || lowered == "api-key"
    })
}

async fn send_json(
    app: axum::Router,
    uri: &str,
    body: Value,
    headers: &[(&str, &str)],
) -> (StatusCode, Value, axum::http::HeaderMap) {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    if !has_auth_header(headers) {
        builder = builder.header(INTERNAL_AUTH.0, INTERNAL_AUTH.1);
    }
    let response = app
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let value = serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text));
    (status, value, headers)
}

async fn send_raw_capture_body(
    app: axum::Router,
    uri: &str,
    body: Value,
    headers: &[(&str, &str)],
) -> (StatusCode, String) {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    if !has_auth_header(headers) {
        builder = builder.header(INTERNAL_AUTH.0, INTERNAL_AUTH.1);
    }
    let response = app
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

// ---------------------------------------------------------------------------
// C4:流式无原地重试
// ---------------------------------------------------------------------------

/// C4 断言:已发出首字节的上游流中途炸掉 → 客户端拿到首帧 + 流终止,
/// 网关不得原地重试(手搓 tokio 上游:发首块后立即断开)。

#[tokio::test]
async fn streaming_no_inplace_retry() {
    // wiremock 0.6 无法直接抛 body error —— 用真实 tokio TcpListener
    // 手搓一个「发首块即断开」的上游。
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            // 读到请求头 + body(长度不校验,读一轮即可——mock 场景足够)。
            let mut buffer = vec![0u8; 8192];
            let _ = socket.read(&mut buffer).await;
            let payload = "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m-test\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hi\"},\"finish_reason\":null}]}\n\n";
            // chunk 长度按实际字节数(hex)计算。
            let first = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n{:x}\r\n{payload}\r\n",
                payload.len()
            );
            let _ = socket.write_all(first.as_bytes()).await;
            let _ = socket.flush().await;
            // 立即断开:无终止 0-chunk、无 [DONE] → 读侧 transport error。
            drop(socket);
        }
    });

    let providers = vec![provider_entry(
        "prov-boom",
        &format!("http://{addr}"),
        json!({"max_retries": 3}),
    )];
    let (app, dir) = build_app_with("stream-no-retry", base_payload(providers)).await;
    let (status, body) = send_raw_capture_body(
        app,
        "/v1/chat/completions",
        json!({"model": "prov-boom/m-test", "messages": [], "stream": true}),
        &[],
    )
    .await;
    // 首帧已发出(200 + SSE);P3 原生 Raw 原字节透传上游首帧。
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains(r#""content":"Hi""#), "body={body}");
    // 无 finalize 的 stop+[DONE](mid_stream_error 直接终止)。
    assert!(!body.contains("[DONE]"), "不得补发终止帧: body={body}");
    let _ = std::fs::remove_dir_all(dir);
}

// ---------------------------------------------------------------------------
// C7 端到端命名断言
// ---------------------------------------------------------------------------

/// C7:tool_call args 首片段(开场块捆绑的 `{"lo`)端到端不丢——
/// 客户端按 index 拼接全部 arguments 后必须能还原完整 JSON。
#[tokio::test]
async fn toolcall_first_arg_fragment() {
    let server = MockServer::start().await;
    let sse = concat!(
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m-test\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"get_weather\",\"arguments\":\"{\\\"lo\"}}]},\"finish_reason\":null}]}

",
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m-test\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"c\\\": \\\"SF\\\"}\"}}]},\"finish_reason\":null}]}

",
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m-test\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}

",
        "data: [DONE]

",
    );
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream"))
        .mount(&server)
        .await;
    let (app, dir) = build_app_with(
        "toolcall-c7",
        base_payload(vec![provider_entry("prov-c7", &server.uri(), json!({}))]),
    )
    .await;
    let (status, body) = send_raw_capture_body(
        app,
        "/v1/chat/completions",
        json!({"model": "prov-c7/m-test", "messages": [], "stream": true}),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // 拼接客户端收到的全部 arguments 分片。
    let mut arguments = String::new();
    for line in body.lines() {
        let Some(payload) = line.strip_prefix("data: ") else {
            continue;
        };
        let Ok(chunk) = serde_json::from_str::<Value>(payload) else {
            continue;
        };
        if let Some(calls) = chunk
            .pointer("/choices/0/delta/tool_calls")
            .and_then(Value::as_array)
        {
            for call in calls {
                if let Some(frag) = call.pointer("/function/arguments").and_then(Value::as_str) {
                    arguments.push_str(frag);
                }
            }
        }
    }
    // 首片段 `{"lo` 在 + 全量拼接可解析为合法 JSON。
    assert!(
        arguments.starts_with(r#"{"lo"#),
        "首片段丢失: {arguments:?} BODY={body}"
    );
    let parsed: Value = serde_json::from_str(&arguments).expect("arguments 拼接后必须是合法 JSON");
    assert_eq!(parsed, json!({"loc": "SF"}));
    let _ = std::fs::remove_dir_all(dir);
}

// ---------------------------------------------------------------------------
// 三层回退(R3.13 新增场景)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn retry_chain_midway_success() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({
            "error": {"message": "boom", "type": "upstream_error"}
        })))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_chat()))
        .expect(1)
        .mount(&server)
        .await;
    let providers = vec![provider_entry(
        "prov-retry",
        &server.uri(),
        json!({"max_retries": 2}),
    )];
    let (app, dir) = build_app_with("retry-midway", base_payload(providers)).await;
    let (status, body, headers) = send_json(
        app,
        "/v1/chat/completions",
        json!({"model": "prov-retry/m-test", "messages": []}),
        &[],
    )
    .await;
    // 同 provider 第二次尝试成功(C4 第三层)。
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], json!("up-ok"));
    // attempts=2 → failures=1。
    assert_eq!(headers.get("x-wr-fallback-attempts").unwrap(), "2");
    assert_eq!(headers.get("x-wr-fallback-failures").unwrap(), "1");
    let _ = std::fs::remove_dir_all(dir);
}

/// 20261015_01 P1-3:401/403 凭据类失败在当前请求内推进候选。
/// - 多候选:首候选 401 → 推进次候选成功,客户端拿到 200;
/// - 单候选:401 原样透传(401 而非 502,与终态语义一致)。
#[tokio::test]
async fn credential_failure_advances_candidates() {
    let unauthorized = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {"message": "invalid api key", "type": "authentication_error"}
        })))
        .mount(&unauthorized)
        .await;
    let good = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_chat()))
        .expect(1)
        .mount(&good)
        .await;
    let providers = vec![
        provider_entry("prov-a", &unauthorized.uri(), json!({"max_retries": 0})),
        provider_entry("prov-b", &good.uri(), json!({"max_retries": 0})),
    ];
    let mut payload = base_payload(providers);
    payload["modelGroups"] = json!([{
        "id": "g-cred",
        "name": "g-cred",
        "enabled": true,
        "strategy": "first",
        "targets": [
            {"providerId": "prov-a", "model": "m-test", "weight": 1, "priority": 0, "enabled": true},
            {"providerId": "prov-b", "model": "m-test", "weight": 1, "priority": 0, "enabled": true},
        ],
    }]);
    let (app, dir) = build_app_with("credential-advance", payload).await;

    // 多候选:prov-a 401 → 当前提进 prov-b → 200。
    let (status, body, _) = send_json(
        app.clone(),
        "/v1/chat/completions",
        json!({"model": "wrouter/g-cred", "messages": []}),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["id"], json!("up-ok"));
    assert_eq!(
        unauthorized.received_requests().await.unwrap().len(),
        1,
        "401 不原地重试,只打一次"
    );
    let _ = std::fs::remove_dir_all(dir);

    // 单候选(显式 provider/model 引用):401 原样透传。
    let unauthorized2 = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {"message": "invalid api key", "type": "authentication_error"}
        })))
        .expect(1)
        .mount(&unauthorized2)
        .await;
    let mut payload = base_payload(vec![provider_entry(
        "prov-solo",
        &unauthorized2.uri(),
        json!({"max_retries": 0}),
    )]);
    payload["modelGroups"] = json!([]);
    let (app, dir) = build_app_with("credential-solo", payload).await;
    let (status, body, _) = send_json(
        app,
        "/v1/chat/completions",
        json!({"model": "prov-solo/m-test", "messages": []}),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["error"]["message"], json!("invalid api key"));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn candidate_chain_switch() {
    // 20261015_01 R1:provider 级 fallback_providers 已移除,候选链切换
    // 由聚合组承载(顺序即候选序):首目标不可达 → 推进次目标。
    let good = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_chat()))
        .expect(1)
        .mount(&good)
        .await;
    let providers = vec![
        provider_entry(
            "prov-a",
            "http://127.0.0.1:9/", // 不可达端口:候选 A 直接传输错误
            json!({"max_retries": 0}),
        ),
        provider_entry("prov-b", &good.uri(), json!({})),
    ];
    let mut payload = base_payload(providers);
    payload["modelGroups"] = json!([{
        "id": "g-chain",
        "name": "g-chain",
        "enabled": true,
        "strategy": "first",
        "targets": [
            {"providerId": "prov-a", "model": "m-test", "weight": 1, "priority": 0, "enabled": true},
            {"providerId": "prov-b", "model": "m-test", "weight": 1, "priority": 0, "enabled": true},
        ],
    }]);
    let (app, dir) = build_app_with("candidate-switch", payload).await;
    let (status, body, _) = send_json(
        app,
        "/v1/chat/completions",
        json!({"model": "wrouter/g-chain", "messages": []}),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], json!("up-ok"));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn breaker_trigger_skips_provider() {
    // Python 实测语义(14 号文档 R4 完成记录):**非流式 5xx 永远开不了闸**
    // —— `_request_with_fallback` 里的 `record_response` 计次之后,
    // `forward_json` 又会无条件 `record_success` 归零。真正会开闸的是
    // **网络层失败**(连接被拒 / 超时:retry_fallback.py 的 except 分支
    // 记 failure 后异常外抛,不再走 record_success)。
    // 因此这里用一个必然连接失败的死端口来触发熔断。
    let good = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_chat()))
        .mount(&good)
        .await;
    let dead_port = dead_tcp_port().await;

    let providers = vec![
        provider_entry(
            "prov-a",
            &format!("http://127.0.0.1:{dead_port}/v1"),
            json!({"max_retries": 0}),
        ),
        provider_entry("prov-b", &good.uri(), json!({})),
    ];
    let mut payload = base_payload(providers);
    payload["reliability"] = json!({
        "circuit_breaker": {"enabled": true, "failure_threshold": 1, "cooldown_ms": 60000}
    });
    // 20261015_01 R1:候选链由聚合组承载。
    payload["modelGroups"] = json!([{
        "id": "g-breaker",
        "name": "g-breaker",
        "enabled": true,
        "strategy": "first",
        "targets": [
            {"providerId": "prov-a", "model": "m-test", "weight": 1, "priority": 0, "enabled": true},
            {"providerId": "prov-b", "model": "m-test", "weight": 1, "priority": 0, "enabled": true},
        ],
    }]);
    let (app, dir) = build_app_with("breaker", payload).await;

    // 请求 1:A 连接失败(网络层失败开闸)→ 聚合候选推进到 B。
    let (status, body, _) = send_json(
        app.clone(),
        "/v1/chat/completions",
        json!({"model": "wrouter/g-breaker", "messages": []}),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], json!("up-ok"));

    // 请求 2:A 已被熔断挡下(聚合展开健康门控直接过滤),直接由 B 服务。
    let (status, body, _) = send_json(
        app.clone(),
        "/v1/chat/completions",
        json!({"model": "wrouter/g-breaker", "messages": []}),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], json!("up-ok"));
    assert_eq!(
        good.received_requests().await.unwrap().len(),
        2,
        "两次请求都应落到链上的 B"
    );

    // R4.8:开闸状态必须已落 sqlite(跨进程可见)。
    let store = wr_infra::sqlite::SqliteStore::open(&dir).await.unwrap();
    let state = wr_app::ports::BreakerStateStore::load_breaker_state(&store, "prov-a")
        .await
        .unwrap()
        .expect("熔断状态应持久化");
    assert_eq!(state.consecutive_failures, 1);
    assert!(state.opened_until_ms > 0, "开闸时刻必须写入");
    store.close().await;
    let _ = std::fs::remove_dir_all(dir);
}

/// C4 相关(Python 真实语义,非理想行为):非流式 5xx 经 `record_success`
/// 归零,反复 5xx 也**不会**开闸。与 `breaker_ignores_501` 一起钉住
/// 「哪些状态码不参与熔断」的完整口径。
#[tokio::test]
async fn breaker_5xx_never_opens_on_json_path() {
    let bad = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({
            "error": {"message": "unavailable", "type": "server_error"}
        })))
        .mount(&bad)
        .await;
    let providers = vec![provider_entry(
        "prov-a",
        &bad.uri(),
        json!({"max_retries": 0}),
    )];
    let mut payload = base_payload(providers);
    payload["reliability"] = json!({
        "circuit_breaker": {"enabled": true, "failure_threshold": 1, "cooldown_ms": 60000}
    });
    let (app, dir) = build_app_with("breaker5xx", payload).await;
    for _ in 0..3 {
        let (status, body, _) = send_json(
            app.clone(),
            "/v1/chat/completions",
            json!({"model": "prov-a/m-test", "messages": []}),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"]["message"], json!("unavailable"));
    }
    // 三次 503 之后:每次都打到 A(闸没开),且库里没有开闸记录。
    assert_eq!(bad.received_requests().await.unwrap().len(), 3);
    let store = wr_infra::sqlite::SqliteStore::open(&dir).await.unwrap();
    let state = wr_app::ports::BreakerStateStore::load_breaker_state(&store, "prov-a")
        .await
        .unwrap();
    if let Some(row) = state {
        // 从未跃迁过就不写库(与 Python 的写放大保护一致);
        // 一旦写了,计数必须已被 record_success 归零。
        assert_eq!(
            row.consecutive_failures, 0,
            "record_success 归零后落库的计数必须是 0"
        );
    }
    store.close().await;
    let _ = std::fs::remove_dir_all(dir);
}

/// 拿一个必然拒绝连接的本地端口(绑定后立即释放)。
async fn dead_tcp_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    // 给端口一点时间真正关闭(Windows 上 TIME_WAIT 不影响 connect 被拒)。
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    port
}

/// C4 断言(主题 #7):501 不在默认 failure_status_codes —— 反复 501
/// 也不得触发熔断(可重试但不计熔断票)。
#[tokio::test]
async fn breaker_ignores_501() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(501).set_body_json(json!({
            "error": {"message": "not implemented", "type": "bad_gateway"}
        })))
        .mount(&server)
        .await;
    let fallback = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_chat()))
        .mount(&fallback)
        .await;
    let providers = vec![
        provider_entry("prov-501", &server.uri(), json!({"max_retries": 0})),
        provider_entry("prov-b", &fallback.uri(), json!({})),
    ];
    let (app, dir) = build_app_with("breaker-501", base_payload(providers)).await;
    // 连打 4 次(远超阈值 3):A 始终不被熔断挡下。
    for _ in 0..4 {
        let (status, _, _) = send_json(
            app.clone(),
            "/v1/chat/completions",
            json!({"model": "prov-501/m-test", "messages": []}),
            &[],
        )
        .await;
        // 501 不重试不回退(client 类透传)→ 客户端拿到 501 信封。
        assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 4);
    let _ = std::fs::remove_dir_all(dir);
}

// ---------------------------------------------------------------------------
// 路由规则端到端(策略链改写 body.model)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn global_sticky_switch_pins_provider_across_selectors() {
    // 20261101_02 T3-B:粘性触发源从规则动作改为全局 `savings.sticky.enabled`。
    // 请求 1(显式 prov-a)成功回写粘性绑定;请求 2(显式 prov-b、同会话键)
    // 命中粘住仍打到 prov-a。对照组:开关缺省(关)时不粘,请求落到 prov-b。
    for sticky_enabled in [true, false] {
        let tag = if sticky_enabled { "sticky-on" } else { "sticky-off" };
        let prov_a = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_chat()))
            .mount(&prov_a)
            .await;
        let prov_b = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_chat()))
            .mount(&prov_b)
            .await;
        let providers = vec![
            provider_entry("prov-a", &prov_a.uri(), json!({})),
            provider_entry("prov-b", &prov_b.uri(), json!({})),
        ];
        let mut payload = base_payload(providers);
        if sticky_enabled {
            payload["savings"] = json!({"sticky": {"enabled": true, "ttlSecs": 900}});
        }
        let (app, dir) = build_app_with(tag, payload).await;
        let session_key = format!("sess-{tag}");
        let session = [("x-session-id", session_key.as_str())];

        // 请求 1:显式 prov-a/m-test → 成功,粘性表回写(开关开时)。
        let (status, _, _) = send_json(
            app.clone(),
            "/v1/chat/completions",
            json!({"model": "prov-a/m-test", "messages": []}),
            &session,
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // 请求 2:显式 prov-b/m-test、同会话键。开关开 → 粘住 prov-a;
        // 开关关 → 正常链落 prov-b。
        let (status, _, _) = send_json(
            app.clone(),
            "/v1/chat/completions",
            json!({"model": "prov-b/m-test", "messages": []}),
            &session,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let hits_a = prov_a.received_requests().await.unwrap().len();
        let hits_b = prov_b.received_requests().await.unwrap().len();
        if sticky_enabled {
            assert_eq!(hits_a, 2, "粘住应把第二次请求带回 prov-a");
            assert_eq!(hits_b, 0);
        } else {
            assert_eq!(hits_a, 1);
            assert_eq!(hits_b, 1, "无粘性时第二次请求落显式目标 prov-b");
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[tokio::test]
async fn model_chain_plan_advances_past_unroutable_first_link() {
    let good = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_chat()))
        .mount(&good)
        .await;
    // 2026-09-26 调用契约:不可解析的裸名不再进计划(400),链推进语义
    // 由「首环不可达」钉住 —— 第一环指向必然拒绝连接的端口。
    let dead_port = dead_tcp_port().await;
    let providers = vec![
        provider_entry("prov-a", &format!("http://127.0.0.1:{dead_port}"), json!({})),
        provider_entry("prov-b", &good.uri(), json!({})),
    ];
    let mut payload = base_payload(providers);
    // 全局 Router.fallback = model-chain:prov-a/m-test → prov-b/m-test。
    payload["Router"] = json!({
        "fallback": {
            "mode": "model-chain", "retryCount": 0, "models": ["prov-b/m-test"],
        }
    });
    let (app, dir) = build_app_with("model-chain", payload).await;
    let (status, body, headers) = send_json(
        app,
        "/v1/chat/completions",
        json!({"model": "prov-a/m-test", "messages": []}),
        &[],
    )
    .await;
    // 第一环传输失败 → 推进第二环成功。
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], json!("up-ok"));
    // 观测头携带剥离前缀后的规范模型。
    assert_eq!(headers.get("x-wr-routed-model").unwrap(), "m-test");
    let _ = std::fs::remove_dir_all(dir);
}

// ---------------------------------------------------------------------------
// 跨协议转换(经网关面端到端)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn anthropic_client_to_openai_upstream_non_stream_conversion() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_chat()))
        .mount(&server)
        .await;
    let providers = vec![provider_entry(
        "prov-oai",
        &server.uri(),
        json!({"protocol": "openai", "models": ["m-test"]}),
    )];
    let (app, dir) = build_app_with("xlate-a2o", base_payload(providers)).await;
    let (status, body, _) = send_json(
        app,
        "/v1/messages",
        json!({"model": "prov-oai/m-test", "max_tokens": 16, "messages": [{"role": "user", "content": "hi"}]}),
        &[("anthropic-version", "2023-06-01")],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // openai chat JSON → anthropic Message 信封。
    assert_eq!(body["type"], json!("message"));
    assert_eq!(body["content"][0]["type"], json!("text"));
    assert_eq!(body["content"][0]["text"], json!("hello"));
    assert_eq!(body["stop_reason"], json!("end_turn"));
    assert_eq!(body["usage"]["input_tokens"], json!(1));
    // 上游收到 openai 形态请求体。
    let received = server.received_requests().await.unwrap();
    let sent: Value = serde_json::from_slice(&received[0].body).unwrap();
    assert_eq!(sent["messages"][0]["role"], json!("user"));
    assert_eq!(sent["max_tokens"], json!(16));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn openai_client_stream_against_anthropic_upstream() {
    let server = MockServer::start().await;
    let sse = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_9\",\"model\":\"c-m\",\"usage\":{\"input_tokens\":3}}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"He\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"llo\"}}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream"))
        .mount(&server)
        .await;
    let providers = vec![provider_entry(
        "prov-ant",
        &server.uri(),
        json!({"protocol": "anthropic", "models": ["m-test"]}),
    )];
    let (app, dir) = build_app_with("xlate-stream", base_payload(providers)).await;
    let (status, body) = send_raw_capture_body(
        app,
        "/v1/chat/completions",
        json!({"model": "prov-ant/m-test", "messages": [], "stream": true}),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // anthropic 事件 → openai chat chunk 文本。
    assert!(body.contains(r#""content": "He""#), "body={body}");
    assert!(body.contains(r#""content": "llo""#));
    // message_delta(end_turn)→ finish_reason stop;[DONE] 仅 finalize 一份。
    assert!(body.contains(r#""finish_reason": "stop""#));
    assert_eq!(body.matches("data: [DONE]").count(), 1);
    // usage 终块带缓存明细(Python final_chunk 形态)。
    assert!(body.contains(r#""prompt_tokens": 3"#));
    let _ = std::fs::remove_dir_all(dir);
}

// ---------------------------------------------------------------------------
// A6 流合成(流式请求收到单 JSON 上游)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn stream_synthesis_from_json_upstream() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            // 上游忽略 stream=true,回单 JSON(content-type: application/json)。
            ResponseTemplate::new(200).set_body_json(ok_chat()),
        )
        .mount(&server)
        .await;
    let providers = vec![provider_entry("prov-oai", &server.uri(), json!({}))];
    let (app, dir) = build_app_with("a6-synth", base_payload(providers)).await;
    let (status, body) = send_raw_capture_body(
        app,
        "/v1/chat/completions",
        json!({"model": "prov-oai/m-test", "messages": [], "stream": true}),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // 合成帧序列:role+content 首块、finish 尾块、[DONE]。
    assert!(body.contains(r#""delta": {"role": "assistant", "content": "hello"}"#));
    assert!(body.contains(r#""finish_reason": "stop""#));
    assert!(body.contains("data: [DONE]"));
    let _ = std::fs::remove_dir_all(dir);
}

// ---------------------------------------------------------------------------
// 访问控制策略(A3)deny → 403
// ---------------------------------------------------------------------------

#[tokio::test]
async fn access_policy_deny_returns_403() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_chat()))
        .mount(&server)
        .await;
    let providers = vec![provider_entry("prov-p", &server.uri(), json!({"models": ["m-test", "secret-x"]}))];
    let mut payload = base_payload(providers);
    payload["policies"] = json!({
        "enabled": true,
        "defaultEffect": "allow",
        "rules": [{
            "id": "deny-internal-models",
            "effect": "deny",
            "resource": "model",
            "identity": "*",
            "patterns": ["secret-*"],
        }],
    });
    let (app, dir) = build_app_with("access-deny", payload).await;
    let (status, body, _) = send_json(
        app.clone(),
        "/v1/chat/completions",
        json!({"model": "prov-p/secret-x", "messages": []}),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["type"], json!("policy_denied"));
    assert_eq!(body["error"]["policy_id"], json!("deny-internal-models"));
    // 未命中 deny 的模型正常放行(证明策略未挡常规流量)。
    let (status, body, _) = send_json(
        app.clone(),
        "/v1/chat/completions",
        json!({"model": "prov-p/m-test", "messages": []}),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], json!("up-ok"));
    let _ = std::fs::remove_dir_all(dir);
}
