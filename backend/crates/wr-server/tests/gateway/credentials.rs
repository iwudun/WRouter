//! 账号注入 + API key 模型白名单(R5 切片 F;14a §6-F)

use super::*;

// ---------------------------------------------------------------------------
// 账号注入 + API key 模型白名单(R5 切片 F;14a §6-F)
// ---------------------------------------------------------------------------

fn seed_account_secret(data_dir: &std::path::Path, provider: &str, account: &str, value: &str) {
    let conn = Connection::open(data_dir.join("config.sqlite")).unwrap();
    conn.execute(
        "INSERT INTO secrets(name, value) VALUES(?1, ?2)          ON CONFLICT(name) DO UPDATE SET value=excluded.value",
        [
            format!("provider:{provider}:account:{account}:api_key"),
            value.to_owned(),
        ],
    )
    .unwrap();
}

#[tokio::test]
async fn select_provider_injects_first_enabled_account_credential() {
    let server = MockServer::start().await;
    // 上游只接受 Bearer sk-acc1:注入成功才可能命中此 mock。
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer sk-acc1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_chat()))
        .mount(&server)
        .await;
    let (app, dir) = build_app(
        "acct-inject",
        json!([provider_entry(
            &server.uri(),
            json!({
                "accounts": [
                    {"id": "a1", "label": "A1", "enabled": true},
                    {"id": "a2", "label": "A2", "enabled": true},
                ],
            })
        )]),
        json!([]),
    )
    .await;
    seed_account_secret(&dir, "prov-test", "a1", "sk-acc1");
    seed_account_secret(&dir, "prov-test", "a2", "sk-acc2");
    let (status, _body, _) = send_json(
        app,
        "/v1/chat/completions",
        json!({"model": "prov-test/m-test"}),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn account_secret_empty_keeps_provider_key() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer sk-prov"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_chat()))
        .mount(&server)
        .await;
    let (app, dir) = build_app(
        "acct-empty-keep",
        json!([provider_entry(
            &server.uri(),
            json!({
                "api_key": "sk-prov",
                "accounts": [{"id": "a1", "label": "A1", "enabled": true}],
            })
        )]),
        json!([]),
    )
    .await;
    // a1 无 secrets 行:account_secret 为空串 → 不得覆盖 provider 级密钥。
    let (status, _body, _) = send_json(
        app,
        "/v1/chat/completions",
        json!({"model": "prov-test/m-test"}),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn api_key_model_allowlist_allows_listed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_chat()))
        .mount(&server)
        .await;
    let (app, dir) = build_app(
        "allowlist-allow",
        json!([provider_entry(&server.uri(), json!({}))]),
        json!([{"id": "k1", "key": "sk-user-1", "enabled": true, "models": ["prov-test/m-test"]}]),
    )
    .await;
    let (status, body, _) = send_json(
        app,
        "/v1/chat/completions",
        json!({"model": "prov-test/m-test"}),
        &[("x-api-key", "sk-user-1")],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["choices"][0]["message"]["content"], json!("hello"));
    let _ = std::fs::remove_dir_all(dir);
}

/// W3-7:旧版裸模型名/裸聚合名白名单条目不再兜底匹配,只认完整选择器。
#[tokio::test]
async fn api_key_model_allowlist_legacy_bare_entries_no_longer_match() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_chat()))
        .mount(&server)
        .await;
    let (app, dir) = build_app(
        "allowlist-legacy",
        json!([provider_entry(&server.uri(), json!({}))]),
        json!([{"id": "k1", "key": "sk-user-1", "enabled": true, "models": ["m-test"]}]),
    )
    .await;
    let (status, body, _) = send_json(
        app,
        "/v1/chat/completions",
        json!({"model": "prov-test/m-test"}),
        &[("x-api-key", "sk-user-1")],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["type"], "model_forbidden");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn api_key_model_allowlist_rejects_foreign_model() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_chat()))
        .mount(&server)
        .await;
    let (app, dir) = build_app(
        "allowlist-reject",
        // provider 额外服务 m-extra:请求可解析,但不在 k1 白名单内。
        json!([provider_entry(
            &server.uri(),
            json!({"models": ["m-test", "m-extra"]})
        )]),
        json!([{"id": "k1", "key": "sk-user-1", "enabled": true, "models": ["prov-test/m-test"]}]),
    )
    .await;
    let (status, body, _) = send_json(
        app,
        "/v1/chat/completions",
        json!({"model": "prov-test/m-extra"}),
        &[("x-api-key", "sk-user-1")],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["type"], "model_forbidden");
    assert_eq!(
        body["error"]["message"],
        "Model 'prov-test/m-extra' is not allowed for this API key."
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn api_key_model_allowlist_ignores_internal_identity() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_chat()))
        .mount(&server)
        .await;
    let (app, dir) = build_app(
        "allowlist-internal",
        // provider 可服务 m-extra,但 k1 白名单只列 m-test:internal 不受白名单约束。
        json!([provider_entry(
            &server.uri(),
            json!({"models": ["m-test", "m-extra"]})
        )]),
        json!([{"id": "k1", "key": "sk-user-1", "enabled": true, "models": ["prov-test/m-test"]}]),
    )
    .await;
    // 管理 token → internal 身份:白名单不适用,未列模型照常转发。
    let (status, body, _) = send_json(
        app,
        "/v1/chat/completions",
        json!({"model": "prov-test/m-extra"}),
        &[("Authorization", "Bearer test-token")],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["choices"][0]["message"]["content"], json!("hello"));
    let _ = std::fs::remove_dir_all(dir);
}
