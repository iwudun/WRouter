//! 聚合模型(M2/M3/M4 冒烟)

use super::*;

// ---------------------------------------------------------------------------
// 20260905_04:聚合模型(M2/M3/M4 冒烟)
// ---------------------------------------------------------------------------

/// first 策略:首选目标 500 → 自动切下一目标;每次尝试用该目标的 model
/// 重写请求体;客户端响应 model 恒为聚合名(§3.3 回写)。
#[tokio::test]
async fn aggregate_first_strategy_fails_over_and_rewrites_model() {
    let server_a = MockServer::start().await;
    let server_b = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(500).set_body_json(json!({"error": {"message": "boom"}})),
        )
        .mount(&server_a)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_chat_model("m-b")))
        .mount(&server_b)
        .await;
    let (app, dir) = build_app_with_groups(
        "agg-failover",
        json!([
            group_provider("pa", &server_a.uri(), &["m-a"]),
            group_provider("pb", &server_b.uri(), &["m-b"]),
        ]),
        json!([{"key": "sk-test-1", "enabled": true, "name": "t"}]),
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
        json!({"model": "wrouter/agg", "messages": [{"role": "user", "content": "hi"}]}),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // M4:响应 model 回写为聚合名(上游实际是 m-b)。
    assert_eq!(body["model"], json!("agg"));
    assert_eq!(body["choices"][0]["message"]["content"], json!("hello"));

    // 两个上游各收到一次请求,且请求体 model 是各自目标模型(M3 重写)。
    let requests_a = server_a.received_requests().await.unwrap();
    assert_eq!(requests_a.len(), 1);
    let body_a: Value = serde_json::from_slice(&requests_a[0].body).unwrap();
    assert_eq!(body_a["model"], json!("m-a"));
    let requests_b = server_b.received_requests().await.unwrap();
    assert_eq!(requests_b.len(), 1);
    let body_b: Value = serde_json::from_slice(&requests_b[0].body).unwrap();
    assert_eq!(body_b["model"], json!("m-b"));
    let _ = std::fs::remove_dir_all(dir);
}

/// 聚合名进 /v1/models 列表(带 available 静态标记),且首目标健康时
/// first 策略直接命中首选。
#[tokio::test]
async fn aggregate_appears_in_models_list_and_routes_first_target() {
    let server_a = MockServer::start().await;
    let server_b = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_chat_model("m-a")))
        .mount(&server_a)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_chat_model("m-b")))
        .mount(&server_b)
        .await;
    let (app, dir) = build_app_with_groups(
        "agg-list",
        json!([
            group_provider("pa", &server_a.uri(), &["m-a"]),
            group_provider("pb", &server_b.uri(), &["m-b"]),
        ]),
        json!([{"key": "sk-test-1", "enabled": true, "name": "t"}]),
        json!([{
            "id": "g1",
            "name": "agg",
            "targets": [
                {"providerId": "pa", "model": "m-a"},
                {"providerId": "pb", "model": "m-b"},
            ],
        }]),
    )
    .await;

    // /v1/models 含聚合名(owned_by=wrouter,available 静态标记)。
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models")
                .header("authorization", "Bearer test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let list: Value = serde_json::from_slice(&bytes).unwrap();
    let entry = list["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == json!("wrouter/agg"))
        .expect("聚合名必须出现在 /v1/models");
    assert_eq!(entry["owned_by"], json!("wrouter"));
    assert_eq!(entry["aggregate"], json!(true));
    assert_eq!(entry["available"], json!(true));

    // first 策略:首选健康 → 只打 pa,响应 model 回写聚合名。
    let (status, body, _) = send_json(
        app,
        "/v1/chat/completions",
        json!({"model": "wrouter/agg", "messages": [{"role": "user", "content": "hi"}]}),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["model"], json!("agg"));
    assert_eq!(server_a.received_requests().await.unwrap().len(), 1);
    assert_eq!(server_b.received_requests().await.unwrap().len(), 0);
    let _ = std::fs::remove_dir_all(dir);
}

/// fail-closed(2026-09-26 调用契约):聚合目标全部指向不存在的 provider →
/// 明确 502 失败,不再回退全池选择(裸名 fail-open 通道已下线)。
#[tokio::test]
async fn aggregate_all_targets_unavailable_fails_closed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_chat_model("agg")))
        .mount(&server)
        .await;
    let (app, dir) = build_app_with_groups(
        "agg-failclosed",
        json!([group_provider("p-live", &server.uri(), &["*"])]),
        json!([{"key": "sk-test-1", "enabled": true, "name": "t"}]),
        json!([{
            "id": "g1",
            "name": "agg",
            "targets": [
                {"providerId": "ghost-1", "model": "m-x"},
                {"providerId": "ghost-2", "model": "m-y", "enabled": false},
            ],
        }]),
    )
    .await;
    let (status, body, _) = send_json(
        app,
        "/v1/chat/completions",
        json!({"model": "wrouter/agg", "messages": [{"role": "user", "content": "hi"}]}),
        &[],
    )
    .await;
    // 明确失败:不回退全池,存活 provider 不得被调用。
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no available targets"),
        "应返回聚合无可用目标的错误: {body}"
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 0);
    let _ = std::fs::remove_dir_all(dir);
}
