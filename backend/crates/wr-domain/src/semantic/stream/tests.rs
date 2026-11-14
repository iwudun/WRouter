use serde_json::{Value, json};

use crate::provider::ProviderProtocol;
use crate::semantic::error::SemanticError;
use crate::semantic::ir::StreamUpdate;
use crate::semantic::model::{
    ContentPart, FinishReason, SemanticEvent, ToolArguments, ToolCall, ToolCallId, Usage,
};
use crate::sse::SseEvent;

use super::*;


    fn sse(name: Option<&str>, body: &str) -> SseEvent {
        SseEvent {
            event: name.map(str::to_owned),
            data: body.to_owned(),
            raw: body.to_owned(),
        }
    }

    fn decode_events(protocol: ProviderProtocol, event: &SseEvent) -> Vec<SemanticEvent> {
        decode_upstream_frame(protocol, event).events
    }

    #[test]
    fn lifecycle_tracks_terminal_and_late_usage_without_zero_fabrication() {
        let mut lifecycle = StreamLifecycle::new();
        let mut usage = Usage::reported();
        usage.input = Some(4);
        lifecycle.observe(&SemanticEvent::Usage(usage)).unwrap();
        lifecycle
            .observe(&SemanticEvent::Finish {
                candidate: 0,
                reason: FinishReason::Stop,
            })
            .unwrap();
        lifecycle
            .observe(&SemanticEvent::StreamEnd { incomplete: false })
            .unwrap();
        assert!(lifecycle.complete());
        assert_eq!(lifecycle.usage().unwrap().input, Some(4));
        assert_eq!(lifecycle.usage().unwrap().output, None);
        assert_eq!(
            lifecycle.observe(&SemanticEvent::Usage(usage)),
            Err(StreamViolation::EventAfterTerminal)
        );
    }

    #[test]
    fn responses_terminal_frame_keeps_usage_and_finish() {
        let decoded = decode_events(
            ProviderProtocol::OpenAi,
            &sse(
                Some("response.completed"),
                r#"{"response":{"usage":{"input_tokens":8,"output_tokens":2}}}"#,
            ),
        );
        assert_eq!(decoded.len(), 3);
        assert!(matches!(decoded[0], SemanticEvent::Usage(_)));
        assert!(matches!(decoded[1], SemanticEvent::Finish { .. }));
        assert!(matches!(
            decoded[2],
            SemanticEvent::StreamEnd { incomplete: false }
        ));
    }

    #[test]
    fn anthropic_tool_json_delta_maps_to_tool_argument_fragment() {
        let decoded = decode_events(
            ProviderProtocol::Anthropic,
            &sse(
                Some("content_block_delta"),
                r#"{"index":1,"delta":{"type":"input_json_delta","partial_json":"{\"x\":"}}"#,
            ),
        );
        assert!(
            matches!(&decoded[0], SemanticEvent::ToolArgumentsDelta { index: 1, fragment, .. } if fragment == "{\"x\":")
        );
    }

    #[test]
    fn unknown_upstream_events_are_preserved() {
        let decoded = decode_events(
            ProviderProtocol::Gemini,
            &sse(Some("vendor"), r#"{"future":true}"#),
        );
        assert!(matches!(decoded[0], SemanticEvent::Opaque { .. }));
    }

    #[test]
    fn registry_exposes_upstream_stream_adapters() {
        assert_eq!(
            outbound_stream(ProviderProtocol::OpenAi).protocol(),
            ProviderProtocol::OpenAi
        );
        assert_eq!(
            outbound_stream(ProviderProtocol::Anthropic).protocol(),
            ProviderProtocol::Anthropic
        );
        assert_eq!(
            outbound_stream(ProviderProtocol::Gemini).protocol(),
            ProviderProtocol::Gemini
        );
    }

    #[test]
    fn aggregator_reassembles_text_reasoning_and_tool_arguments() {
        let mut aggregator = StreamAggregator::new();
        aggregator
            .push(SemanticEvent::MessageStart {
                id: Some("r1".into()),
                model: Some("m1".into()),
            })
            .unwrap();
        aggregator
            .push(SemanticEvent::PartDelta {
                candidate: 0,
                index: 0,
                text: "hello ".into(),
            })
            .unwrap();
        aggregator
            .push(SemanticEvent::PartDelta {
                candidate: 0,
                index: 0,
                text: "world".into(),
            })
            .unwrap();
        aggregator
            .push(SemanticEvent::ReasoningDelta {
                candidate: 0,
                index: 1,
                text: "think".into(),
            })
            .unwrap();
        let call = ToolCall {
            id: ToolCallId {
                local: "call-1".into(),
                origin: crate::semantic::model::IdOrigin::Source,
            },
            name: "lookup".into(),
            arguments: ToolArguments::Raw("{\\\"q\\\":".into()),
            extensions: Default::default(),
        };
        aggregator
            .push(SemanticEvent::ToolCallStart {
                candidate: 0,
                index: 0,
                call,
            })
            .unwrap();
        aggregator
            .push(SemanticEvent::ToolArgumentsDelta {
                candidate: 0,
                index: 0,
                fragment: "1}".into(),
            })
            .unwrap();
        aggregator
            .push(SemanticEvent::Finish {
                candidate: 0,
                reason: FinishReason::ToolCall,
            })
            .unwrap();
        aggregator
            .push(SemanticEvent::StreamEnd { incomplete: false })
            .unwrap();
        let response = aggregator.finish().unwrap();
        assert_eq!(response.id.as_deref(), Some("r1"));
        assert_eq!(response.first_candidate().unwrap().content.len(), 1);
        assert!(
            matches!(&response.first_candidate().unwrap().content[0], ContentPart::Text { text } if text == "hello world")
        );
        assert_eq!(
            response
                .first_candidate()
                .unwrap()
                .reasoning
                .as_ref()
                .unwrap()
                .text,
            "think"
        );
        assert_eq!(
            response.first_candidate().unwrap().tool_calls[0]
                .arguments
                .as_raw_string(),
            "{\\\"q\\\":1}"
        );
    }

    /// W1 characterization(Q-06):响应缓存依赖聚合器完整保留长流内容。
    /// W2-5 的预算只约束超限缓存,不得截断正常长度的聚合结果。
    #[test]
    fn aggregator_keeps_long_stream_for_cache_assembly() {
        let mut aggregator = StreamAggregator::new();
        aggregator
            .push(SemanticEvent::MessageStart {
                id: Some("r-long".into()),
                model: Some("m1".into()),
            })
            .unwrap();
        let mut expected = String::new();
        for index in 0..256u16 {
            let delta = format!("{index:04}");
            expected.push_str(&delta);
            aggregator
                .push(SemanticEvent::PartDelta {
                    candidate: 0,
                    index: 0,
                    text: delta,
                })
                .unwrap();
        }
        aggregator
            .push(SemanticEvent::Finish {
                candidate: 0,
                reason: FinishReason::Stop,
            })
            .unwrap();
        aggregator
            .push(SemanticEvent::StreamEnd { incomplete: false })
            .unwrap();
        let response = aggregator.finish().unwrap();
        assert_eq!(
            response
                .first_candidate()
                .unwrap()
                .content
                .first()
                .and_then(|part| match part {
                    ContentPart::Text { text } => Some(text.as_str()),
                    _ => None,
                }),
            Some(expected.as_str())
        );
    }

    /// W2-5:超出预算后聚合器停止累积并标记 suppressed,调用方据此
    /// 放弃写缓存(不写入截断结果)。
    #[test]
    fn aggregator_budget_suppresses_oversized_stream() {
        let mut aggregator = StreamAggregator::with_budget(512);
        aggregator
            .push(SemanticEvent::MessageStart {
                id: Some("r-big".into()),
                model: Some("m1".into()),
            })
            .unwrap();
        for _ in 0..100 {
            aggregator
                .push(SemanticEvent::PartDelta {
                    candidate: 0,
                    index: 0,
                    text: "0123456789abcdef".into(),
                })
                .unwrap();
        }
        assert!(aggregator.is_suppressed(), "超限必须停止累积");
        let response = aggregator.into_partial_response();
        let retained = response
            .first_candidate()
            .and_then(|candidate| candidate.content.first())
            .and_then(|part| match part {
                ContentPart::Text { text } => Some(text.len()),
                _ => None,
            })
            .unwrap_or(0);
        assert!(
            retained < 100 * 16,
            "超限后不得继续累积全部内容: {retained}"
        );
    }

    /// W2-5:预算内的正常流不受影响,内容完整保留。
    #[test]
    fn aggregator_budget_keeps_stream_within_limit() {
        let mut aggregator = StreamAggregator::with_budget(64 * 1024);
        aggregator
            .push(SemanticEvent::MessageStart {
                id: Some("r-small".into()),
                model: Some("m1".into()),
            })
            .unwrap();
        let mut expected = String::new();
        for index in 0..256u16 {
            let delta = format!("{index:04}");
            expected.push_str(&delta);
            aggregator
                .push(SemanticEvent::PartDelta {
                    candidate: 0,
                    index: 0,
                    text: delta,
                })
                .unwrap();
        }
        assert!(!aggregator.is_suppressed(), "预算内不得误抑制");
        aggregator
            .push(SemanticEvent::Finish {
                candidate: 0,
                reason: FinishReason::Stop,
            })
            .unwrap();
        aggregator
            .push(SemanticEvent::StreamEnd { incomplete: false })
            .unwrap();
        let response = aggregator.finish().unwrap();
        assert_eq!(
            response
                .first_candidate()
                .unwrap()
                .content
                .first()
                .and_then(|part| match part {
                    ContentPart::Text { text } => Some(text.as_str()),
                    _ => None,
                }),
            Some(expected.as_str())
        );
    }

    #[test]
    fn openai_chat_delta_maps_content_reasoning_and_tool_fragments() {
        let decoded = decode_events(
            ProviderProtocol::OpenAi,
            &sse(
                None,
                r#"{"choices":[{"index":0,"delta":{"content":"hi","reasoning_content":"why","tool_calls":[{"index":2,"id":"c2","function":{"name":"lookup","arguments":"{\\\"q\\\":"}}]},"finish_reason":null}]}"#,
            ),
        );
        assert!(
            decoded.iter().any(
                |event| matches!(event, SemanticEvent::PartDelta { text, .. } if text == "hi")
            )
        );
        assert!(decoded.iter().any(
            |event| matches!(event, SemanticEvent::ReasoningDelta { text, .. } if text == "why")
        ));
        assert!(
            decoded
                .iter()
                .any(|event| matches!(event, SemanticEvent::ToolArgumentsDelta { index: 2, .. }))
        );
    }

    #[test]
    fn gemini_delta_maps_candidate_text_finish_and_usage() {
        let decoded = decode_events(
            ProviderProtocol::Gemini,
            &sse(
                None,
                r#"{"candidates":[{"content":{"parts":[{"text":"hello"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":5,"candidatesTokenCount":2,"totalTokenCount":7}}"#,
            ),
        );
        assert!(decoded.iter().any(|event| matches!(event, SemanticEvent::PartDelta { candidate: 0, text, .. } if text == "hello")));
        assert!(
            decoded.iter().any(
                |event| matches!(event, SemanticEvent::Usage(usage) if usage.input == Some(5))
            )
        );
        assert!(decoded.iter().any(|event| matches!(
            event,
            SemanticEvent::Finish {
                reason: FinishReason::Stop,
                ..
            }
        )));
    }

    #[test]
    fn stream_aggregator_preserves_opaque_and_rejects_failed_or_incomplete() {
        let mut opaque = StreamAggregator::new();
        opaque
            .push(SemanticEvent::Opaque {
                raw: json!({"vendor":"future"}),
            })
            .unwrap();
        opaque
            .push(SemanticEvent::Finish {
                candidate: 0,
                reason: FinishReason::Stop,
            })
            .unwrap();
        opaque
            .push(SemanticEvent::StreamEnd { incomplete: false })
            .unwrap();
        let response = opaque.finish().unwrap();
        assert_eq!(response.opaque.len(), 1);
        assert_eq!(response.opaque[0].value, json!({"vendor":"future"}));

        let mut failed = StreamAggregator::new();
        failed
            .push(SemanticEvent::Error(SemanticError::unsupported("failed")))
            .unwrap();
        assert!(matches!(failed.finish(), Err(StreamViolation::Failed)));

        let incomplete = StreamAggregator::new();
        assert!(matches!(
            incomplete.finish(),
            Err(StreamViolation::Incomplete)
        ));
    }

    #[test]
    fn transport_eof_without_finish_is_incomplete() {
        let mut lifecycle = StreamLifecycle::new();
        assert!(!lifecycle.complete());
        assert_eq!(lifecycle.status(), StreamStatus::Incomplete);
        lifecycle.interrupt();
    }

    /// 2026-10-13:流式 usage 提取必须带上缓存读写字段,否则调用日志里
    /// 流式请求的「缓存读/缓存写」恒 0(与 normalize_usage 方言链对齐)。
    #[test]
    fn streaming_usage_captures_cache_counters() {
        let openai = decode_events(
            ProviderProtocol::OpenAi,
            &sse(
                None,
                r#"{"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":10,"total_tokens":110,"prompt_tokens_details":{"cached_tokens":64,"cache_write_tokens":16}}}"#,
            ),
        );
        let usage = openai
            .iter()
            .find_map(|e| match e {
                SemanticEvent::Usage(u) => Some(*u),
                _ => None,
            })
            .expect("openai usage event");
        assert_eq!(usage.cache_read, Some(64));
        assert_eq!(usage.cache_write, Some(16));

        let responses = decode_events(
            ProviderProtocol::OpenAi,
            &sse(
                Some("response.completed"),
                r#"{"response":{"id":"r","status":"completed","usage":{"input_tokens":100,"output_tokens":10,"total_tokens":110,"input_tokens_details":{"cached_tokens":64},"output_tokens_details":{"reasoning_tokens":3}}}}"#,
            ),
        );
        let usage = responses
            .iter()
            .find_map(|e| match e {
                SemanticEvent::Usage(u) => Some(*u),
                _ => None,
            })
            .expect("responses usage event");
        assert_eq!(usage.cache_read, Some(64));
        assert_eq!(usage.reasoning, Some(3));

        let anthropic = decode_events(
            ProviderProtocol::Anthropic,
            &sse(
                Some("message_start"),
                r#"{"message":{"id":"a","usage":{"input_tokens":100,"output_tokens":0,"cache_read_input_tokens":64,"cache_creation_input_tokens":16}}}"#,
            ),
        );
        let usage = anthropic
            .iter()
            .find_map(|e| match e {
                SemanticEvent::Usage(u) => Some(*u),
                _ => None,
            })
            .expect("anthropic usage event");
        assert_eq!(usage.cache_read, Some(64));
        assert_eq!(usage.cache_write, Some(16));

        let gemini = decode_events(
            ProviderProtocol::Gemini,
            &sse(
                None,
                r#"{"usageMetadata":{"promptTokenCount":100,"candidatesTokenCount":10,"totalTokenCount":110,"cachedContentTokenCount":64}}"#,
            ),
        );
        let usage = gemini
            .iter()
            .find_map(|e| match e {
                SemanticEvent::Usage(u) => Some(*u),
                _ => None,
            })
            .expect("gemini usage event");
        assert_eq!(usage.cache_read, Some(64));
    }

    // -----------------------------------------------------------------------
    // C3:Responses lifecycle / item id
    // -----------------------------------------------------------------------

    #[test]
    fn responses_lifecycle_events_cover_items_and_terminal() {
        let started = decode_events(
            ProviderProtocol::OpenAi,
            &sse(
                Some("response.created"),
                r#"{"response":{"id":"resp_1","model":"gpt-5"}}"#,
            ),
        );
        assert!(matches!(
            &started[0],
            SemanticEvent::MessageStart { id, model }
                if id.as_deref() == Some("resp_1") && model.as_deref() == Some("gpt-5")
        ));

        let item_added = decode_events(
            ProviderProtocol::OpenAi,
            &sse(
                Some("response.output_item.added"),
                r#"{"output_index":1,"item":{"type":"function_call","call_id":"call_9","name":"lookup","arguments":""}}"#,
            ),
        );
        assert!(matches!(
            &item_added[0],
            SemanticEvent::ToolCallStart { index: 1, call, .. }
                if call.id.local == "call_9" && call.name == "lookup"
        ));

        // item id 无内核槽位 → 旧事件以 opaque 保留完整 item;层级 IR
        // 用 ItemSnapshot/ItemEnd 携带权威终值与身份。
        let item_frame = decode_upstream_frame(
            ProviderProtocol::OpenAi,
            &sse(
                Some("response.output_item.done"),
                r#"{"output_index":1,"item":{"id":"fc_1","call_id":"call_9","type":"function_call","name":"lookup","arguments":"{\"q\":1}"}}"#,
            ),
        );
        assert!(item_frame.events.iter().any(|event| matches!(
            event,
            SemanticEvent::Opaque { raw }
                if raw.pointer("/item/id").and_then(Value::as_str) == Some("fc_1")
        )));
        assert!(item_frame.updates.iter().any(|update| matches!(
            update,
            StreamUpdate::ItemSnapshot { key, snapshot }
                if key.item == 1
                    && snapshot.pointer("/id").and_then(Value::as_str) == Some("fc_1")
        )));
        assert!(item_frame.updates.iter().any(|update| matches!(
            update,
            StreamUpdate::ItemEnd { key } if key.item == 1
        )));

        // incomplete_details.reason 决定 finish 语义。
        let incomplete = decode_events(
            ProviderProtocol::OpenAi,
            &sse(
                Some("response.incomplete"),
                r#"{"response":{"usage":{"input_tokens":1},"incomplete_details":{"reason":"max_output_tokens"}}}"#,
            ),
        );
        assert!(incomplete.iter().any(|event| matches!(
            event,
            SemanticEvent::Finish {
                reason: FinishReason::Length,
                ..
            }
        )));
        assert!(matches!(
            incomplete.last(),
            Some(SemanticEvent::StreamEnd { incomplete: true })
        ));
    }

    #[test]
    fn responses_in_progress_is_recognized_not_opaque() {
        let decoded = decode_events(
            ProviderProtocol::OpenAi,
            &sse(
                Some("response.in_progress"),
                r#"{"response":{"id":"resp_1","model":"m"}}"#,
            ),
        );
        assert_eq!(decoded.len(), 1);
        assert!(matches!(decoded[0], SemanticEvent::MessageStart { .. }));
    }

    // -----------------------------------------------------------------------
    // C4:Anthropic block lifecycle / signature / ping / error
    // -----------------------------------------------------------------------

    #[test]
    fn anthropic_block_lifecycle_and_signature_are_preserved() {
        let start = decode_events(
            ProviderProtocol::Anthropic,
            &sse(
                Some("content_block_start"),
                r#"{"index":0,"content_block":{"type":"thinking","thinking":""}}"#,
            ),
        );
        assert!(matches!(
            start[0],
            SemanticEvent::ReasoningStart { index: 0, .. }
        ));

        let signature = decode_events(
            ProviderProtocol::Anthropic,
            &sse(
                Some("content_block_delta"),
                r#"{"index":0,"delta":{"type":"signature_delta","signature":"sig-abc"}}"#,
            ),
        );
        assert!(matches!(
            &signature[0],
            SemanticEvent::ReasoningSignature { signature, .. } if signature == "sig-abc"
        ));

        let stop = decode_events(
            ProviderProtocol::Anthropic,
            &sse(Some("content_block_stop"), r#"{"index":0}"#),
        );
        assert!(matches!(stop[0], SemanticEvent::PartEnd { index: 0, .. }));

        // ping 无语义:不产生事件,也不算 unknown opaque。
        let ping = decode_events(
            ProviderProtocol::Anthropic,
            &sse(Some("ping"), r#"{"type":"ping"}"#),
        );
        assert!(ping.is_empty(), "ping 不应变成 opaque 事件");

        // 真正未知事件仍保留。
        let unknown = decode_events(
            ProviderProtocol::Anthropic,
            &sse(Some("future_event"), r#"{"type":"future_event","x":1}"#),
        );
        assert!(matches!(unknown[0], SemanticEvent::Opaque { .. }));
    }

    #[test]
    fn anthropic_redacted_thinking_and_unknown_blocks_survive() {
        let redacted = decode_events(
            ProviderProtocol::Anthropic,
            &sse(
                Some("content_block_start"),
                r#"{"index":2,"content_block":{"type":"redacted_thinking","data":"opaque"}}"#,
            ),
        );
        assert!(matches!(
            &redacted[0],
            SemanticEvent::PartStart {
                part: ContentPart::Opaque { kind_hint: Some(kind), .. },
                ..
            } if kind == "redacted_thinking"
        ));
        let unknown = decode_events(
            ProviderProtocol::Anthropic,
            &sse(
                Some("content_block_start"),
                r#"{"index":3,"content_block":{"type":"server_tool_use","id":"s1"}}"#,
            ),
        );
        assert!(matches!(
            &unknown[0],
            SemanticEvent::PartStart {
                part: ContentPart::Opaque { kind_hint: Some(kind), .. },
                ..
            } if kind == "server_tool_use"
        ));
    }

    #[test]
    fn anthropic_error_event_carries_upstream_details() {
        let decoded = decode_events(
            ProviderProtocol::Anthropic,
            &sse(
                Some("error"),
                r#"{"type":"error","error":{"type":"overloaded_error","message":"overloaded"}}"#,
            ),
        );
        assert!(matches!(
            &decoded[0],
            SemanticEvent::Error(error)
                if error.message == "overloaded" && error.legacy_type.as_deref() == Some("overloaded_error")
        ));
    }

    // -----------------------------------------------------------------------
    // C5:Gemini thought / functionCall / functionResponse / safety
    // -----------------------------------------------------------------------

    #[test]
    fn gemini_stream_maps_thought_signature_calls_and_safety() {
        let decoded = decode_events(
            ProviderProtocol::Gemini,
            &sse(
                None,
                r#"{"candidates":[{"content":{"parts":[{"text":"why","thought":true,"thoughtSignature":"ts-1"},{"functionCall":{"name":"lookup","args":{"q":1}}}]},"safetyRatings":[{"category":"HARM_CATEGORY_DANGEROUS_CONTENT","probability":"LOW"}]}]}"#,
            ),
        );
        assert!(decoded.iter().any(
            |event| matches!(event, SemanticEvent::ReasoningDelta { text, .. } if text == "why")
        ));
        assert!(decoded.iter().any(|event| matches!(
            event,
            SemanticEvent::ReasoningSignature { signature, .. } if signature == "ts-1"
        )));
        assert!(decoded.iter().any(|event| matches!(
            event,
            SemanticEvent::ToolCallStart { call, .. } if call.name == "lookup"
        )));
        assert!(decoded.iter().any(|event| matches!(
            event,
            SemanticEvent::Opaque { raw } if raw.get("safetyRatings").is_some()
        )));
    }

    #[test]
    fn gemini_block_reason_and_error_are_semantic_errors() {
        let blocked = decode_events(
            ProviderProtocol::Gemini,
            &sse(None, r#"{"promptFeedback":{"blockReason":"SAFETY"}}"#),
        );
        assert!(matches!(blocked[0], SemanticEvent::Error(_)));

        let failed = decode_events(
            ProviderProtocol::Gemini,
            &sse(
                None,
                r#"{"error":{"code":"503","message":"overloaded","status":"UNAVAILABLE"}}"#,
            ),
        );
        assert!(matches!(
            &failed[0],
            SemanticEvent::Error(error) if error.code.as_deref() == Some("503")
        ));
    }

    #[test]
    fn gemini_max_tokens_is_length_not_incomplete() {
        let decoded = decode_events(
            ProviderProtocol::Gemini,
            &sse(
                None,
                r#"{"candidates":[{"content":{"parts":[{"text":"x"}]},"finishReason":"MAX_TOKENS"}]}"#,
            ),
        );
        assert!(matches!(
            decoded
                .iter()
                .find(|event| matches!(event, SemanticEvent::Finish { .. })),
            Some(SemanticEvent::Finish {
                reason: FinishReason::Length,
                ..
            })
        ));
        assert!(matches!(
            decoded.last(),
            Some(SemanticEvent::StreamEnd { incomplete: false })
        ));
    }

    #[test]
    fn openai_upstream_error_object_becomes_semantic_error() {
        let decoded = decode_events(
            ProviderProtocol::OpenAi,
            &sse(
                None,
                r#"{"error":{"message":"bad key","type":"invalid_request_error","code":"invalid_api_key"}}"#,
            ),
        );
        assert!(matches!(
            &decoded[0],
            SemanticEvent::Error(error)
                if error.message == "bad key" && error.code.as_deref() == Some("invalid_api_key")
        ));
    }

    // -----------------------------------------------------------------------
    // C1/C4:签名与块边界进入聚合结果
    // -----------------------------------------------------------------------

    #[test]
    fn aggregator_keeps_reasoning_signature_and_marks_part_end() {
        let mut aggregator = StreamAggregator::new();
        aggregator
            .push(SemanticEvent::ReasoningStart {
                candidate: 0,
                index: 0,
            })
            .unwrap();
        aggregator
            .push(SemanticEvent::ReasoningDelta {
                candidate: 0,
                index: 0,
                text: "think".into(),
            })
            .unwrap();
        aggregator
            .push(SemanticEvent::ReasoningSignature {
                candidate: 0,
                index: 0,
                signature: "sig-a".into(),
            })
            .unwrap();
        aggregator
            .push(SemanticEvent::ReasoningSignature {
                candidate: 0,
                index: 0,
                signature: "sig-b".into(),
            })
            .unwrap();
        aggregator
            .push(SemanticEvent::Finish {
                candidate: 0,
                reason: FinishReason::Stop,
            })
            .unwrap();
        aggregator
            .push(SemanticEvent::StreamEnd { incomplete: false })
            .unwrap();
        let response = aggregator.finish().unwrap();
        let reasoning = response
            .first_candidate()
            .unwrap()
            .reasoning
            .as_ref()
            .unwrap();
        assert_eq!(reasoning.text, "think");
        assert_eq!(reasoning.signature.as_deref(), Some("sig-asig-b"));
    }

    #[test]
    fn partial_response_marks_incomplete_without_fabricating_finish() {
        let mut aggregator = StreamAggregator::new();
        aggregator
            .push(SemanticEvent::PartDelta {
                candidate: 0,
                index: 0,
                text: "half".into(),
            })
            .unwrap();
        let response = aggregator.into_partial_response();
        assert_eq!(response.joined_text(), "half");
        assert!(
            response.incomplete.is_some(),
            "缺 terminal 必须标 incomplete"
        );
        assert!(response.finish.is_none(), "不得伪造 finish");
    }
