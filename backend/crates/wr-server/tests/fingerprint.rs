//! 模型指纹探针的集成测试(方案 20261030_01 R14)。
//!
//! 覆盖:
//! - 三种方言的探针请求体与响应解析(openai chat / anthropic / openai_responses);
//! - 拒答与截断**计为缺口**(不计入样本);
//! - 探针的 token 预算放大(256 会把 300+ 整数截断 → 样本无效);
//! - 用户消息组装(前缀 + 任务正文)。
//!
//! 探针走**直连 reqwest**,不经网关转发管线 —— 因此结构上不可能写入
//! 调用日志或进入费用账本(决策点 4)。

use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use wr_domain::fingerprint::ProbeSpec;
use wr_domain::provider::Provider;
use wr_server::admin::model_fingerprint_probe::{
    fingerprint_probe_body, finish_reason_is_gap, response_finish_reason, run_fingerprint_probes,
};

fn provider(base_url: &str, protocol: &str) -> Provider {
    let raw = json!({
        "id": "p1",
        "name": "p1",
        "api_base_url": base_url,
        "api_key": "sk-x",
        "protocol": protocol,
        "enabled": true,
        "models": ["m1"],
    });
    Provider::from_raw(raw.as_object().expect("raw is an object")).expect("provider parses")
}

fn spec(challenge_id: &str, condition: &str) -> ProbeSpec {
    ProbeSpec {
        challenge_id: challenge_id.to_owned(),
        condition: condition.to_owned(),
        transport: "clean".to_owned(),
        style: "json".to_owned(),
        system: String::new(),
        user_prefix: String::new(),
        user_task_separator: String::new(),
        expected_count: 300,
        prompt: "请给出 300 个 1 到 355 的整数。".to_owned(),
    }
}

fn chat_response(content: &str, finish_reason: &str) -> Value {
    json!({
        "id": "chatcmpl-1",
        "object": "chat.completion",
        "model": "m1",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": content}, "finish_reason": finish_reason}],
    })
}

#[tokio::test]
async fn openai_chat_probe_sends_enlarged_body_and_parses_response() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response("1, 2, 3", "stop")))
        .mount(&server)
        .await;

    let provider = provider(&server.uri(), "openai");
    let runs = run_fingerprint_probes(&provider, "openai", "m1", &[spec("c1", "environment-01")], None)
        .await;

    assert_eq!(runs.len(), 1);
    assert!(runs[0].outcome.accepted);
    assert_eq!(runs[0].outcome.text, "1, 2, 3");
    assert_eq!(runs[0].expected_count, 300);
    assert_eq!(runs[0].condition_id, "environment-01");

    // 请求体:max_tokens 必须放大到 4096(256 会截断 300+ 整数)。
    let requests = server.received_requests().await.expect("requests recorded");
    assert_eq!(requests.len(), 1);
    let body: Value = serde_json::from_slice(&requests[0].body).expect("body is json");
    assert_eq!(body["max_tokens"], 4096);
    assert_eq!(body["model"], "m1");
    assert_eq!(body["messages"][0]["role"], "user");
    assert!(body["messages"][0]["content"]
        .as_str()
        .expect("content")
        .contains("300 个"));
}

#[tokio::test]
async fn anthropic_probe_uses_top_level_system_field() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg-1",
            "type": "message",
            "role": "assistant",
            "model": "m1",
            "content": [{"type": "text", "text": "4, 5, 6"}],
            "stop_reason": "end_turn",
        })))
        .mount(&server)
        .await;

    let provider = provider(&server.uri(), "anthropic");
    let mut probe = spec("c1", "environment-03");
    probe.system = "SYSTEM-PREFIX".to_owned();
    let runs = run_fingerprint_probes(&provider, "anthropic", "m1", &[probe], None).await;

    assert!(runs[0].outcome.accepted);
    assert_eq!(runs[0].outcome.text, "4, 5, 6");

    let requests = server.received_requests().await.expect("requests recorded");
    let body: Value = serde_json::from_slice(&requests[0].body).expect("body is json");
    assert_eq!(body["system"], "SYSTEM-PREFIX");
    assert_eq!(body["max_tokens"], 4096);
    // anthropic 源协议下 system 不是消息,而是顶层字段。
    assert!(body["messages"][0]["content"].as_str().is_some());
}

#[tokio::test]
async fn responses_probe_uses_instructions_and_input() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "resp-1",
            "object": "response",
            "model": "m1",
            "status": "completed",
            "output": [{"type": "message", "content": [{"type": "output_text", "text": "7, 8, 9"}]}],
        })))
        .mount(&server)
        .await;

    let provider = provider(&server.uri(), "openai");
    let mut provider = provider;
    provider.protocol_style = Some("openai_responses".to_owned());
    let mut probe = spec("c1", "environment-01");
    probe.system = "SYSTEM-PREFIX".to_owned();
    let runs = run_fingerprint_probes(&provider, "openai_responses", "m1", &[probe], None).await;

    assert!(runs[0].outcome.accepted);
    assert_eq!(runs[0].outcome.text, "7, 8, 9");

    let requests = server.received_requests().await.expect("requests recorded");
    let body: Value = serde_json::from_slice(&requests[0].body).expect("body is json");
    assert_eq!(body["instructions"], "SYSTEM-PREFIX");
    assert_eq!(body["max_output_tokens"], 4096);
    assert_eq!(body["stream"], false);
}

#[tokio::test]
async fn truncated_response_is_counted_as_gap() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response("1, 2, 3", "length")))
        .mount(&server)
        .await;

    let provider = provider(&server.uri(), "openai");
    let runs = run_fingerprint_probes(&provider, "openai", "m1", &[spec("c1", "environment-01")], None)
        .await;

    assert!(!runs[0].outcome.accepted, "截断的回答不计入样本");
    assert_eq!(runs[0].outcome.reason.as_deref(), Some("length"));
    assert!(runs[0].outcome.text.is_empty());
}

#[tokio::test]
async fn http_error_is_counted_as_gap() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({"error": {"message": "slow down"}})))
        .mount(&server)
        .await;

    let provider = provider(&server.uri(), "openai");
    let runs = run_fingerprint_probes(&provider, "openai", "m1", &[spec("c1", "environment-01")], None)
        .await;

    assert!(!runs[0].outcome.accepted);
    assert_eq!(runs[0].outcome.reason.as_deref(), Some("http:429"));
}

#[tokio::test]
async fn each_probe_hits_upstream_exactly_once() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response("1, 2, 3", "stop")))
        .mount(&server)
        .await;

    let provider = provider(&server.uri(), "openai");
    let specs = vec![
        spec("c1", "environment-01"),
        spec("c2", "environment-02"),
        spec("c3", "environment-03"),
    ];
    let runs = run_fingerprint_probes(&provider, "openai", "m1", &specs, None).await;
    assert_eq!(runs.len(), 3);
    assert!(runs.iter().all(|run| run.outcome.accepted));
    let requests = server.received_requests().await.expect("requests recorded");
    assert_eq!(requests.len(), 3, "每条探针恰好一次上游请求");
}

#[test]
fn probe_body_carries_system_and_user_prefix_per_protocol() {
    let mut probe = spec("c1", "environment-07");
    probe.system = "SYS".to_owned();
    probe.user_prefix = "USER-PREFIX ".to_owned();

    let chat = fingerprint_probe_body("openai", "m1", &probe);
    assert_eq!(chat["messages"][0]["role"], "system");
    assert_eq!(chat["messages"][0]["content"], "SYS");
    assert!(chat["messages"][1]["content"]
        .as_str()
        .expect("content")
        .starts_with("USER-PREFIX "));

    let anthropic = fingerprint_probe_body("anthropic", "m1", &probe);
    assert_eq!(anthropic["system"], "SYS");
    assert!(anthropic["messages"][0]["content"]
        .as_str()
        .expect("content")
        .starts_with("USER-PREFIX "));

    let responses = fingerprint_probe_body("openai_responses", "m1", &probe);
    assert_eq!(responses["instructions"], "SYS");
    assert!(responses["input"].as_str().expect("input").starts_with("USER-PREFIX "));
}

#[test]
fn spec_assembles_prefix_into_user_content() {
    // wrouter-v1 的模板不带分隔符,组装结果就是「前缀 + 任务正文」。
    let mut probe = spec("c1", "environment-10");
    probe.transport = "user".to_owned();
    probe.user_prefix = "USER-PREFIX ".to_owned();

    let body = fingerprint_probe_body("openai", "m1", &probe);
    assert_eq!(
        body["messages"][0]["content"],
        format!("USER-PREFIX {}", probe.prompt)
    );
}

#[test]
fn finish_reasons_are_classified() {
    assert!(finish_reason_is_gap("length"));
    assert!(finish_reason_is_gap("content_filter"));
    assert!(finish_reason_is_gap("refusal"));
    assert!(!finish_reason_is_gap("stop"));
    assert!(!finish_reason_is_gap("end_turn"));

    assert_eq!(
        response_finish_reason("anthropic", &json!({"stop_reason": "max_tokens"})),
        Some("max_tokens".to_owned())
    );
    assert_eq!(
        response_finish_reason(
            "openai",
            &json!({"choices": [{"finish_reason": "length"}]})
        ),
        Some("length".to_owned())
    );
    assert_eq!(
        response_finish_reason("openai_responses", &json!({"status": "incomplete"})),
        Some("incomplete".to_owned())
    );
    assert_eq!(
        response_finish_reason("openai_responses", &json!({"status": "completed"})),
        None
    );
}
