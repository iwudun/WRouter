//! 并发

use super::*;

// ---------------------------------------------------------------------------
// 并发
// ---------------------------------------------------------------------------

/// 20261016_01 A-3:非流式与流式同语义 —— 槽位耗尽(排队超时)时落
/// 429 `concurrency_limited`,不再置 None 后静默超发上游(旧行为镜像
/// Python `retry_fallback.py`,是并发超发放大器,方案 §3.1 判定为缺陷)。
#[tokio::test]
async fn concurrency_non_stream_rejects_with_429() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_millis(150))
                .set_body_json(ok_chat()),
        )
        .mount(&server)
        .await;
    let (app, dir) = build_app(
        "concurrency-ns",
        json!([provider_entry(&server.uri(), json!({"max_in_flight": 1}))]),
        json!([]),
    )
    .await;
    let app = Arc::new(app);
    let first = {
        let app = app.clone();
        tokio::spawn(async move {
            send_json(
                (*app).clone(),
                "/v1/chat/completions",
                json!({"model": "prov-test/m-test"}),
                &[],
            )
            .await
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    let second = send_json(
        (*app).clone(),
        "/v1/chat/completions",
        json!({"model": "prov-test/m-test"}),
        &[],
    )
    .await;
    // 第二路被并发闸拒绝:429 + concurrency_limited,且不打上游。
    let (status2, body2, _) = second;
    assert_eq!(status2, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body2["error"]["type"], json!("concurrency_limited"));
    let (status1, body1, _) = first.await.unwrap();
    assert_eq!(status1, StatusCode::OK);
    assert_eq!(body1["id"], json!("up-ok"));
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        1,
        "被拒请求不得触达上游"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// 流式路径保留 R2 语义:槽位耗尽 → SSE 并发拒绝信封(镜像
/// `forwarding.py` 的 acquire 失败分支)。
#[tokio::test]
async fn concurrency_stream_rejects_with_sse_envelope() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_millis(250))
                .set_body_json(ok_chat()),
        )
        .mount(&server)
        .await;
    let (app, dir) = build_app(
        "concurrency-s",
        json!([provider_entry(&server.uri(), json!({"max_in_flight": 1}))]),
        json!([]),
    )
    .await;
    let app = Arc::new(app);
    let first = {
        let app = app.clone();
        tokio::spawn(async move {
            let request = Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("authorization", "Bearer test-token")
                .body(Body::from(
                    json!({"model": "prov-test/m-test", "messages": [], "stream": true})
                        .to_string(),
                ))
                .unwrap();
            let response = (*app).clone().oneshot(request).await.unwrap();
            response.into_body().collect().await.unwrap().to_bytes()
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(40)).await;
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .header("authorization", "Bearer test-token")
        .body(Body::from(
            json!({"model": "prov-test/m-test", "messages": [], "stream": true}).to_string(),
        ))
        .unwrap();
    let response = (*app).clone().oneshot(request).await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "提交前并发拒绝应返回真实状态"
    );
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["error"]["type"], json!("concurrency_limited"));
    // 第一路正常完成(A6 合成帧含内容)。
    let first_bytes = first.await.unwrap();
    let first_text = String::from_utf8_lossy(&first_bytes).into_owned();
    assert!(
        first_text.contains("hello"),
        "第一路应正常出帧: {first_text}"
    );
    let _ = std::fs::remove_dir_all(dir);
}
