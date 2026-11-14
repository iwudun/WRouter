//! 语义流符合性(原 C8 differential;阶段 E2 起旧 relay 已删除,「旧/新
//! 逐事件比较」失去对照半边,本文件改为对**唯一** wire 引擎
//! [`wr_domain::semantic::client_stream`] 的符合性测试)。
//!
//! 每份 fixture 自带 `expected_text` / `expected_tool_calls` /
//! `expected_usage`,语义泵产出的客户端帧必须满足它们;另有聚合与
//! 编码的自洽检查,以及断流不合成成功终态的契约测试(C7)。

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Value, json};
use wr_domain::provider::ProviderProtocol;
use wr_domain::semantic::client_stream::build_client_stream;
use wr_domain::semantic::model::SemanticResponse;
use wr_domain::semantic::source::{SourceFacts, SourceObserver};
use wr_domain::sse::{SseEvent, parse_sse_block};

/// 客户端可观察结果(语义层,不含 framing)。
#[derive(Debug, Default, PartialEq)]
struct Observation {
    text: String,
    /// index → (name, arguments)
    tool_calls: BTreeMap<usize, (String, String)>,
    finish: Option<String>,
    usage: Option<(Option<i64>, Option<i64>, Option<i64>)>,
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../testdata/fixtures")
}

fn load_fixture(name: &str) -> Value {
    let path = fixtures_dir().join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("读取 fixture {} 失败: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("解析 {name} 失败: {error}"))
}

fn fixture_events(fixture: &Value) -> Vec<SseEvent> {
    fixture["upstream_sse"]
        .as_array()
        .expect("fixture 必须有 upstream_sse")
        .iter()
        .filter_map(|line| parse_sse_block(line.as_str().expect("sse 行必须是字符串")))
        .collect()
}

// ---------------------------------------------------------------------------
// 帧 → 可观察量(按客户端协议)
// ---------------------------------------------------------------------------

fn frame_payloads(frames: &[String]) -> Vec<Value> {
    frames
        .iter()
        .filter_map(|frame| {
            let data = frame
                .lines()
                .filter_map(|line| line.strip_prefix("data: "))
                .collect::<Vec<_>>()
                .join("\n");
            if data.is_empty() || data == "[DONE]" {
                return None;
            }
            serde_json::from_str(&data).ok()
        })
        .collect()
}

fn observe(frames: &[String], client_protocol: &str) -> Observation {
    let payloads = frame_payloads(frames);
    match client_protocol {
        "anthropic" => observe_anthropic(&payloads),
        "openai_responses" => observe_responses(&payloads),
        "gemini" | "gemini_stream" => observe_gemini(&payloads),
        _ => observe_openai_chat(&payloads),
    }
}

fn observe_openai_chat(payloads: &[Value]) -> Observation {
    let mut observation = Observation::default();
    for payload in payloads {
        if let Some(choices) = payload.get("choices").and_then(Value::as_array) {
            for choice in choices {
                if let Some(delta) = choice.get("delta") {
                    if let Some(text) = delta.get("content").and_then(Value::as_str) {
                        observation.text.push_str(text);
                    }
                    if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                        for call in calls {
                            let index =
                                call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                            let entry = observation.tool_calls.entry(index).or_default();
                            if let Some(name) =
                                call.pointer("/function/name").and_then(Value::as_str)
                            {
                                entry.0 = name.to_owned();
                            }
                            if let Some(args) =
                                call.pointer("/function/arguments").and_then(Value::as_str)
                            {
                                entry.1.push_str(args);
                            }
                        }
                    }
                }
                if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                    observation.finish = Some(reason.to_owned());
                }
            }
        }
        if let Some(usage) = payload.get("usage").filter(|usage| !usage.is_null()) {
            observation.usage = Some((
                usage.get("prompt_tokens").and_then(Value::as_i64),
                usage.get("completion_tokens").and_then(Value::as_i64),
                usage.get("total_tokens").and_then(Value::as_i64),
            ));
        }
    }
    observation
}

fn observe_anthropic(payloads: &[Value]) -> Observation {
    let mut observation = Observation::default();
    for payload in payloads {
        match payload.get("type").and_then(Value::as_str).unwrap_or("") {
            "content_block_start" => {
                if let Some(block) = payload.get("content_block")
                    && block.get("type").and_then(Value::as_str) == Some("tool_use")
                {
                    let index = payload.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                    let entry = observation.tool_calls.entry(index).or_default();
                    entry.0 = block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                }
            }
            "content_block_delta" => {
                let index = payload.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                if let Some(delta) = payload.get("delta") {
                    match delta.get("type").and_then(Value::as_str).unwrap_or("") {
                        "text_delta" => {
                            if let Some(text) = delta.get("text").and_then(Value::as_str) {
                                observation.text.push_str(text);
                            }
                        }
                        "input_json_delta" => {
                            if let Some(fragment) =
                                delta.get("partial_json").and_then(Value::as_str)
                            {
                                observation
                                    .tool_calls
                                    .entry(index)
                                    .or_default()
                                    .1
                                    .push_str(fragment);
                            }
                        }
                        _ => {}
                    }
                }
            }
            "message_delta" => {
                if let Some(reason) = payload
                    .pointer("/delta/stop_reason")
                    .and_then(Value::as_str)
                {
                    observation.finish = Some(reason.to_owned());
                }
                if let Some(usage) = payload.get("usage") {
                    let input = usage.get("input_tokens").and_then(Value::as_i64);
                    let output = usage.get("output_tokens").and_then(Value::as_i64);
                    let existing = observation.usage.unwrap_or((None, None, None));
                    observation.usage =
                        Some((input.or(existing.0), output.or(existing.1), existing.2));
                }
            }
            _ => {}
        }
    }
    observation
}

fn observe_responses(payloads: &[Value]) -> Observation {
    let mut observation = Observation::default();
    for payload in payloads {
        match payload.get("type").and_then(Value::as_str).unwrap_or("") {
            "response.output_text.delta" => {
                if let Some(text) = payload.get("delta").and_then(Value::as_str) {
                    observation.text.push_str(text);
                }
            }
            "response.output_item.added" => {
                if let Some(item) = payload.get("item")
                    && item.get("type").and_then(Value::as_str) == Some("function_call")
                {
                    let index = payload
                        .get("output_index")
                        .and_then(Value::as_u64)
                        .unwrap_or(0) as usize;
                    let entry = observation.tool_calls.entry(index).or_default();
                    entry.0 = item
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                }
            }
            "response.function_call_arguments.delta" => {
                let index = payload
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize;
                if let Some(fragment) = payload.get("delta").and_then(Value::as_str) {
                    observation
                        .tool_calls
                        .entry(index)
                        .or_default()
                        .1
                        .push_str(fragment);
                }
            }
            "response.completed" | "response.incomplete" => {
                if let Some(usage) = payload.pointer("/response/usage") {
                    observation.usage = Some((
                        usage.get("input_tokens").and_then(Value::as_i64),
                        usage.get("output_tokens").and_then(Value::as_i64),
                        usage.get("total_tokens").and_then(Value::as_i64),
                    ));
                }
                observation.finish = Some(
                    payload
                        .pointer("/response/status")
                        .and_then(Value::as_str)
                        .unwrap_or("completed")
                        .to_owned(),
                );
            }
            _ => {}
        }
    }
    observation
}

fn observe_gemini(payloads: &[Value]) -> Observation {
    let mut observation = Observation::default();
    for payload in payloads {
        if let Some(candidates) = payload.get("candidates").and_then(Value::as_array) {
            for (candidate_index, candidate) in candidates.iter().enumerate() {
                if let Some(parts) = candidate
                    .pointer("/content/parts")
                    .and_then(Value::as_array)
                {
                    for (part_index, part) in parts.iter().enumerate() {
                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                            observation.text.push_str(text);
                        }
                        if let Some(call) = part.get("functionCall") {
                            let key = candidate_index * 1000 + part_index;
                            let entry = observation.tool_calls.entry(key).or_default();
                            entry.0 = call
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned();
                            entry.1 = call
                                .get("args")
                                .map(|args| wr_domain::json_py::py_json_dumps(args))
                                .unwrap_or_default();
                        }
                    }
                }
                if let Some(reason) = candidate.get("finishReason").and_then(Value::as_str) {
                    observation.finish = Some(reason.to_owned());
                }
            }
        }
        if let Some(usage) = payload.get("usageMetadata") {
            observation.usage = Some((
                usage.get("promptTokenCount").and_then(Value::as_i64),
                usage.get("candidatesTokenCount").and_then(Value::as_i64),
                usage.get("totalTokenCount").and_then(Value::as_i64),
            ));
        }
    }
    observation
}

/// 非流 JSON 投影的可观察量(与 [`observe`] 同口径)。
fn observe_json(value: &Value, client_protocol: &str) -> Observation {
    match client_protocol {
        "anthropic" => {
            let mut observation = Observation {
                finish: value
                    .get("stop_reason")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                ..Observation::default()
            };
            if let Some(usage) = value.get("usage") {
                observation.usage = Some((
                    usage.get("input_tokens").and_then(Value::as_i64),
                    usage.get("output_tokens").and_then(Value::as_i64),
                    None,
                ));
            }
            if let Some(blocks) = value.get("content").and_then(Value::as_array) {
                for (index, block) in blocks.iter().enumerate() {
                    match block.get("type").and_then(Value::as_str).unwrap_or("") {
                        "text" => observation
                            .text
                            .push_str(block.get("text").and_then(Value::as_str).unwrap_or("")),
                        "tool_use" => {
                            let entry = observation.tool_calls.entry(index).or_default();
                            entry.0 = block
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_owned();
                            entry.1 = block
                                .get("input")
                                .map(wr_domain::json_py::py_json_dumps)
                                .unwrap_or_default();
                        }
                        _ => {}
                    }
                }
            }
            observation
        }
        "openai_responses" => {
            let mut observation = Observation {
                finish: value
                    .get("status")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                ..Observation::default()
            };
            if let Some(usage) = value.get("usage") {
                observation.usage = Some((
                    usage.get("input_tokens").and_then(Value::as_i64),
                    usage.get("output_tokens").and_then(Value::as_i64),
                    usage.get("total_tokens").and_then(Value::as_i64),
                ));
            }
            if let Some(items) = value.get("output").and_then(Value::as_array) {
                for (index, item) in items.iter().enumerate() {
                    match item.get("type").and_then(Value::as_str).unwrap_or("") {
                        "message" => {
                            if let Some(parts) = item.get("content").and_then(Value::as_array) {
                                for part in parts {
                                    observation.text.push_str(
                                        part.get("text").and_then(Value::as_str).unwrap_or(""),
                                    );
                                }
                            }
                        }
                        "function_call" => {
                            let entry = observation.tool_calls.entry(index).or_default();
                            entry.0 = item
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_owned();
                            entry.1 = item
                                .get("arguments")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_owned();
                        }
                        _ => {}
                    }
                }
            }
            observation
        }
        "gemini" | "gemini_stream" => {
            let mut observation = Observation::default();
            if let Some(usage) = value.get("usageMetadata") {
                observation.usage = Some((
                    usage.get("promptTokenCount").and_then(Value::as_i64),
                    usage.get("candidatesTokenCount").and_then(Value::as_i64),
                    usage.get("totalTokenCount").and_then(Value::as_i64),
                ));
            }
            if let Some(candidates) = value.get("candidates").and_then(Value::as_array) {
                for (candidate_index, candidate) in candidates.iter().enumerate() {
                    observation.finish = candidate
                        .get("finishReason")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    if let Some(parts) = candidate
                        .pointer("/content/parts")
                        .and_then(Value::as_array)
                    {
                        for (part_index, part) in parts.iter().enumerate() {
                            if let Some(text) = part.get("text").and_then(Value::as_str) {
                                observation.text.push_str(text);
                            }
                            if let Some(call) = part.get("functionCall") {
                                let key = candidate_index * 1000 + part_index;
                                let entry = observation.tool_calls.entry(key).or_default();
                                entry.0 = call
                                    .get("name")
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_owned();
                                entry.1 = call
                                    .get("args")
                                    .map(wr_domain::json_py::py_json_dumps)
                                    .unwrap_or_default();
                            }
                        }
                    }
                }
            }
            observation
        }
        _ => {
            let mut observation = Observation {
                finish: value
                    .pointer("/choices/0/finish_reason")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                ..Observation::default()
            };
            if let Some(usage) = value.get("usage") {
                observation.usage = Some((
                    usage.get("prompt_tokens").and_then(Value::as_i64),
                    usage.get("completion_tokens").and_then(Value::as_i64),
                    usage.get("total_tokens").and_then(Value::as_i64),
                ));
            }
            if let Some(choices) = value.get("choices").and_then(Value::as_array) {
                for choice in choices {
                    let message = choice.get("message").cloned().unwrap_or_else(|| json!({}));
                    if let Some(text) = message.get("content").and_then(Value::as_str) {
                        observation.text.push_str(text);
                    }
                    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
                        for (index, call) in calls.iter().enumerate() {
                            let entry = observation.tool_calls.entry(index).or_default();
                            entry.0 = call
                                .pointer("/function/name")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_owned();
                            entry.1 = call
                                .pointer("/function/arguments")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_owned();
                        }
                    }
                }
            }
            observation
        }
    }
}

// ---------------------------------------------------------------------------
// wire 引擎(语义流泵)
// ---------------------------------------------------------------------------

/// 语义流泵逐事件 consume + 正常收口(wire 实际路径)。
fn pump_frames(client_protocol: &str, upstream_protocol: &str, events: &[SseEvent]) -> Vec<String> {
    let mut pump = build_client_stream(client_protocol);
    let protocol = match upstream_protocol {
        "anthropic" => ProviderProtocol::Anthropic,
        "gemini" => ProviderProtocol::Gemini,
        _ => ProviderProtocol::OpenAi,
    };
    let mut frames = Vec::new();
    for event in events {
        frames.extend(pump.consume_facts(&SourceFacts::from_sse(protocol, event)).frames);
    }
    frames.extend(pump.finalize());
    frames
}

/// 聚合自洽路径:语义解码 → StreamAggregator → response_to_events 展开。
fn aggregated_response(upstream_protocol: &str, events: &[SseEvent]) -> SemanticResponse {
    let target = match upstream_protocol {
        "anthropic" => wr_domain::provider::ProviderProtocol::Anthropic,
        "gemini" => wr_domain::provider::ProviderProtocol::Gemini,
        _ => wr_domain::provider::ProviderProtocol::OpenAi,
    };
    wr_domain::semantic::synthesis::aggregate_upstream_events(target, events).response
}

// ---------------------------------------------------------------------------
// 用例
// ---------------------------------------------------------------------------

struct ConformanceCase {
    fixture: &'static str,
    /// 客户端协议(帧形状)。
    client: &'static str,
    /// 上游协议(fixture 的 upstream_protocol)。
    upstream: &'static str,
}

const CASES: &[ConformanceCase] = &[
    ConformanceCase {
        fixture: "chat_stream_multi_tool.json",
        client: "openai",
        upstream: "openai",
    },
    ConformanceCase {
        fixture: "chat_stream_args_in_opening_chunk.json",
        client: "openai",
        upstream: "openai",
    },
    ConformanceCase {
        fixture: "usage_in_stream.json",
        client: "openai",
        upstream: "openai",
    },
    ConformanceCase {
        fixture: "anthropic_stream_tool_use.json",
        client: "openai",
        upstream: "anthropic",
    },
    ConformanceCase {
        fixture: "responses_stream.json",
        client: "openai_responses",
        upstream: "anthropic",
    },
    // 2026-11-06 回归:Responses **原生**上游(此前样本只有 anthropic
    // 上游,原生上游的 done 帧语义完全没被覆盖)。
    ConformanceCase {
        fixture: "responses_native_stream_text.json",
        client: "openai_responses",
        upstream: "openai",
    },
    ConformanceCase {
        fixture: "responses_native_stream_tool.json",
        client: "openai_responses",
        upstream: "openai",
    },
    ConformanceCase {
        fixture: "gemini_stream_generate_content.json",
        client: "openai",
        upstream: "gemini",
    },
    ConformanceCase {
        fixture: "gemini_stream_multi_tool_call.json",
        client: "openai",
        upstream: "gemini",
    },
];

#[test]
fn fixture_expected_values_hold_on_wire_engine() {
    for case in CASES {
        let fixture = load_fixture(case.fixture);
        let events = fixture_events(&fixture);
        assert!(!events.is_empty(), "{}: 事件不得为空", case.fixture);

        let observation = observe(
            &pump_frames(case.client, case.upstream, &events),
            case.client,
        );

        if let Some(expected) = fixture.get("expected_text").and_then(Value::as_str) {
            assert_eq!(
                observation.text, expected,
                "{}: wire 引擎文本与 fixture 期望不符",
                case.fixture
            );
        }
        if let Some(expected) = fixture.get("expected_tool_calls") {
            for (index, call) in expected.as_object().expect("expected_tool_calls 是对象") {
                let index: usize = index.parse().expect("tool call 键是数字");
                let name = call.get("name").and_then(Value::as_str).unwrap_or_default();
                let arguments = call
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                assert_eq!(
                    observation
                        .tool_calls
                        .get(&index)
                        .map(|entry| entry.0.as_str()),
                    Some(name),
                    "{}: 工具 #{index} 名称不符",
                    case.fixture
                );
                assert_eq!(
                    observation
                        .tool_calls
                        .get(&index)
                        .map(|entry| entry.1.as_str()),
                    Some(arguments),
                    "{}: 工具 #{index} 参数不符",
                    case.fixture
                );
            }
        }
        if let Some(expected) = fixture.get("expected_usage") {
            let expected_input = expected.get("input_tokens").and_then(Value::as_i64);
            let expected_output = expected.get("output_tokens").and_then(Value::as_i64);
            let expected_total = expected.get("total_tokens").and_then(Value::as_i64);
            let usage = observation
                .usage
                .unwrap_or_else(|| panic!("{}: wire 引擎必须产出 usage", case.fixture));
            assert_eq!(usage.0, expected_input, "{}: 输入 token", case.fixture);
            assert_eq!(usage.1, expected_output, "{}: 输出 token", case.fixture);
            // Anthropic 上游不报 total;语义层保持 unknown(None)而不是
            // 填 0 或臆造 —— 与 fixture 的推导值一致即可,None 也算合法。
            assert!(
                usage.2 == expected_total || usage.2.is_none(),
                "{}: 总 token 既不符预期也非 unknown: {:?}",
                case.fixture,
                usage.2
            );
        }
        if let Some(expected) = fixture.get("expected_names").and_then(Value::as_array) {
            let names: Vec<String> = observation
                .tool_calls
                .values()
                .map(|entry| entry.0.clone())
                .collect();
            let expected_names: Vec<String> = expected
                .iter()
                .map(|name| name.as_str().unwrap_or_default().to_owned())
                .collect();
            assert_eq!(names, expected_names, "{}: 工具名称集合", case.fixture);
        }

        // 聚合自洽:StreamAggregator 的响应文本与 wire 帧可观察文本一致。
        let response = aggregated_response(case.upstream, &events);
        assert_eq!(
            response.joined_text(),
            observation.text,
            "{}: 聚合文本与 wire 帧不一致",
            case.fixture
        );
    }
}

/// C7 断流契约:缺 terminal 的 EOF 不得被说成成功。
#[test]
fn truncated_stream_stays_incomplete() {
    let events = vec![parse_sse_block(
        r#"data: {"id":"c1","model":"m","choices":[{"index":0,"delta":{"content":"partial"},"finish_reason":null}]}"#,
    )
    .unwrap()];

    // 聚合层:incomplete 标记 + 已收内容保留 + 不伪造 finish。
    let response = aggregated_response("openai", &events);
    assert!(
        response.incomplete.is_some(),
        "缺 terminal 的流必须标 incomplete"
    );
    assert!(response.finish.is_none(), "不得伪造 finish");
    assert_eq!(response.joined_text(), "partial", "已收到的内容必须保留");

    // Chat Completions 客户端:finalize_incomplete 不补 stop/[DONE]。
    let mut pump = build_client_stream("openai");
    let mut observer = SourceObserver::new(ProviderProtocol::OpenAi);
    for event in &events {
        observer.observe(&SourceFacts::from_sse(ProviderProtocol::OpenAi, event));
        pump.consume_facts(&SourceFacts::from_sse(ProviderProtocol::OpenAi, event));
    }
    assert!(!observer.complete(), "完整性门不得放行断流");
    let frames = pump.finalize_incomplete();
    let joined = frames.join("");
    assert!(
        !joined.contains(r#""finish_reason""#) && !joined.contains("[DONE]"),
        "断流不得合成正常终止:{joined}"
    );

    // Responses 客户端:必须显式表达 incomplete。
    let mut responses_pump = build_client_stream("openai_responses");
    let mut responses_observer = SourceObserver::new(ProviderProtocol::OpenAi);
    for event in &events {
        responses_observer.observe(&SourceFacts::from_sse(ProviderProtocol::OpenAi, event));
        responses_pump.consume_facts(&SourceFacts::from_sse(ProviderProtocol::OpenAi, event));
    }
    assert!(!responses_observer.complete());
    let responses_joined = responses_pump.finalize_incomplete().join("");
    assert!(
        responses_joined.contains("response.incomplete"),
        "Responses 客户端应收到 incomplete 终态:{responses_joined}"
    );
    assert!(
        !responses_joined.contains(r#""type": "response.completed""#),
        "不得发 completed:{responses_joined}"
    );
}

/// 聚合 → 客户端编码的自洽:response_to_events 展开后可观察量不变
/// (C6 synthesis 契约的回归面)。
#[test]
fn aggregation_roundtrip_matches_wire_observables() {
    for case in CASES {
        let fixture = load_fixture(case.fixture);
        let events = fixture_events(&fixture);
        let response = aggregated_response(case.upstream, &events);
        let source = wr_domain::semantic::model::SourceProtocol::parse(case.client)
            .unwrap_or(wr_domain::semantic::model::SourceProtocol::OpenAi);
        let frames =
            wr_domain::semantic::synthesis::synthesize_client_frames(source, &response).unwrap();
        let observation = observe(&frames, case.client);
        assert_eq!(
            observation.text,
            response.joined_text(),
            "{}: 聚合文本与编码帧不一致",
            case.fixture
        );
    }
}

/// P5-4(C14):同一聚合响应在「非流 JSON」与「流式帧」两种表示下,
/// 文本、工具调用与用量投影一致(表示不同,语义不漂移)。
#[test]
fn aggregation_json_and_stream_projections_match() {
    for case in CASES {
        let fixture = load_fixture(case.fixture);
        let events = fixture_events(&fixture);
        let response = aggregated_response(case.upstream, &events);
        let source = wr_domain::semantic::model::SourceProtocol::parse(case.client)
            .unwrap_or(wr_domain::semantic::model::SourceProtocol::OpenAi);
        let json_value =
            wr_domain::semantic::synthesis::synthesize_client_response(source, &response).unwrap();
        let frames =
            wr_domain::semantic::synthesis::synthesize_client_frames(source, &response).unwrap();
        let from_json = observe_json(&json_value, case.client);
        let from_stream = observe(&frames, case.client);
        assert_eq!(
            from_json.text, from_stream.text,
            "{}: JSON 与流式文本投影不一致",
            case.fixture
        );
        let mut json_tools: Vec<(String, String)> = from_json.tool_calls.into_values().collect();
        let mut stream_tools: Vec<(String, String)> =
            from_stream.tool_calls.into_values().collect();
        json_tools.sort();
        stream_tools.sort();
        assert_eq!(
            json_tools, stream_tools,
            "{}: JSON 与流式工具投影不一致",
            case.fixture
        );
        assert_eq!(
            from_json.usage, from_stream.usage,
            "{}: JSON 与流式 usage 投影不一致",
            case.fixture
        );
    }
}

/// 2026-11-06 回归:Responses **原生**上游的「一次只发一份」四要素。
///
/// - **开场帧唯一**:同一个 function_call 只推一次
///   `response.output_item.added`(旧实现推两次 → 客户端把完整参数收到
///   两遍 → 工具参数被污染成 `{}`;ZCode 实测 8 次子智能体 + 3 次 Bash
///   全部执行失败);
/// - **正文全文唯一**:同一条消息只开一个 message item、正文只出现一遍
///   (旧实现把 `content_part.done` 的全文当增量重发 → 文本重复两遍,
///   Qoder 判 `inconsistent_text` 后断连、WRouter 记 499);
/// - **完成唯一**:每个 item 只有一条 `response.output_item.done`(旧实现
///   的 opaque 分支会把上游 item 再推一次 done → 同一条消息完成两次);
/// - **出站 id 合规**:`function_call` 的 `id` 恒以 `fc_` 开头(Responses
///   规范;违反即 400)。
#[test]
fn responses_native_upstream_emits_each_item_exactly_once() {
    let count_of = |payloads: &[Value], kind: &str| {
        payloads
            .iter()
            .filter(|payload| payload.get("type").and_then(Value::as_str) == Some(kind))
            .count()
    };
    let join_delta = |payloads: &[Value], kind: &str, field: &str| -> String {
        payloads
            .iter()
            .filter(|payload| payload.get("type").and_then(Value::as_str) == Some(kind))
            .filter_map(|payload| payload.get(field).and_then(Value::as_str))
            .collect()
    };

    // 文本流:一个 message item、一次 done、正文一遍。
    let text_fixture = load_fixture("responses_native_stream_text.json");
    let text_payloads = frame_payloads(&pump_frames(
        "openai_responses",
        "openai",
        &fixture_events(&text_fixture),
    ));
    assert_eq!(
        count_of(&text_payloads, "response.output_item.added"),
        1,
        "文本流只允许开一个 message item(content_part.done 的全文不得再开一个)"
    );
    assert_eq!(
        count_of(&text_payloads, "response.output_item.done"),
        1,
        "同一条消息只允许被完成一次"
    );
    assert_eq!(
        join_delta(&text_payloads, "response.output_text.delta", "delta"),
        "Hello",
        "正文只允许出现一遍"
    );
    let text_completed = text_payloads
        .iter()
        .find(|payload| payload.get("type").and_then(Value::as_str) == Some("response.completed"))
        .expect("text completed");
    assert_eq!(text_completed["response"]["output_text"], json!("Hello"));
    assert_eq!(
        text_completed["response"]["output"][0]["content"][0]["text"],
        json!("Hello"),
        "终态 output 必须与增量正文一致(G-06)"
    );

    // 工具流:一个开场帧、一段参数、id 恒 fc_ 前缀。
    let tool_fixture = load_fixture("responses_native_stream_tool.json");
    let tool_payloads = frame_payloads(&pump_frames(
        "openai_responses",
        "openai",
        &fixture_events(&tool_fixture),
    ));
    assert_eq!(
        count_of(&tool_payloads, "response.output_item.added"),
        1,
        "同一个 function_call 只允许开一次场(output_item.done 不得重复开场)"
    );
    assert_eq!(
        count_of(&tool_payloads, "response.output_item.done"),
        1,
        "同一个 function_call 只允许被完成一次"
    );
    assert_eq!(
        join_delta(
            &tool_payloads,
            "response.function_call_arguments.delta",
            "delta"
        ),
        "{\"command\":\"ls\"}",
        "工具参数只允许出现一遍"
    );
    let tool_completed = tool_payloads
        .iter()
        .find(|payload| payload.get("type").and_then(Value::as_str) == Some("response.completed"))
        .expect("tool completed");
    let completed_call = tool_completed["response"]["output"]
        .as_array()
        .and_then(|output| {
            output
                .iter()
                .find(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
        })
        .expect("completed function_call");
    assert_eq!(
        completed_call["arguments"],
        json!("{\"command\":\"ls\"}"),
        "终态 output 必须与参数增量一致(G-06)"
    );
    for payload in &tool_payloads {
        if payload.get("type").and_then(Value::as_str) == Some("response.output_item.added")
            && let Some(item) = payload.get("item")
            && item.get("type").and_then(Value::as_str) == Some("function_call")
        {
            let id = item.get("id").and_then(Value::as_str).unwrap_or_default();
            assert!(
                id.starts_with("fc_"),
                "出站 function_call.id 必须以 fc_ 开头,实际 {id}"
            );
        }
    }
}
