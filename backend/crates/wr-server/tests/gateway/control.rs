//! 提交边界与源事实控制链

use super::*;

// ---------------------------------------------------------------------------
// P2:提交边界与源事实控制链
// ---------------------------------------------------------------------------

/// 已提交内容后上游断流:不得无痕切换到下一个候选;客户端只看到
/// 已提交的部分与真实的不完整收口。
#[tokio::test]
async fn streaming_committed_content_blocks_candidate_switch() {
    let first = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            concat!(
                "data: {\"id\":\"c1\",\"model\":\"m-a\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n",
            ),
            "text/event-stream",
        ))
        .mount(&first)
        .await;
    let second = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(ok_sse(), "text/event-stream"))
        .mount(&second)
        .await;
    let (app, dir) = build_app_with_groups(
        "stream-commit-gate",
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
    let (status, text) = send_stream_internal(app, "wrouter/agg").await;
    assert_eq!(status, StatusCode::OK);
    assert!(text.contains("partial"), "已提交内容必须保留: {text}");
    assert!(
        !text.contains(r#""content": "hi""#),
        "已提交后不得无痕切到第二个候选: {text}"
    );
    assert!(
        !text.contains("data: [DONE]"),
        "断流不得合成成功终态: {text}"
    );
    assert_eq!(first.received_requests().await.unwrap().len(), 1);
    assert_eq!(
        second.received_requests().await.unwrap().len(),
        0,
        "已提交后不得触达下一个候选"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// 提交前失败仍可正常切换候选(提交门只阻止提交后的替换)。
#[tokio::test]
async fn streaming_pre_commit_failure_still_fails_over_candidates() {
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
        .respond_with(ResponseTemplate::new(200).set_body_raw(ok_sse(), "text/event-stream"))
        .mount(&second)
        .await;
    let (app, dir) = build_app_with_groups(
        "stream-precommit-failover",
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
    let (status, text) = send_stream_internal(app, "wrouter/agg").await;
    assert_eq!(status, StatusCode::OK);
    assert!(text.contains(r#""content": "hi""#), "{text}");
    assert_eq!(first.received_requests().await.unwrap().len(), 1);
    assert_eq!(second.received_requests().await.unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(dir);
}

/// 尾随 usage(OpenAI include_usage 尾块)必须独立进入观察/计量,
/// 并出现在客户端终帧与调用日志中。
#[tokio::test]
async fn streaming_trailing_usage_is_observed_and_logged() {
    let server = MockServer::start().await;
    let sse = concat!(
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m-test\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"hi\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m-test\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m-test\",\"choices\":[],\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":7,\"total_tokens\":18,\"prompt_tokens_details\":{\"cached_tokens\":3}}}\n\n",
        "data: [DONE]\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream"))
        .mount(&server)
        .await;
    let (app, dir) = build_app(
        "stream-trailing-usage",
        json!([provider_entry(&server.uri(), json!({}))]),
        json!([{"key": "sk-test-1", "enabled": true, "name": "t"}]),
    )
    .await;
    let (status, text) = send_stream(app, "prov-test/m-test", "sk-test-1").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        text.contains(r#""total_tokens":18"#),
        "终帧应带尾随 usage(原生 Raw 原字节透传): {text}"
    );

    use wr_app::ports::RequestLogStore as _;
    let store = wr_infra::sqlite::SqliteStore::open(&dir).await.unwrap();
    let mut row: Option<Value> = None;
    for _ in 0..60 {
        if let Some(found) = store
            .list_request_logs(10)
            .await
            .unwrap()
            .into_iter()
            .next()
        {
            row = Some(found);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let row = row.expect("终态日志行必须落库");
    assert_eq!(row["input_tokens"], json!(11));
    assert_eq!(row["output_tokens"], json!(7));
    assert_eq!(row["total_tokens"], json!(18));
    store.close().await;
    let _ = std::fs::remove_dir_all(dir);
}

/// NDJSON 响应按其逐行格式分帧,不能套用 SSE 的 `data:` 语法。
#[tokio::test]
async fn streaming_ndjson_response_is_framed_as_ndjson() {
    let server = MockServer::start().await;
    let ndjson = concat!(
        "{\"id\":\"c1\",\"model\":\"m-test\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"nd\"},\"finish_reason\":null}]}\n",
        "{\"id\":\"c1\",\"model\":\"m-test\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"json\"},\"finish_reason\":null}]}\n",
        "[DONE]\n",
    );
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(ndjson, "application/x-ndjson"))
        .mount(&server)
        .await;
    let (app, dir) = build_app(
        "stream-ndjson",
        json!([provider_entry(&server.uri(), json!({}))]),
        json!([]),
    )
    .await;
    let (status, text) = send_stream_internal(app, "prov-test/m-test").await;
    assert_eq!(status, StatusCode::OK);
    assert!(text.contains(r#""content": "nd""#), "{text}");
    assert!(text.contains(r#""content": "json""#), "{text}");
    assert_eq!(text.matches("data: [DONE]").count(), 1);
    let _ = std::fs::remove_dir_all(dir);
}

/// 未知响应表示提交前显式失败,不默认为 JSON 合成/纯透传。
#[tokio::test]
async fn streaming_unknown_content_type_rejects_before_commit() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("not a stream", "text/plain"))
        .mount(&server)
        .await;
    let (app, dir) = build_app(
        "stream-unknown-type",
        json!([provider_entry(&server.uri(), json!({}))]),
        json!([]),
    )
    .await;
    let (status, text) = send_stream_internal(app, "prov-test/m-test").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    let body = error_body(&text);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("Content-Type"),
        "{body}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// 已提交响应信封(Responses `response.created`)但尚无正文时断流:
/// 仍不得无痕切换到下一个候选(P2-5 提交门)。
#[tokio::test]
async fn streaming_responses_envelope_commit_blocks_candidate_switch() {
    let first = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"model\":\"m-a\",\"status\":\"in_progress\"}}\n\n",
            "text/event-stream",
        ))
        .mount(&first)
        .await;
    let second = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_2\",\"model\":\"m-b\",\"status\":\"in_progress\"}}\n\n",
            "text/event-stream",
        ))
        .mount(&second)
        .await;
    let mut pa = group_provider("pa", &first.uri(), &["m-a"]);
    pa["protocol_style"] = json!("openai_responses");
    let mut pb = group_provider("pb", &second.uri(), &["m-b"]);
    pb["protocol_style"] = json!("openai_responses");
    let (app, dir) = build_app_with_groups(
        "stream-envelope-commit",
        json!([pa, pb]),
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
    let (status, text) = send_stream_internal_path(app, "/v1/responses", "wrouter/agg").await;
    assert_eq!(status, StatusCode::OK, "body={text}");
    assert!(text.contains("response.created"), "{text}");
    assert!(!text.contains("resp_2"), "已提交信封后不得切换响应: {text}");
    assert_eq!(second.received_requests().await.unwrap().len(), 0);
    let _ = std::fs::remove_dir_all(dir);
}

/// 客户端在首帧后断开:管线下一次入队失败即收尾并记录 499,
/// 不继续无界生成(通道 drop 是取消信号)。
#[tokio::test]
async fn streaming_client_disconnect_after_commit_records_499() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        let Ok((mut socket, _)) = listener.accept() else {
            return;
        };
        let mut buf = [0u8; 8192];
        let mut head = Vec::new();
        while !head.windows(4).any(|window| window == b"\r\n\r\n") {
            match socket.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => head.extend_from_slice(&buf[..n]),
            }
        }
        if socket
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n",
            )
            .is_err()
        {
            return;
        }
        for index in 0..50 {
            let payload = format!(
                "data: {{\"id\":\"c1\",\"model\":\"m-test\",\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"{index}\"}},\"finish_reason\":null}}]}}\n\n"
            );
            if socket
                .write_all(format!("{:x}\r\n{}\r\n", payload.len(), payload).as_bytes())
                .is_err()
            {
                return;
            }
            let _ = socket.flush();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    });
    let (app, dir) = build_app(
        "stream-client-drop",
        json!([provider_entry(
            &format!("http://127.0.0.1:{port}"),
            json!({"timeoutSeconds": 5}),
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
            json!({"model": "prov-test/m-test", "messages": [], "stream": true}).to_string(),
        ))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let request_id = response
        .headers()
        .get("x-wrouter-request-id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    // 丢弃响应体 = 客户端断开;管线下一次入队失败即取消。
    drop(response);

    use wr_app::ports::RequestLogStore as _;
    let store = wr_infra::sqlite::SqliteStore::open(&dir).await.unwrap();
    let mut row: Option<Value> = None;
    for _ in 0..100 {
        if let Some(found) = store.get_request_log(&request_id).await.unwrap() {
            row = Some(found);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let row = row.expect("客户端断开后必须落终态行");
    assert_eq!(row["status"], json!(499), "断开终态应为 499: {row}");
    store.close().await;
    let _ = std::fs::remove_dir_all(dir);
}
