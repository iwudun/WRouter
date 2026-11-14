//! 上游终态帧早退(2026-10-17)

use super::*;

// ---------------------------------------------------------------------------
// 上游终态帧早退(2026-10-17)
// ---------------------------------------------------------------------------

/// 上游发完 [DONE] 后挂住连接不关(中转站典型行为):修复前管线读到 EOF
/// 才收尾 —— 响应挂起、日志滞留「处理中」、duration 虚高(吞吐被低估)、
/// 并发许可被占;客户端连接池闲置断开还会把成功请求错记成 499。修复后
/// 消费完终结帧即收尾,响应必须在超时内完成,不依赖上游 EOF。
#[tokio::test]
async fn streaming_completes_on_terminal_frame_without_upstream_eof() {
    // 内嵌 TCP 上游:两个内容 delta + [DONE],随后 20s 不发 chunked 终止符、
    // 不关连接(旧实现会等满 20s)。
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        let (mut socket, _) = listener.accept().unwrap();
        let mut buf = [0u8; 8192];
        let mut head = Vec::new();
        while !head.windows(4).any(|window| window == b"\r\n\r\n") {
            let n = match socket.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            head.extend_from_slice(&buf[..n]);
        }
        let _ = socket.write_all(
            b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n",
        );
        let delta = |content: &str| {
            format!(
                "data: {{\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m-test\",\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"{content}\"}},\"finish_reason\":null}}]}}\n\n"
            )
        };
        let mut send_chunk = |payload: &str| {
            let _ = socket.write_all(format!("{:x}\r\n{}\r\n", payload.len(), payload).as_bytes());
            let _ = socket.flush();
        };
        send_chunk(&delta("He"));
        send_chunk(&delta("llo"));
        send_chunk("data: [DONE]\n\n");
        // 终结帧之后:挂住连接,什么都不再发(测试结束时随进程退出丢弃)。
        std::thread::sleep(std::time::Duration::from_secs(20));
    });
    let (app, dir) = build_app(
        "terminal-frame-early-exit",
        json!([provider_entry(
            &format!("http://127.0.0.1:{port}"),
            json!({})
        )]),
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
    let text = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    })
    .await
    .expect("上游 [DONE] 后必须立即收尾,不得等连接关闭(旧实现会超时)");
    assert!(text.contains(r#""content":"He""#), "body={text}");
    assert!(text.contains(r#""content":"llo""#), "body={text}");
    // P3-1 Raw:上游 [DONE] 原样透传后即收尾;不补 stop 终块。
    assert!(!text.contains(r#""finish_reason":"stop""#));
    assert_eq!(text.matches("data: [DONE]").count(), 1);
    let _ = std::fs::remove_dir_all(dir);
}
