//! 跨协议语义与编码收敛

use super::*;

// ---------------------------------------------------------------------------
// P4:跨协议语义与编码收敛
// ---------------------------------------------------------------------------

/// F-09:异协议响应无法按目标语义解码时,必须返回显式 502 错误信封,
/// 不能把上游原对象(这里是 JSON 字符串)当成功响应。
#[tokio::test]
async fn non_stream_conversion_failure_is_explicit_502() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw("\"not-an-object\"", "application/json"),
        )
        .mount(&server)
        .await;
    let (app, dir) = build_app(
        "p4-conversion-failure",
        json!([provider_entry(
            &server.uri(),
            json!({"protocol":"anthropic"})
        )]),
        json!([]),
    )
    .await;
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .header(INTERNAL_AUTH.0, INTERNAL_AUTH.1)
        .body(Body::from(
            json!({"model":"prov-test/m-test","messages":[{"role":"user","content":"hi"}]})
                .to_string(),
        ))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["error"]["type"], json!("wrouter_error"), "{body}");
    assert!(body.get("error").is_some(), "不得返回上游原对象: {body}");
    let _ = std::fs::remove_dir_all(dir);
}

/// G-03:Anthropic 上游两个 content block → Responses 客户端必须得到
/// 两个独立 content_index 的 part,最终 output 保留两部分文本。
#[tokio::test]
async fn responses_stream_from_anthropic_keeps_multiple_parts() {
    let server = MockServer::start().await;
    let sse = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"m-test\",\"usage\":{\"input_tokens\":3}}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"one \"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"two\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":4}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream"))
        .mount(&server)
        .await;
    let (app, dir) = build_app(
        "p4-responses-multipart",
        json!([provider_entry(
            &server.uri(),
            json!({"protocol":"anthropic"})
        )]),
        json!([]),
    )
    .await;
    let request = Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header("content-type", "application/json")
        .header(INTERNAL_AUTH.0, INTERNAL_AUTH.1)
        .body(Body::from(
            json!({"model":"prov-test/m-test","input":"hi","stream":true}).to_string(),
        ))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    let values: Vec<Value> = text
        .split("\n\n")
        .filter_map(|block| {
            let data = block.lines().find_map(|line| line.strip_prefix("data: "))?;
            serde_json::from_str(data).ok()
        })
        .collect();
    let part_indices: Vec<u64> = values
        .iter()
        .filter(|value| value["type"] == json!("response.content_part.added"))
        .filter_map(|value| value["content_index"].as_u64())
        .collect();
    assert_eq!(part_indices, vec![0, 1], "{values:?}");
    let completed = values
        .iter()
        .find(|value| value["type"] == json!("response.completed"))
        .expect("completed");
    let content = completed["response"]["output"][0]["content"]
        .as_array()
        .expect("message content");
    assert_eq!(content.len(), 2, "{completed}");
    assert_eq!(content[0]["text"], json!("one "));
    assert_eq!(content[1]["text"], json!("two"));
    assert_eq!(completed["response"]["output_text"], json!("one two"));
    let _ = std::fs::remove_dir_all(dir);
}

/// G-01:Anthropic 工具参数分段到达, Gemini 客户端收到的 functionCall
/// 必须是完整对象,而不是开场空 `{}` 或忽略后续增量。
#[tokio::test]
async fn gemini_stream_from_anthropic_assembles_tool_arguments() {
    let server = MockServer::start().await;
    let sse = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"m-test\",\"usage\":{\"input_tokens\":2}}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"lookup\",\"input\":{}}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"q\\\":\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"1}\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":3}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream"))
        .mount(&server)
        .await;
    let (app, dir) = build_app(
        "p4-gemini-tool-args",
        json!([provider_entry(
            &server.uri(),
            json!({"protocol":"anthropic"})
        )]),
        json!([]),
    )
    .await;
    let request = Request::builder()
        .method("POST")
        .uri("/v1beta/models/prov-test/m-test:streamGenerateContent")
        .header("content-type", "application/json")
        .header(INTERNAL_AUTH.0, INTERNAL_AUTH.1)
        .body(Body::from(
            json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}]}).to_string(),
        ))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    let values: Vec<Value> = text
        .split("\n\n")
        .filter_map(|block| {
            let data = block.lines().find_map(|line| line.strip_prefix("data: "))?;
            serde_json::from_str(data).ok()
        })
        .collect();
    let call = values
        .iter()
        .find_map(|value| value.pointer("/candidates/0/content/parts/0/functionCall"))
        .expect("functionCall");
    assert_eq!(call["args"], json!({"q": 1}), "{values:?}");
    assert_eq!(call["id"], json!("toolu_1"));
    let _ = std::fs::remove_dir_all(dir);
}
