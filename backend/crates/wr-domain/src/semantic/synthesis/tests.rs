
use super::*;
use crate::semantic::model::IdOrigin;
use crate::semantic::model::{
    FinishReason, ResponseCandidate, ToolArguments, ToolCall, ToolCallId, Usage,
};
use serde_json::json;

fn sse(name: Option<&str>, body: &str) -> SseEvent {
    SseEvent {
        event: name.map(str::to_owned),
        data: body.to_owned(),
        raw: body.to_owned(),
    }
}

/// 客户端非流 / 上游流:上游事件聚合为语义响应,再编码回客户端 JSON。
#[test]
fn upstream_events_aggregate_to_client_response() {
    let events = vec![
        sse(
            None,
            r#"{"id":"c1","model":"m","choices":[{"index":0,"delta":{"content":"he"}}]}"#,
        ),
        sse(
            None,
            r#"{"choices":[{"index":0,"delta":{"content":"llo"},"finish_reason":"stop"}]}"#,
        ),
        sse(
            None,
            r#"{"choices":[],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}}"#,
        ),
        sse(None, "[DONE]"),
    ];
    let aggregated = aggregate_upstream_events(ProviderProtocol::OpenAi, &events);
    assert!(aggregated.is_complete(), "正常终结不应判为不完整");
    assert_eq!(aggregated.response.joined_text(), "hello");
    assert_eq!(aggregated.response.id.as_deref(), Some("c1"));
    assert_eq!(aggregated.response.usage.unwrap().input, Some(3));

    let client =
        synthesize_client_response(SourceProtocol::OpenAi, &aggregated.response).unwrap();
    assert_eq!(client["choices"][0]["message"]["content"], json!("hello"));
    assert_eq!(client["usage"]["prompt_tokens"], json!(3));
}

/// 客户端流 / 上游非流:完整语义响应展开为客户端 SSE 帧。
#[test]
fn semantic_response_synthesizes_client_stream_frames() {
    let mut response = SemanticResponse::new();
    response.id = Some("resp_1".to_owned());
    response.model = Some("m".to_owned());
    let mut candidate = ResponseCandidate::new(0);
    candidate.content.push(ContentPart::Text {
        text: "hi there".to_owned(),
    });
    candidate.finish = Some(FinishReason::Stop);
    response.candidates.push(candidate);
    response.finish = Some(FinishReason::Stop);
    response.usage = Some(Usage {
        input: Some(1),
        output: Some(2),
        total: Some(3),
        reasoning: None,
        cache_read: None,
        cache_write: None,
    });

    let frames = synthesize_client_frames(SourceProtocol::OpenAi, &response).unwrap();
    let joined = frames.join("");
    assert!(joined.contains("\"content\": \"hi there\"") || joined.contains("hi there"));
    assert!(joined.contains("\"finish_reason\": \"stop\"") || joined.contains("stop"));
    assert!(joined.contains("prompt_tokens"));
    assert_eq!(
        joined.matches("data: [DONE]").count(),
        1,
        "合成完整响应必须由目标编码器收口一次"
    );
}

/// Anthropic 客户端流帧带事件名与 content_block 生命周期。
#[test]
fn anthropic_client_frames_include_block_lifecycle() {
    let mut response = SemanticResponse::new();
    let mut candidate = ResponseCandidate::new(0);
    candidate.content.push(ContentPart::Text {
        text: "yo".to_owned(),
    });
    candidate.finish = Some(FinishReason::Stop);
    response.candidates.push(candidate);
    let frames = synthesize_client_frames(SourceProtocol::Anthropic, &response).unwrap();
    let joined = frames.join("");
    assert!(joined.contains("event: message_start"));
    assert!(joined.contains("event: content_block_start"));
    assert!(joined.contains("event: content_block_stop"));
    assert!(joined.contains("event: message_stop"));
}

/// 工具调用在合成帧里保留 id/name/arguments。
#[test]
fn synthesized_frames_carry_tool_calls() {
    let mut response = SemanticResponse::new();
    let mut candidate = ResponseCandidate::new(0);
    candidate.tool_calls.push(ToolCall {
        id: ToolCallId {
            local: "call_7".to_owned(),
            origin: IdOrigin::Source,
        },
        name: "lookup".to_owned(),
        arguments: ToolArguments::Parsed(json!({"q": 1})),
        extensions: Default::default(),
    });
    candidate.finish = Some(FinishReason::ToolCall);
    response.candidates.push(candidate);
    let frames = synthesize_client_frames(SourceProtocol::OpenAi, &response).unwrap();
    let joined = frames.join("");
    assert!(joined.contains("call_7"));
    assert!(joined.contains("lookup"));
    assert!(joined.contains("tool_calls"));
}

/// partial response:缺 terminal 时不判 complete,且不合成正常终止帧。
#[test]
fn partial_stream_is_incomplete_and_not_terminated() {
    let events = vec![sse(
        None,
        r#"{"id":"c1","choices":[{"index":0,"delta":{"content":"half"}}]}"#,
    )];
    let aggregated = aggregate_upstream_events(ProviderProtocol::OpenAi, &events);
    assert!(!aggregated.is_complete(), "缺 terminal 必须判不完整");
    assert_eq!(aggregated.violation, Some(StreamViolation::Incomplete));
    assert_eq!(aggregated.response.joined_text(), "half");

    let frames =
        synthesize_client_frames(SourceProtocol::OpenAi, &aggregated.response).unwrap();
    let joined = frames.join("");
    assert!(joined.contains("half"));
    assert!(
        !joined.contains("\"finish_reason\": \"stop\""),
        "断流不得合成正常 finish"
    );
}

/// 上游 error 事件让聚合结果判为失败而非成功。
#[test]
fn stream_error_marks_aggregate_failed() {
    let events = vec![sse(
        Some("error"),
        r#"{"type":"error","error":{"type":"overloaded_error","message":"busy"}}"#,
    )];
    let aggregated = aggregate_upstream_events(ProviderProtocol::Anthropic, &events);
    assert!(!aggregated.is_complete());
    assert_eq!(aggregated.violation, Some(StreamViolation::Failed));
}

/// Gemini 上游流聚合(Gemini 无 terminal sentinel,finishReason 收尾)。
#[test]
fn gemini_upstream_stream_aggregates() {
    let events = vec![sse(
        None,
        r#"{"candidates":[{"content":{"parts":[{"text":"ok"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":2,"candidatesTokenCount":1,"totalTokenCount":3}}"#,
    )];
    let aggregated = aggregate_upstream_events(ProviderProtocol::Gemini, &events);
    assert!(aggregated.is_complete());
    assert_eq!(aggregated.response.joined_text(), "ok");
    assert_eq!(aggregated.response.usage.unwrap().total, Some(3));
}

/// 往返:语义响应 → 事件 → 再聚合,文本与 finish 保持。
#[test]
fn response_events_roundtrip_preserves_semantics() {
    let mut response = SemanticResponse::new();
    let mut candidate = ResponseCandidate::new(0);
    candidate.content.push(ContentPart::Text {
        text: "round trip".to_owned(),
    });
    candidate.reasoning = Some(crate::semantic::model::ReasoningBlock {
        text: "because".to_owned(),
        signature: Some("sig".to_owned()),
        opaque: Vec::new(),
    });
    candidate.finish = Some(FinishReason::Stop);
    response.candidates.push(candidate);
    response.finish = Some(FinishReason::Stop);

    let mut aggregator = StreamAggregator::new();
    for event in response_to_events(&response) {
        aggregator.push(event).unwrap();
    }
    let roundtripped = aggregator.finish().unwrap();
    assert_eq!(roundtripped.joined_text(), "round trip");
    assert_eq!(
        roundtripped
            .first_candidate()
            .unwrap()
            .reasoning
            .as_ref()
            .unwrap()
            .text,
        "because"
    );
    assert_eq!(
        roundtripped
            .first_candidate()
            .unwrap()
            .reasoning
            .as_ref()
            .unwrap()
            .signature
            .as_deref(),
        Some("sig")
    );
    assert_eq!(roundtripped.finish, Some(FinishReason::Stop));
}

/// incomplete 响应不得在合成里被补成正常 finish。
#[test]
fn incomplete_response_events_do_not_fabricate_finish() {
    let mut response = SemanticResponse::new();
    let mut candidate = ResponseCandidate::new(0);
    candidate.content.push(ContentPart::Text {
        text: "cut".to_owned(),
    });
    response.candidates.push(candidate);
    response.incomplete = Some(crate::semantic::model::IncompleteReason::Other(
        "upstream_stream_incomplete".to_owned(),
    ));
    let events = response_to_events(&response);
    assert!(matches!(
        events.last(),
        Some(SemanticEvent::StreamEnd { incomplete: true })
    ));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, SemanticEvent::Finish { .. }))
    );
}

/// Responses 客户端合成帧带 lifecycle 事件名。
#[test]
fn responses_client_frames_use_lifecycle_names() {
    let mut response = SemanticResponse::new();
    let mut candidate = ResponseCandidate::new(0);
    candidate.content.push(ContentPart::Text {
        text: "hey".to_owned(),
    });
    candidate.finish = Some(FinishReason::Stop);
    response.candidates.push(candidate);
    let frames = synthesize_client_frames(SourceProtocol::OpenAiResponses, &response).unwrap();
    let joined = frames.join("");
    assert!(joined.contains("response.created"));
    assert!(joined.contains("response.output_text.delta"));
    assert!(joined.contains("response.completed"));
}

/// F-04/P4-1:Responses 终态快照进入流式事实并作为权威 output,
/// 不再与已收到的 delta 叠加成重复正文。
#[test]
fn terminal_responses_snapshot_replaces_streamed_deltas() {
    let events = vec![
        sse(
            Some("response.output_item.added"),
            r#"{"output_index":0,"item":{"id":"msg_1","type":"message","role":"assistant","content":[]}}"#,
        ),
        sse(
            Some("response.content_part.added"),
            r#"{"output_index":0,"content_index":0,"item_id":"msg_1","part":{"type":"output_text","text":""}}"#,
        ),
        sse(
            Some("response.output_text.delta"),
            r#"{"output_index":0,"content_index":0,"delta":"he"}"#,
        ),
        sse(
            Some("response.output_text.delta"),
            r#"{"output_index":0,"content_index":0,"delta":"llo"}"#,
        ),
        sse(
            Some("response.output_item.done"),
            r#"{"output_index":0,"item":{"id":"msg_1","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"hello"}]}}"#,
        ),
        sse(
            Some("response.completed"),
            r#"{"response":{"id":"resp_1","status":"completed","usage":{"input_tokens":3,"output_tokens":2,"total_tokens":5},"output":[{"id":"msg_1","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"hello"}]}]}}"#,
        ),
    ];
    let aggregated = aggregate_upstream_events(ProviderProtocol::OpenAi, &events);
    assert!(aggregated.is_complete());
    assert_eq!(
        aggregated.response.joined_text(),
        "hello",
        "快照是权威终值,不能把 delta 再叠加一遍"
    );
    assert_eq!(aggregated.response.usage.unwrap().input, Some(3));
}

fn frame_values(frames: &[String]) -> Vec<Value> {
    frames
        .iter()
        .filter_map(|frame| crate::sse::parse_sse_block(frame.trim().trim_end_matches('\n')))
        .filter_map(|event| serde_json::from_str(&event.data).ok())
        .collect()
}

/// P5-1:合成到 Anthropic 时,reasoning/正文/工具使用互不相同的
/// 全局 block index,不能因正文 part 也编号 0 而互相覆盖。
#[test]
fn anthropic_target_synthesis_assigns_global_block_indices() {
    let mut response = SemanticResponse::new();
    let mut candidate = ResponseCandidate::new(0);
    candidate.reasoning = Some(crate::semantic::model::ReasoningBlock {
        text: "think".to_owned(),
        signature: Some("sig".to_owned()),
        opaque: Vec::new(),
    });
    candidate.content.push(ContentPart::Text {
        text: "answer".to_owned(),
    });
    candidate.tool_calls.push(ToolCall {
        id: ToolCallId {
            local: "call_1".to_owned(),
            origin: IdOrigin::Source,
        },
        name: "lookup".to_owned(),
        arguments: ToolArguments::Parsed(json!({"q": "x"})),
        extensions: Default::default(),
    });
    candidate.finish = Some(FinishReason::ToolCall);
    response.candidates.push(candidate);
    response.finish = Some(FinishReason::ToolCall);

    let frames = synthesize_client_frames(SourceProtocol::Anthropic, &response).unwrap();
    let values = frame_values(&frames);
    let starts: Vec<(usize, String)> = values
        .iter()
        .filter(|value| {
            value.get("type").and_then(Value::as_str) == Some("content_block_start")
        })
        .map(|value| {
            (
                value.get("index").and_then(Value::as_u64).unwrap() as usize,
                value
                    .pointer("/content_block/type")
                    .and_then(Value::as_str)
                    .unwrap()
                    .to_owned(),
            )
        })
        .collect();
    assert_eq!(
        starts,
        vec![
            (0, "thinking".to_owned()),
            (1, "text".to_owned()),
            (2, "tool_use".to_owned()),
        ]
    );
    assert_eq!(
        values
            .iter()
            .filter(|value| value.get("type").and_then(Value::as_str) == Some("message_stop"))
            .count(),
        1
    );
}

/// P5-1:合成到 Chat 时多候选各自保留 index,refusal 不被改成正文。
#[test]
fn chat_target_synthesis_keeps_candidates_and_refusal() {
    let mut response = SemanticResponse::new();
    let mut first = ResponseCandidate::new(0);
    first.content.push(ContentPart::Text {
        text: "ok".to_owned(),
    });
    first.finish = Some(FinishReason::Stop);
    let mut second = ResponseCandidate::new(1);
    second.content.push(ContentPart::Refusal {
        text: "cannot comply".to_owned(),
    });
    second.finish = Some(FinishReason::Refusal);
    response.candidates.push(first);
    response.candidates.push(second);
    response.refusal = Some("cannot comply".to_owned());
    response.finish = Some(FinishReason::Stop);

    let frames = synthesize_client_frames(SourceProtocol::OpenAi, &response).unwrap();
    let values = frame_values(&frames);
    let indices: Vec<u64> = values
        .iter()
        .filter_map(|value| value.pointer("/choices/0/index").and_then(Value::as_u64))
        .collect();
    assert!(indices.contains(&0));
    assert!(indices.contains(&1));
    assert!(values.iter().any(|value| {
        value
            .pointer("/choices/0/delta/refusal")
            .and_then(Value::as_str)
            == Some("cannot comply")
    }));
}

/// P5-1:Gemini 合成在终态前组装完整 functionCall 参数对象。
#[test]
fn gemini_target_synthesis_emits_complete_tool_arguments() {
    let mut response = SemanticResponse::new();
    let mut candidate = ResponseCandidate::new(0);
    candidate.tool_calls.push(ToolCall {
        id: ToolCallId {
            local: "call_9".to_owned(),
            origin: IdOrigin::Source,
        },
        name: "lookup".to_owned(),
        arguments: ToolArguments::Parsed(json!({"q": "x", "limit": 2})),
        extensions: Default::default(),
    });
    candidate.finish = Some(FinishReason::ToolCall);
    response.candidates.push(candidate);

    let frames = synthesize_client_frames(SourceProtocol::Gemini, &response).unwrap();
    let call = frame_values(&frames)
        .into_iter()
        .find_map(|value| {
            value
                .pointer("/candidates/0/content/parts/0/functionCall")
                .cloned()
        })
        .expect("functionCall frame");
    assert_eq!(call["name"], json!("lookup"));
    assert_eq!(call["args"], json!({"q": "x", "limit": 2}));
}

/// P5-1:不可表达的 content part 显式失败,不静默丢掉后返回成功流。
#[test]
fn synthesis_rejects_unsupported_content_parts() {
    let mut response = SemanticResponse::new();
    let mut candidate = ResponseCandidate::new(0);
    candidate.content.push(ContentPart::ImageInline {
        media_type: Some("image/png".to_owned()),
        data_base64: "AAAA".to_owned(),
    });
    response.candidates.push(candidate);
    let error = synthesize_client_frames(SourceProtocol::OpenAi, &response).unwrap_err();
    assert!(error.message.contains("image_inline"), "{}", error.message);
}

/// P5-1:缓存/ A6 的客户端 JSON 先解码回语义响应,再经目标编码器重放;
/// Chat 的顶层 refusal 不会因重放丢失。
#[test]
fn cached_client_json_replays_through_semantic_decoder() {
    let payload = json!({
        "id": "cached_1",
        "object": "chat.completion",
        "model": "m",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "", "refusal": "no"},
            "finish_reason": "refusal",
        }],
    });
    let frames = synthesize_client_response_frames("openai", &payload).unwrap();
    assert!(frame_values(&frames).iter().any(|value| {
        value
            .pointer("/choices/0/delta/refusal")
            .and_then(Value::as_str)
            == Some("no")
    }));
}

/// F-04:终态快照的格式进入 SourceFacts,目标编码器据此对账。
#[test]
fn snapshot_fact_carries_responses_format() {
    let facts = crate::semantic::source::SourceFacts::from_sse(
        ProviderProtocol::OpenAi,
        &sse(
            Some("response.completed"),
            r#"{"response":{"id":"resp_1","status":"completed","output":[]}}"#,
        ),
    );
    let snapshot = facts.snapshot.expect("snapshot");
    assert_eq!(
        snapshot.format,
        crate::semantic::ir::SnapshotFormat::OpenAiResponses
    );
}
