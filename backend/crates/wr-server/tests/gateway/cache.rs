//! 缓存重放与表示接线

use super::*;

// ---------------------------------------------------------------------------
// P5:缓存重放与表示接线
// ---------------------------------------------------------------------------

/// 流式请求写入的缓存必须是**客户端协议响应 JSON**,命中时经语义解码 +
/// 目标编码器重放;不能把原始 SSE 当 JSON 存储,也不能丢失缓存年龄/费用头。
#[tokio::test]
async fn streaming_cache_hit_replays_semantic_frames_with_headers() {
    let server = MockServer::start().await;
    let sse = concat!(
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m-test\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"hi\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m-test\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m-test\",\"choices\":[],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}\n\n",
        "data: [DONE]\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream"))
        .mount(&server)
        .await;
    let (app, dir) = build_app_with_savings(
        "stream-cache-replay",
        json!([provider_entry(&server.uri(), json!({}))]),
        json!([]),
        json!({"responseCache": {"enabled": true}}),
    )
    .await;
    let body = json!({
        "model": "prov-test/m-test",
        "messages": [{"role": "user", "content": "hi"}],
        "temperature": 0,
        "stream": true,
    });
    let (status, headers, first) = send_stream_body(app.clone(), body.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(first.contains("hi"), "{first}");
    assert!(headers.get("x-wr-cache").is_none(), "首次不应是缓存命中");

    let (status, headers, second) = send_stream_body(app, body).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers.get("x-wr-cache").unwrap(), "hit");
    assert!(headers.get("x-wr-cache-age-ms").is_some());
    assert_eq!(headers.get("x-wr-cost-saved").unwrap(), "0.000000");
    assert!(second.contains("hi"), "{second}");
    assert!(
        second.contains("\"finish_reason\": \"stop\""),
        "命中重放必须保留语义终态: {second}"
    );
    assert_eq!(second.matches("data: [DONE]").count(), 1);
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        1,
        "命中后不得再次触达上游"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// W2-5:缓存聚合预算超限时不写缓存(不返回截断结果),第二次请求
/// 仍回源;流式输出本身不受影响。
#[tokio::test]
async fn streaming_cache_budget_suppresses_oversized_aggregation() {
    let server = MockServer::start().await;
    let sse = concat!(
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m-test\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"hi\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m-test\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m-test\",\"choices\":[],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}\n\n",
        "data: [DONE]\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream"))
        .mount(&server)
        .await;
    let (app, dir) = build_app_with_savings(
        "stream-cache-budget",
        json!([provider_entry(&server.uri(), json!({}))]),
        json!([]),
        json!({"responseCache": {"enabled": true, "memoryMaxBytes": 128}}),
    )
    .await;
    let body = json!({
        "model": "prov-test/m-test",
        "messages": [{"role": "user", "content": "hi"}],
        "temperature": 0,
        "stream": true,
    });
    let (status, headers, first) = send_stream_body(app.clone(), body.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(first.contains("hi"), "非缓存输出不受预算影响: {first}");
    assert!(headers.get("x-wr-cache").is_none());

    let (status, headers, second) = send_stream_body(app, body).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers.get("x-wr-cache").is_none(),
        "预算超限不得写入缓存(更不能命中截断结果)"
    );
    assert!(second.contains("hi"), "{second}");
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        2,
        "未缓存时第二次请求必须回源"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// P5-5:请求体超过配置上限时在进入转发管线前拒绝,不会读入无界内存。
#[tokio::test]
async fn oversized_request_body_is_rejected_before_pipeline() {
    let server = MockServer::start().await;
    let (app, dir) = build_app_with_body_limit(
        "body-limit",
        json!([provider_entry(&server.uri(), json!({}))]),
        json!([]),
        128,
    )
    .await;
    let body = json!({
        "model": "prov-test/m-test",
        "messages": [{"role": "user", "content": "x".repeat(512)}],
    });
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .header(INTERNAL_AUTH.0, INTERNAL_AUTH.1)
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        0,
        "超限请求不得触达上游"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// P5(G-2):Responses 同序号同内容的重放帧不得再次进入转换泵。
#[tokio::test]
async fn responses_sequence_replay_is_not_encoded_twice() {
    let server = MockServer::start().await;
    let sse = concat!(
        "event: response.created\ndata: {\"type\":\"response.created\",\"sequence_number\":0,\"response\":{\"id\":\"resp_1\",\"status\":\"in_progress\"}}\n\n",
        "event: response.output_item.added\ndata: {\"type\":\"response.output_item.added\",\"sequence_number\":1,\"output_index\":0,\"item\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[]}}\n\n",
        "event: response.content_part.added\ndata: {\"type\":\"response.content_part.added\",\"sequence_number\":2,\"output_index\":0,\"content_index\":0,\"item_id\":\"msg_1\",\"part\":{\"type\":\"output_text\",\"text\":\"\"}}\n\n",
        "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"sequence_number\":3,\"output_index\":0,\"content_index\":0,\"item_id\":\"msg_1\",\"delta\":\"hi\"}\n\n",
        "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"sequence_number\":3,\"output_index\":0,\"content_index\":0,\"item_id\":\"msg_1\",\"delta\":\"hi\"}\n\n",
        "event: response.output_text.done\ndata: {\"type\":\"response.output_text.done\",\"sequence_number\":4,\"output_index\":0,\"content_index\":0,\"item_id\":\"msg_1\",\"text\":\"hi\"}\n\n",
        "event: response.output_item.done\ndata: {\"type\":\"response.output_item.done\",\"sequence_number\":5,\"output_index\":0,\"item\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"status\":\"completed\",\"content\":[{\"type\":\"output_text\",\"text\":\"hi\",\"annotations\":[]}]}}\n\n",
        "event: response.completed\ndata: {\"type\":\"response.completed\",\"sequence_number\":6,\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"output\":[{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"status\":\"completed\",\"content\":[{\"type\":\"output_text\",\"text\":\"hi\",\"annotations\":[]}]}]}}\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream"))
        .mount(&server)
        .await;
    let (app, dir) = build_app(
        "sequence-replay",
        json!([provider_entry(
            &server.uri(),
            json!({"type": "openai_responses"})
        )]),
        json!([]),
    )
    .await;
    let (status, _headers, text) = send_stream_body(
        app,
        json!({
            "model": "prov-test/m-test",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        text.matches(r#""content": "hi""#).count(),
        1,
        "同序号重放只能编码一次: {text}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// P5-2:非流缓存命中重放缓存的客户端 JSON(不重新生成),并报告
/// 缓存年龄与省下的费用。
#[tokio::test]
async fn non_stream_cache_hit_reports_age_and_cost_saved() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_chat()))
        .mount(&server)
        .await;
    let (app, dir) = build_app_with_savings(
        "json-cache-replay",
        json!([provider_entry(
            &server.uri(),
            json!({"billing": {"input_per_million": 1.0, "output_per_million": 1.0}})
        )]),
        json!([]),
        json!({"responseCache": {"enabled": true}}),
    )
    .await;
    let body = json!({
        "model": "prov-test/m-test",
        "messages": [{"role": "user", "content": "hi"}],
        "temperature": 0,
    });
    let (status, first, headers) =
        send_json(app.clone(), "/v1/chat/completions", body.clone(), &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers.get("x-wr-cache").is_none());
    assert_eq!(first["choices"][0]["message"]["content"], json!("hello"));

    let (status, second, headers) = send_json(app, "/v1/chat/completions", body, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(second, first, "命中重放缓存中的客户端 JSON");
    assert_eq!(headers.get("x-wr-cache").unwrap(), "hit");
    assert!(headers.get("x-wr-cache-age-ms").is_some());
    assert_eq!(
        headers.get("x-wr-cost-saved").unwrap(),
        "0.000002",
        "1 input + 1 output token × $1/M"
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(dir);
}

/// P5-1:A6(流式客户端 / 非流上游)也必须先解码到语义响应,再经目标
/// 编码器成帧;thinking/工具调用/结束原因不能因 JSON 合成而丢失。
#[tokio::test]
async fn streaming_a6_from_anthropic_json_keeps_reasoning_and_tools() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_a6",
            "type": "message",
            "role": "assistant",
            "model": "m-test",
            "content": [
                {"type": "thinking", "thinking": "think", "signature": "sig"},
                {"type": "text", "text": "hello"},
                {"type": "tool_use", "id": "toolu_1", "name": "lookup", "input": {"q": 1}},
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 2, "output_tokens": 3},
        })))
        .mount(&server)
        .await;
    let (app, dir) = build_app(
        "stream-a6-anthropic",
        json!([provider_entry(
            &server.uri(),
            json!({"protocol": "anthropic"})
        )]),
        json!([]),
    )
    .await;
    let (status, headers, text) = send_stream_body(
        app,
        json!({
            "model": "prov-test/m-test",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers.get("content-type").unwrap(), "text/event-stream");
    assert!(text.contains("\"reasoning_content\": \"think\""), "{text}");
    assert!(text.contains("\"content\": \"hello\""), "{text}");
    assert!(text.contains("\"name\": \"lookup\""), "{text}");
    assert!(text.contains("\"arguments\": \"{\\\"q\\\": 1}\""), "{text}");
    assert!(text.contains("\"finish_reason\": \"tool_calls\""), "{text}");
    assert_eq!(text.matches("data: [DONE]").count(), 1);
    let _ = std::fs::remove_dir_all(dir);
}

/// P5-2:非流路径同样不能在已发出有状态引用后换供应商/账号。
#[tokio::test]
async fn non_stream_stateful_reference_blocks_cross_candidate_switch() {
    let first = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(500).set_body_json(json!({"error": {"message": "boom"}})),
        )
        .mount(&first)
        .await;
    let second = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_chat()))
        .mount(&second)
        .await;
    let (app, dir) = build_app_with_groups(
        "stateful-nonstream",
        json!([
            group_provider("pa", &first.uri(), &["m-a"]),
            group_provider("pb", &second.uri(), &["m-b"]),
        ]),
        json!([]),
        json!([{
            "id": "g1",
            "name": "agg",
            "strategy": "first",
            "targets": [
                {"providerId": "pa", "model": "m-a"},
                {"providerId": "pb", "model": "m-b"},
            ],
        }]),
    )
    .await;
    let (status, body, _) = send_json(
        app,
        "/v1/chat/completions",
        json!({
            "model": "wrouter/agg",
            "messages": [{"role": "user", "content": "hi"}],
            "previous_response_id": "resp_bound_to_pa",
        }),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["error"]["message"], json!("boom"));
    assert_eq!(first.received_requests().await.unwrap().len(), 1);
    assert_eq!(
        second.received_requests().await.unwrap().len(),
        0,
        "有状态引用已出站后不得换供应商重试"
    );
    let _ = std::fs::remove_dir_all(dir);
}
