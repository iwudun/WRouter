//! 客户端指纹:透传 / 预设 / 默认 off

use super::*;

// ---------------------------------------------------------------------------
// 客户端指纹(20261016_03):透传 / 预设 / 默认 off
// ---------------------------------------------------------------------------

fn anthropic_provider_entry(base_url: &str, extra: Value) -> Value {
    let mut entry = json!({
        "id": "prov-claude",
        "name": "prov-claude",
        "api_base_url": base_url,
        "protocol": "anthropic",
        "models": ["m-test"],
        "max_retries": 0,
        "api_key": "sk-provider",
    });
    if let (Some(target), Some(obj)) = (entry.as_object_mut(), extra.as_object()) {
        for (key, value) in obj {
            target.insert(key.clone(), value.clone());
        }
    }
    entry
}

fn anthropic_ok() -> Value {
    json!({
        "id": "msg-ok",
        "type": "message",
        "role": "assistant",
        "model": "m-test",
        "content": [{"type": "text", "text": "hello"}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 1, "output_tokens": 1},
    })
}

fn fingerprint_request_headers() -> Vec<(&'static str, &'static str)> {
    vec![
        ("x-api-key", "sk-gw-client"),
        ("user-agent", "client-ua/1.0"),
        ("anthropic-beta", "beta-test-flag"),
        ("x-client-secret", "noise"),
    ]
}

#[tokio::test]
async fn client_fingerprint_passthrough_forwards_identity_and_strips_auth() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(anthropic_ok()))
        .mount(&server)
        .await;
    let (app, dir) = build_app(
        "fp-passthrough",
        json!([anthropic_provider_entry(
            &server.uri(),
            json!({"clientFingerprint": {"mode": "passthrough"}}),
        )]),
        json!([{"key": "sk-gw-client", "enabled": true, "name": "t"}]),
    )
    .await;
    let (status, body, _) = send_json_no_auth(
        app,
        "/v1/messages",
        json!({"model": "prov-claude/m-test", "max_tokens": 16, "messages": [{"role": "user", "content": "hi"}]}),
        &fingerprint_request_headers(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let upstream = &requests[0];
    let header = |name: &str| {
        upstream
            .headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    };
    // 透传:客户端身份头按白名单出站。
    assert_eq!(header("user-agent"), "client-ua/1.0");
    assert_eq!(header("anthropic-beta"), "beta-test-flag");
    // 红线:客户端打 WRouter 的网关凭据不得上送,注入的是 provider 密钥。
    assert_eq!(header("x-api-key"), "sk-provider");
    // 白名单外(x-client-secret)不外带。
    assert_eq!(header("x-client-secret"), "");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn client_fingerprint_preset_injects_identity_headers_and_query() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(query_param("beta", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(anthropic_ok()))
        .mount(&server)
        .await;
    let (app, dir) = build_app(
        "fp-preset",
        json!([anthropic_provider_entry(
            &server.uri(),
            json!({"clientFingerprint": {"mode": "preset", "presetId": "claude-code"}}),
        )]),
        json!([{"key": "sk-gw-client", "enabled": true, "name": "t"}]),
    )
    .await;
    let (status, body, _) = send_json_no_auth(
        app,
        "/v1/messages",
        json!({"model": "prov-claude/m-test", "max_tokens": 16, "messages": [{"role": "user", "content": "hi"}]}),
        &fingerprint_request_headers(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1, "query 不匹配 beta=true 时 mock 不会命中");
    let upstream = &requests[0];
    let header = |name: &str| {
        upstream
            .headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    };
    // 预设注入:claude-code 身份头 + 端点 query(beta=true 由 matcher 断言)。
    assert!(
        header("user-agent").starts_with("claude-cli/"),
        "{}",
        header("user-agent")
    );
    assert_eq!(header("x-app"), "cli");
    assert_eq!(
        header("anthropic-beta"),
        "claude-code-20250219,interleaved-thinking-2025-05-14,context-1m-2025-08-07,context-management-2025-06-27,structured-outputs-2025-12-15,web-search-2025-03-05,advanced-tool-use-2025-11-20,tool-search-tool-2025-10-19,effort-2025-11-24,prompt-caching-scope-2026-01-05,thinking-token-count-2026-05-13"
    );
    // 客户端身份不透传(preset 模式不用入站快照),凭据仍是 provider 的。
    assert_eq!(header("x-api-key"), "sk-provider");
    assert_eq!(header("x-client-secret"), "");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn client_fingerprint_off_keeps_outbound_free_of_client_identity() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(anthropic_ok()))
        .mount(&server)
        .await;
    let (app, dir) = build_app(
        "fp-off",
        json!([anthropic_provider_entry(&server.uri(), json!({}))]),
        json!([{"key": "sk-gw-client", "enabled": true, "name": "t"}]),
    )
    .await;
    let (status, body, _) = send_json_no_auth(
        app,
        "/v1/messages",
        json!({"model": "prov-claude/m-test", "max_tokens": 16, "messages": [{"role": "user", "content": "hi"}]}),
        &fingerprint_request_headers(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let upstream = &requests[0];
    let header = |name: &str| {
        upstream
            .headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    };
    // 默认 off:客户端身份头一概不透传(golden 基线行为零变化)。
    assert_ne!(header("user-agent"), "client-ua/1.0");
    assert_eq!(header("anthropic-beta"), "");
    assert_eq!(header("x-client-secret"), "");
    assert_eq!(header("x-api-key"), "sk-provider");
    let _ = std::fs::remove_dir_all(dir);
}
