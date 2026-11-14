//! 流式的 per-key 模型白名单(B-4)与 precheck 限流(A-1)

use super::*;

// ---------------------------------------------------------------------------
// 20261104_01 阶段二:流式的 per-key 模型白名单(B-4)与 precheck 限流(A-1)
// ---------------------------------------------------------------------------

/// B-4:流式请求的 per-key 模型白名单越权 → 提交前真实 HTTP 403,
/// 且**不触达上游**(P2 起不再伪装成 200 + SSE 信封)。
/// 非流式早有这条判定(`api_key_model_allowlist_rejects_foreign_model`),
/// 流式此前完全没接 → 白名单可被流式请求绕过。
#[tokio::test]
async fn streaming_api_key_model_allowlist_rejects_foreign_model() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(ok_sse(), "text/event-stream"))
        .mount(&server)
        .await;
    let (app, dir) = build_app(
        "stream-allowlist-reject",
        json!([provider_entry(
            &server.uri(),
            json!({"models": ["m-test", "m-extra"]})
        )]),
        json!([{"id": "k1", "key": "sk-user-1", "enabled": true, "models": ["prov-test/m-test"]}]),
    )
    .await;
    let (status, text) = send_stream(app, "prov-test/m-extra", "sk-user-1").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "提交前拒绝应返回真实状态");
    let body = error_body(&text);
    assert_eq!(body["error"]["type"], json!("model_forbidden"));
    assert_eq!(body["error"]["code"], json!("model_forbidden"));
    assert_eq!(
        body["error"]["message"],
        json!("Model 'prov-test/m-extra' is not allowed for this API key.")
    );
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        0,
        "白名单拒绝必须早于打上游"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// B-4 另一侧:白名单内的模型照常转发(不误伤)。
#[tokio::test]
async fn streaming_api_key_model_allowlist_allows_listed_model() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(ok_sse(), "text/event-stream"))
        .mount(&server)
        .await;
    let (app, dir) = build_app(
        "stream-allowlist-allow",
        json!([provider_entry(&server.uri(), json!({}))]),
        json!([{"id": "k1", "key": "sk-user-1", "enabled": true, "models": ["prov-test/m-test"]}]),
    )
    .await;
    let (status, text) = send_stream(app, "prov-test/m-test", "sk-user-1").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        text.contains(r#""content":"hi""#),
        "白名单内的模型应正常出帧: {text}"
    );
    assert!(
        !text.contains("model_forbidden"),
        "白名单内的模型不该被拒: {text}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// A-1:流式请求的 per-key 限额生效 —— 配额用尽后收到 SSE 信封内的 429,
/// 且**不再触达上游**。窗口用 24h(`maxRequests` + `windowMs`)而不是默认
/// 60s:固定窗口按绝对时间栅格换桶,60s 窗口在跨分钟边界的用例里会偶发
/// 计数重置(1/60 的概率)→ 24h 窗口把该概率压到 1/86400。
#[tokio::test]
async fn streaming_precheck_rate_limit_rejects_after_quota_exhausted() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(ok_sse(), "text/event-stream"))
        .mount(&server)
        .await;
    let (app, dir) = build_app(
        "stream-precheck-limit",
        json!([provider_entry(&server.uri(), json!({}))]),
        json!([{
            "id": "k1", "key": "sk-user-1", "enabled": true,
            "limits": {"maxRequests": 1, "windowMs": 86400000}
        }]),
    )
    .await;
    let (status, text) = send_stream(app.clone(), "prov-test/m-test", "sk-user-1").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        text.contains(r#""content":"hi""#),
        "第一请求应在配额内放行: {text}"
    );
    let (status, text) = send_stream(app, "prov-test/m-test", "sk-user-1").await;
    assert_eq!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "提交前限额拒绝返回真实状态"
    );
    let body = error_body(&text);
    assert_eq!(body["error"]["type"], json!("rate_limit_exceeded"));
    assert_eq!(body["error"]["code"], json!("rate_limit_exceeded"));
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Rate limit exceeded"),
        "错误文案应说明限额被越过: {body}"
    );
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        1,
        "被限流拒绝的请求不得触达上游"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// 顺序断言:per-key 模型白名单判定**先于**限额判定(与 JSON 路径同序),
/// 且白名单拒绝**不消耗**限额额度(被拒时压根没走到 precheck)。
#[tokio::test]
async fn streaming_allowlist_check_precedes_rate_limit_check() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(ok_sse(), "text/event-stream"))
        .mount(&server)
        .await;
    let (app, dir) = build_app(
        "stream-gate-order",
        json!([provider_entry(
            &server.uri(),
            json!({"models": ["m-test", "m-extra"]})
        )]),
        json!([{
            "id": "k1", "key": "sk-user-1", "enabled": true,
            "models": ["prov-test/m-test"],
            "limits": {"maxRequests": 1, "windowMs": 86400000}
        }]),
    )
    .await;
    // ① 越权模型:即便额度只剩 1,报的也必须是白名单而非限额。
    let (_, text) = send_stream(app.clone(), "prov-test/m-extra", "sk-user-1").await;
    let body = error_body(&text);
    assert_eq!(
        body["error"]["type"],
        json!("model_forbidden"),
        "白名单判定必须先于限额判定: {body}"
    );
    // ② 白名单内模型:额度未被 ① 消耗(白名单拒绝在 precheck 之前返回)。
    let (_, text) = send_stream(app.clone(), "prov-test/m-test", "sk-user-1").await;
    assert!(
        text.contains(r#""content":"hi""#),
        "白名单拒绝不应消耗限额额度: {text}"
    );
    // ③ 额度用尽 → 这次才轮限额说话。
    let (_, text) = send_stream(app, "prov-test/m-test", "sk-user-1").await;
    let body = error_body(&text);
    assert_eq!(
        body["error"]["type"],
        json!("rate_limit_exceeded"),
        "额度用尽后应被限额拒绝: {body}"
    );
    let _ = std::fs::remove_dir_all(dir);
}
