//! 首字口径(20261016_01 A-1/A-2)

use super::*;

// ---------------------------------------------------------------------------
// 首字口径(20261016_01 A-1/A-2)
// ---------------------------------------------------------------------------

/// 终态 `first_byte_ms` 必须 = 请求开始 → 首个内容帧的耗时(首字发生时
/// 一次算定),不得在完成时被重算成「首字后剩余时长」—— 旧实现把
/// 首字 2s / 总 12s 的请求记成 first_byte_ms=10s,吞吐虚高 5 倍。
/// mock 上游:首帧延迟 250ms,此后每 150ms 一帧 → 首字 ≈250ms,
/// 总时长 ≈600ms,解码窗口(duration − first_byte)必须 ≈350ms。
#[tokio::test]
async fn streaming_first_byte_ms_fixed_at_first_content() {
    // 内嵌 TCP 上游(wiremock 该版本不支持流式 body):首帧延迟 250ms,
    // 此后每 150ms 一帧,精确分离「首字延迟」与「解码时长」。
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
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n",
            )
            .unwrap();
        let delta = |content: &str| {
            format!(
                "data: {{\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m-test\",\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"{content}\"}},\"finish_reason\":null}}]}}\n\n"
            )
        };
        let mut send_chunk = |payload: &str| {
            socket
                .write_all(format!("{:x}\r\n{}\r\n", payload.len(), payload).as_bytes())
                .unwrap();
            socket.flush().unwrap();
        };
        std::thread::sleep(std::time::Duration::from_millis(250));
        send_chunk(&delta("He"));
        std::thread::sleep(std::time::Duration::from_millis(150));
        send_chunk(&delta("llo"));
        std::thread::sleep(std::time::Duration::from_millis(150));
        send_chunk("data: [DONE]\n\n");
        socket.write_all(b"0\r\n\r\n").unwrap();
    });
    let (app, dir) = build_app(
        "first-byte-fixed",
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
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let request_id = response
        .headers()
        .get("x-wrouter-request-id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(
        text.contains("data: [DONE]"),
        "流应正常收口(上游终结帧原样透传)"
    );

    // 终态行(日志入队是异步的,轮询等它落库)。
    use wr_app::ports::RequestLogStore as _;
    let poll_store = wr_infra::sqlite::SqliteStore::open(&dir).await.unwrap();
    let mut row: Option<Value> = None;
    for _ in 0..60 {
        if let Some(found) = poll_store.get_request_log(&request_id).await.unwrap() {
            row = Some(found);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let row = row.expect("终态日志行必须落库");
    let first_byte = row["first_byte_ms"].as_f64().expect("首字段必须存在");
    let duration = row["duration_ms"].as_f64().unwrap();
    assert!(
        (250.0..500.0).contains(&first_byte),
        "首字 ≈ 上游首帧延迟 250ms(旧实现会记成首字后剩余时长 ≈0),实测 {first_byte}"
    );
    assert!(
        duration >= first_byte + 200.0,
        "总时长必须显著大于首字(后续帧还有 2×150ms),first_byte={first_byte} duration={duration}"
    );
    poll_store.close().await;
    let _ = std::fs::remove_dir_all(dir);
}
