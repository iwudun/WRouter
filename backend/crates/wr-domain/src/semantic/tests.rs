
use super::*;
use crate::protocol::{
    endpoint, format_error_for_protocol, try_decode_response, upstream_payload,
};
use crate::provider::Provider;
use serde_json::json;

fn provider(protocol: &str, style: Option<&str>) -> Provider {
    let mut raw = serde_json::Map::new();
    raw.insert("id".to_owned(), json!("p"));
    raw.insert("api_base_url".to_owned(), json!("https://upstream.example/v1"));
    raw.insert("protocol".to_owned(), json!(protocol));
    if let Some(style) = style {
        raw.insert("type".to_owned(), json!(style));
    }
    Provider::from_raw(&raw).unwrap()
}

/// 测试便利包装:成功转换直接取值;失败路径必须断言
/// [`try_decode_response`] 的 Err,不得走本包装。
fn decode_response(status: u16, value: &Value, protocol: &str, provider: &Provider) -> Value {
    try_decode_response(status, value, protocol, provider).unwrap()
}

// -----------------------------------------------------------------------
// B1:模型与诊断单测
// -----------------------------------------------------------------------

#[test]
fn extensions_store_by_source() {
    let mut extensions = Extensions::default();
    extensions.insert("gemini", "thoughtSignature", json!("sig"));
    extensions.insert("gemini", "safetyRatings", json!([{"a": 1}]));
    extensions.insert("openai", "item_id", json!("fc_1"));
    assert_eq!(
        extensions.get("gemini", "thoughtSignature"),
        Some(&json!("sig"))
    );
    assert!(extensions.get("anthropic", "x").is_none());
}

#[test]
fn diagnostic_report_freezes_codes() {
    let mut report = ConversionReport::semantic();
    report.push(DiagnosticCode::DroppedField, "store", "服务端状态不跨族");
    report.push_dropped(
        DiagnosticCode::DowngradedContent,
        "arguments",
        "非法 JSON",
        json!({"big": "x".repeat(600)}),
    );
    assert!(report.has_code(DiagnosticCode::DroppedField));
    assert!(report.has_code(DiagnosticCode::DowngradedContent));
    // dropped 摘要截断。
    let dropped = report.diagnostics[1].dropped.as_ref().unwrap();
    assert!(dropped.as_str().unwrap().ends_with("(truncated)"));
}

#[test]
fn tool_arguments_dual_form() {
    let parsed = ToolArguments::Parsed(json!({"a": 1}));
    assert_eq!(parsed.as_object(), (json!({"a": 1}), false));
    assert_eq!(parsed.as_raw_string(), "{\"a\": 1}");
    let raw = ToolArguments::Raw("not-json".to_owned());
    let (value, downgraded) = raw.as_object();
    assert_eq!(value, json!({"raw": "not-json"}));
    assert!(downgraded);
}

#[test]
fn finish_reason_mapping_table() {
    let mut report = ConversionReport::semantic();
    assert_eq!(
        finish::from_gemini(Some("STOP"), &mut report, "f"),
        FinishReason::Stop
    );
    assert_eq!(
        finish::from_gemini(Some("MAX_TOKENS"), &mut report, "f"),
        FinishReason::Length
    );
    // gemini→anthropic 的 MAX_TOKENS 落 end_turn 为旧实现缺口;映射表修复。
    assert_eq!(finish::to_anthropic(&FinishReason::Length), "max_tokens");
    assert_eq!(finish::to_anthropic(&FinishReason::Stop), "end_turn");
    assert_eq!(
        finish::to_openai_chat(&FinishReason::ToolCall),
        "tool_calls"
    );
    // 未知枚举:Opaque + 诊断,openai 侧小写透传(镜像旧行为)。
    let opaque = finish::from_gemini(Some("PROHIBITED"), &mut report, "f");
    assert!(report.has_code(DiagnosticCode::UnmappedEnum));
    assert_eq!(opaque, FinishReason::Opaque("PROHIBITED".to_owned()));
    assert_eq!(finish::to_openai_chat(&opaque), "prohibited");
}

// -----------------------------------------------------------------------
// B6:矩阵门禁与透传格
// -----------------------------------------------------------------------

#[test]
fn gate_rejects_unsupported_combinations() {
    let anthropic = provider("anthropic", None);
    let gemini = provider("gemini", None);
    // gap 3:embeddings × anthropic → unsupported(旧:clone 透传 + /messages)。
    let err =
        upstream_payload("openai_embeddings", &json!({"input": "x"}), &anthropic).unwrap_err();
    assert_eq!(err.envelope("openai_embeddings", None).0, 502);
    // gap 4:embeddings × gemini → unsupported(旧:空 contents 畸变)。
    assert!(upstream_payload("openai_embeddings", &json!({"input": "x"}), &gemini).is_err());
    // gap 2:interactions × 非 gemini → unsupported(旧:clone 透传)。
    let openai = provider("openai", None);
    assert!(upstream_payload("gemini_interactions", &json!({}), &openai).is_err());
    // 支持格不受影响。
    assert!(upstream_payload("openai_embeddings", &json!({"input": "x"}), &openai).is_ok());
    assert!(upstream_payload("gemini_interactions", &json!({"input": "hi"}), &gemini).is_ok());
}

#[test]
fn passthrough_cells_keep_wire_fidelity() {
    let openai = provider("openai", None);
    let payload =
        json!({"model": "m", "messages": [{"role": "user", "content": "hi"}], "custom": true});
    assert_eq!(
        upstream_payload("openai", &payload, &openai).unwrap(),
        payload
    );
    let native = provider("openai", Some("openai_responses"));
    let responses_payload = json!({"model": "m", "input": "hi"});
    assert_eq!(
        upstream_payload("openai_responses", &responses_payload, &native).unwrap(),
        responses_payload
    );
    let anthropic = provider("anthropic", None);
    let anthropic_payload = json!({"model": "m", "max_tokens": 8, "messages": []});
    assert_eq!(
        upstream_payload("anthropic", &anthropic_payload, &anthropic).unwrap(),
        anthropic_payload
    );
    let gemini = provider("gemini", None);
    let gemini_payload = json!({"contents": []});
    assert_eq!(
        upstream_payload("gemini", &gemini_payload, &gemini).unwrap(),
        gemini_payload
    );
}

#[test]
fn gemini_client_urls_follow_target_protocol() {
    // gap 1 修复:gemini 客户端 × 非 gemini 上游走常规端点。
    let openai = provider("openai", None);
    assert_eq!(
        endpoint("gemini", &openai, "m", false),
        "https://upstream.example/v1/chat/completions"
    );
    let anthropic = provider("anthropic", None);
    assert_eq!(
        endpoint("gemini", &anthropic, "m", false),
        "https://upstream.example/v1/messages"
    );
    let native = provider("openai", Some("openai_responses"));
    assert_eq!(
        endpoint("gemini", &native, "m", false),
        "https://upstream.example/v1/responses"
    );
    // gemini 上游保持 gemini 形态 URL。
    let gemini = provider("gemini", None);
    assert_eq!(
        endpoint("gemini", &gemini, "m", false),
        "https://upstream.example/v1/models/m:generateContent"
    );
    // E2:gemini_stream × 非 gemini 上游按目标协议选端点(请求体
    // 经语义转换),不再保留 Gemini URL 兼容分支。
    assert_eq!(
        endpoint("gemini_stream", &openai, "m", true),
        "https://upstream.example/v1/chat/completions"
    );
    assert_eq!(
        endpoint("gemini_stream", &gemini, "m", true),
        "https://upstream.example/v1/models/m:streamGenerateContent"
    );
}

// -----------------------------------------------------------------------
// B8:请求面 differential(新语义管线 vs 旧两两转换)
// -----------------------------------------------------------------------

// B8 differential 测试的「旧实现」半边已随阶段 E2 删除;这些测试
// 改为钉定语义管线的输出形状(迁移期基线由 golden replay 承接)。
#[test]
fn semantic_multi_system_to_anthropic_encodes_block_array() {
    // 回归 2026-10-14:多条 system 归并曾复用单值编码,产出
    // ["str",…] 裸字符串数组 —— Anthropic wire 要求数组元素为 block
    // 对象,DeepSeek anthropic 端点实测 422 "expected internally
    // tagged enum TextBlockParam"(请求 3fd4b7c1)。
    let payload = json!({
        "model": "m",
        "messages": [
            {"role": "system", "content": "be nice"},
            {"role": "system", "content": "be terse"},
            {"role": "user", "content": "hi"},
        ],
        "max_tokens": 32,
    });
    let new = upstream_payload("openai", &payload, &provider("anthropic", None)).unwrap();
    let system = new["system"].as_array().expect("多条 system 应编码为数组");
    assert_eq!(system.len(), 2);
    for block in system {
        assert_eq!(block["type"], json!("text"));
        assert!(
            block["text"].is_string(),
            "数组元素必须是 text block,不得为裸字符串"
        );
    }
    // 单条 system 保持字符串形态(镜像旧值形状,合法)。
    let single = json!({
        "model": "m",
        "messages": [
            {"role": "system", "content": "be nice"},
            {"role": "user", "content": "hi"},
        ],
        "max_tokens": 32,
    });
    let new = upstream_payload("openai", &single, &provider("anthropic", None)).unwrap();
    assert_eq!(new["system"], json!("be nice"));
}

#[test]
fn semantic_openai_to_anthropic_request_shape() {
    let payload = json!({
        "model": "m",
        "messages": [
            {"role": "system", "content": "be nice"},
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": "hello", "tool_calls": [
                {"id": "c1", "function": {"name": "get", "arguments": "{\"a\": 1}"}},
            ]},
            {"role": "tool", "tool_call_id": "c1", "content": "ok"},
        ],
        "max_tokens": 128,
        "temperature": 0.5,
        "tools": [{"type": "function", "function": {"name": "get", "description": "d", "parameters": {"type": "object"}}}],
        "tool_choice": "auto",
    });
    let new = upstream_payload("openai", &payload, &provider("anthropic", None)).unwrap();
    assert_eq!(new["model"], json!("m"));
    assert_eq!(new["max_tokens"], json!(128));
    assert_eq!(
        new["system"],
        json!("be nice"),
        "openai system 应升格为顶层 system"
    );
    let messages = new["messages"].as_array().unwrap();
    assert_eq!(messages[0]["content"][0]["text"], json!("hi"));
    let tool_use = &messages[1]["content"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["type"] == json!("tool_use"))
        .expect("tool_use 块应存在");
    assert_eq!(tool_use["id"], json!("c1"));
    assert_eq!(tool_use["input"], json!({"a": 1}), "参数应为已解析对象");
    let tool_result = &messages[2]["content"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["type"] == json!("tool_result"))
        .expect("tool_result 块应存在");
    assert_eq!(tool_result["tool_use_id"], json!("c1"));
}

// -------------------------------------------------------------------
// 思考意图跨族贯通(2026-10-14 审计缺陷 1–3 回归)
// -------------------------------------------------------------------

#[test]
fn semantic_anthropic_to_openai_thinking_maps_to_effort_and_drops_dialect_keys() {
    // 缺陷 1:anthropic 源的 thinking 原样重放进 chat 体(chat 线格式
    // 无此键),budget 不换算 reasoning_effort —— 思考意图反向断链。
    let payload = json!({
        "model": "m",
        "messages": [{"role": "user", "content": [{"type": "text", "text": "hi"}]}],
        "max_tokens": 128,
        "thinking": {"type": "enabled", "budget_tokens": 20000},
        "metadata": {"user_id": "u1"},
        "top_k": 40,
    });
    let new = upstream_payload("anthropic", &payload, &provider("openai", None)).unwrap();
    assert_eq!(
        new["reasoning_effort"],
        json!("high"),
        "budget 20000 应分档为 high"
    );
    for key in ["thinking", "metadata", "top_k"] {
        assert!(new.get(key).is_none(), "{key} 不应重放进 Chat 体: {new}");
    }
    // effort 直通源:responses 客户端的 reasoning.effort(缺陷 2 解码侧)。
    let responses = json!({
        "model": "m",
        "input": "hi",
        "reasoning": {"effort": "low"},
    });
    let new =
        upstream_payload("openai_responses", &responses, &provider("openai", None)).unwrap();
    assert_eq!(new["reasoning_effort"], json!("low"));
}

#[test]
fn semantic_responses_effort_reaches_anthropic_and_native_body() {
    // 缺陷 2 出码侧:responses 源 effort → anthropic thinking;原生
    // responses 出站写 reasoning.effort(旧白名单体整体丢弃)。
    let payload = json!({
        "model": "m",
        "input": "hi",
        "reasoning": {"effort": "medium"},
        "max_output_tokens": 20000,
    });
    let new =
        upstream_payload("openai_responses", &payload, &provider("anthropic", None)).unwrap();
    assert_eq!(
        new["thinking"],
        json!({"type": "enabled", "budget_tokens": 15000}),
        "{new}"
    );
    let new = upstream_payload(
        "openai_responses",
        &payload,
        &provider("openai", Some("openai_responses")),
    )
    .unwrap();
    assert_eq!(new["reasoning"], json!({"effort": "medium"}), "{new}");
}

#[test]
fn semantic_openai_effort_derives_gemini_thinking_budget_with_clamp() {
    // 缺陷 3:gemini 出站 effort 档位推导 thinkingBudget;上限 32768。
    let payload = json!({
        "model": "m",
        "messages": [{"role": "user", "content": "hi"}],
        "reasoning_effort": "high",
    });
    let new = upstream_payload("openai", &payload, &provider("gemini", None)).unwrap();
    assert_eq!(new["generationConfig"]["thinkingBudget"], json!(30000));
    let payload = json!({
        "model": "m",
        "messages": [{"role": "user", "content": "hi"}],
        "reasoning_effort": "xhigh",
    });
    let new = upstream_payload("openai", &payload, &provider("gemini", None)).unwrap();
    assert_eq!(new["generationConfig"]["thinkingBudget"], json!(32768));
}

#[test]
fn semantic_gemini_unknown_top_level_keys_leave_no_trace_but_diagnostics() {
    // 缺陷 4:gemini 入站未建模顶层键进 opaque;白名单出站丢弃且不再
    // 静默(safetySettings 等)。
    let payload = json!({
        "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
        "safetySettings": [{"category": "HARM_CATEGORY_X", "threshold": "BLOCK_NONE"}],
    });
    let (_, report) = crate::protocol::upstream_payload_with_report(
        "gemini",
        &payload,
        &provider("anthropic", None),
    )
    .unwrap();
    let report = report.expect("语义路径应有转换报告");
    assert!(
        report
            .diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::DroppedField && d.path == "safetySettings"),
        "{report:?}"
    );
}

// -------------------------------------------------------------------
// 三批修复回归(P1-1/2, P1-3, P2-5/6, P3-7/8/12;2026-10-14)
// -------------------------------------------------------------------

#[test]
fn anthropic_thinking_disables_custom_sampling() {
    // P1-1:Anthropic thinking 启用时不可自定义采样参数。
    let payload = json!({
        "model": "m",
        "messages": [{"role": "user", "content": "hi"}],
        "reasoning_effort": "high",
        "temperature": 0.5,
        "top_p": 0.9,
    });
    let new = upstream_payload("openai", &payload, &provider("anthropic", None)).unwrap();
    assert_eq!(new["thinking"]["type"], json!("enabled"));
    assert!(new.get("temperature").is_none(), "{new}");
    assert!(new.get("top_p").is_none(), "{new}");
    // 无思考意图:采样参数照常透传。
    let mut payload = payload.clone();
    payload["reasoning_effort"] = json!("none");
    let new = upstream_payload("openai", &payload, &provider("anthropic", None)).unwrap();
    assert_eq!(new["temperature"], json!(0.5));
    assert_eq!(new["top_p"], json!(0.9));
}

#[test]
fn anthropic_drops_unsigned_history_thinking() {
    // P1-2:openai 客户端 reasoning_content 无签名,Claude 校验会拒。
    let payload = json!({
        "model": "m",
        "messages": [
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": "hello", "reasoning_content": "因为…所以…"},
            {"role": "user", "content": "again"},
        ],
        "max_tokens": 128,
    });
    let new = upstream_payload("openai", &payload, &provider("anthropic", None)).unwrap();
    let assistant = &new["messages"][1];
    let blocks = assistant["content"].as_array().unwrap();
    assert!(
        blocks.iter().all(|b| b["type"] != json!("thinking")),
        "{blocks:?}"
    );
}

#[test]
fn tool_names_sanitized_for_anthropic_and_gemini() {
    // P1-3:MCP 名含 `.` 直发 anthropic/gemini 会 400。
    let payload = json!({
        "model": "m",
        "messages": [{"role": "user", "content": "hi"}],
        "tools": [{"type": "function", "function": {"name": "server.tool", "parameters": {"type": "object"}}}],
    });
    let new = upstream_payload("openai", &payload, &provider("anthropic", None)).unwrap();
    assert_eq!(new["tools"][0]["name"], json!("server_2E_tool"));
    let new = upstream_payload("openai", &payload, &provider("gemini", None)).unwrap();
    assert_eq!(
        new["tools"][0]["functionDeclarations"][0]["name"],
        json!("server_2E_tool")
    );
}

#[test]
fn developer_message_upgrades_to_system_on_anthropic() {
    // P2-5:anthropic 无 developer 角色,系统级指令不应降级为 user 轮。
    let payload = json!({
        "model": "m",
        "messages": [
            {"role": "developer", "content": "policy"},
            {"role": "user", "content": "hi"},
        ],
        "max_tokens": 32,
    });
    let new = upstream_payload("openai", &payload, &provider("anthropic", None)).unwrap();
    assert_eq!(new["system"], json!("policy"));
    assert!(
        new["messages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["role"] != json!("system"))
    );
}

#[test]
fn gemini3_models_use_thinking_level_generations() {
    // P2-6:gemini-3 分代(thinkingLevel 与 thinkingBudget 互斥)。
    let build = |model: &str, effort: &str| {
        let payload = json!({
            "model": model,
            "messages": [{"role": "user", "content": "hi"}],
            "reasoning_effort": effort,
        });
        upstream_payload("openai", &payload, &provider("gemini", None)).unwrap()
    };
    assert_eq!(
        build("gemini-3-pro", "high")["generationConfig"]["thinkingLevel"],
        json!("high")
    );
    assert!(
        build("gemini-3-pro", "high")["generationConfig"]
            .get("thinkingBudget")
            .is_none()
    );
    // medium 仅 3-flash/3.1-pro 支持,其余改档 high。
    assert_eq!(
        build("gemini-3-flash", "medium")["generationConfig"]["thinkingLevel"],
        json!("medium")
    );
    assert_eq!(
        build("gemini-3-pro", "medium")["generationConfig"]["thinkingLevel"],
        json!("high")
    );
    // minimal 仅 3-flash;3 系无法关闭 → 最低档+不展示。
    assert_eq!(
        build("gemini-3-flash", "minimal")["generationConfig"]["thinkingLevel"],
        json!("minimal")
    );
    let off = build("gemini-3-pro", "none")["generationConfig"].clone();
    assert_eq!(off["thinkingLevel"], json!("low"));
    assert_eq!(off["includeThoughts"], json!(false));
    // 2.5 系仍走 thinkingBudget。
    assert_eq!(
        build("gemini-2.5-pro", "high")["generationConfig"]["thinkingBudget"],
        json!(30000)
    );
}

#[test]
fn anthropic_parallel_tool_calls_maps_to_disable_switch() {
    // P3-7:parallel_tool_calls=false 的 anthropic 表达。
    let payload = json!({
        "model": "m",
        "messages": [{"role": "user", "content": "hi"}],
        "tool_choice": "auto",
        "parallel_tool_calls": false,
    });
    let new = upstream_payload("openai", &payload, &provider("anthropic", None)).unwrap();
    assert_eq!(new["tool_choice"]["disable_parallel_tool_use"], json!(true));
    // 无 tool_choice 时合成 auto 档承载开关。
    let mut payload = payload.clone();
    payload.as_object_mut().unwrap().remove("tool_choice");
    let new = upstream_payload("openai", &payload, &provider("anthropic", None)).unwrap();
    assert_eq!(
        new["tool_choice"],
        json!({"type": "auto", "disable_parallel_tool_use": true})
    );
}

#[test]
fn vendor_ultra_effort_maps_to_max_tier_and_parse_is_case_insensitive() {
    // P3-8/P3-12:ultra 并入最深档;"High" 大小写不敏感。
    let payload = json!({
        "model": "m",
        "messages": [{"role": "user", "content": "hi"}],
        "reasoning_effort": "ultra",
        "max_tokens": 64000,
    });
    let new = upstream_payload("openai", &payload, &provider("anthropic", None)).unwrap();
    assert_eq!(new["thinking"]["budget_tokens"], json!(60000));
    let payload = json!({
        "model": "m",
        "messages": [{"role": "user", "content": "hi"}],
        "reasoning_effort": "High",
        "max_tokens": 64000,
    });
    let new = upstream_payload("openai", &payload, &provider("anthropic", None)).unwrap();
    assert_eq!(new["thinking"]["budget_tokens"], json!(30000));
}

#[test]
fn negative_usage_reported_as_unknown() {
    // P2-4:负 usage(cache 折扣方言)视为未报告,不污染命中率/成本。
    let raw = json!({
        "id": "msg_1", "type": "message", "role": "assistant",
        "model": "m", "content": [{"type": "text", "text": "hi"}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": -5, "output_tokens": 3, "cache_read_input_tokens": -2},
    });
    let mut report = ConversionReport::semantic();
    let response = anthropic::decode_anthropic_message(&raw, &mut report);
    let usage = response.usage.expect("usage 应存在");
    assert!(usage.input.is_none());
    assert!(usage.cache_read.is_none());
    assert_eq!(usage.output, Some(3));
}

#[test]
fn semantic_anthropic_to_openai_request_shape() {
    let payload = json!({
        "model": "m",
        "system": "be nice",
        "messages": [
            {"role": "user", "content": [{"type": "text", "text": "hi"}]},
            {"role": "assistant", "content": [
                {"type": "text", "text": "hello"},
                {"type": "tool_use", "id": "c1", "name": "get", "input": {"a": 1}},
            ]},
            {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "c1", "content": "ok"}]},
        ],
        "max_tokens": 128,
        "tools": [{"name": "get", "description": "d", "input_schema": {"type": "object"}}],
        "tool_choice": {"type": "tool", "name": "get"},
    });
    let new = upstream_payload("anthropic", &payload, &provider("openai", None)).unwrap();
    assert_eq!(new["model"], json!("m"));
    assert_eq!(new["max_tokens"], json!(128));
    // 有意变更:system 升格为 messages[0],顶层 system 键不再残留。
    assert!(new.get("system").is_none());
    assert_eq!(
        new["messages"][0],
        json!({"role": "system", "content": "be nice"})
    );
    let tool_calls = &new["messages"][2]["tool_calls"];
    assert_eq!(tool_calls[0]["id"], json!("c1"));
    assert_eq!(tool_calls[0]["function"]["name"], json!("get"));
    assert_eq!(tool_calls[0]["function"]["arguments"], json!("{\"a\": 1}"));
    assert_eq!(new["messages"][3]["role"], json!("tool"));
    assert_eq!(new["messages"][3]["tool_call_id"], json!("c1"));
}

#[test]
fn semantic_responses_to_chat_request_shape() {
    let payload = json!({
        "model": "m",
        "instructions": "sys",
        "input": [
            {"type": "reasoning", "summary_text": "deep thought"},
            {"type": "function_call", "call_id": "call_9", "name": "f", "arguments": "{}"},
            {"type": "function_call_output", "call_id": "call_9", "output": "done"},
            {"type": "message", "role": "user", "content": "hi"},
        ],
        "max_output_tokens": 64,
        "tools": [{"type": "function", "name": "f", "parameters": {"type": "object"}}],
        "tool_choice": {"type": "function", "name": "f"},
    });
    let new =
        upstream_payload("openai_responses", &payload, &provider("openai", None)).unwrap();
    assert_eq!(new["model"], json!("m"));
    assert_eq!(new["max_tokens"], json!(64));
    let messages = new["messages"].as_array().unwrap();
    assert_eq!(messages[0]["role"], json!("system"));
    assert_eq!(messages[0]["content"], json!("sys"));
    assert_eq!(
        messages[1]["reasoning_content"],
        json!("deep thought"),
        "reasoning summary 应保留为 reasoning_content"
    );
    // reasoning 与 function_call 合并进同一条 assistant 消息。
    assert_eq!(messages[1]["tool_calls"][0]["id"], json!("call_9"));
    assert_eq!(messages[2]["role"], json!("tool"));
    assert_eq!(messages[2]["tool_call_id"], json!("call_9"));
    assert_eq!(messages[2]["content"], json!("done"));
    assert_eq!(messages[3]["content"], json!("hi"));
}

#[test]
fn semantic_chat_to_responses_request_shape() {
    let payload = json!({
        "model": "m",
        "messages": [
            {"role": "system", "content": "be nice"},
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": "yo", "reasoning_content": "think"},
            {"role": "tool", "tool_call_id": "c1", "content": "ok"},
        ],
        "max_tokens": 32,
        "tools": [{"type": "function", "function": {"name": "f", "parameters": {"type": "object"}}}],
    });
    let new = upstream_payload(
        "openai",
        &payload,
        &provider("openai", Some("openai_responses")),
    )
    .unwrap();
    assert_eq!(new["model"], json!("m"));
    assert_eq!(new["max_output_tokens"], json!(32));
    assert_eq!(new["instructions"], json!("be nice"));
    let input = new["input"].as_array().unwrap();
    assert_eq!(input[0]["content"], json!("hi"));
    assert_eq!(input[3]["type"], json!("function_call_output"));
    assert_eq!(input[3]["call_id"], json!("c1"));
}

#[test]
fn semantic_openai_to_gemini_request_shape() {
    let payload = json!({
        "model": "m",
        "messages": [
            {"role": "system", "content": "be nice"},
            {"role": "user", "content": "hi"},
            {"role": "assistant", "tool_calls": [{"id": "c1", "function": {"name": "get", "arguments": "{\"a\":1}"}}]},
            {"role": "tool", "tool_call_id": "c1", "content": "ok"},
        ],
        "tools": [{"type": "function", "function": {"name": "get", "parameters": {"type": "object"}}}],
        "tool_choice": "required",
        "max_tokens": 128,
    });
    let new = upstream_payload("openai", &payload, &provider("gemini", None)).unwrap();
    // system 不升格 systemInstruction(Python 行为保留):按普通消息
    // 进 contents(role 归一为 user)。
    let contents = new["contents"].as_array().unwrap();
    assert_eq!(contents[0]["parts"][0]["text"], json!("be nice"));
    assert_eq!(contents[1]["parts"][0]["text"], json!("hi"));
    let call = &contents[2]["parts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p.get("functionCall").is_some())
        .expect("functionCall part 应存在");
    assert_eq!(call["functionCall"]["name"], json!("get"));
    assert_eq!(call["functionCall"]["args"], json!({"a": 1}));
    let response = &contents[3]["parts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p.get("functionResponse").is_some())
        .expect("functionResponse part 应存在");
    assert_eq!(response["functionResponse"]["name"], json!("get"));
    assert_eq!(new["generationConfig"]["maxOutputTokens"], json!(128));
    assert!(new["tools"][0]["functionDeclarations"].is_array());
    assert_eq!(
        new["toolConfig"]["functionCallingConfig"]["mode"],
        json!("ANY"),
        "tool_choice=required → ANY"
    );
}

// -----------------------------------------------------------------------
// B8:响应面 differential + 有意变更断言
// -----------------------------------------------------------------------

#[test]
fn non_stream_response_uses_same_combination_gate_as_request() {
    let anthropic = provider("anthropic", None);
    let error = try_decode_response(200, &json!({}), "openai_embeddings", &anthropic)
        .expect_err("embeddings×anthropic 响应面必须拒绝");
    assert!(
        error.message().contains("embeddings"),
        "{}",
        error.message()
    );
    let openai = provider("openai", None);
    let error = try_decode_response(200, &json!({}), "gemini_interactions", &openai)
        .expect_err("interactions×非 gemini 响应面必须拒绝");
    assert!(
        error.message().contains("interactions"),
        "{}",
        error.message()
    );
}

#[test]
fn differential_anthropic_response_to_openai() {
    let value = json!({
        "id": "msg_1",
        "type": "message",
        "role": "assistant",
        "model": "m",
        "content": [
            {"type": "text", "text": "hello"},
            {"type": "tool_use", "id": "c1", "name": "get", "input": {"a": 1}},
        ],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 3, "output_tokens": 5},
    });
    let old_modern = {
        // 旧实现为私有 fn;经 decode_response 旧分支语义等价(此处直接
        // 断言新输出形状,stop_reason 经映射表为有意变更)。
        decode_response(200, &value, "openai", &provider("anthropic", None))
    };
    assert_eq!(old_modern["id"], json!("msg_1"));
    assert_eq!(old_modern["object"], json!("chat.completion"));
    assert_eq!(
        old_modern["choices"][0]["message"]["content"],
        json!("hello")
    );
    assert_eq!(
        old_modern["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
        json!("{\"a\": 1}")
    );
    // 有意变更:end_turn 经映射表 → 映射值;tool_calls 覆盖语义统一
    // 保留(旧 gemini 路径行为;旧 anthropic 路径透传 "end_turn")。
    assert_eq!(
        old_modern["choices"][0]["finish_reason"],
        json!("tool_calls")
    );
    assert_eq!(
        old_modern["usage"],
        json!({"prompt_tokens": 3, "completion_tokens": 5, "total_tokens": 8})
    );
}

#[test]
fn differential_chat_response_to_anthropic() {
    let value = json!({
        "id": "chatcmpl_1",
        "object": "chat.completion",
        "model": "m",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "hello"},
            "finish_reason": "stop",
        }],
        "usage": {"prompt_tokens": 3, "completion_tokens": 5, "total_tokens": 8},
    });
    let converted = decode_response(200, &value, "anthropic", &provider("openai", None));
    assert_eq!(converted["id"], json!("chatcmpl_1"));
    assert_eq!(converted["type"], json!("message"));
    assert_eq!(
        converted["content"],
        json!([{"type": "text", "text": "hello"}])
    );
    assert_eq!(converted["stop_reason"], json!("end_turn"));
    assert_eq!(
        converted["usage"],
        json!({"input_tokens": 3, "output_tokens": 5})
    );
    // 有意变更:length → max_tokens(旧实现原样透传 "length")。
    let length = json!({
        "id": "x", "model": "m",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "hi"}, "finish_reason": "length"}],
        "usage": {},
    });
    let converted = decode_response(200, &length, "anthropic", &provider("openai", None));
    assert_eq!(converted["stop_reason"], json!("max_tokens"));
}

#[test]
fn differential_chat_response_to_responses_envelope() {
    let value = json!({
        "id": "chatcmpl_1",
        "object": "chat.completion",
        "model": "m",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "hello"},
            "finish_reason": "max_tokens",
        }],
        "usage": {"prompt_tokens": 3, "completion_tokens": 5, "total_tokens": 8},
    });
    let converted = decode_response(200, &value, "openai_responses", &provider("openai", None));
    assert_eq!(converted["object"], json!("response"));
    assert_eq!(converted["status"], json!("incomplete"));
    assert_eq!(
        converted["incomplete_details"]["reason"],
        json!("max_output_tokens")
    );
    let types: Vec<&str> = converted["output"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["type"].as_str())
        .collect();
    assert!(!types.contains(&"function_call"));
    assert!(types.contains(&"message"));
    let usage = &converted["usage"];
    assert_eq!(usage["input_tokens"], json!(3));
    assert_eq!(usage["total_tokens"], json!(8));
    assert_eq!(usage["input_tokens_details"]["cached_tokens"], json!(0));
}

#[test]
fn differential_responses_response_to_chat() {
    let value = json!({
        "id": "resp_1",
        "object": "response",
        "model": "m",
        "status": "completed",
        "output": [
            {"type": "reasoning", "summary_text": "think"},
            {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "hello"}]},
            {"type": "function_call", "call_id": "call_1", "name": "f", "arguments": "{}"},
        ],
        "usage": {"input_tokens": 3, "output_tokens": 5, "total_tokens": 8},
    });
    let converted = decode_response(
        200,
        &value,
        "openai",
        &provider("openai", Some("openai_responses")),
    );
    assert_eq!(converted["object"], json!("chat.completion"));
    assert_eq!(
        converted["choices"][0]["message"]["content"],
        json!("hello")
    );
    assert_eq!(
        converted["choices"][0]["message"]["reasoning_content"],
        json!("think")
    );
    assert_eq!(
        converted["choices"][0]["message"]["tool_calls"][0]["id"],
        json!("call_1")
    );
    // 有意变更:tool_calls 覆盖语义统一(旧 responses→chat 恒 "stop",
    // 客户端无法感知需要执行工具)。
    assert_eq!(
        converted["choices"][0]["finish_reason"],
        json!("tool_calls")
    );
    assert_eq!(
        converted["usage"],
        json!({"prompt_tokens": 3, "completion_tokens": 5, "total_tokens": 8})
    );
}

#[test]
fn conversion_failure_is_explicit_and_never_returns_upstream_object() {
    // P4-5/F-09:异协议响应形态无法解码时,生产路径必须得到错误,
    // 不能把上游原对象当成功响应。
    let anthropic = provider("anthropic", None);
    let upstream = json!("not-an-object");
    let error = try_decode_response(200, &upstream, "openai", &anthropic)
        .expect_err("异协议非对象响应必须解码失败");
    let (status, body) = error.envelope("openai", None);
    assert_eq!(status, 502);
    assert_eq!(body["error"]["type"], json!("wrouter_error"));
}

#[test]
fn chat_response_preserves_all_candidates_and_finish_reasons() {
    let upstream = json!({
        "candidates": [
            {"content": {"parts": [{"text": "first"}]}, "finishReason": "STOP"},
            {"content": {"parts": [{"text": "second"}]}, "finishReason": "MAX_TOKENS"},
        ],
        "usageMetadata": {"promptTokenCount": 2, "candidatesTokenCount": 3, "totalTokenCount": 5},
    });
    let converted = decode_response(200, &upstream, "openai", &provider("gemini", None));
    let choices = converted["choices"].as_array().expect("choices");
    assert_eq!(choices.len(), 2, "{converted}");
    assert_eq!(choices[0]["index"], json!(0));
    assert_eq!(choices[0]["message"]["content"], json!("first"));
    assert_eq!(choices[0]["finish_reason"], json!("stop"));
    assert_eq!(choices[1]["index"], json!(1));
    assert_eq!(choices[1]["message"]["content"], json!("second"));
    assert_eq!(choices[1]["finish_reason"], json!("length"));
}

#[test]
fn refusal_is_preserved_across_responses_chat_and_anthropic() {
    let responses = json!({
        "id": "resp_1", "object": "response", "model": "m", "status": "completed",
        "output": [{"type": "message", "role": "assistant", "content": [
            {"type": "refusal", "refusal": "I can't help with that"}
        ]}],
        "usage": {"input_tokens": 2, "output_tokens": 5, "total_tokens": 7},
    });
    let native = provider("openai", Some("openai_responses"));
    let chat = decode_response(200, &responses, "openai", &native);
    assert_eq!(
        chat["choices"][0]["message"]["refusal"],
        json!("I can't help with that")
    );
    assert_eq!(chat["choices"][0]["finish_reason"], json!("refusal"));
    let anthropic = decode_response(200, &responses, "anthropic", &native);
    assert_eq!(anthropic["stop_reason"], json!("refusal"));
    assert_eq!(
        anthropic["content"][0]["text"],
        json!("I can't help with that")
    );

    // 反向:Anthropic refusal stop_reason 不再被当普通成功正文。
    let anthropic_upstream = json!({
        "id": "msg_1", "type": "message", "role": "assistant", "model": "m",
        "content": [{"type": "text", "text": "refused"}],
        "stop_reason": "refusal",
        "usage": {"input_tokens": 1, "output_tokens": 1},
    });
    let chat = decode_response(
        200,
        &anthropic_upstream,
        "openai",
        &provider("anthropic", None),
    );
    assert_eq!(chat["choices"][0]["finish_reason"], json!("refusal"));
    assert_eq!(chat["choices"][0]["message"]["refusal"], json!("refused"));
}

#[test]
fn responses_annotations_survive_non_stream_roundtrip() {
    let upstream = json!({
        "candidates": [{"content": {"parts": [{"text": "see source"}]}, "finishReason": "STOP"}],
        "usageMetadata": {"promptTokenCount": 1, "candidatesTokenCount": 2, "totalTokenCount": 3},
    });
    // Chat/Gemini 目标没有 annotations 表达,先构造语义响应,再由
    // Responses 适配器编码时保留扩展。
    let converted = decode_response(
        200,
        &upstream,
        "openai_responses",
        &provider("gemini", None),
    );
    assert_eq!(converted["output"][0]["type"], json!("message"));
    assert!(converted["output"][0]["content"][0]["annotations"].is_array());
}

#[test]
fn vendor_signature_is_not_forced_into_another_target() {
    let upstream = json!({
        "candidates": [{
            "content": {"parts": [
                {"text": "why", "thought": true, "thoughtSignature": "google-sig"},
                {"text": "answer"},
            ]},
            "finishReason": "STOP",
        }],
        "usageMetadata": {"promptTokenCount": 1, "candidatesTokenCount": 2, "totalTokenCount": 3},
    });
    let (converted, report) = convert_response(
        SourceProtocol::Anthropic,
        ProviderProtocol::Gemini,
        false,
        &upstream,
    )
    .expect("转换成功");
    let thinking = converted["content"]
        .as_array()
        .unwrap()
        .iter()
        .find(|block| block["type"] == json!("thinking"))
        .expect("thinking block");
    assert!(
        thinking.get("signature").is_none(),
        "不得把 Gemini 签名硬塞进 Anthropic: {thinking}"
    );
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == DiagnosticCode::DroppedField
            && diagnostic.path == "reasoning.signature"
    }));
}

// -----------------------------------------------------------------------
// B8:gap 1 新能力 —— gemini 客户端 × openai/anthropic 上游
// -----------------------------------------------------------------------

#[test]
fn gap1_gemini_client_openai_upstream_roundtrip() {
    let request = json!({
        "model": "m",
        "contents": [{"role": "user", "parts": [{"text": "Say hi"}]}],
    });
    let outbound = upstream_payload("gemini", &request, &provider("openai", None)).unwrap();
    assert_eq!(
        outbound,
        json!({"model": "m", "messages": [{"role": "user", "content": "Say hi"}]}),
        "gemini 请求应转换为 chat 体(旧实现 clone 透传)"
    );
    let upstream = json!({
        "id": "up-ok", "object": "chat.completion", "model": "m",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "hello from upstream"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 3, "completion_tokens": 5, "total_tokens": 8},
    });
    let client_visible = decode_response(200, &upstream, "gemini", &provider("openai", None));
    assert_eq!(
        client_visible,
        json!({
            "candidates": [{"content": {"role": "model", "parts": [{"text": "hello from upstream"}]}, "finishReason": "STOP"}],
            "usageMetadata": {"promptTokenCount": 3, "candidatesTokenCount": 5, "totalTokenCount": 8},
            "modelVersion": "m",
            "responseId": "up-ok",
        }),
        "gemini 客户端应收到 generateContent 形态响应"
    );
}

#[test]
fn gap1_gemini_client_anthropic_upstream() {
    let request = json!({
        "model": "m",
        "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
    });
    let outbound = upstream_payload("gemini", &request, &provider("anthropic", None)).unwrap();
    assert_eq!(outbound["messages"][0]["content"][0]["text"], json!("hi"));
    assert_eq!(
        outbound["max_tokens"],
        json!(4096),
        "gemini 源无 max_tokens 时 anthropic 需默认值"
    );
    let upstream = json!({
        "id": "msg_1", "type": "message", "role": "assistant", "model": "m",
        "content": [{"type": "text", "text": "hello"}],
        "stop_reason": "max_tokens",
        "usage": {"input_tokens": 3, "output_tokens": 5},
    });
    let client_visible =
        decode_response(200, &upstream, "gemini", &provider("anthropic", None));
    assert_eq!(
        client_visible["candidates"][0]["finishReason"],
        json!("MAX_TOKENS")
    );
}

#[test]
fn gap5_anthropic_client_native_responses_upstream() {
    // 请求面:anthropic 体 → responses 形态;端点 → /responses。
    let request = json!({
        "model": "m",
        "system": "be nice",
        "messages": [{"role": "user", "content": [{"type": "text", "text": "hi"}]}],
        "max_tokens": 64,
    });
    let native = provider("openai", Some("openai_responses"));
    let outbound = upstream_payload("anthropic", &request, &native).unwrap();
    assert_eq!(outbound["instructions"], json!("be nice"));
    assert_eq!(outbound["max_output_tokens"], json!(64));
    assert_eq!(outbound["input"][0]["content"], json!("hi"));
    // 响应面:Responses JSON 不再被当 chat 解析(gap 5 缺口修复)。
    let upstream = json!({
        "id": "resp_1", "object": "response", "model": "m", "status": "completed",
        "output": [{"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "hello"}]}],
        "usage": {"input_tokens": 3, "output_tokens": 5, "total_tokens": 8},
    });
    let client_visible = decode_response(200, &upstream, "anthropic", &native);
    assert_eq!(client_visible["content"][0]["text"], json!("hello"));
    assert_eq!(client_visible["stop_reason"], json!("end_turn"));
}

#[test]
fn gemini_response_to_openai_client_parity() {
    let value = json!({
        "candidates": [{
            "content": {"parts": [
                {"text": "hello"},
                {"text": "think", "thought": true},
                {"functionCall": {"name": "f", "args": {"x": 1}}},
            ]},
            "finishReason": "STOP",
            "safetyRatings": [{"category": "HARM_A"}],
        }],
        "usageMetadata": {"promptTokenCount": 2, "candidatesTokenCount": 3, "totalTokenCount": 5},
        "responseId": "resp-9", "modelVersion": "gm",
    });
    let converted = decode_response(200, &value, "openai", &provider("gemini", None));
    assert_eq!(
        converted["choices"][0]["message"]["content"],
        json!("hello")
    );
    assert_eq!(
        converted["choices"][0]["message"]["reasoning_content"],
        json!("think")
    );
    assert_eq!(
        converted["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
        json!("f")
    );
    assert_eq!(
        converted["choices"][0]["finish_reason"],
        json!("tool_calls")
    );
    assert_eq!(converted["id"], json!("resp-9"));
    assert_eq!(converted["model"], json!("gm"));
    assert_eq!(converted["safetyRatings"], json!([{"category": "HARM_A"}]));
    let anthropic = decode_response(200, &value, "anthropic", &provider("gemini", None));
    assert_eq!(anthropic["content"][0]["type"], json!("thinking"));
    assert_eq!(anthropic["stop_reason"], json!("tool_use"));
    // MAX_TOKENS 不再落 end_turn(映射表修复)。
    let maxed = json!({
        "candidates": [{"content": {"parts": [{"text": "x"}]}, "finishReason": "MAX_TOKENS"}],
        "usageMetadata": {},
    });
    let anthropic = decode_response(200, &maxed, "anthropic", &provider("gemini", None));
    assert_eq!(anthropic["stop_reason"], json!("max_tokens"));
}

#[test]
fn gemini_interactions_envelope_matches_golden() {
    let upstream = json!({
        "id": "int-up-1",
        "status": "completed",
        "steps": [
            {"type": "message", "text": "hello from interaction"},
            {"type": "function_call", "id": "call-1", "name": "lookup", "arguments": {"q": "hi"}},
        ],
        "usageMetadata": {"promptTokenCount": 2, "candidatesTokenCount": 3, "totalTokenCount": 5},
    });
    let converted = decode_response(
        200,
        &upstream,
        "gemini_interactions",
        &provider("gemini", None),
    );
    assert_eq!(converted["id"], json!("int-up-1"));
    assert_eq!(converted["status"], json!("completed"));
    let content = &converted["output"][0]["content"];
    assert_eq!(
        content[0],
        json!({"type": "output_text", "text": "hello from interaction"})
    );
    assert_eq!(content[1]["type"], json!("function_call"));
    assert_eq!(content[1]["arguments"], json!("{\"q\": \"hi\"}"));
    assert_eq!(
        converted["usage"],
        json!({
            "input_tokens": 2, "output_tokens": 3, "total_tokens": 5,
            "cache_read_tokens": 0, "cache_write_tokens": 0,
        })
    );
}

// -----------------------------------------------------------------------
// B7:错误信封线级一致性
// -----------------------------------------------------------------------

#[test]
fn error_envelopes_match_legacy_shapes() {
    let upstream_error = crate::protocol::UpstreamError {
        status: 429,
        message: "slow down".to_owned(),
        error_type: "rate_limit_error".to_owned(),
        code: Some("429".to_owned()),
        param: None,
    };
    let attempts = vec!["p1".to_owned()];
    for (protocol, probe) in [
        (
            "openai",
            json!({"error": {"message": "slow down", "type": "rate_limit_error", "code": "429", "wrouter_attempts": ["p1"]}}),
        ),
        (
            "openai_responses",
            json!({"type": "error", "error": {"code": "429", "message": "slow down", "wrouter_attempts": ["p1"]}}),
        ),
        (
            "anthropic",
            json!({"type": "error", "error": {"type": "rate_limit_error", "message": "slow down", "wrouter_attempts": ["p1"]}}),
        ),
    ] {
        let (_, body) = format_error_for_protocol(protocol, &upstream_error, Some(&attempts));
        assert_eq!(body, probe, "protocol {protocol}");
    }
    let (_, gemini_body) =
        format_error_for_protocol("gemini", &upstream_error, Some(&attempts));
    assert_eq!(gemini_body["error"]["code"], json!(429));
    assert_eq!(gemini_body["error"]["status"], json!("rate_limit_error"));
    assert_eq!(gemini_body["error"]["details"][0]["reason"], json!("429"));
    // interactions 沿用 openai 形态(B7 决策,矩阵 §1.6 #11)。
    let (_, interactions) =
        format_error_for_protocol("gemini_interactions", &upstream_error, None);
    assert_eq!(interactions["error"]["type"], json!("rate_limit_error"));
    assert!(interactions["error"].get("code").is_none());
    // 矩阵 unsupported 升格 wrouter_error 信封。
    let openai = provider("anthropic", None);
    let failure =
        upstream_payload("openai_embeddings", &json!({"input": "x"}), &openai).unwrap_err();
    let (status, body) = failure.envelope("openai_embeddings", None);
    assert_eq!(status, 502);
    assert_eq!(body["error"]["type"], json!("wrouter_error"));
    assert_eq!(
        body["error"]["code"],
        json!("unsupported_protocol_combination")
    );
}
