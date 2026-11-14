use serde_json::{Value, json};

use super::*;
use crate::provider::ProviderProtocol;
use crate::sse::parse_sse_block;

fn decode_all(upstream: ProviderProtocol, lines: &[&str]) -> Vec<SemanticEvent> {
    let mut out = Vec::new();
    for line in lines {
        let event = parse_sse_block(line).unwrap();
        out.extend(
            crate::semantic::stream::decode_upstream_frame(upstream, &event).events,
        );
    }
    out
}

/// 数一条 SSE 帧序列里的非空 finish_reason 值(按出现顺序)。
fn finish_reasons(frames: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for frame in frames {
        let Some(payload) = frame.trim().strip_prefix("data:") else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(payload.trim()) else {
            continue; // `[DONE]` 不是 JSON
        };
        let Some(choices) = value.get("choices").and_then(Value::as_array) else {
            continue;
        };
        for choice in choices {
            if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                out.push(reason.to_owned());
            }
        }
    }
    out
}

fn frame_values(frames: &[String]) -> Vec<Value> {
    frames
        .iter()
        .filter_map(|frame| {
            let payload = frame.trim().strip_prefix("data:")?.trim();
            serde_json::from_str::<Value>(payload).ok()
        })
        .collect()
}

/// 找出发了 usage 的帧:返回 (choices 条数, usage 文本)。
fn usage_frame(frames: &[String]) -> Option<(usize, String)> {
    frames.iter().find_map(|frame| {
        let payload = frame.trim().strip_prefix("data:")?;
        let value = serde_json::from_str::<Value>(payload.trim()).ok()?;
        let usage = value.get("usage")?;
        let choices = value
            .get("choices")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        Some((choices, usage.to_string()))
    })
}

/// 走真实外壳(语义流泵):逐事件 consume + 正常收口。
///
/// 直接喂 `ChatClient` 会跳过外壳的 usage 逐事件累计(用量的到达时机会
/// 失真)—— 2026-11-05 的回归必须走真实路径。
fn pump_frames(lines: &[&str]) -> Vec<String> {
    let mut pump = build_client_stream("openai");
    let mut frames = Vec::new();
    for line in lines {
        let event = parse_sse_block(line).unwrap();
        let protocol = ProviderProtocol::OpenAi;
        frames.extend(
            pump.consume_facts(&crate::semantic::source::SourceFacts::from_sse(protocol, &event))
                .frames,
        );
    }
    frames.extend(pump.finalize());
    frames
}

/// W1 characterization(Q-01):上游只给 input/output、缺 total 时,
/// Gemini 目标 usage 帧的 totalTokenCount 回退为二者之和。
/// W2-1 统一到 `Usage::to_domain` 后,这一输出必须保持不变。
#[test]
fn gemini_usage_frame_falls_back_to_input_plus_output() {
    let mut pump = build_client_stream("gemini");
    let event = parse_sse_block(
        r#"data: {"usageMetadata":{"promptTokenCount":7,"candidatesTokenCount":3}}"#,
    )
    .unwrap();
    let facts = crate::semantic::source::SourceFacts::from_sse(ProviderProtocol::Gemini, &event);
    let frames = pump.consume_facts(&facts).frames;
    let values = frame_values(&frames);
    let usage = values
        .iter()
        .find_map(|value| value.get("usageMetadata"))
        .expect("usage 帧必须下发");
    assert_eq!(usage["totalTokenCount"], json!(10), "{frames:?}");
}

/// W1 characterization(Q-01):total 显式给出时,语义转换保留原值。
#[test]
fn usage_to_domain_keeps_explicit_total() {
    let usage = crate::semantic::model::Usage {
        input: Some(7),
        output: Some(3),
        total: Some(10),
        ..crate::semantic::model::Usage::reported()
    };
    assert_eq!(usage.to_domain().total_tokens, 10);
}

#[test]
fn chat_client_rebuilds_envelope_and_single_finish() {
    let mut frames = pump_frames(&[
        r#"data: {"id":"c1","model":"m1","choices":[{"index":0,"delta":{"role":"assistant","content":"Hel"},"finish_reason":null}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{"content":"lo"},"finish_reason":null}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":7,"completion_tokens":3,"total_tokens":10}}"#,
        "data: [DONE]",
    ]);
    let done = frames.pop().unwrap();
    assert_eq!(done, "data: [DONE]\n\n");
    assert!(
        frames[0].contains(r#""delta": {"role": "assistant", "content": "Hel"}"#),
        "{}",
        frames[0]
    );
    assert!(frames[1].contains(r#""delta": {"content": "lo"}"#));
    assert!(frames[2].contains(r#""finish_reason": "stop""#));
    assert_eq!(
        frames.len(),
        3,
        "2 内容 + 唯一的 finish chunk(usage 已随它下发)"
    );
    // 2026-11-05:整条流只有一个 finish_reason(旧实现是 stop + stop 双块)。
    assert_eq!(finish_reasons(&frames), vec!["stop".to_owned()]);
}

/// 2026-11-05 回归(Qoder CN `conflicting_finish_reason`):
/// 上游给 `tool_calls` 时,流里只能有这一个 finish_reason —— 旧实现收尾
/// 又补一个 `stop`,两个**不同**值 → 严格客户端拒绝整条流。
#[test]
fn chat_client_emits_single_finish_reason_for_tool_calls() {
    let frames = pump_frames(&[
        r#"data: {"id":"c1","model":"m1","choices":[{"index":0,"delta":{"role":"assistant","tool_calls":[{"index":0,"id":"call_bash","type":"function","function":{"name":"Bash","arguments":"{}"}}]},"finish_reason":null}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        "data: [DONE]",
    ]);
    assert_eq!(
        finish_reasons(&frames),
        vec!["tool_calls".to_owned()],
        "上游的结束原因必须原样保留,且流中只有一个:{frames:?}"
    );
    assert!(
        !frames.join("").contains(r#""finish_reason": "stop""#),
        "不得补第二个 stop 终块:{frames:?}"
    );
}

/// usage 单独尾块下发(OpenAI `include_usage` 形态)→ 收尾补 usage 尾块,
/// 且该尾块**不带** finish_reason(不与上游的结束原因冲突)。
/// usage 已随上游终止块下发 → 收尾**不得**重复发(重复计量)。
#[test]
fn chat_client_usage_tail_carries_no_second_finish_reason() {
    let frames = pump_frames(&[
        r#"data: {"id":"c1","model":"m1","choices":[{"index":0,"delta":{"content":"hi"},"finish_reason":null}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
        r#"data: {"choices":[],"usage":{"prompt_tokens":7,"completion_tokens":3,"total_tokens":10}}"#,
        "data: [DONE]",
    ]);
    assert_eq!(finish_reasons(&frames), vec!["stop".to_owned()]);
    let (choices_len, usage_text) = usage_frame(&frames).expect("usage 必须下发");
    assert_eq!(choices_len, 0, "usage 尾块的 choices 必须为空:{frames:?}");
    assert!(usage_text.contains("\"total_tokens\":10"), "{usage_text}");
    assert_eq!(
        frames.join("").matches("\"prompt_tokens\"").count(),
        1,
        "usage 只能下发一次:{frames:?}"
    );
}

/// W2-1:上游 usage 只报 input/output(无 total)时,chat 客户端尾块
/// 仍下发推导后的 total,且只下发一次。
#[test]
fn chat_usage_tail_derives_missing_total_once() {
    let frames = pump_frames(&[
        r#"data: {"id":"c1","model":"m1","choices":[{"index":0,"delta":{"content":"hi"},"finish_reason":"stop"}]}"#,
        r#"data: {"choices":[],"usage":{"prompt_tokens":7,"completion_tokens":3}}"#,
        "data: [DONE]",
    ]);
    let (choices_len, usage_text) = usage_frame(&frames).expect("usage 必须下发");
    assert_eq!(choices_len, 0, "{frames:?}");
    assert!(usage_text.contains("\"total_tokens\":10"), "{usage_text}");
    assert_eq!(
        frames.join("").matches("\"prompt_tokens\"").count(),
        1,
        "usage 只能下发一次:{frames:?}"
    );
}

/// usage 随上游终止块下发 → 收尾不得再补 usage 尾块(重复计量)。
#[test]
fn chat_client_does_not_duplicate_usage_already_sent() {
    let frames = pump_frames(&[
        r#"data: {"id":"c1","model":"m1","choices":[{"index":0,"delta":{"content":"hi"},"finish_reason":null}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":7,"completion_tokens":3,"total_tokens":10}}"#,
        "data: [DONE]",
    ]);
    assert_eq!(finish_reasons(&frames), vec!["stop".to_owned()]);
    assert_eq!(
        frames.join("").matches("\"prompt_tokens\"").count(),
        1,
        "usage 只能下发一次:{frames:?}"
    );
}

/// 上游没给 finish_reason(异常上游)→ 兜底补一个 `stop` 终块,
/// 否则客户端永远收不到结束原因。
#[test]
fn chat_client_falls_back_to_stop_without_upstream_finish() {
    let frames = pump_frames(&[
        r#"data: {"id":"c1","model":"m1","choices":[{"index":0,"delta":{"content":"hi"},"finish_reason":null}]}"#,
        "data: [DONE]",
    ]);
    assert_eq!(finish_reasons(&frames), vec!["stop".to_owned()]);
}

/// 上游重复给 finish_reason(协议违规)→ 客户端只看到一个。
#[test]
fn chat_client_suppresses_duplicate_upstream_finish() {
    let frames = pump_frames(&[
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
        "data: [DONE]",
    ]);
    assert_eq!(
        finish_reasons(&frames),
        vec!["tool_calls".to_owned()],
        "重复的 finish_reason 不得再出帧:{frames:?}"
    );
}

#[test]
fn chat_client_keeps_candidates_and_finishes_separate() {
    let mut client = ChatClient::new();
    let mut frames = Vec::new();
    for event in decode_all(
        ProviderProtocol::OpenAi,
        &[
            r#"data: {"id":"c1","model":"m1","choices":[{"index":0,"delta":{"content":"a"},"finish_reason":null},{"index":1,"delta":{"content":"b"},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"},{"index":1,"delta":{},"finish_reason":"length"}]}"#,
        ],
    ) {
        frames.extend(client.consume(std::slice::from_ref(&event), Usage::empty()));
    }
    frames.extend(client.finalize(Usage::empty()));
    let values = frame_values(&frames);
    let content: Vec<(u64, String)> = values
        .iter()
        .filter_map(|value| {
            let choice = value.get("choices")?.as_array()?.first()?;
            let index = choice.get("index")?.as_u64()?;
            let content = choice.pointer("/delta/content")?.as_str()?.to_owned();
            Some((index, content))
        })
        .collect();
    assert_eq!(content, vec![(0, "a".to_owned()), (1, "b".to_owned())]);
    let finishes: Vec<(u64, String)> = values
        .iter()
        .filter_map(|value| {
            let choice = value.get("choices")?.as_array()?.first()?;
            let index = choice.get("index")?.as_u64()?;
            let reason = choice.get("finish_reason")?.as_str()?.to_owned();
            Some((index, reason))
        })
        .collect();
    assert_eq!(
        finishes,
        vec![(0, "stop".to_owned()), (1, "length".to_owned())],
        "{frames:?}"
    );
}

#[test]
fn gemini_client_assembles_tool_arguments_before_function_call() {
    let mut client = GeminiClient::default();
    let mut frames = Vec::new();
    for event in decode_all(
        ProviderProtocol::Anthropic,
        &[
            r#"event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"lookup","input":{}}}"#,
            r#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"q\":1}"}}"#,
            r#"event: content_block_stop
data: {"type":"content_block_stop","index":0}"#,
        ],
    ) {
        frames.extend(client.consume(std::slice::from_ref(&event)));
    }
    frames.extend(client.finalize());
    let values = frame_values(&frames);
    let calls: Vec<&Value> = values
        .iter()
        .filter_map(|value| value.pointer("/candidates/0/content/parts/0/functionCall"))
        .collect();
    assert_eq!(calls.len(), 1, "工具只输出一次:{frames:?}");
    assert_eq!(calls[0]["args"], json!({"q": 1}));
    assert_eq!(calls[0]["name"], json!("lookup"));
    assert_eq!(calls[0]["id"], json!("toolu_1"));
}

fn pump_facts(client_protocol: &str, lines: &[&str]) -> Vec<String> {
    let mut pump = build_client_stream(client_protocol);
    let mut frames = Vec::new();
    for line in lines {
        let event = parse_sse_block(line).unwrap();
        let facts =
            crate::semantic::source::SourceFacts::from_sse(ProviderProtocol::OpenAi, &event);
        frames.extend(pump.consume_facts(&facts).frames);
    }
    frames.extend(pump.finalize());
    frames
}

#[test]
fn chat_client_reconciles_done_snapshot_as_strict_prefix_extension() {
    let frames = pump_facts(
        "openai",
        &[
            r#"event: response.output_item.added
data: {"output_index":0,"item":{"id":"fc_1","type":"function_call","call_id":"call_1","name":"lookup","arguments":""}}"#,
            r#"event: response.function_call_arguments.delta
data: {"output_index":0,"delta":"{\"q\":"}"#,
            r#"event: response.output_item.done
data: {"output_index":0,"item":{"id":"fc_1","type":"function_call","call_id":"call_1","name":"lookup","arguments":"{\"q\":1}"}}"#,
            r#"event: response.completed
data: {"response":{"id":"resp_1","status":"completed","usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}"#,
        ],
    );
    let values = frame_values(&frames);
    let arguments: String = values
        .iter()
        .filter_map(|value| {
            value
                .pointer("/choices/0/delta/tool_calls/0/function/arguments")?
                .as_str()
        })
        .collect();
    assert_eq!(
        arguments, "{\"q\":1}",
        "快照只补后缀,不得重放已推送前缀:{frames:?}"
    );
}

#[test]
fn gemini_client_reconciles_done_snapshot_as_strict_prefix_extension() {
    let frames = pump_facts(
        "gemini",
        &[
            r#"event: response.output_item.added
data: {"output_index":0,"item":{"id":"fc_1","type":"function_call","call_id":"call_1","name":"lookup","arguments":""}}"#,
            r#"event: response.function_call_arguments.delta
data: {"output_index":0,"delta":"{\"q\":"}"#,
            r#"event: response.output_item.done
data: {"output_index":0,"item":{"id":"fc_1","type":"function_call","call_id":"call_1","name":"lookup","arguments":"{\"q\":1}"}}"#,
            r#"event: response.completed
data: {"response":{"id":"resp_1","status":"completed","usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}"#,
        ],
    );
    let values = frame_values(&frames);
    let call = values
        .iter()
        .find_map(|value| value.pointer("/candidates/0/content/parts/0/functionCall"))
        .expect("functionCall");
    assert_eq!(call["args"], json!({"q": 1}));
}

#[test]
fn anthropic_client_single_terminal_with_source_stop_reason() {
    let mut client = AnthropicClient::new();
    let mut usage = Usage::empty();
    let mut frames = Vec::new();
    for line in [
        r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_1","model":"claude-m","content":[],"usage":{"input_tokens":8}}}"#,
        r#"event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"get_weather","input":{}}}"#,
        r#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"loc\":\"SF\"}"}}"#,
        r#"event: content_block_stop
data: {"type":"content_block_stop","index":0}"#,
        r#"event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":3}}"#,
        r#"event: message_stop
data: {"type":"message_stop"}"#,
    ] {
        let event = parse_sse_block(line).unwrap();
        let semantic = crate::semantic::stream::decode_upstream_frame(
            ProviderProtocol::Anthropic,
            &event,
        )
        .events;
        for semantic_event in &semantic {
            if let SemanticEvent::Usage(update) = semantic_event {
                usage.merge_max(update.to_domain());
            }
        }
        frames.extend(client.consume(&semantic, usage));
    }
    frames.extend(client.finalize(usage));
    let text = frames.join("");
    // 终结序列只出现一次,stop_reason 来自上游 tool_use 而非 end_turn。
    assert_eq!(
        text.matches("\"type\": \"message_stop\"").count(),
        1,
        "{text}"
    );
    assert_eq!(
        text.matches("\"type\": \"message_delta\"").count(),
        1,
        "{text}"
    );
    assert!(text.contains(r#""stop_reason": "tool_use""#), "{text}");
    assert!(text.contains(r#""input_tokens": 8"#), "{text}");
    assert!(text.contains(r#""output_tokens": 3"#), "{text}");
    assert!(text.starts_with("event: message_start\n"));
}

#[test]
fn responses_client_lifecycle_accumulates_output() {
    let mut client = ResponsesTargetClient::new();
    let mut frames = Vec::new();
    for line in [
        r#"data: {"id":"resp_1","model":"m1","choices":[{"index":0,"delta":{"content":"hi"},"finish_reason":null}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":5,"completion_tokens":2,"total_tokens":7}}"#,
    ] {
        let event = parse_sse_block(line).unwrap();
        let decoded =
            crate::semantic::stream::decode_upstream_frame(ProviderProtocol::OpenAi, &event);
        frames.extend(client.consume(&decoded.updates));
    }
    frames.extend(client.finalize());
    let text = frames.join("");
    assert!(text.contains(r#""type": "response.created""#), "{text}");
    assert!(text.contains(r#""delta": "hi""#), "{text}");
    assert!(text.contains(r#""type": "response.completed""#), "{text}");
    assert!(text.contains(r#""output_text": "hi""#), "{text}");
    assert!(text.contains(r#""input_tokens": 5"#), "{text}");
    assert!(text.contains(r#""sequence_number""#), "{text}");
    assert!(text.ends_with("data: [DONE]\n\n"));
}

#[test]
fn responses_finalize_incomplete_announces_truncation() {
    let mut client = ResponsesTargetClient::new();
    let mut frames = Vec::new();
    let line = r#"data: {"id":"resp_2","model":"m1","choices":[{"index":0,"delta":{"content":"par"},"finish_reason":null}]}"#;
    let event = parse_sse_block(line).unwrap();
    let decoded =
        crate::semantic::stream::decode_upstream_frame(ProviderProtocol::OpenAi, &event);
    frames.extend(client.consume(&decoded.updates));
    frames.extend(client.finalize_incomplete());
    let text = frames.join("");
    assert!(text.contains(r#""type": "response.incomplete""#), "{text}");
    assert!(!text.contains("response.completed"), "{text}");
}

#[test]
fn gemini_client_emits_gemini_shapes() {
    let mut client = GeminiClient::default();
    let mut frames = Vec::new();
    for event in decode_all(
        ProviderProtocol::OpenAi,
        &[
            r#"data: {"id":"c1","model":"m1","choices":[{"index":0,"delta":{"content":"你好"},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
        ],
    ) {
        frames.extend(client.consume(std::slice::from_ref(&event)));
    }
    let text = frames.join("");
    assert!(text.contains(r#""parts": [{"text": "你好"}]"#), "{text}");
    assert!(text.contains(r#""finishReason": "STOP""#), "{text}");
    assert!(!text.contains("chat.completion.chunk"), "{text}");
}

/// 首字锚点:信封帧(message_start / 空 text 块起始)不算内容,
/// 首个内容 delta 才算 —— 投递层据此打首字耗时。
#[test]
fn content_started_ignores_envelope_frames() {
    let mut stream = build_client_stream("anthropic");
    let batch = |text: &str, stream: &mut ClientStream| {
        let event = parse_sse_block(text).unwrap();
        let facts =
            crate::semantic::source::SourceFacts::from_sse(ProviderProtocol::Anthropic, &event);
        stream.consume_facts(&facts)
    };
    // 信封:message_start(被持有,不发帧)+ 空 text 块起始。
    let first = batch(
        r#"event: message_start
data: {"type":"message_start","message":{"id":"m","model":"x","content":[],"usage":{"input_tokens":2}}}"#,
        &mut stream,
    );
    assert!(!first.has_content, "message_start 不是内容");
    let second = batch(
        r#"event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        &mut stream,
    );
    assert!(!second.has_content, "空 text 块起始不是内容");
    // 首个内容 delta:打点。
    let third = batch(
        r#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}"#,
        &mut stream,
    );
    assert!(third.has_content, "首个 text delta 是内容");
}

#[test]
fn shell_dispatch_usage_and_completeness_gate() {
    let mut stream = build_client_stream("anthropic");
    let mut observer =
        crate::semantic::source::SourceObserver::new(ProviderProtocol::Anthropic);
    let event = parse_sse_block(
        r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_9","model":"claude-m","usage":{"input_tokens":4}}}"#,
    )
    .unwrap();
    let facts =
        crate::semantic::source::SourceFacts::from_sse(ProviderProtocol::Anthropic, &event);
    observer.observe(&facts);
    let frame = stream.consume_facts(&facts).frames;
    assert_eq!(frame.len(), 0, "message_start 持有待 usage/后续帧");
    assert!(!observer.complete());
    assert_eq!(observer.take_violations(), 0);
    let finalize = stream.finalize_with_usage(observer.usage());
    let text = finalize.join("");
    assert!(text.starts_with("event: message_start\n"), "{text}");
    assert!(text.contains(r#""input_tokens": 4"#), "{text}");
    assert!(text.contains("message_stop"), "{text}");
}

/// 阶段四 C-7:均匀投递的内容 → flow ≈ 整个内容跨度(×10/8 折算)。
#[test]
fn flow_measures_ten_to_ninety_span_scaled_to_full() {
    let mut meter = FlowMeter::default();
    let base = Instant::now();
    // 10 份内容,每份 100 字节,每 100ms 到一份(共 1000ms)。
    for index in 0..10 {
        meter.mark(base + std::time::Duration::from_millis(index * 100), 100);
    }
    // 10% 的时刻 = 0ms(第一份已到 10%),90% 的时刻 = 800ms(第九份到
    // 90%)→ 跨度 800ms → ×10/8 = 1000ms。
    assert_eq!(meter.flow_ms(), Some(1000.0));
}

/// 阶段四 C-7:一次性倒出来的内容 → flow ≈ 0(测的是批量投递)。
#[test]
fn flow_collapses_for_one_shot_delivery() {
    let mut meter = FlowMeter::default();
    let base = Instant::now();
    meter.mark(base, 100);
    meter.mark(base + std::time::Duration::from_millis(5), 900);
    // 跨度 5ms → ×10/8 = 6.25ms。
    assert_eq!(meter.flow_ms(), Some(6.25));
}

/// 20261108_01:只有一个推进点(内容一次性到齐)→ **不可测**。
///
/// 旧实现取 1ms 下限,把这个「测不出解码速度」伪装成 1ms,吞吐分母
/// 随之变成 1ms(296 token → 296000 tok/s);现在显式返回 `None`。
#[test]
fn flow_single_mark_is_unmeasurable() {
    let mut meter = FlowMeter::default();
    meter.mark(Instant::now(), 100);
    assert_eq!(meter.flow_ms(), None);
    // 同一毫秒的多个推进点仍只有 1 个 mark → 不可测。
    let mut same_ms = FlowMeter::default();
    let base = Instant::now();
    same_ms.mark(base, 100);
    same_ms.mark(base, 200);
    assert_eq!(same_ms.flow_ms(), None);
    // 没有内容 → 不可测。
    let empty = FlowMeter::default();
    assert_eq!(empty.flow_ms(), None);
}

/// P2:源观察者拥有续写投影与用量;编码器只负责目标帧。
#[test]
fn continuation_accumulates_text_suppresses_restart_and_adds_usage() {
    let mut stream = build_client_stream("anthropic");
    let mut observer =
        crate::semantic::source::SourceObserver::new(ProviderProtocol::Anthropic);

    fn feed(
        text: &str,
        stream: &mut ClientStream,
        observer: &mut crate::semantic::source::SourceObserver,
    ) -> Vec<String> {
        let event = parse_sse_block(text).unwrap();
        let facts =
            crate::semantic::source::SourceFacts::from_sse(ProviderProtocol::Anthropic, &event);
        observer.observe(&facts);
        let frames = stream.consume_facts(&facts).frames;
        observer.mark_committed();
        frames
    }

    // 第一次尝试:message_start + 半句正文。
    feed(
        r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_1","model":"claude-m","usage":{"input_tokens":10}}}"#,
        &mut stream,
        &mut observer,
    );
    feed(
        r#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"半句"}}"#,
        &mut stream,
        &mut observer,
    );
    assert_eq!(observer.committed_text(), "半句", "已提交正文是续写前缀");
    assert_eq!(observer.usage().input_tokens, 10);

    // 续写:观察者与编码器分别切模式。
    observer.begin_continuation();
    stream.begin_continuation();
    assert!(observer.in_continuation());
    let frames = feed(
        r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_2","model":"claude-m","usage":{"input_tokens":20}}}"#,
        &mut stream,
        &mut observer,
    );
    assert!(
        !frames.join("").contains("message_start"),
        "续写不得重复下发 message_start"
    );
    feed(
        r#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"续写"}}"#,
        &mut stream,
        &mut observer,
    );
    assert_eq!(observer.committed_text(), "半句续写");
    assert_eq!(observer.usage().input_tokens, 30, "两段用量相加(10 + 20)");
}

/// 阶段五 C-3:未进入续写模式时,message_start 照常(被持有待发)。
#[test]
fn message_start_is_kept_without_continuation() {
    let mut stream = build_client_stream("anthropic");
    let frames = stream
        .consume_facts(&SourceFacts::from_sse(
            crate::provider::ProviderProtocol::Anthropic,
            &parse_sse_block(
                r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_1","model":"claude-m"}}"#,
            )
            .unwrap(),
        ))
        .frames;
    assert!(frames.is_empty(), "message_start 先持有,等后续帧再发");
    let finalized = stream.finalize().join("");
    assert!(finalized.contains("message_start"), "{finalized}");
}

// ---------------------------------------------------------------------------
// 20261108_01 §5.1:答案段 / 思考段 FlowMeter(吞吐分母与诊断)。
// ---------------------------------------------------------------------------

fn flow_facts(events: Vec<SemanticEvent>) -> SourceFacts {
    SourceFacts {
        identity: crate::semantic::source::FrameIdentity::synthetic(
            ProviderProtocol::OpenAi,
            crate::wire::record::Representation::Sse,
        ),
        events,
        updates: Vec::new(),
        usage: Vec::new(),
        finishes: Vec::new(),
        terminal: None,
        snapshot: None,
        opaque: Vec::new(),
        recognized: true,
        sequence: None,
    }
}

/// 答案计量排除 `ReasoningDelta`;推理增量只进思考计量,全内容计量仍含它。
/// 单个推进点(整段内容一个瞬间到达)视为不可测。
#[test]
fn answer_flow_excludes_reasoning_delta() {
    let mut pump = build_client_stream("openai");
    pump.mark_flow(&flow_facts(vec![SemanticEvent::ReasoningDelta {
        candidate: 0,
        index: 0,
        text: "思考".repeat(100),
    }]));
    assert!(pump.answer_flow_ms().is_none(), "纯思考不产生答案跨度");
    assert!(pump.answer_origin().is_none(), "答案 TTFT 尚未开始");
    assert!(pump.flow_ms().is_none(), "单个推进点不可测(不伪装成 1ms)");
    assert!(pump.reasoning_flow_ms().is_none(), "单个推进点不可测");
    assert!(pump.saw_reasoning(), "思考增量已观察");

    // 第二个推进点(不同毫秒)之后才可测。
    std::thread::sleep(std::time::Duration::from_millis(5));
    pump.mark_flow(&flow_facts(vec![SemanticEvent::ReasoningDelta {
        candidate: 0,
        index: 0,
        text: "继续思考".repeat(100),
    }]));
    assert!(pump.flow_ms().is_some());
    assert!(pump.reasoning_flow_ms().is_some());
    assert!(pump.answer_flow_ms().is_none(), "答案计量仍未被思考污染");

    // 答案增量只进答案计量(两个推进点才可测)。
    std::thread::sleep(std::time::Duration::from_millis(5));
    pump.mark_flow(&flow_facts(vec![SemanticEvent::PartDelta {
        candidate: 0,
        index: 0,
        text: "答案".repeat(50),
    }]));
    std::thread::sleep(std::time::Duration::from_millis(5));
    pump.mark_flow(&flow_facts(vec![SemanticEvent::PartDelta {
        candidate: 0,
        index: 0,
        text: "继续答案".repeat(50),
    }]));
    assert!(pump.answer_flow_ms().is_some());
    assert!(pump.answer_origin().is_some());
}

/// 原生 Raw(`mark_flow`)与协议转换(`consume_facts`)共用同一插桩:
/// 同一批源事实得到相同的答案跨度与答案起点。
#[test]
fn raw_and_conversion_paths_mark_same_answer_flow() {
    let first = flow_facts(vec![
        SemanticEvent::ReasoningDelta {
            candidate: 0,
            index: 0,
            text: "推理".repeat(40),
        },
        SemanticEvent::PartDelta {
            candidate: 0,
            index: 0,
            text: "答案".repeat(40),
        },
    ]);
    let second = flow_facts(vec![
        SemanticEvent::ReasoningDelta {
            candidate: 0,
            index: 0,
            text: "继续推理".repeat(40),
        },
        SemanticEvent::PartDelta {
            candidate: 0,
            index: 0,
            text: "继续答案".repeat(40),
        },
    ]);
    let mut raw = build_client_stream("openai");
    raw.mark_flow(&first);
    let mut converted = build_client_stream("openai");
    let _ = converted.consume_facts(&first);
    std::thread::sleep(std::time::Duration::from_millis(5));
    raw.mark_flow(&second);
    let _ = converted.consume_facts(&second);
    assert!(raw.answer_flow_ms().is_some());
    assert_eq!(raw.answer_flow_ms(), converted.answer_flow_ms());
    assert_eq!(raw.reasoning_flow_ms(), converted.reasoning_flow_ms());
    assert_eq!(raw.flow_ms(), converted.flow_ms());
    assert_eq!(
        raw.answer_origin().is_some(),
        converted.answer_origin().is_some()
    );
    assert_eq!(raw.saw_reasoning(), converted.saw_reasoning());
}
