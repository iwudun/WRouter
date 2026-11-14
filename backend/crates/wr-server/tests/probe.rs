//! R5.3 探针引擎 + 账号快照引擎验收测试(14a §6-F)。
//!
//! 探针语义对 wiremock 上游逐门验证(5 重判定门/候选回退/矩阵聚合);
//! 账号快照走真实 SqliteStore + admin RPC 链路(缓存 TTL/unsupported/
//! 连接器映射求值)。

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use wiremock::matchers::{body_string_contains, header, method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

use wr_server::admin::connectivity_probe::{
    check_connectivity, fetch_provider_models, probe_candidate, probe_candidates,
    probe_model_protocol,
};

fn chat_response(model: &str, content: &str, usage: Value) -> Value {
    json!({
        "id": "chatcmpl-1",
        "object": "chat.completion",
        "model": model,
        "choices": [{"index": 0, "message": {"role": "assistant", "content": content}, "finish_reason": "stop"}],
        "usage": usage,
    })
}

fn ok_usage() -> Value {
    json!({"prompt_tokens": 3, "completion_tokens": 1, "total_tokens": 4})
}

// ---------------------------------------------------------------------------
// _probe_model_protocol:5 重判定门
// ---------------------------------------------------------------------------

#[tokio::test]
async fn probe_model_5_gates_happy_path() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response(
            "m1",
            "ok",
            ok_usage(),
        )))
        .mount(&server)
        .await;
    let result = probe_model_protocol(&server.uri(), "sk-x", "openai", "m1", None, false, None).await;
    assert_eq!(result["ok"], Value::Bool(true));
    assert_eq!(result["status"], 200);
    assert_eq!(result["message"], "HTTP 200");
    assert_eq!(
        result["usage"],
        json!({"prompt_tokens": 3, "completion_tokens": 1})
    );
    assert!(
        result["endpoint"]
            .as_str()
            .unwrap()
            .ends_with("/v1/chat/completions")
    );
}

// ---- 20261014_01:完整 URL 模式(原样调用,不拼端点) --------------------

#[tokio::test]
async fn full_url_mode_probe_hits_path_verbatim() {
    let server = MockServer::start().await;
    // 无版本段、无已知后缀的自定义路径:full 模式必须打原样地址;若
    // 误拼 /v1/chat/completions 则 mock 404,5 重判定门直接判失败。
    Mock::given(method("POST"))
        .and(path("/llm/claude"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response(
            "m1",
            "ok",
            ok_usage(),
        )))
        .mount(&server)
        .await;
    let base = format!("{}/llm/claude", server.uri());
    let result = probe_model_protocol(&base, "sk-x", "openai", "m1", None, true, None).await;
    assert_eq!(result["ok"], Value::Bool(true), "full 模式应命中原样路径");
    assert!(result["endpoint"].as_str().unwrap().ends_with("/llm/claude"));
}

#[tokio::test]
async fn connectivity_candidate_url_mode_full_is_honored() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/llm/claude"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response(
            "m1",
            "ok",
            ok_usage(),
        )))
        .mount(&server)
        .await;
    let payload = json!({
        "apiKey": "sk-x",
        "models": ["m1"],
        "protocols": ["openai"],
        "candidates": [{
            "baseUrl": format!("{}/llm/claude", server.uri()),
            "protocols": ["openai"],
            "urlMode": "full",
        }],
    });
    let report = check_connectivity(payload.as_object().unwrap(), None)
        .await
        .unwrap();
    assert_eq!(
        report["passed"].as_array().map(Vec::len),
        Some(1),
        "候选载荷 urlMode=full 应原样探测命中: {}",
        report
    );
}

#[tokio::test]
async fn probe_model_rejects_error_object_2xx() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"error": {"message": "boom"}})),
        )
        .mount(&server)
        .await;
    let result = probe_model_protocol(&server.uri(), "", "openai", "m1", None, false, None).await;
    assert_eq!(result["ok"], Value::Bool(false));
    assert_eq!(result["message"], "Model not available: boom");
}

#[tokio::test]
async fn probe_model_rejects_empty_output() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response(
            "m1",
            "   ",
            ok_usage(),
        )))
        .mount(&server)
        .await;
    let result = probe_model_protocol(&server.uri(), "", "openai", "m1", None, false, None).await;
    assert_eq!(result["ok"], Value::Bool(false));
    assert_eq!(
        result["message"],
        "Model not available: Upstream returned an empty response — the model may not be available."
    );
}

#[tokio::test]
async fn probe_model_rejects_missing_usage() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response(
            "m1",
            "ok",
            json!({}),
        )))
        .mount(&server)
        .await;
    let result = probe_model_protocol(&server.uri(), "", "openai", "m1", None, false, None).await;
    assert_eq!(result["ok"], Value::Bool(false));
    assert_eq!(
        result["message"],
        "Model not available: Upstream did not report token usage — the model may not have been invoked."
    );
}

#[tokio::test]
async fn probe_model_rejects_swapped_model_echo() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response(
            "gpt-4o-mini",
            "ok",
            ok_usage(),
        )))
        .mount(&server)
        .await;
    let result = probe_model_protocol(&server.uri(), "", "openai", "gpt-4o", None, false, None).await;
    assert_eq!(result["ok"], Value::Bool(false));
    assert_eq!(
        result["message"],
        "Model not available: Upstream echoed model 'gpt-4o-mini' instead of requested 'gpt-4o'."
    );
}

#[tokio::test]
async fn probe_model_accepts_dated_version_alias() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response(
            "deepseek-v4-flash-ga-260731",
            "ok",
            ok_usage(),
        )))
        .mount(&server)
        .await;
    let result = probe_model_protocol(&server.uri(), "", "openai", "deepseek-v4-flash", None, false, None).await;
    assert_eq!(result["ok"], Value::Bool(true));
}

#[tokio::test]
async fn probe_model_unreachable_reports_transport() {
    // 连接不存在的端口 → HTTPError 分支:message = "Endpoint unreachable: …"。
    let result = probe_model_protocol("http://127.0.0.1:9", "", "openai", "m1", None, false, None).await;
    assert_eq!(result["ok"], Value::Bool(false));
    assert_eq!(result["status"], Value::Null);
    assert!(
        result["message"]
            .as_str()
            .unwrap()
            .starts_with("Endpoint unreachable")
    );
}

// ---------------------------------------------------------------------------
// _probe_candidate:GET 模型列表
// ---------------------------------------------------------------------------

#[tokio::test]
async fn probe_candidate_models_extraction() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id": "m1"}, {"id": "m2"}, "m3", {"other": 1}]
        })))
        .mount(&server)
        .await;
    let candidate = json!({"baseUrl": server.uri(), "protocols": ["openai"]});
    let result = probe_candidate(&candidate, "sk", None).await.unwrap();
    assert_eq!(result["ok"], Value::Bool(true));
    assert_eq!(result["models"], json!(["m1", "m2", "m3"]));
    assert_eq!(result["message"], "Endpoint responded successfully.");
}

#[tokio::test]
async fn probe_candidate_falls_back_to_base_models_path() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"models": ["a", "b"]})))
        .mount(&server)
        .await;
    let candidate = json!({"baseUrl": server.uri()});
    let result = probe_candidate(&candidate, "", None).await.unwrap();
    assert_eq!(result["ok"], Value::Bool(true));
    assert_eq!(result["models"], json!(["a", "b"]));
}

#[tokio::test]
async fn probe_candidate_all_failed_reports_last_status() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;
    let candidate = json!({"baseUrl": server.uri()});
    let result = probe_candidate(&candidate, "", None).await.unwrap();
    assert_eq!(result["ok"], Value::Bool(false));
    assert_eq!(result["message"], "Endpoint returned HTTP 403.");
    assert_eq!(result["status"], 403);
}

#[tokio::test]
async fn probe_candidate_no_base_url() {
    let result = probe_candidate(&json!({"baseUrl": ""}), "", None)
        .await
        .unwrap();
    assert_eq!(result["ok"], Value::Bool(false));
    assert_eq!(result["message"], "No base URL.");
}

// ---------------------------------------------------------------------------
// fetchProviderModels:添加模型弹窗快速获取(20261017_01)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn fetch_models_openai_bearer_dedups_preserving_order() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("authorization", "Bearer sk-test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id": "m1"}, {"id": "m1"}, "m2"]
        })))
        .mount(&server)
        .await;
    let payload = json!({
        "baseUrl": server.uri(),
        "protocol": "openai_chat_completions",
        "apiKey": "sk-test",
    });
    let result = fetch_provider_models(payload.as_object().unwrap(), None)
        .await
        .unwrap();
    assert_eq!(result["ok"], Value::Bool(true));
    assert_eq!(result["status"], 200);
    assert_eq!(result["models"], json!(["m1", "m2"]));
    assert_eq!(result["message"], "Fetched 2 models.");
}

#[tokio::test]
async fn fetch_models_anthropic_retries_bearer_after_x_api_key_401() {
    let server = MockServer::start().await;
    // x-api-key 轮:401(兼容只认 Bearer 的中转站)。
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("x-api-key", "sk-test"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    // Bearer 回落轮:200。
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("authorization", "Bearer sk-test"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"data": [{"id": "claude-x", "display_name": "X"}]})),
        )
        .mount(&server)
        .await;
    let payload = json!({
        "baseUrl": server.uri(),
        "protocol": "anthropic_messages",
        "apiKey": "sk-test",
    });
    let result = fetch_provider_models(payload.as_object().unwrap(), None)
        .await
        .unwrap();
    assert_eq!(result["ok"], Value::Bool(true));
    assert_eq!(result["models"], json!(["claude-x"]));
}

#[tokio::test]
async fn fetch_models_anthropic_happy_path_sends_version_header() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("x-api-key", "sk-test"))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": [{"id": "m"}]})))
        .mount(&server)
        .await;
    let payload = json!({
        "baseUrl": server.uri(),
        "protocol": "anthropic_messages",
        "apiKey": "sk-test",
    });
    let result = fetch_provider_models(payload.as_object().unwrap(), None)
        .await
        .unwrap();
    assert_eq!(result["ok"], Value::Bool(true));
    assert_eq!(result["models"], json!(["m"]));
}

#[tokio::test]
async fn fetch_models_gemini_paginates_strips_prefix_and_sends_api_key() {
    let server = MockServer::start().await;
    // 首页:无 pageToken,带 x-goog-api-key 与 pageSize,回下一页 token。
    Mock::given(method("GET"))
        .and(path("/v1beta/models"))
        .and(query_param_is_missing("pageToken"))
        .and(header("x-goog-api-key", "sk-test"))
        .and(query_param("pageSize", "1000"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "models": [{"name": "models/gemini-2.0-flash"}, {"name": "models/gemini-1.5"}],
            "nextPageToken": "cursor-2",
        })))
        .mount(&server)
        .await;
    // 第二页:回显 token,无 nextPageToken → 终止。
    Mock::given(method("GET"))
        .and(path("/v1beta/models"))
        .and(query_param("pageToken", "cursor-2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "models": [{"name": "models/gemini-2.5"}],
        })))
        .mount(&server)
        .await;
    let payload = json!({
        "baseUrl": server.uri(),
        "protocol": "gemini_generate_content",
        "apiKey": "sk-test",
    });
    let result = fetch_provider_models(payload.as_object().unwrap(), None)
        .await
        .unwrap();
    assert_eq!(result["ok"], Value::Bool(true));
    assert_eq!(
        result["models"],
        json!(["gemini-2.0-flash", "gemini-1.5", "gemini-2.5"])
    );
}

#[tokio::test]
async fn fetch_models_full_url_mode_keeps_path_verbatim() {
    let server = MockServer::start().await;
    // full 模式地址原样追加候选;若走了 strip 路径(/v1/models 根路径)
    // 则无 mock 命中,404 → ok=false。
    Mock::given(method("GET"))
        .and(path("/llm/claude/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": [{"id": "m"}]})))
        .mount(&server)
        .await;
    let payload = json!({
        "baseUrl": format!("{}/llm/claude", server.uri()),
        "protocol": "anthropic_messages",
        "apiKey": "sk-test",
        "urlMode": "full",
    });
    let result = fetch_provider_models(payload.as_object().unwrap(), None)
        .await
        .unwrap();
    assert_eq!(result["ok"], Value::Bool(true));
    assert_eq!(result["models"], json!(["m"]));
}

#[tokio::test]
async fn fetch_models_all_failed_reports_last_status() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;
    let payload = json!({
        "baseUrl": server.uri(),
        "protocol": "openai_chat_completions",
        "apiKey": "sk-test",
    });
    let result = fetch_provider_models(payload.as_object().unwrap(), None)
        .await
        .unwrap();
    assert_eq!(result["ok"], Value::Bool(false));
    assert_eq!(result["status"], 403);
    assert_eq!(result["message"], "Endpoint returned HTTP 403.");
    assert_eq!(result["models"], json!([]));
}

#[tokio::test]
async fn fetch_models_no_base_url() {
    let result = fetch_provider_models(&json!({"baseUrl": ""}).as_object().unwrap().clone(), None)
        .await
        .unwrap();
    assert_eq!(result["ok"], Value::Bool(false));
    assert_eq!(result["message"], "No base URL.");
}

// ---------------------------------------------------------------------------
// check_connectivity / probe_candidates 聚合
// ---------------------------------------------------------------------------

#[tokio::test]
async fn check_connectivity_no_candidates_failed_rows() {
    let result = check_connectivity(&json!({}).as_object().unwrap().clone(), None)
        .await
        .unwrap();
    assert_eq!(result["passed"], json!([]));
    assert_eq!(result["results"], json!([]));
    assert_eq!(result["probe"]["detectedProtocol"], Value::Null);
    let failed = result["failed"].as_array().unwrap();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0]["model"], "default");
    assert_eq!(failed[0]["message"], "No endpoint candidates available.");
    assert_eq!(failed[0]["supported"], Value::Bool(false));
}

#[tokio::test]
async fn check_connectivity_matrix_and_detection() {
    let server = MockServer::start().await;
    // 每个模型回显自身(5 门之 model-echo 校验依赖回显一致)。
    Mock::given(method("POST"))
        .and(body_string_contains("\"m1\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response(
            "m1",
            "ok",
            ok_usage(),
        )))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("\"m2\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response(
            "m2",
            "ok",
            ok_usage(),
        )))
        .mount(&server)
        .await;
    let payload = json!({
        "apiKey": "sk",
        "models": ["m1", "m2"],
        "candidates": [{"baseUrl": format!("{}/", server.uri().trim_end_matches('/')), "protocols": ["openai", "gemini"]}],
    });
    let result = check_connectivity(&payload.as_object().unwrap().clone(), None)
        .await
        .unwrap();
    // openai 行 supported(m1/m2),gemini 行短路不支持。
    assert_eq!(result["probe"]["detectedProtocol"], "openai");
    assert_eq!(result["probe"]["models"], json!(["m1", "m2"]));
    let passed = result["passed"].as_array().unwrap();
    assert_eq!(passed.len(), 2);
    let failed = result["failed"].as_array().unwrap();
    assert_eq!(failed.len(), 2);
    assert_eq!(failed[0]["protocols"][0]["protocol"], "gemini");
    assert_eq!(
        failed[0]["protocols"][0]["message"],
        "This API format does not support real chat probing."
    );
    // capabilities 按 (base_url, protocol) 去重,首见 protocol 行进 probe。
    let caps = result["probe"]["capabilities"].as_array().unwrap();
    assert_eq!(caps.len(), 2);
    assert_eq!(caps[0]["source"], "preset");
    let protos = result["probe"]["protocols"].as_array().unwrap();
    assert_eq!(protos.len(), 2);
    assert_eq!(protos[0]["protocol"], "openai");
    // durationMs camelCase;usage 保留 snake_case 键。
    assert!(passed[0]["protocols"][0]["durationMs"].as_f64().is_some());
    assert_eq!(
        passed[0]["protocols"][0]["usage"],
        json!({"prompt_tokens": 3, "completion_tokens": 1})
    );
}

#[tokio::test]
async fn probe_candidates_picks_first_ok() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"data": [{"id": "m1"}, {"id": "m2"}]})),
        )
        .mount(&server)
        .await;
    let payload = json!({
        "apiKey": "sk",
        "candidates": [
            {"baseUrl": server.uri(), "protocols": ["openai"]},
            {"baseUrl": server.uri(), "protocols": ["anthropic", "openai"]},
        ],
    });
    let result = probe_candidates(&payload.as_object().unwrap().clone(), None)
        .await
        .unwrap();
    // best = 首个 ok 候选;模型跨候选合并后保序去重(dict.fromkeys 语义)。
    assert_eq!(result["probe"]["models"], json!(["m1", "m2"]));
    assert_eq!(result["probe"]["detectedProtocol"], "openai");
    let caps = result["probe"]["capabilities"].as_array().unwrap();
    assert_eq!(caps.len(), 2);
    assert_eq!(caps[0]["source"], "detected");
    assert_eq!(caps[0]["type"], "openai");
    let protos = result["probe"]["protocols"].as_array().unwrap();
    assert_eq!(protos.len(), 1);
    assert_eq!(protos[0]["message"], "Endpoint responded successfully.");
    assert_eq!(result["candidate"]["baseUrl"], server.uri());
}

#[tokio::test]
async fn probe_candidates_empty_shape() {
    let result = probe_candidates(&json!({}).as_object().unwrap().clone(), None)
        .await
        .unwrap();
    assert_eq!(
        result,
        json!({"candidate": Value::Null, "probe": {"capabilities": [], "detectedProtocol": Value::Null, "models": [], "protocols": []}})
    );
}

// ---------------------------------------------------------------------------
// RPC 接线 + 账号快照(真实 SqliteStore 链路)
// ---------------------------------------------------------------------------

fn test_settings(tag: &str) -> Settings {
    let data_dir =
        std::env::temp_dir().join(format!("wr-probe-tests-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);
    let mut settings = Settings::from_env();
    settings.data_dir = data_dir.clone();
    settings.config_dir = data_dir.join("config");
    settings.auth_token = "test-admin-token".to_owned();
    settings
}

use wr_app::ports::SecretStore as _;
use wr_server::settings::Settings;

async fn build_app(
    tag: &str,
) -> (
    axum::Router,
    Arc<wr_infra::sqlite::SqliteStore>,
    Arc<wr_server::gateway::GatewayDeps>,
) {
    let settings = Arc::new(test_settings(tag));
    let source = Arc::new(wr_infra::sqlite_config::SqliteConfigSource::new(
        &settings.data_dir,
    ));
    let store = Arc::new(
        wr_infra::sqlite::SqliteStore::open(&settings.data_dir)
            .await
            .unwrap(),
    );
    let deps = Arc::new(wr_server::gateway::GatewayDeps::new(
        settings.clone(),
        source,
        store.clone(),
    ));
    (
        wr_server::management_router(settings, deps.clone(), store.clone()),
        store,
        deps,
    )
}

async fn send_json(
    app: axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-wrouter-web-auth", "test-admin-token")
        .header("content-type", "application/json")
        .body(Body::from(body.map(|v| v.to_string()).unwrap_or_default()))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

/// 空种子下的 getProviderAccountSnapshots:provider 不在配置里 → 空表。
#[tokio::test]
async fn rpc_account_snapshots_unknown_provider_empty() {
    let (app, _store, _deps) = build_app("snap-unknown").await;
    let (status, body) = send_json(
        app,
        "POST",
        "/api/wr/rpc",
        Some(json!({
            "method": "getProviderAccountSnapshots",
            "args": [{"provider": "ghost"}]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], Value::Bool(true));
    assert_eq!(body["value"], json!([]));
}

/// provider 无 connectors 且有账号行 → unsupported 快照(60s 缓存落表)。
#[tokio::test]
async fn rpc_account_snapshot_unsupported_without_connectors() {
    let (app, store, _deps) = build_app("snap-unsupported").await;
    // 配置里落一个 provider(无 account.connectors)。
    let (status, _) = send_json(
        app.clone(),
        "PUT",
        "/api/v1/config",
        Some(json!({
            "version": 2,
            "Providers": [{"id": "p1", "name": "P1", "api_base_url": "http://x.example", "protocol": "openai"}],
            "APIKEYS": [],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    store
        .upsert_account(&wr_app::ports::AccountUpsert {
            provider_id: "p1",
            account_id: "a1",
            api_key: "sk-a1",
            label: "A1",
            enabled: true,
            priority: 0,
            weight: 1,
            account_config: None,
        })
        .await
        .unwrap();
    let (status, body) = send_json(
        app,
        "POST",
        "/api/wr/rpc",
        Some(json!({
            "method": "getProviderAccountSnapshots",
            "args": [{"provider": "p1"}]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let value = body["value"].as_array().unwrap();
    assert_eq!(value.len(), 1);
    assert_eq!(value[0]["provider"], "p1");
    assert_eq!(value[0]["status"], "unsupported");
    assert_eq!(value[0]["source"], "unsupported");
    assert_eq!(value[0]["message"], "No account connectors configured");
    assert_eq!(value[0]["credential_id"], "a1");
    assert_eq!(value[0]["credential_label"], "A1");
    // 落了 60s 缓存行。
    let cached = store
        .get_account_snapshot("p1", "a1")
        .await
        .unwrap()
        .unwrap();
    let expires_at = cached["expires_at"].as_i64().unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let ttl = expires_at - now;
    assert!((0..=ERROR_TTL).contains(&ttl), "unexpected ttl {ttl}");
}

const ERROR_TTL: i64 = 60_000 + 2_000;

/// http-json 连接器:mapping 求值 → meters;缓存命中后不再打上游;
/// forceRefresh 绕过缓存。
#[tokio::test]
async fn rpc_account_snapshot_connector_mapping_and_cache() {
    let (app, store, _deps) = build_app("snap-connector").await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/balance"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"total": 100.0, "balance": 5.0})),
        )
        .mount(&server)
        .await;
    let account_block = json!({
        "enabled": true,
        "refresh_interval_ms": 300_000,
        "connectors": [{
            "type": "http-json",
            "id": "c1",
            "endpoint": format!("{}/balance", server.uri()),
            "method": "GET",
            "mapping": {
                "meters": [
                    {"id": "bal", "label": "Balance", "unit": "USD", "limit": "$.total", "remaining": "$.balance"}
                ]
            },
        }],
    });
    let (status, _) = send_json(
        app.clone(),
        "PUT",
        "/api/v1/config",
        Some(json!({
            "version": 2,
            "Providers": [{"id": "p1", "name": "P1", "api_base_url": "http://x.example", "protocol": "openai", "account": account_block}],
            "APIKEYS": [],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    store
        .upsert_account(&wr_app::ports::AccountUpsert {
            provider_id: "p1",
            account_id: "a1",
            api_key: "sk-a1",
            label: "A1",
            enabled: true,
            priority: 0,
            weight: 1,
            account_config: None,
        })
        .await
        .unwrap();
    let call = |app: axum::Router, force: bool| {
        let app = app.clone();
        async move {
            let mut args = json!({"provider": "p1"});
            if force {
                args["options"] = json!({"forceRefresh": true});
            }
            send_json(
                app,
                "POST",
                "/api/wr/rpc",
                Some(json!({
                    "method": "getProviderAccountSnapshots", "args": [args]
                })),
            )
            .await
        }
    };
    // 第一次:实抓,5/100 ≤ 0.05 → critical。
    let (status, body) = call(app.clone(), false).await;
    assert_eq!(status, StatusCode::OK);
    let value = body["value"].as_array().unwrap();
    assert_eq!(value[0]["status"], "critical");
    assert_eq!(value[0]["source"], "http-json");
    let meters = value[0]["meters"].as_array().unwrap();
    assert_eq!(meters[0]["id"], "bal");
    assert_eq!(meters[0]["limit"], 100.0);
    assert_eq!(meters[0]["remaining"], 5.0);
    // 第二次:TTL 内 → 缓存命中,上游不再被打。
    let (status, body) = call(app.clone(), false).await;
    assert_eq!(status, StatusCode::OK);
    let cached_value = body["value"].as_array().unwrap();
    assert_eq!(cached_value[0]["status"], "critical");
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    // 强刷:再打一次上游。
    let (status, _) = call(app, true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

/// 连接器 500 → error 快照(errors[0].message="HTTP 500")。
#[tokio::test]
async fn rpc_account_snapshot_error_connector() {
    let (app, store, _deps) = build_app("snap-error").await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let account_block = json!({
        "connectors": [{
            "type": "http-json", "id": "c1",
            "endpoint": format!("{}/balance", server.uri()),
            "method": "GET",
        }],
    });
    let (status, _) = send_json(
        app.clone(),
        "PUT",
        "/api/v1/config",
        Some(json!({
            "version": 2,
            "Providers": [{"id": "p1", "name": "P1", "api_base_url": "http://x.example", "protocol": "openai", "account": account_block}],
            "APIKEYS": [],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    store
        .upsert_account(&wr_app::ports::AccountUpsert {
            provider_id: "p1",
            account_id: "a1",
            api_key: "",
            label: "A1",
            enabled: true,
            priority: 0,
            weight: 1,
            account_config: None,
        })
        .await
        .unwrap();
    let (status, body) = send_json(
        app,
        "POST",
        "/api/wr/rpc",
        Some(json!({
            "method": "getProviderAccountSnapshots",
            "args": [{"provider": "p1"}]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let value = body["value"].as_array().unwrap();
    assert_eq!(value[0]["status"], "error");
    assert_eq!(value[0]["source"], "http-json");
    assert_eq!(value[0]["errors"][0]["message"], "HTTP 500");
    assert_eq!(value[0]["message"], "HTTP 500");
}

/// checkProviderConnectivity RPC 接线:真实上游探活 → passed 非空。
#[tokio::test]
async fn rpc_check_provider_connectivity_wiring() {
    let (app, _store, _deps) = build_app("rpc-connectivity").await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response(
            "m1",
            "ok",
            ok_usage(),
        )))
        .mount(&server)
        .await;
    let (status, body) = send_json(
        app,
        "POST",
        "/api/wr/rpc",
        Some(json!({
            "method": "checkProviderConnectivity",
            "args": [{
                "apiKey": "sk",
                "models": ["m1"],
                "candidates": [{"baseUrl": server.uri(), "protocols": ["openai"]}],
            }]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], Value::Bool(true));
    assert_eq!(body["value"]["probe"]["detectedProtocol"], "openai");
    assert_eq!(body["value"]["passed"].as_array().unwrap().len(), 1);
}

/// probeProviderCandidates RPC 接线。
#[tokio::test]
async fn rpc_probe_provider_candidates_wiring() {
    let (app, _store, _deps) = build_app("rpc-candidates").await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": [{"id": "m1"}]})))
        .mount(&server)
        .await;
    let (status, body) = send_json(
        app,
        "POST",
        "/api/wr/rpc",
        Some(json!({
            "method": "probeProviderCandidates",
            "args": [{"apiKey": "sk", "candidates": [{"baseUrl": server.uri(), "protocols": ["openai"]}]}]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["value"]["probe"]["models"], json!(["m1"]));
    assert_eq!(body["value"]["probe"]["detectedProtocol"], "openai");
}

/// REST POST /api/v1/providers/probe:无 id → 400;未知 provider → 404;
/// 已知 provider → wiremock 探活 + 健康快照落表。
#[tokio::test]
async fn rest_probe_provider_flow() {
    let (app, _store, _deps) = build_app("rest-probe").await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
        .mount(&server)
        .await;
    // 空 id → 400。
    let (status, body) = send_json(
        app.clone(),
        "POST",
        "/api/v1/providers/probe",
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["detail"], "provider_id is required");
    // 未知 provider → 404。
    let (status, body) = send_json(
        app.clone(),
        "POST",
        "/api/v1/providers/probe",
        Some(json!({"provider_id": "ghost"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["detail"], "Provider not found: ghost");
    // 落一个 provider 再探:探测走 GET /v1/models(probe_endpoint)。
    let provider = json!({
        "id": "p1", "name": "P1", "api_base_url": server.uri(),
        "protocol": "openai", "models": [], "enabled": true,
    });
    let (status, _) = send_json(app.clone(), "PUT", "/api/v1/providers/p1", Some(provider)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = send_json(
        app.clone(),
        "POST",
        "/api/v1/providers/probe",
        Some(json!({"providerId": "p1"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], Value::Bool(true));
    assert_eq!(body["provider"], "p1");
    assert_eq!(body["status"], 200);
    assert!(body["url"].as_str().unwrap().ends_with("/v1/models"));
    assert!(body["duration_ms"].as_f64().is_some());
    // 健康表已落:GET /api/v1/providers/health 显示 healthy。
    let (status, body) = send_json(app, "GET", "/api/v1/providers/health", None).await;
    assert_eq!(status, StatusCode::OK);
    let items = body.as_array().unwrap();
    assert_eq!(items[0]["provider"], "p1");
    assert_eq!(items[0]["status"], "healthy");
    assert_eq!(items[0]["available"], Value::Bool(true));
    assert!(items[0]["checked_at"].is_string());
    assert!(items[0]["breaker_state"].is_string());
}

/// REST POST /api/v1/providers/health/check:逐个探活返回列表。
#[tokio::test]
async fn rest_probe_all_providers_flow() {
    let (app, _store, _deps) = build_app("rest-probe-all").await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
        .mount(&server)
        .await;
    let (status, _) = send_json(
        app.clone(),
        "PUT",
        "/api/v1/providers/p1",
        Some(json!({
            "id": "p1", "name": "P1", "api_base_url": server.uri(),
            "protocol": "openai", "models": [], "enabled": true,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = send_json(app, "POST", "/api/v1/providers/health/check", None).await;
    assert_eq!(status, StatusCode::OK);
    let items = body.as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["provider"], "p1");
    assert_eq!(items[0]["ok"], Value::Bool(true));
}
