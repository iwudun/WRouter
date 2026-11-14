//! 断流 prefill 续写(Anthropic → Anthropic)

use super::*;

// ---------------------------------------------------------------------------
// 阶段五 C-3:断流 prefill 续写(Anthropic → Anthropic)
// ---------------------------------------------------------------------------

/// 上游中途断开 → 网关用「已收到的那半句」作 assistant prefill 重发一次
/// → 客户端拿到**一条完整回复**(前半 + 续写后半),而不是半句话。
///
/// 断言三件事:
/// 1. 客户端收到的 SSE 里前半与后半都在;
/// 2. `message_start` 只出现一次(续写是同一条回复的延续);
/// 3. 第二次上游请求体带 assistant prefill(已收到的那半句)。
#[tokio::test]
async fn streaming_anthropic_continues_after_mid_stream_disconnect() {
    // 内嵌 TCP 上游:第一轮发半截正文后**直接断开**(chunked body 缺终止符
    // → 客户端侧读成传输错误);第二轮带 prefill 回来,发剩余部分。
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen_for_thread = seen.clone();
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        for round in 0..2 {
            let Ok((mut socket, _)) = listener.accept() else {
                break;
            };
            let mut buf = [0u8; 8192];
            let mut head = Vec::new();
            while !head.windows(4).any(|window| window == b"\r\n\r\n") {
                let n = match socket.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                head.extend_from_slice(&buf[..n]);
            }
            // 请求体可能与请求头同包到达 → 直接从已读缓冲里切出来,
            // 不要再单独读 body(否则会一直等一个已经到齐的 body)。
            let body_start = head
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|index| index + 4)
                .unwrap_or(head.len());
            let body = head[body_start..].to_vec();
            seen_for_thread
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(&body).to_string());
            if round == 0 {
                // 断流构造:声明一个远大于实际内容的 content-length,发完
                // 半截就断开 → 客户端侧必然读到「不完整 body」的传输错误
                // (比 chunked 缺终止符更确定,不会挂到读超时)。
                let payload = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"m-test\",\"usage\":{\"input_tokens\":5}}}\n\n\
event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\
event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"前半\"}}\n\n";
                socket
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{}",
                            payload.len() + 4096,
                            payload
                        )
                        .as_bytes(),
                    )
                    .unwrap();
                socket.flush().unwrap();
                // 显式 shutdown:确保 FIN 真的发出去(Windows 上仅靠 drop
                // 不保证对端立即感知断开)。
                let _ = socket.shutdown(std::net::Shutdown::Both);
                drop(socket);
                continue;
            }
            // 第二轮:完整响应(内容长度准确)。
            let payload = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_2\",\"model\":\"m-test\",\"usage\":{\"input_tokens\":7}}}\n\n\
event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"后半\"}}\n\n\
event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":4}}\n\n\
event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{}",
                        payload.len(),
                        payload
                    )
                    .as_bytes(),
                )
                .unwrap();
            socket.flush().unwrap();
        }
    });

    let (app, dir) = build_app(
        "anthropic-continuation",
        json!([provider_entry(
            &format!("http://127.0.0.1:{port}"),
            // 兜底超时:断流构造若失效,不至于把测试挂到默认 120s。
            json!({"protocol": "anthropic", "timeoutSeconds": 10}),
        )]),
        json!([]),
    )
    .await;
    let request = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("content-type", "application/json")
        .header("x-api-key", "test-token")
        .body(Body::from(
            json!({
                "model": "prov-test/m-test",
                "max_tokens": 64,
                "stream": true,
                "messages": [{"role": "user", "content": "hi"}],
            })
            .to_string(),
        ))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(text.contains("前半"), "客户端应收到断流前的那半句:{text}");
    assert!(text.contains("后半"), "客户端应收到续写补齐的后半句:{text}");
    assert_eq!(
        text.matches("event: message_start").count(),
        1,
        "续写不得重复下发 message_start:{text}"
    );

    // 第二次上游请求必须带 assistant prefill(已收到的那半句)。
    let bodies = seen.lock().unwrap().clone();
    assert_eq!(bodies.len(), 2, "应发出两次上游请求(原请求 + 续写请求)");
    assert!(
        bodies[1].contains("前半"),
        "续写请求必须带 assistant prefill:{bodies:?}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// 2026-11-05(B-5 上游字节基线暴露的缺陷):model-chain 回退那一环,请求体的
/// `model` 必须是**厂商侧的裸模型名**,不能是内部路由选择器(`Provider/模型`)。
///
/// 缺陷表现:第一个上游 503 后回退到第二个上游时,body 里发的是
/// `"prov-b/m-test"` —— 真实厂商会回 `model not found`,**回退在生产里等于失效**;
/// mock 不校验 model 字段 → 客户端可见响应完全看不出来(只有「上游字节」层面的
/// 断言才抓得到)。
///
/// 修复:候选的 model 取 target 解析出的裸模型名(`RouteModelRef::Provider.model`),
/// 而不是回退链项的配置原串。主 attempt 天然是裸名(客户端前缀已剥),两条路径同口径。
#[tokio::test]
async fn model_chain_fallback_sends_bare_model_to_upstream() {
    let first = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(503).set_body_json(json!({"error": {"message": "boom"}})),
        )
        .mount(&first)
        .await;
    let second = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_chat()))
        .mount(&second)
        .await;

    let providers = json!([
        provider_entry(
            &first.uri(),
            json!({"id": "prov-a", "name": "prov-a", "models": ["m-test"], "max_retries": 0}),
        ),
        provider_entry(
            &second.uri(),
            json!({"id": "prov-b", "name": "prov-b", "models": ["m-test"], "max_retries": 0}),
        ),
    ]);
    // 回退链:model-chain 形态,链项是 `Provider/模型` 选择器。
    let fallback = json!({"mode": "model-chain", "retryCount": 0, "models": ["prov-b/m-test"]});
    let (app, dir) =
        build_app_with_fallback("model-chain-fallback", providers, json!([]), fallback).await;

    let (status, body, _headers) = send_json(
        app,
        "/v1/chat/completions",
        json!({"model": "prov-a/m-test", "messages": [{"role": "user", "content": "hi"}]}),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, ok_chat());

    // 主 attempt:裸模型名(既有行为,客户端前缀在选路前已剥)。
    let first_received = first.received_requests().await.unwrap();
    assert_eq!(first_received.len(), 1, "主 attempt 应打第一个上游");
    let first_body: Value = serde_json::from_slice(&first_received[0].body).unwrap();
    assert_eq!(first_body["model"], json!("m-test"));

    // 回退 attempt:**也必须是裸模型名** —— 本用例的回归点。
    let second_received = second.received_requests().await.unwrap();
    assert_eq!(second_received.len(), 1, "503 后应回退到第二个上游");
    let second_body: Value = serde_json::from_slice(&second_received[0].body).unwrap();
    assert_eq!(
        second_body["model"],
        json!("m-test"),
        "回退时不能把内部路由选择器(prov-b/m-test)发给厂商"
    );
    let _ = std::fs::remove_dir_all(dir);
}
