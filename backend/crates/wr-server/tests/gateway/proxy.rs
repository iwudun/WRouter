//! 供应商级代理覆盖的网关接线

use super::*;

// ---------------------------------------------------------------------------
// 20260906_01 B4:供应商级代理覆盖的网关接线
// ---------------------------------------------------------------------------

/// 带预设的全局代理段(clash 预设故意带凭据,用于验证掩码)。
fn proxy_section_with_presets() -> Value {
    json!({
        "upstream": {"mode": "preset", "presetId": "corp"},
        "presets": [
            {"id": "corp", "name": "公司网关", "server": "gw.corp", "port": 8080},
            {
                "id": "clash", "name": "Clash", "server": "127.0.0.1", "port": 7890,
                "username": "proxy-user", "password": "super-secret"
            }
        ]
    })
}

/// 直读 `request_logs` 最新一行的 payload 文本(异步 drain 尚未落库时 None)。
/// 表主键是 request_id,没有 id 列 → 按 rowid 取最新。
fn read_latest_log_payload(data_dir: &std::path::Path) -> Option<String> {
    let conn = Connection::open(data_dir.join("config.sqlite")).ok()?;
    let mut statement = conn
        .prepare("SELECT payload FROM request_logs ORDER BY rowid DESC LIMIT 1")
        .ok()?;
    let text: String = statement.query_row([], |row| row.get(0)).ok()?;
    (!text.is_empty()).then_some(text)
}

/// 直读最新一行的 attempt 采集侧表 payload(ordinal=0;20261017_02 批5:
/// 主行 retryAttempts 瘦身,采集四件套落 request_log_attempts)。
fn read_latest_attempt_capture(data_dir: &std::path::Path) -> Option<String> {
    let conn = Connection::open(data_dir.join("config.sqlite")).ok()?;
    let mut statement = conn
        .prepare(
            "SELECT a.payload FROM request_logs r \
             JOIN request_log_attempts a ON a.request_id = r.request_id AND a.ordinal = 0 \
             ORDER BY r.rowid DESC LIMIT 1",
        )
        .ok()?;
    let text: String = statement.query_row([], |row| row.get(0)).ok()?;
    (!text.is_empty()).then_some(text)
}

/// 网关按「供应商 × 目标 URL」决策代理:供应商 direct 压过全局预设、
/// 供应商 preset 压过全局预设、缺省继承全局、预设被删回落全局默认;
/// 日志标签只留 `preset:<name>@<host:port>`(决策 #10:凭据不落日志)。
#[tokio::test]
async fn gateway_proxy_decision_honors_provider_override() {
    use wr_infra::proxy::ProxyDecision;

    let data_dir = temp_dir("proxy-override");
    seed_config_with_proxy(
        &data_dir,
        json!([
            provider_entry(
                "https://api.upstream.test/v1",
                json!({
                    "id": "p-direct", "proxy": {"mode": "direct"}
                })
            ),
            provider_entry(
                "https://api.upstream.test/v1",
                json!({
                    "id": "p-clash", "proxy": {"mode": "preset", "presetId": "clash"}
                })
            ),
            provider_entry("https://api.upstream.test/v1", json!({"id": "p-inherit"})),
            provider_entry(
                "https://api.upstream.test/v1",
                json!({
                    "id": "p-gone", "proxy": {"mode": "preset", "presetId": "deleted"}
                })
            ),
        ]),
        json!([{"key": "sk-test-1", "enabled": true, "name": "t"}]),
        proxy_section_with_presets(),
    );
    let mut settings = test_settings();
    settings.data_dir = data_dir.clone();
    let settings = Arc::new(settings);
    let source = Arc::new(wr_infra::sqlite_config::SqliteConfigSource::new(
        &settings.data_dir,
    ));
    let store = Arc::new(
        wr_infra::sqlite::SqliteStore::open(&data_dir)
            .await
            .unwrap(),
    );
    let deps = wr_server::gateway::GatewayDeps::new(settings, source, store);
    let snapshot = deps.snapshot().await.unwrap();

    // 目标 URL 非 loopback:决策链第 1 步不触发,覆盖/全局依次生效。
    let url = "https://api.upstream.test/v1/chat/completions";
    let decision_for = |id: &str| {
        let provider = snapshot
            .providers
            .iter()
            .find(|item| item.id == id)
            .unwrap_or_else(|| panic!("provider {id} 应在快照里"));
        wr_server::gateway::GatewayDeps::proxy_decision_for(&snapshot, provider, url)
    };
    // 验收 #1:供应商「直连」压过全局预设。
    assert_eq!(decision_for("p-direct"), ProxyDecision::Direct);
    // 供应商指定预设压过全局默认。
    assert_eq!(
        decision_for("p-clash"),
        ProxyDecision::Url("http://proxy-user:super-secret@127.0.0.1:7890".to_owned())
    );
    // 缺省 = 继承全局默认(验收 #1 的「其余继承全局」)。
    assert_eq!(
        decision_for("p-inherit"),
        ProxyDecision::Url("http://gw.corp:8080".to_owned())
    );
    // 决策 #6 / 验收 #3:预设被删 → 回落全局默认。
    assert_eq!(
        decision_for("p-gone"),
        ProxyDecision::Url("http://gw.corp:8080".to_owned())
    );
    // 日志标签:名称 + host:port,凭据不出现。
    let label =
        wr_server::gateway::GatewayDeps::proxy_source_label(&snapshot, &decision_for("p-clash"));
    assert_eq!(label, "preset:Clash@127.0.0.1:7890");
    assert!(
        !label.contains("super-secret") && !label.contains("proxy-user"),
        "代理凭据不得进入日志标签:{label}"
    );
    let _ = std::fs::remove_dir_all(data_dir);
}

/// 请求日志 payload 带 `proxy` 字段(方案 §4.7),且**全库不出现代理凭据**。
/// 上游是 loopback mock → 决策链第 1 步直连,故字段值为 `direct`;
/// 预设来源的标签形态由上一个测试覆盖。
#[tokio::test]
async fn request_log_records_proxy_source_without_credentials() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_chat_model("m-test")))
        .mount(&server)
        .await;
    let data_dir = temp_dir("proxy-log");
    seed_config_with_proxy(
        &data_dir,
        json!([provider_entry(
            &server.uri(),
            json!({
                "id": "p-clash", "proxy": {"mode": "preset", "presetId": "clash"}
            })
        )]),
        json!([{"key": "sk-test-1", "enabled": true, "name": "t"}]),
        proxy_section_with_presets(),
    );
    let mut settings = test_settings();
    settings.data_dir = data_dir.clone();
    let settings = Arc::new(settings);
    let source = Arc::new(wr_infra::sqlite_config::SqliteConfigSource::new(
        &settings.data_dir,
    ));
    let store = Arc::new(
        wr_infra::sqlite::SqliteStore::open(&data_dir)
            .await
            .unwrap(),
    );
    let deps = Arc::new(wr_server::gateway::GatewayDeps::new(
        settings.clone(),
        source,
        store,
    ));
    let app = wr_server::gateway_router(settings, deps);
    let (status, _, _) = send_json(
        app,
        "/v1/chat/completions",
        json!({"model": "prov-test/m-test", "messages": [{"role": "user", "content": "hi"}]}),
        &[("authorization", "Bearer sk-test-1")],
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // 落库是异步 drain:轮询到出现为止(上限 ~2s)。
    let mut payload = String::new();
    for _ in 0..100 {
        if let Some(text) = read_latest_log_payload(&data_dir) {
            payload = text;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(!payload.is_empty(), "请求日志应已落库");
    let parsed: Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(
        parsed.get("proxy").and_then(Value::as_str),
        Some("direct"),
        "loopback 上游应记为 direct,实际 payload={payload}"
    );
    // 决策 #10:凭据一律不落日志。
    assert!(
        !payload.contains("super-secret") && !payload.contains("proxy-user"),
        "代理凭据泄漏进请求日志:{payload}"
    );
    let _ = std::fs::remove_dir_all(data_dir);
}

/// 阶段 D3/D4/D9:captureBodies 开启时,每个 attempt 落稳定 attempt id、
/// 上游请求/响应正文与响应头;敏感 header 必须脱敏。
#[tokio::test]
async fn attempt_capture_records_upstream_io_with_redaction() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-upstream-secret", "should-not-be-captured-verbatim")
                .set_body_json(ok_chat_model("m-test")),
        )
        .mount(&server)
        .await;
    let data_dir = temp_dir("attempt-capture");
    seed_config_with_observability(
        &data_dir,
        json!([provider_entry(
            &server.uri(),
            json!({"id": "prov-test", "api_key": "upstream-key-123"})
        )]),
        json!([{"key": "sk-test-1", "enabled": true, "name": "t"}]),
        json!({"callLogs": true, "captureBodies": true}),
    );
    let mut settings = test_settings();
    settings.data_dir = data_dir.clone();
    let settings = Arc::new(settings);
    let source = Arc::new(wr_infra::sqlite_config::SqliteConfigSource::new(
        &settings.data_dir,
    ));
    let store = Arc::new(
        wr_infra::sqlite::SqliteStore::open(&data_dir)
            .await
            .unwrap(),
    );
    let deps = Arc::new(wr_server::gateway::GatewayDeps::new(
        settings.clone(),
        source,
        store,
    ));
    let app = wr_server::gateway_router(settings, deps);
    let (status, _, _) = send_json(
        app,
        "/v1/chat/completions",
        json!({"model": "prov-test/m-test", "messages": [{"role": "user", "content": "hi"}]}),
        &[("authorization", "Bearer sk-test-1")],
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let mut payload = String::new();
    for _ in 0..100 {
        if let Some(text) = read_latest_log_payload(&data_dir) {
            payload = text;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(!payload.is_empty(), "请求日志应已落库");
    let parsed: Value = serde_json::from_str(&payload).unwrap();
    let attempts = parsed["retry_attempts"].as_array().expect("应有尝试明细");
    assert!(!attempts.is_empty(), "至少一条 attempt 记录");
    let first = &attempts[0];
    assert_eq!(
        first["attempt_id"],
        json!("prov-test#1"),
        "attempt id 应为 稳定 provider#序号: {payload}"
    );
    // 20261017_02 批5:主行 retryAttempts 瘦身 —— 只带 captured 标记,
    // 采集四件套落 request_log_attempts 侧表。
    assert_eq!(
        first["captured"],
        json!(true),
        "瘦身主行应带 captured 标记: {payload}"
    );
    assert!(
        first.get("upstream_request").is_none(),
        "主行不得内嵌采集正文: {payload}"
    );

    // 采集正文与脱敏断言改在侧表行上验证。
    let mut capture_text = String::new();
    for _ in 0..100 {
        if let Some(text) = read_latest_attempt_capture(&data_dir) {
            capture_text = text;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(!capture_text.is_empty(), "attempt 采集应落侧表");
    let captured: Value = serde_json::from_str(&capture_text).unwrap();
    let upstream_request = captured["upstream_request"].as_str().unwrap_or_default();
    assert!(
        upstream_request.contains("m-test"),
        "上游请求正文应被捕获: {upstream_request}"
    );
    // 入口 authorization 不得以明文出现在捕获的上游请求里。
    assert!(
        !upstream_request.contains("sk-test-1"),
        "凭据不得进入捕获正文: {upstream_request}"
    );
    let headers = captured["upstream_headers"]
        .as_object()
        .expect("应有上游响应头");
    assert!(headers.contains_key("content-type"));
    // 阶段 D 的脱敏只覆盖敏感头名;非敏感自定义头原样保留。
    assert!(
        headers.contains_key("x-upstream-secret"),
        "非敏感头应保留键名: {headers:?}"
    );
    // 上游请求头一并采集:认证键打码,普通键保真,凭据值不落盘。
    let request_headers = captured["upstream_request_headers"]
        .as_object()
        .expect("应有上游请求头");
    assert_eq!(
        request_headers.get("content-type").and_then(Value::as_str),
        Some("application/json"),
        "普通请求头应保真: {request_headers:?}"
    );
    assert_eq!(
        request_headers.get("authorization").and_then(Value::as_str),
        Some("***"),
        "认证头应打码: {request_headers:?}"
    );
    assert!(
        !capture_text.contains("upstream-key-123"),
        "上游凭据不得进入采集: {capture_text}"
    );
    // 上游响应正文捕获(非流式 JSON)。
    let upstream_response = captured["upstream_response"].as_str().unwrap_or_default();
    assert!(
        upstream_response.contains("chat.completion"),
        "上游响应正文应被捕获: {upstream_response}"
    );
    let _ = std::fs::remove_dir_all(data_dir);
}

/// 直读最新一行的侧表正文(request_body / response_body)。
fn read_latest_log_bodies(data_dir: &std::path::Path) -> Option<(Option<String>, Option<String>)> {
    let conn = Connection::open(data_dir.join("config.sqlite")).ok()?;
    let mut statement = conn
        .prepare(
            "SELECT b.request_body, b.response_body FROM request_logs r \
             LEFT JOIN request_log_bodies b ON b.request_id = r.request_id \
             ORDER BY r.rowid DESC LIMIT 1",
        )
        .ok()?;
    let row = statement
        .query_row([], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
            ))
        })
        .ok()?;
    Some(row)
}

/// 2026-10-14:captureBodies 开启时,**失败**请求的终态行同样落请求正文
/// 与上游错误响应(此前失败路径只落 preview,详情页正文是空的)。
/// 场景对齐线上排障:流式请求 → 上游 400(NO_RETRY 终态)。
#[tokio::test]
async fn failed_stream_request_still_captures_bodies() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "type": "error",
            "error": {"type": "invalid_request_error", "message": "budget_tokens: Field required"}
        })))
        .mount(&server)
        .await;
    let data_dir = temp_dir("failed-capture");
    seed_config_with_observability(
        &data_dir,
        json!([provider_entry(&server.uri(), json!({"id": "prov-test"}))]),
        json!([{"key": "sk-test-1", "enabled": true, "name": "t"}]),
        json!({"callLogs": true, "captureBodies": true}),
    );
    let mut settings = test_settings();
    settings.data_dir = data_dir.clone();
    let settings = Arc::new(settings);
    let source = Arc::new(wr_infra::sqlite_config::SqliteConfigSource::new(
        &settings.data_dir,
    ));
    let store = Arc::new(
        wr_infra::sqlite::SqliteStore::open(&data_dir)
            .await
            .unwrap(),
    );
    let deps = Arc::new(wr_server::gateway::GatewayDeps::new(
        settings.clone(),
        source,
        store,
    ));
    let app = wr_server::gateway_router(settings, deps);
    // 错误体在提交前以真实 HTTP 状态返回;落库终态仍是 400。
    let (status, body, _) = send_json(
        app,
        "/v1/chat/completions",
        json!({
            "model": "prov-test/m-test",
            "stream": true,
            "messages": [{"role": "user", "content": "hi"}]
        }),
        &[("authorization", "Bearer sk-test-1")],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let mut bodies: Option<(Option<String>, Option<String>)> = None;
    for _ in 0..100 {
        if let Some((request_body, response_body)) = read_latest_log_bodies(&data_dir)
            && request_body
                .as_deref()
                .is_some_and(|text| text.contains("m-test"))
        {
            bodies = Some((request_body, response_body));
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let (request_body, response_body) = bodies.expect("失败行应落侧表正文");
    assert!(
        request_body
            .as_deref()
            .is_some_and(|text| text.contains("m-test")),
        "失败行应捕获请求正文: {request_body:?}"
    );
    assert!(
        !request_body
            .as_deref()
            .unwrap_or_default()
            .contains("sk-test-1"),
        "凭据不得进入捕获正文: {request_body:?}"
    );
    assert!(
        response_body
            .as_deref()
            .is_some_and(|text| text.contains("budget_tokens")),
        "失败行应捕获上游错误响应: {response_body:?}"
    );
    let _ = std::fs::remove_dir_all(data_dir);
}

/// 成功流式请求的正文捕获必须保留 SSE 块间 `\n\n` 分隔:此前按
/// `event.raw` 直接拼接,整条流的捕获文本退化成单行连排(前端按行
/// 解析全灭,只能靠括号平衡扫描兜底,详情页「内容预览/JSON 树」失效)。
#[tokio::test]
async fn streaming_capture_preserves_sse_block_separators() {
    let server = MockServer::start().await;
    let sse = concat!(
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m-test\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"He\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m-test\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"llo\"},\"finish_reason\":null}]}\n\n",
        "data: [DONE]\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream"))
        .mount(&server)
        .await;
    let data_dir = temp_dir("sse-separator-capture");
    seed_config_with_observability(
        &data_dir,
        json!([provider_entry(&server.uri(), json!({"id": "prov-test"}))]),
        json!([{"key": "sk-test-1", "enabled": true, "name": "t"}]),
        json!({"callLogs": true, "captureBodies": true}),
    );
    let mut settings = test_settings();
    settings.data_dir = data_dir.clone();
    let settings = Arc::new(settings);
    let source = Arc::new(wr_infra::sqlite_config::SqliteConfigSource::new(
        &settings.data_dir,
    ));
    let store = Arc::new(
        wr_infra::sqlite::SqliteStore::open(&data_dir)
            .await
            .unwrap(),
    );
    let deps = Arc::new(wr_server::gateway::GatewayDeps::new(
        settings.clone(),
        source,
        store,
    ));
    let app = wr_server::gateway_router(settings, deps);
    let (status, _, _) = send_json(
        app,
        "/v1/chat/completions",
        json!({
            "model": "prov-test/m-test",
            "stream": true,
            "messages": [{"role": "user", "content": "hi"}]
        }),
        &[("authorization", "Bearer sk-test-1")],
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let mut captured: Option<String> = None;
    for _ in 0..100 {
        if let Some((_, response_body)) = read_latest_log_bodies(&data_dir)
            && let Some(text) = response_body
            && text.contains("chat.completion.chunk")
        {
            captured = Some(text);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let text = captured.expect("成功流式行应捕获响应正文");
    // 块间分隔保留:每个 data: 帧独占一行,帧间以空行分隔。
    let data_lines = text
        .lines()
        .filter(|line| line.starts_with("data:"))
        .count();
    assert_eq!(data_lines, 3, "捕获应为多行 SSE 文本: {text}");
    assert!(
        text.contains("}\n\ndata:"),
        "帧间应保留 \\n\\n 分隔: {text}"
    );
    assert!(text.contains("data: [DONE]"), "[DONE] 哨兵应保留: {text}");
    let _ = std::fs::remove_dir_all(data_dir);
}
