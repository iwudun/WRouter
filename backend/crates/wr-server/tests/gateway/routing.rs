//! 模型路由

use super::*;

// ---------------------------------------------------------------------------
// 模型路由
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unknown_model_returns_400_contract_error() {
    let server = MockServer::start().await;
    let (app, dir) = build_app(
        "unknown-model",
        json!([provider_entry(&server.uri(), json!({}))]),
        json!([]),
    )
    .await;
    // 2026-09-26 调用契约:裸模型名不再解析 → 400 + 格式提示。
    let (status, body, _) =
        send_json(app, "/v1/chat/completions", json!({"model": "nope"}), &[]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["type"], json!("model_not_found"));
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("provider/model"),
        "错误信息应给出正确调用格式: {body}"
    );
    // 上游不被调用。
    assert_eq!(server.received_requests().await.unwrap().len(), 0);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn unsupported_provider_protocol_yields_502() {
    // 镜像黄金样本 openai-responses-native-provider:
    // provider protocol 不合法 → providers() 抛错 → 502 wrouter_error。
    let (app, dir) = build_app(
        "bad-protocol",
        json!([provider_entry(
            "http://127.0.0.1:9",
            json!({"protocol": "openai_responses"})
        )]),
        json!([]),
    )
    .await;
    let (status, body, _) = send_json(
        app,
        "/v1/chat/completions",
        json!({"model": "prov-test/m-test"}),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Unsupported provider protocol")
    );
    let _ = std::fs::remove_dir_all(dir);
}
