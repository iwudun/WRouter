//! 原生三面 ByteExact 与精确修正

use super::*;

// ---------------------------------------------------------------------------
// P3:原生三面 ByteExact 与精确修正
// ---------------------------------------------------------------------------

/// P3-1 非流式:原生 Raw 成功响应原字节透传 —— 空白、键序、数值词法
/// 全部保留,不经 Value 重序列化。
#[tokio::test]
async fn non_stream_native_raw_preserves_upstream_bytes() {
    let server = MockServer::start().await;
    let upstream_body = "{\n  \"usage\" : {\"prompt_tokens\":1,\"completion_tokens\":2,\"total_tokens\":3},\n  \"model\":\"m-test\", \"id\":\"up-ok\", \"choices\":[{\"index\":0,\"message\":{\"role\":\"assistant\",\"content\":\"hello\"},\"finish_reason\":\"stop\"}], \"temperature\":1e0\n}\n";
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(upstream_body, "application/json"))
        .mount(&server)
        .await;
    let (app, dir) = build_app(
        "nonstream-raw-bytes",
        json!([provider_entry(&server.uri(), json!({}))]),
        json!([]),
    )
    .await;
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .header(INTERNAL_AUTH.0, INTERNAL_AUTH.1)
        .body(Body::from(
            json!({"model":"prov-test/m-test","messages":[]}).to_string(),
        ))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        bytes.as_ref(),
        upstream_body.as_bytes(),
        "非流式原生 Raw 必须原字节透传上游正文"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// P3-1 请求面:剥前缀只定点替换 model 值 span,其余字节与客户端原文
/// 逐字节一致(空白/键序/数值词法不变)。
#[tokio::test]
async fn native_request_patches_only_model_span() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_chat()))
        .mount(&server)
        .await;
    let (app, dir) = build_app(
        "request-model-span",
        json!([provider_entry(&server.uri(), json!({}))]),
        json!([]),
    )
    .await;
    let client_raw = "{ \"model\" : \"prov-test/m-test\" , \"messages\":[{\"role\":\"user\",\"content\":\"hi\"}] , \"temperature\":0.70 , \"stream\":false }\n";
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .header(INTERNAL_AUTH.0, INTERNAL_AUTH.1)
        .body(Body::from(client_raw))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let _ = response.into_body().collect().await.unwrap();
    let received = server.received_requests().await.unwrap();
    assert_eq!(received.len(), 1);
    let expected = "{ \"model\" : \"m-test\" , \"messages\":[{\"role\":\"user\",\"content\":\"hi\"}] , \"temperature\":0.70 , \"stream\":false }\n";
    assert_eq!(
        received[0].body,
        expected.as_bytes(),
        "请求面除 model 值外必须保留客户端原字节"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// P3-1/P3-4 流式:注释、id、retry、未知事件与 CRLF 行尾全部原样保留
/// (未知事件不是解析错误,Raw 不丢帧)。
#[tokio::test]
async fn streaming_native_raw_preserves_sse_extensions() {
    let server = MockServer::start().await;
    let sse = ": keep-alive comment\r\nid: 42\r\nretry: 3000\r\nevent: message\r\ndata: {\"id\":\"c1\",\"model\":\"m-test\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\r\n\r\nevent: vendor.unknown\r\ndata: {\"opaque\":true}\r\n\r\ndata: [DONE]\r\n\r\n";
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream"))
        .mount(&server)
        .await;
    let (app, dir) = build_app(
        "raw-sse-extensions",
        json!([provider_entry(&server.uri(), json!({}))]),
        json!([]),
    )
    .await;
    let (status, text) = send_stream_internal(app, "prov-test/m-test").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(text, sse, "注释/id/retry/未知事件/CRLF 必须原字节保留");
    let _ = std::fs::remove_dir_all(dir);
}

/// P3-5 Messages 原生面:Anthropic → Anthropic 流式原字节透传。
#[tokio::test]
async fn streaming_native_anthropic_raw_preserves_bytes() {
    let server = MockServer::start().await;
    let sse = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"m-test\",\"usage\":{\"input_tokens\":5}}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream"))
        .mount(&server)
        .await;
    let (app, dir) = build_app(
        "anthropic-raw-bytes",
        json!([provider_entry(
            &server.uri(),
            json!({"protocol":"anthropic"})
        )]),
        json!([]),
    )
    .await;
    let (status, text) = send_stream_internal_path(app, "/v1/messages", "prov-test/m-test").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(text, sse, "Messages 原生流必须原字节透传");
    let _ = std::fs::remove_dir_all(dir);
}

/// P3-5 Responses 原生面:合法终态快照(response.completed)原样保留,
/// 不重复追加全文、不重建信封。
#[tokio::test]
async fn streaming_native_responses_raw_preserves_bytes() {
    let server = MockServer::start().await;
    let sse = concat!(
        "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"model\":\"m-test\",\"status\":\"in_progress\"}}\n\n",
        "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"hi\"}\n\n",
        "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"model\":\"m-test\",\"status\":\"completed\",\"usage\":{\"input_tokens\":3,\"output_tokens\":2,\"total_tokens\":5}}}\n\n",
    );
    // 端点由上游形态决定(base + /v1/responses);不限定路径 matcher,
    // 与 P2 Responses 信封测试同口径。
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream"))
        .mount(&server)
        .await;
    let (app, dir) = build_app(
        "responses-raw-bytes",
        json!([provider_entry(
            &server.uri(),
            json!({"type":"openai_responses"})
        )]),
        json!([]),
    )
    .await;
    let (status, text) = send_stream_internal_path(app, "/v1/responses", "prov-test/m-test").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(text, sse, "Responses 原生流(含合法快照)必须原字节透传");
    let _ = std::fs::remove_dir_all(dir);
}

/// P3-5 Gemini 原生面:streamGenerateContent 的 SSE 原字节透传
/// (含 finishReason 终态;Gemini 由末候选结束帧收口)。
#[tokio::test]
async fn streaming_native_gemini_raw_preserves_bytes() {
    let server = MockServer::start().await;
    let sse = concat!(
        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hi\"}],\"role\":\"model\"},\"index\":0}]}\n\n",
        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"!\"}],\"role\":\"model\"},\"index\":0,\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":3,\"candidatesTokenCount\":2,\"totalTokenCount\":5}}\n\n",
    );
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream"))
        .mount(&server)
        .await;
    let (app, dir) = build_app(
        "gemini-raw-bytes",
        json!([provider_entry(&server.uri(), json!({"protocol":"gemini"}))]),
        json!([]),
    )
    .await;
    let request = Request::builder()
        .method("POST")
        .uri("/v1beta/models/prov-test/m-test:streamGenerateContent")
        .header("content-type", "application/json")
        .header(INTERNAL_AUTH.0, INTERNAL_AUTH.1)
        .body(Body::from(
            json!({"contents":[{"parts":[{"text":"hi"}]}]}).to_string(),
        ))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert_eq!(text, sse, "Gemini 原生流必须原字节透传");
    let _ = std::fs::remove_dir_all(dir);
}

/// W2-3:Interactions 没有流式契约;`stream=true` 在规划层被拒绝,
/// 不触达上游、不把 Gemini `candidates[]` 流冒充成 interactions 表示。
#[tokio::test]
async fn interactions_streaming_is_rejected_before_upstream() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("{}", "application/json"))
        .mount(&server)
        .await;
    let (app, dir) = build_app(
        "interactions-stream-reject",
        json!([provider_entry(&server.uri(), json!({"protocol":"gemini"}))]),
        json!([]),
    )
    .await;
    let request = Request::builder()
        .method("POST")
        .uri("/v1/interactions")
        .header("content-type", "application/json")
        .header(INTERNAL_AUTH.0, INTERNAL_AUTH.1)
        .body(Body::from(
            json!({
                "model": "prov-test/m-test",
                "input": "hi",
                "stream": true,
            })
            .to_string(),
        ))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("gemini_interactions"), "{text}");
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        0,
        "规划拒绝必须发生在上游调用前"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// P3-5 Embeddings 原生面:非流 JSON 原字节透传(当前唯一支持的
/// OpenAI embeddings 组合;到 Anthropic/Gemini 仍由规划器拒绝)。
#[tokio::test]
async fn non_stream_embeddings_native_raw_preserves_bytes() {
    let server = MockServer::start().await;
    let body = "{\"object\":\"list\",\"data\":[{\"object\":\"embedding\",\"index\":0,\"embedding\":[0.1,0.2]}],\"model\":\"m-test\",\"usage\":{\"prompt_tokens\":2,\"total_tokens\":2}}";
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
        .mount(&server)
        .await;
    let (app, dir) = build_app(
        "embeddings-raw-bytes",
        json!([provider_entry(&server.uri(), json!({}))]),
        json!([]),
    )
    .await;
    let request = Request::builder()
        .method("POST")
        .uri("/v1/embeddings")
        .header("content-type", "application/json")
        .header(INTERNAL_AUTH.0, INTERNAL_AUTH.1)
        .body(Body::from(
            json!({"model":"prov-test/m-test","input":"hi"}).to_string(),
        ))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        bytes.as_ref(),
        body.as_bytes(),
        "Embeddings 原生非流响应必须原字节透传"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// P3-4 R8:聚合别名只回写协议 model 路径;工具参数里的业务 `model`
/// 字段不被递归污染(F-08 反例)。
#[tokio::test]
async fn aggregate_alias_rewrites_protocol_model_not_business_fields() {
    let server = MockServer::start().await;
    let sse = concat!(
        "data: {\"id\":\"c1\",\"model\":\"m-a\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"f\",\"arguments\":\"{\\\"model\\\":\\\"business\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
        "data: [DONE]\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream"))
        .mount(&server)
        .await;
    let (app, dir) = build_app_with_groups(
        "aggregate-r8-scope",
        json!([group_provider("pa", &server.uri(), &["m-a"])]),
        json!([]),
        json!([{
            "id": "g1",
            "name": "agg",
            "strategy": "first",
            "targets": [{"providerId": "pa", "model": "m-a"}],
        }]),
    )
    .await;
    let (status, text) = send_stream_internal(app, "wrouter/agg").await;
    assert_eq!(status, StatusCode::OK);
    let first_block = text.split("\n\n").next().unwrap();
    let data = first_block.strip_prefix("data: ").unwrap();
    let value: Value = serde_json::from_str(data).unwrap();
    assert_eq!(value["model"], "agg", "协议 model 路径应回写为聚合名");
    let args = value["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"]
        .as_str()
        .unwrap();
    assert_eq!(
        args, "{\"model\":\"business\"}",
        "工具参数里的业务 model 字段不得被别名回写污染"
    );
    let _ = std::fs::remove_dir_all(dir);
}
