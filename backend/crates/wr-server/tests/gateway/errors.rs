//! 错误路径

use super::*;

// ---------------------------------------------------------------------------
// 错误路径
// ---------------------------------------------------------------------------

#[tokio::test]
async fn streaming_eof_drains_unterminated_final_event() {
    let server = MockServer::start().await;
    let sse = concat!(
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m-test\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"tail\"},\"finish_reason\":null}]}\n\n",
        "data: [DONE]",
    );
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream"))
        .mount(&server)
        .await;
    let (app, dir) = build_app(
        "unterminated-final-sse",
        json!([provider_entry(&server.uri(), json!({}))]),
        json!([]),
    )
    .await;
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .header("authorization", "Bearer test-token")
        .body(Body::from(
            json!({"model":"prov-test/m-test","stream":true}).to_string(),
        ))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    // EOF 残片(无空行终止的 [DONE])仍原样收割下发;整段与上游逐字节一致。
    assert_eq!(text, sse, "EOF 残片必须原字节收割:body={text}");
    assert_eq!(text.matches("data: [DONE]").count(), 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn truncated_stream_does_not_synthesize_success_terminal() {
    let server = MockServer::start().await;
    let sse = concat!(
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m-test\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream"))
        .mount(&server)
        .await;
    let (app, dir) = build_app(
        "truncated-stream-no-terminal",
        json!([provider_entry(&server.uri(), json!({}))]),
        json!([]),
    )
    .await;
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .header("authorization", "Bearer test-token")
        .body(Body::from(
            json!({"model":"prov-test/m-test","stream":true}).to_string(),
        ))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    // 截断流:已收字节原样透传,不合成 stop/[DONE](Raw 不伪造正常终态)。
    assert_eq!(text, sse, "截断前字节必须原样保留:body={text}");
    assert!(!text.contains(r#""finish_reason":"stop""#));
    assert!(!text.contains("data: [DONE]"));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn upstream_500_with_retries_disabled_returns_error_envelope() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({
            "error": {"message": "upstream boom", "type": "upstream_error", "code": "500"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let (app, dir) = build_app(
        "upstream-500",
        json!([provider_entry(&server.uri(), json!({}))]),
        json!([]),
    )
    .await;
    let (status, body, _) = send_json(
        app,
        "/v1/chat/completions",
        json!({"model": "prov-test/m-test"}),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["error"]["message"], json!("upstream boom"));
    assert_eq!(body["error"]["type"], json!("upstream_error"));
    assert_eq!(body["error"]["code"], json!("500"));
    // max_retries=0 → 只打一次。
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn upstream_400_never_retries_even_with_budget() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(400).set_body_json(json!({
                "error": {"message": "bad request upstream", "type": "invalid_request_error", "code": "400"}
            })),
        )
        .expect(1)
        .mount(&server)
        .await;
    let (app, dir) = build_app(
        "upstream-400",
        json!([provider_entry(&server.uri(), json!({"max_retries": 3}))]),
        json!([]),
    )
    .await;
    let (status, body, _) = send_json(
        app,
        "/v1/chat/completions",
        json!({"model": "prov-test/m-test"}),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["message"], json!("bad request upstream"));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn connect_error_yields_502_wrouter_error() {
    // 指向不存在的本地端口 → 连接失败 → 502 wrouter_error。
    let (app, dir) = build_app(
        "connect-error",
        json!([provider_entry("http://127.0.0.1:9", json!({}))]),
        json!([]),
    )
    .await;
    let (status, body, _) = send_json(
        app,
        "/v1/chat/completions",
        json!({"model": "prov-test/m-test"}),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["error"]["type"], json!("wrouter_error"));
    assert!(body["error"]["request_id"].is_string());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn slow_non_stream_survives_small_read_timeout() {
    // 2026-10-08 语义修正:非流式「等响应头」≈ 完整生成时长,不再复用
    // 逐读 timeout_seconds —— 300ms 的慢响应在 0.05s 读超时下照常成功
    // (此前必 ReadTimeout → 重试耗尽 → 502,非流式失败率高的根因)。
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_millis(300))
                .set_body_json(ok_chat()),
        )
        .mount(&server)
        .await;
    // timeout_seconds=0.05(Python float() 接受数字;0.05 truthy)。
    let (app, dir) = build_app(
        "read-timeout",
        json!([provider_entry(
            &server.uri(),
            json!({"timeout_seconds": 0.05})
        )]),
        json!([]),
    )
    .await;
    let (status, body, _) = send_json(
        app,
        "/v1/chat/completions",
        json!({"model": "prov-test/m-test"}),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "慢非流式生成不应被逐读超时掐断");
    assert_eq!(body["object"], json!("chat.completion"));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn non_stream_header_timeout_override_yields_502() {
    // 显式 non_stream_timeout_seconds 覆盖生效:等响应头超限 → ReadTimeout
    // → 重试耗尽 → 502 wrouter_error。
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_millis(300))
                .set_body_json(ok_chat()),
        )
        .mount(&server)
        .await;
    let (app, dir) = build_app(
        "header-timeout",
        json!([provider_entry(
            &server.uri(),
            json!({"timeout_seconds": 30, "non_stream_timeout_seconds": 0.05})
        )]),
        json!([]),
    )
    .await;
    let (status, body, _) = send_json(
        app,
        "/v1/chat/completions",
        json!({"model": "prov-test/m-test"}),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["error"]["type"], json!("wrouter_error"));
    let _ = std::fs::remove_dir_all(dir);
}
