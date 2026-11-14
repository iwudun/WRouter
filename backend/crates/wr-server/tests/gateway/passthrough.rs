//! 透传

use super::*;

// ---------------------------------------------------------------------------
// 透传
// ---------------------------------------------------------------------------

#[tokio::test]
async fn non_stream_passthrough_returns_upstream_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_chat()))
        .expect(1)
        .mount(&server)
        .await;
    let (app, dir) = build_app(
        "passthrough-json",
        json!([provider_entry(&server.uri(), json!({}))]),
        json!([]),
    )
    .await;
    let request_body =
        json!({"model": "prov-test/m-test", "messages": [{"role": "user", "content": "hi"}]});
    let (status, body, headers) =
        send_json(app, "/v1/chat/completions", request_body.clone(), &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, ok_chat());
    // 路由可观测头。
    assert_eq!(headers.get("x-wr-routed-model").unwrap(), "m-test");
    assert_eq!(headers.get("x-wr-fallback-attempts").unwrap(), "1");
    // 请求体透传:model 被改写为前缀剥离后的规范名(2026-09-26 契约)。
    let received = server.received_requests().await.unwrap();
    assert_eq!(received.len(), 1);
    let sent: Value = serde_json::from_slice(&received[0].body).unwrap();
    let mut expected = request_body.clone();
    expected["model"] = json!("m-test");
    assert_eq!(sent, expected);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn streaming_native_raw_relays_upstream_bytes() {
    let server = MockServer::start().await;
    // P3-1:原生 Raw 执行器原字节透传上游 SSE(含上游自己的 [DONE]),
    // 不重建、不补 stop 终块、不吞 [DONE]。
    let sse = concat!(
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m-test\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"He\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m-test\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"llo\"},\"finish_reason\":null}]}\n\n",
        "data: [DONE]\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream"))
        .mount(&server)
        .await;
    let (app, dir) = build_app(
        "passthrough-sse",
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
            json!({"model": "prov-test/m-test", "messages": [], "stream": true}).to_string(),
        ))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "text/event-stream"
    );
    assert!(response.headers().get("x-wrouter-request-id").is_some());
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    // ByteExact:客户端正文与上游 SSE 逐字节一致(含紧凑键序与上游 [DONE])。
    assert_eq!(text, sse, "原生 Raw 必须原字节透传上游 SSE");
    assert_eq!(text.matches("data: [DONE]").count(), 1);
    // Raw 不合成成功终态:上游没给 stop,网关也不补。
    assert!(!text.contains(r#""finish_reason":"stop""#));
    let _ = std::fs::remove_dir_all(dir);
}

/// 2026-11-05 回归(Qoder CN `conflicting_finish_reason`):
/// 上游的结束原因是 `tool_calls` 时,下发给客户端的流里**只能有一个**
/// finish_reason,且必须是 `tool_calls` —— 旧实现的收尾块硬编码
/// `finish_reason: "stop"`,与上游的 `tool_calls` 构成两个不同值,
/// 严格客户端直接拒绝整条流(短文本对话因上游也是 `stop` 而侥幸正常)。
#[tokio::test]
async fn streaming_emits_single_finish_reason_matching_upstream() {
    let server = MockServer::start().await;
    let sse = concat!(
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m-test\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"Bash\",\"arguments\":\"{}\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m-test\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream"))
        .mount(&server)
        .await;
    let (app, dir) = build_app(
        "finish-reason-single",
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
            json!({"model": "prov-test/m-test", "messages": [], "stream": true}).to_string(),
        ))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    // P3-1 Raw:原字节透传 → 上游只有一个 tool_calls 终态,客户端就只见一个;
    // 网关不再补第二个 stop 终块(逐字节相等即证明)。
    assert_eq!(text, sse, "原生 Raw 必须原样下发上游单一终态:body={text}");
    assert_eq!(
        text.matches(r#""finish_reason":"tool_calls""#).count(),
        1,
        "上游的结束原因必须原样下发且只出现一次:body={text}"
    );
    assert_eq!(
        text.matches(r#""finish_reason":"stop""#).count(),
        0,
        "不得补第二个 stop 终块:body={text}"
    );
    assert_eq!(text.matches("data: [DONE]").count(), 1, "body={text}");
    let _ = std::fs::remove_dir_all(dir);
}
