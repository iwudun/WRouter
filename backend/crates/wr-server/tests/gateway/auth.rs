//! 认证

use super::*;

// ---------------------------------------------------------------------------
// 认证
// ---------------------------------------------------------------------------

#[tokio::test]
async fn auth_missing_credentials_rejected_with_401() {
    let server = MockServer::start().await;
    let (app, dir) = build_app(
        "auth-missing",
        json!([provider_entry(&server.uri(), json!({}))]),
        json!([{"key": "sk-test-1", "enabled": true, "name": "t"}]),
    )
    .await;
    let (status, body, headers) = send_json_no_auth(
        app,
        "/v1/chat/completions",
        json!({"model": "prov-test/m-test"}),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["type"], json!("authentication_error"));
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("Missing credentials")
    );
    // x-wrouter-request-id header 存在(UUID)。
    assert!(headers.get("x-wrouter-request-id").is_some());
    // mock 未被命中。
    assert_eq!(server.received_requests().await.unwrap().len(), 0);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn auth_invalid_key_rejected_and_valid_key_forwarded() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_chat()))
        .mount(&server)
        .await;
    let (app, dir) = build_app(
        "auth-valid",
        json!([provider_entry(&server.uri(), json!({}))]),
        json!([{"key": "sk-test-1", "enabled": true, "name": "t"}]),
    )
    .await;
    let (status, body, _) = send_json(
        app.clone(),
        "/v1/chat/completions",
        json!({"model": "prov-test/m-test"}),
        &[("Authorization", "Bearer sk-wrong")],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["message"], json!("Invalid API key."));

    let (status, body, _) = send_json(
        app.clone(),
        "/v1/chat/completions",
        json!({"model": "prov-test/m-test"}),
        &[("x-api-key", "sk-test-1")],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["choices"][0]["message"]["content"], json!("hello"));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn auth_disabled_key_is_403_and_empty_registry_requires_key() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_chat()))
        .mount(&server)
        .await;
    let (app, dir) = build_app(
        "auth-disabled",
        json!([provider_entry(&server.uri(), json!({}))]),
        json!([{"key": "sk-off", "enabled": false}]),
    )
    .await;
    let (status, body, _) = send_json_no_auth(
        app.clone(),
        "/v1/chat/completions",
        json!({"model": "prov-test/m-test"}),
        &[("Authorization", "Bearer sk-off")],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["message"], json!("API key is disabled."));

    // 禁用 key 占住注册表 → 匿名仍拒绝。
    let (status, _, _) = send_json_no_auth(
        app.clone(),
        "/v1/chat/completions",
        json!({"model": "prov-test/m-test"}),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // 管理 token 恒放行(internal)。
    let (status, _, _) = send_json_no_auth(
        app.clone(),
        "/v1/chat/completions",
        json!({"model": "prov-test/m-test"}),
        &[("Authorization", "Bearer test-token")],
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // dev-open 已移除:无任何 key 配置 + 匿名 → 401(不再放行)。
    let (app2, dir2) = build_app(
        "auth-empty-registry",
        json!([provider_entry(&server.uri(), json!({}))]),
        json!([]),
    )
    .await;
    let (status, body, _) = send_json_no_auth(
        app2,
        "/v1/chat/completions",
        json!({"model": "prov-test/m-test"}),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("Missing credentials")
    );
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(dir2);
}
