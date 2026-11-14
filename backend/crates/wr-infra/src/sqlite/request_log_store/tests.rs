use super::*;
use serde_json::json;

fn provider_map(protocol: &str, style: Option<&str>) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert("id".to_owned(), json!("p"));
    map.insert("protocol".to_owned(), json!(protocol));
    if let Some(style) = style {
        map.insert("type".to_owned(), json!(style));
    }
    map
}

/// 20261017_02 批5:retryAttempts 瘦身剥离 —— 采集四件套出主行、
/// 补 captured 标记、侧表行按 ordinal 对齐;无采集的行为不变。
#[test]
fn split_attempt_captures_extracts_capture_keys() {
    let payload = json!({
        "protocol": "openai",
        "model": "m1",
        "retry_attempts": [
            {"attempt_id": "p#1", "status": 0, "error": "boom"},
            {
                "attempt_id": "p#2", "status": 200,
                "upstream_request": "{\"model\":\"m1\"}",
                "upstream_response": "{\"id\":\"c1\"}",
                "upstream_headers": {"content-type": "application/json"},
                "conversion": {"strategy": "semantic"},
                "upstream_response_size": 12,
            },
        ],
    });
    let (text, rows) = split_attempt_captures(&payload);
    assert_eq!(rows.len(), 1, "只有第二个 attempt 有采集");
    assert_eq!(rows[0].0, 1, "ordinal = 数组下标");
    let captured: Value = serde_json::from_str(&rows[0].1).unwrap();
    assert_eq!(captured["upstream_request"], json!("{\"model\":\"m1\"}"));

    let slim: Value = serde_json::from_str(&text).unwrap();
    let slim_attempts = slim["retry_attempts"].as_array().unwrap();
    assert_eq!(slim_attempts.len(), 2);
    assert_eq!(slim_attempts[1]["captured"], json!(true), "瘦身项带标记");
    assert!(slim_attempts[1].get("upstream_request").is_none(), "正文出主行");
    assert_eq!(slim_attempts[1]["upstream_response_size"], json!(12), "轻量标记保留");
    assert_eq!(slim_attempts[1]["attempt_id"], json!("p#2"), "元数据保留");
    assert_eq!(slim_attempts[0]["error"], json!("boom"), "无采集项原样");
    assert!(slim_attempts[0].get("captured").is_none(), "未捕获项无标记");

    // 无 retry_attempts 键的 payload 原样返回、无侧表行。
    let plain = json!({"protocol": "openai"});
    let (text, rows) = split_attempt_captures(&plain);
    assert!(rows.is_empty());
    let roundtrip: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(roundtrip, plain);
}

/// 20261017_02 批5:并回只认侧表 —— 有侧表行时整项替换(含旧内嵌项)。
#[test]
fn with_split_attempts_merges_from_side_table_only() {
    let slim_item = json!({"attempt_id": "p#2", "captured": true, "status": 200});
    let inline_item = json!({
        "attempt_id": "p#3", "status": 200,
        "upstream_request": "{\"model\":\"legacy\"}",
    });
    let mut payload = json!({"retry_attempts": [json!({"attempt_id": "p#1"}), slim_item.clone(), inline_item.clone()]});
    let rows = vec![
        (
            1i64,
            r#"{"attempt_id":"p#2","status":200,"upstream_request":"{\"model\":\"m1\"}"}"#.to_owned(),
        ),
        (
            2i64,
            r#"{"attempt_id":"p#3","status":200,"upstream_request":"{\"model\":\"side\"}"}"#.to_owned(),
        ),
    ];
    with_split_attempts(&mut payload, &rows);
    let attempts = payload["retry_attempts"].as_array().unwrap();
    assert_eq!(attempts[0]["attempt_id"], json!("p#1"), "无侧表行的不动");
    assert_eq!(
        attempts[1]["upstream_request"], json!("{\"model\":\"m1\"}"),
        "瘦身项被侧表行整项替换"
    );
    assert_eq!(
        attempts[2]["upstream_request"], json!("{\"model\":\"side\"}"),
        "有侧表行时以侧表为准"
    );

    // 侧表行 ordinal 越界静默;无 retry_attempts 键的 payload 不动。
    let mut plain = json!({"protocol": "openai"});
    with_split_attempts(&mut plain, &[(0, r#"{"a":1}"#.to_owned())]);
    assert_eq!(plain, json!({"protocol": "openai"}));
}

/// 2026-10-13:有效上游协议 = 家族 + 线格式。只读 `protocol` 会把
/// 所有 openai 家族供应商显示成 `OpenAI Compatible`,原生 Responses
/// 供应商的转换链路因此错乱。
#[test]
fn effective_protocol_merges_family_and_style() {
    assert_eq!(
        effective_provider_protocol(&provider_map("openai", Some("openai_responses"))),
        "openai_responses"
    );
    assert_eq!(
        effective_provider_protocol(&provider_map("openai", Some("openai_chat_completions"))),
        "openai"
    );
    assert_eq!(effective_provider_protocol(&provider_map("openai", None)), "openai");
    assert_eq!(
        effective_provider_protocol(&provider_map("anthropic", Some("anthropic_messages"))),
        "anthropic"
    );
    assert_eq!(effective_provider_protocol(&provider_map("gemini", None)), "gemini");
    // 家族缺失 → 默认 openai(镜像 Provider::from_raw 缺省)。
    assert_eq!(effective_provider_protocol(&provider_map("", None)), "openai");
}

/// 转换链路:客户端标签 → 有效上游标签;同协议只给客户端标签。
#[test]
fn conversion_chain_uses_effective_upstream_protocol() {
    let mut protocols = Map::new();
    protocols.insert("chat".to_owned(), json!("openai"));
    protocols.insert("resp".to_owned(), json!("openai_responses"));
    protocols.insert("anth".to_owned(), json!("anthropic"));

    let chain = |client: &str, provider: &str| {
        conversion_chain(
            Some(&json!(client)),
            Some(&json!(provider)),
            None,
            None,
            &protocols,
        )
        .unwrap_or_default()
    };
    assert_eq!(chain("openai", "chat"), "OpenAI Compatible");
    assert_eq!(chain("openai", "resp"), "OpenAI Compatible → OpenAI Responses");
    assert_eq!(chain("openai", "anth"), "OpenAI Compatible → Anthropic");
    assert_eq!(chain("openai_responses", "chat"), "OpenAI Responses → OpenAI Compatible");
    assert_eq!(chain("openai_responses", "resp"), "OpenAI Responses");
    assert_eq!(chain("anthropic", "anth"), "Anthropic");
    assert_eq!(chain("anthropic", "resp"), "Anthropic → OpenAI Responses");
}

/// 2026-10-14:请求时刻落库的 `provider_protocol` 优先于当前配置现算
/// —— 历史行不随供应商 type 修改而漂移;无存量键的旧行保持现算回落。
#[test]
fn conversion_chain_prefers_stored_request_time_protocol() {
    let mut protocols = Map::new();
    protocols.insert("agnes".to_owned(), json!("openai_responses"));

    let chain = |stored: Option<&str>| {
        let stored = stored.map(|text| json!(text));
        conversion_chain(
            Some(&json!("anthropic")),
            Some(&json!("agnes")),
            None,
            stored.as_ref(),
            &protocols,
        )
        .unwrap_or_default()
    };
    // 请求当时上游是 anthropic 原生(直连),即使当前配置已改成
    // openai_responses,该行也应显示客户端标签本身。
    assert_eq!(chain(Some("anthropic")), "Anthropic");
    assert_eq!(chain(Some("openai")), "Anthropic → OpenAI Compatible");
    // 旧行无存量键 → 回落当前配置(历史行为不变)。
    assert_eq!(chain(None), "Anthropic → OpenAI Responses");
}
