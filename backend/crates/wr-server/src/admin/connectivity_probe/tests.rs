use super::*;

#[test]
fn custom_test_targets_are_validated() {
    let mut payload = Map::new();
    payload.insert(
        "targets".to_owned(),
        json!([
            "https://a.com",
            "ftp://b.com",
            "   ",
            "https://a.com",
            "https://c.com"
        ]),
    );
    // 仅收 http/https,去重保序。
    let parsed = parse_custom_targets(&payload).unwrap();
    assert_eq!(
        parsed,
        vec!["https://a.com".to_owned(), "https://c.com".to_owned()]
    );
    // 缺省/全部非法 → None(回落内置目标)。
    assert!(parse_custom_targets(&Map::new()).is_none());
    payload.insert("targets".to_owned(), json!(["ftp://x.com"]));
    assert!(parse_custom_targets(&payload).is_none());
}

#[test]
fn normalize_model_name_collapses_separators() {
    assert_eq!(
        normalize_model_name(&Value::String("GPT_4o..mini ".to_owned())),
        "gpt-4o-mini"
    );
    assert_eq!(normalize_model_name(&Value::Null), "");
    assert_eq!(normalize_model_name(&Value::String("-x-".to_owned())), "x");
}

#[test]
fn model_echo_matches_dated_alias_but_not_sibling() {
    assert!(model_echo_matches(
        "deepseek-v4-flash",
        "DeepSeek-V4-Flash-GA-260731"
    ));
    assert!(!model_echo_matches("gpt-4o", "gpt-4o-mini"));
    assert!(model_echo_matches("gpt-4o", "gpt-4o"));
    assert!(!model_echo_matches("", "x"));
}

#[test]
fn strip_endpoint_suffix_rules() {
    assert_eq!(
        strip_endpoint_suffix("https://api.deepseek.com/chat/completions"),
        "https://api.deepseek.com"
    );
    assert_eq!(
        strip_endpoint_suffix("https://x.example/v1/chat/completions/"),
        "https://x.example/v1"
    );
    assert_eq!(
        strip_endpoint_suffix("https://x.example/v1/messages"),
        "https://x.example/v1"
    );
    assert_eq!(
        strip_endpoint_suffix("https://x.example/"),
        "https://x.example"
    );
}

// ---- 20261017_01:fetchProviderModels 纯函数 --------------------------

#[test]
fn model_list_candidates_matrix() {
    // OpenAI 系:根地址 → /v1/models 回落 /models;/v1 结尾 → /models。
    assert_eq!(
        model_list_candidates("https://api.example.com", "openai_chat_completions"),
        vec![
            "https://api.example.com/v1/models".to_owned(),
            "https://api.example.com/models".to_owned()
        ]
    );
    assert_eq!(
        model_list_candidates("https://api.example.com/v1", "openai_responses"),
        vec!["https://api.example.com/v1/models".to_owned()]
    );
    // Anthropic 同 OpenAI 端点链(差异只在认证头)。
    assert_eq!(
        model_list_candidates("https://x.example", "anthropic_messages"),
        vec![
            "https://x.example/v1/models".to_owned(),
            "https://x.example/models".to_owned()
        ]
    );
    // Gemini:根 → /v1beta/models;含 /v1(含 /v1beta)→ 同版本段 /models。
    assert_eq!(
        model_list_candidates(
            "https://generativelanguage.googleapis.com",
            "gemini_generate_content"
        ),
        vec!["https://generativelanguage.googleapis.com/v1beta/models".to_owned()]
    );
    assert_eq!(
        model_list_candidates("https://x.example/v1beta", "gemini_interactions"),
        vec!["https://x.example/v1beta/models".to_owned()]
    );
    // 未知协议按 OpenAI 系兜底。
    assert_eq!(
        model_list_candidates("https://x.example/v1", "future_format"),
        vec!["https://x.example/v1/models".to_owned()]
    );
}

#[test]
fn parse_gemini_models_page_strips_prefix_and_falls_back() {
    let (models, token) = parse_gemini_models_page(&json!({
        "models": [{"name": "models/gemini-2.0-flash"}, {"name": "gemini-1.5"}, {"name": " "}],
        "nextPageToken": "cursor-2"
    }));
    assert_eq!(
        models,
        vec!["gemini-2.0-flash".to_owned(), "gemini-1.5".to_owned()]
    );
    assert_eq!(token.as_deref(), Some("cursor-2"));
    // 末页:token 空/缺省 → None。
    let (_, token) = parse_gemini_models_page(&json!({"models": []}));
    assert!(token.is_none());
    // 非分页形态 → 通用抽取(data[] 的 id/model/name)。
    let (models, token) = parse_gemini_models_page(&json!({"data": [{"id": "a"}, "b"]}));
    assert_eq!(models, vec!["a".to_owned(), "b".to_owned()]);
    assert!(token.is_none());
}

#[test]
fn dedup_preserves_first_seen_order() {
    assert_eq!(
        dedup_preserve_order(vec![
            "a".to_owned(),
            "b".to_owned(),
            "a".to_owned(),
            "c".to_owned()
        ]),
        vec!["a".to_owned(), "b".to_owned(), "c".to_owned()]
    );
}

#[test]
fn list_auth_headers_by_shape() {
    // Bearer:有密钥带 authorization,缺密钥不带。
    assert_eq!(
        ListAuth::Bearer.headers("sk"),
        vec![("authorization", "Bearer sk".to_owned())]
    );
    assert!(ListAuth::Bearer.headers("").is_empty());
    // Anthropic:x-api-key + 固定版本头。
    assert_eq!(
        ListAuth::Anthropic.headers("sk"),
        vec![
            ("x-api-key", "sk".to_owned()),
            ("anthropic-version", "2023-06-01".to_owned())
        ]
    );
    // Gemini:x-goog-api-key。
    assert_eq!(
        ListAuth::Gemini.headers("sk"),
        vec![("x-goog-api-key", "sk".to_owned())]
    );
}

#[test]
fn output_text_and_usage_shapes() {
    let openai = json!({"choices": [{"message": {"content": "  ok  "}}]});
    assert_eq!(response_output_text("openai", &openai), "ok");
    let anthropic = json!({"content": [{"type": "text", "text": "ok"}]});
    assert_eq!(response_output_text("anthropic", &anthropic), "ok");
    let responses = json!({"output": [{"content": [{"type": "output_text", "text": "ok"}]}]});
    assert_eq!(response_output_text("openai_responses", &responses), "ok");
    // usage:prompt/completion 命中;全 0 → null。
    let usage = json!({"usage": {"input_tokens": 5, "output_tokens": 2}});
    assert_eq!(
        response_usage(&usage),
        json!({"prompt_tokens": 5, "completion_tokens": 2})
    );
    assert_eq!(response_usage(&json!({"usage": {}})), Value::Null);
    assert!(response_has_usage(&usage));
    assert!(!response_has_usage(&json!({"usage": {"total_tokens": 0}})));
}

/// 2026-10-13:Responses 探针体必须显式 `stream: false`。省略时部分
/// 上游(agnes)按流式处理并回 500 `bad_response_body`,原生 Responses
/// 供应商恒判不可用。
#[test]
fn responses_probe_body_forces_non_stream() {
    let body = probe_body("openai_responses", "m");
    assert_eq!(body["stream"], Value::Bool(false));
    assert_eq!(body["input"], json!(PROBE_PROMPT));
    assert!(body.get("messages").is_none());
    // chat 形态不带 stream 键(与既有行为一致)。
    let chat = probe_body("openai", "m");
    assert!(chat.get("stream").is_none());
    assert!(chat.get("messages").is_some());
}

#[test]
fn gemini_format_short_circuits() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let value = runtime.block_on(probe_model_protocol(
        "https://x.example",
        "",
        "gemini",
        "m",
        None,
        false,
        None,
    ));
    assert_eq!(value["ok"], Value::Bool(false));
    assert_eq!(
        value["message"],
        "This API format does not support real chat probing."
    );
    runtime.shutdown_background();
}

// ---- 20260906_01 B5:探针的供应商级代理覆盖 --------------------------

fn probe_proxy_section() -> Map<String, Value> {
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
    .as_object()
    .unwrap()
    .clone()
}

#[test]
fn payload_proxy_override_drives_probe_decision() {
    let global = probe_proxy_section();
    let url = "https://api.upstream.test/v1/models";
    let payload_map = |value: Value| value.as_object().unwrap().clone();

    // 决策 #9:载荷缺省 → 回落全局默认(向后兼容)。
    let merged = with_payload_override(Some(&global), &Map::new());
    assert_eq!(
        probe_proxy_decision(merged.as_ref(), url),
        ProxyDecision::Url("http://gw.corp:8080".to_owned())
    );
    assert_eq!(
        probe_proxy_label(merged.as_ref(), url),
        "preset:公司网关@gw.corp:8080"
    );

    // 载荷 direct → 直连(草稿里选了「不走代理」就按它测)。
    let merged = with_payload_override(
        Some(&global),
        &payload_map(json!({"proxy": {"mode": "direct"}})),
    );
    assert_eq!(
        probe_proxy_decision(merged.as_ref(), url),
        ProxyDecision::Direct
    );
    assert_eq!(probe_proxy_label(merged.as_ref(), url), "direct");

    // 载荷 preset → 该预设;标签掩掉凭据(决策 #10)。
    let merged = with_payload_override(
        Some(&global),
        &payload_map(json!({"proxy": {"mode": "preset", "presetId": "clash"}})),
    );
    assert_eq!(
        probe_proxy_decision(merged.as_ref(), url),
        ProxyDecision::Url("http://proxy-user:super-secret@127.0.0.1:7890".to_owned())
    );
    let label = probe_proxy_label(merged.as_ref(), url);
    assert_eq!(label, "preset:Clash@127.0.0.1:7890");
    assert!(!label.contains("super-secret") && !label.contains("proxy-user"));

    // loopback 目标仍最高优先(本地 Ollama 不被代理打环)。
    assert_eq!(
        probe_proxy_decision(merged.as_ref(), "http://127.0.0.1:11434/v1"),
        ProxyDecision::Direct
    );

    // 全局快照取回失败(None)+ 载荷指向不存在的预设 → 安全回落直连。
    let merged = with_payload_override(
        None,
        &payload_map(json!({"proxy": {"mode": "preset", "presetId": "clash"}})),
    );
    assert_eq!(
        probe_proxy_decision(merged.as_ref(), url),
        ProxyDecision::Direct
    );
    // 非对象的 proxy 字段被忽略(宽松)。
    let merged = with_payload_override(Some(&global), &payload_map(json!({"proxy": "clash"})));
    assert_eq!(
        probe_proxy_decision(merged.as_ref(), url),
        ProxyDecision::Url("http://gw.corp:8080".to_owned())
    );
}

#[test]
fn test_proxy_preset_validates_input_before_any_request() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    // 缺 preset / id 空 / 非对象 → Err(RPC 500),且不发任何请求。
    for payload in [
        json!({}),
        json!({"preset": null}),
        json!({"preset": "clash"}),
        json!({"preset": {"name": "Clash", "server": "127.0.0.1", "port": 7890}}),
    ] {
        let error = runtime
            .block_on(test_proxy_preset(payload.as_object().unwrap()))
            .unwrap_err();
        assert!(error.contains("preset"), "{payload} → {error}");
    }
    // server 空 / scheme 非白名单 / 端口非法 → Err(不联网)。
    for preset in [
        json!({"id": "p", "name": "n", "server": "", "port": 7890}),
        json!({"id": "p", "name": "n", "server": "ftp://proxy.lan", "port": 21}),
        json!({"id": "p", "name": "n", "server": "proxy.lan", "port": 0}),
    ] {
        let error = runtime
            .block_on(test_proxy_preset(
                json!({"preset": preset}).as_object().unwrap(),
            ))
            .unwrap_err();
        assert!(error.contains("invalid"), "{preset} → {error}");
    }
    runtime.shutdown_background();
}
