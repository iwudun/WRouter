//! 「管理与测速」端点测速引擎验收测试。
//!
//! 单元测试（`src/admin/speedtest.rs` 的 `mod tests`）覆盖无需网络的校验/
//! 短路路径；本文件对 wiremock 上游验证**真实测量路径**：可达地址测得数值
//! 延迟 + 状态码，且结果按入参顺序返回。

use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use wr_server::admin::speedtest::test_endpoints;

#[tokio::test]
async fn measures_latency_and_status_for_reachable_url() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .mount(&server)
        .await;

    let url = format!("{}/v1", server.uri());
    let payload = json!({ "urls": [url], "timeoutSecs": 5 })
        .as_object()
        .cloned()
        .unwrap();
    let result = test_endpoints(&payload, None).await.expect("test_endpoints ok");
    let rows = result.as_array().expect("array result");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["status"], 200);
    assert_eq!(rows[0]["error"], Value::Null);
    let latency = rows[0]["latency"].as_u64().expect("latency measured");
    assert!(latency < 5000, "latency {latency}ms should be within timeout");
    // url 回显归一化（去尾斜杠）后的地址。
    assert_eq!(rows[0]["url"].as_str().unwrap(), url.trim_end_matches('/'));
}

#[tokio::test]
async fn preserves_input_order_and_records_non_2xx_as_reachable() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/a"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/b"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;

    let a = format!("{}/a", server.uri());
    let b = format!("{}/b", server.uri());
    let payload = json!({ "urls": [a, b], "timeoutSecs": 5 })
        .as_object()
        .cloned()
        .unwrap();
    let result = test_endpoints(&payload, None).await.expect("test_endpoints ok");
    let rows = result.as_array().expect("array result");
    assert_eq!(rows.len(), 2);
    // 保序：第 0 项对应 /a，第 1 项对应 /b。
    assert_eq!(rows[0]["url"].as_str().unwrap(), a.trim_end_matches('/'));
    assert_eq!(rows[0]["status"], 200);
    assert!(rows[0]["latency"].as_u64().is_some());
    // 503 仍算“可达”：记录状态码 + 延迟（测速测的是网络往返，不判业务成功）。
    assert_eq!(rows[1]["url"].as_str().unwrap(), b.trim_end_matches('/'));
    assert_eq!(rows[1]["status"], 503);
    assert!(rows[1]["latency"].as_u64().is_some());
    assert_eq!(rows[1]["error"], Value::Null);
}
