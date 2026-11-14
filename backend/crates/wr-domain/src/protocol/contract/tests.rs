
use super::*;

fn plan(
    source: SourceProtocol,
    target: ProviderProtocol,
    stream: bool,
) -> Result<ExchangePlan, ContractViolation> {
    plan_for_source(source, target, stream, false)
}

#[test]
fn same_protocol_is_raw_byte_exact() {
    for (source, target) in [
        (SourceProtocol::OpenAi, ProviderProtocol::OpenAi),
        (SourceProtocol::Anthropic, ProviderProtocol::Anthropic),
        (SourceProtocol::Gemini, ProviderProtocol::Gemini),
        (SourceProtocol::GeminiStream, ProviderProtocol::Gemini),
        (SourceProtocol::OpenAiEmbeddings, ProviderProtocol::OpenAi),
    ] {
        let stream = source == SourceProtocol::GeminiStream;
        let plan = plan(source, target, stream).unwrap();
        assert!(plan.is_byte_exact(), "{source:?} -> {target:?}");
        assert_eq!(plan.request.mode, BodyMode::Raw);
        assert_eq!(plan.response.mode, BodyMode::Raw);
        assert!(plan.loss.is_lossless());
    }
}

#[test]
fn native_responses_flag_selects_raw_or_translate() {
    let raw = plan_for_source(
        SourceProtocol::OpenAiResponses,
        ProviderProtocol::OpenAi,
        false,
        true,
    )
    .unwrap();
    assert!(raw.is_byte_exact());
    let translated = plan_for_source(
        SourceProtocol::OpenAiResponses,
        ProviderProtocol::OpenAi,
        false,
        false,
    )
    .unwrap();
    assert_eq!(translated.request.mode, BodyMode::Translate);
    assert_eq!(translated.fidelity(), FidelityLevel::SemanticEquivalent);
}

#[test]
fn cross_protocol_is_translate_and_declares_capabilities() {
    let plan = plan(SourceProtocol::Anthropic, ProviderProtocol::OpenAi, true).unwrap();
    assert_eq!(plan.request.mode, BodyMode::Translate);
    assert_eq!(plan.response.fidelity, FidelityLevel::SemanticEquivalent);
    assert_eq!(plan.response_representation, ResponseRepresentation::Sse);
    assert!(!plan.loss.is_lossless());
    assert_eq!(plan.loss.capabilities().len(), Capability::ALL.len());
    for entry in plan.loss.capabilities() {
        if !matches!(
            entry.support,
            CapabilitySupportKind::Native | CapabilitySupportKind::Equivalent
        ) {
            assert!(
                entry.note.is_some(),
                "{} lacks a reason",
                entry.capability.as_str()
            );
        }
    }
    assert!(plan.loss.support_for(Capability::Refusal).is_some());
}

#[test]
fn forbidden_combinations_are_rejected() {
    for target in [ProviderProtocol::Anthropic, ProviderProtocol::Gemini] {
        let error = plan(SourceProtocol::OpenAiEmbeddings, target, false).unwrap_err();
        assert!(matches!(
            error,
            ContractViolation::UnsupportedCombination { .. }
        ));
    }
    for target in [ProviderProtocol::OpenAi, ProviderProtocol::Anthropic] {
        let error = plan(SourceProtocol::GeminiInteractions, target, false).unwrap_err();
        assert!(matches!(
            error,
            ContractViolation::UnsupportedCombination { .. }
        ));
    }
}

#[test]
fn interactions_to_gemini_is_patch_request_translate_response() {
    let plan = plan(
        SourceProtocol::GeminiInteractions,
        ProviderProtocol::Gemini,
        false,
    )
    .unwrap();
    assert_eq!(plan.request.mode, BodyMode::Patch);
    assert_eq!(plan.request.fidelity, FidelityLevel::StructureExact);
    assert_eq!(plan.response.mode, BodyMode::Translate);
    assert_eq!(plan.fidelity(), FidelityLevel::SemanticEquivalent);
}

/// W2-3:Interactions 只有非流契约;`stream=true` 在规划层显式拒绝,
/// 不得把 Gemini `candidates[]` 流冒充成 interactions 表示。
#[test]
fn interactions_streaming_is_rejected() {
    let error = plan(
        SourceProtocol::GeminiInteractions,
        ProviderProtocol::Gemini,
        true,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ContractViolation::InvalidStreaming {
            operation: Operation::GeminiInteractions,
            stream: true,
        }
    ));
}

#[test]
fn streaming_operations_are_validated() {
    assert!(RequestProfile::for_source(SourceProtocol::GeminiStream, false).is_err());
    assert!(RequestProfile::for_source(SourceProtocol::OpenAiEmbeddings, true).is_err());
    assert!(RequestProfile::for_source(SourceProtocol::GeminiStream, true).is_ok());
}

#[test]
fn operations_fix_endpoint_paths() {
    assert_eq!(Operation::ChatCompletions.api_path(), "/chat/completions");
    assert_eq!(Operation::Responses.api_path(), "/responses");
    assert_eq!(Operation::Embeddings.api_path(), "/embeddings");
    assert_eq!(Operation::Messages.api_path(), "/messages");
    assert_eq!(
        Operation::GeminiStreamGenerateContent.api_path(),
        "models/{model}:streamGenerateContent"
    );
    assert_eq!(Operation::GeminiInteractions.api_path(), "/interactions");
}

#[test]
fn operation_must_match_source() {
    let error =
        RequestProfile::new(SourceProtocol::Anthropic, Operation::ChatCompletions, false)
            .unwrap_err();
    assert!(matches!(error, ContractViolation::OperationMismatch { .. }));
}

#[test]
fn content_type_is_exact_not_substring() {
    let expected = ResponseRepresentation::Sse;
    assert_eq!(
        resolve_response_representation(expected, Some("text/event-stream; charset=utf-8"))
            .unwrap(),
        Representation::Sse
    );
    assert!(matches!(
        resolve_response_representation(expected, Some("text/event-streaming")),
        Err(ContractViolation::UnknownContentType { .. })
    ));
    assert!(matches!(
        resolve_response_representation(expected, None),
        Err(ContractViolation::MissingContentType)
    ));
}

#[test]
fn ndjson_never_matches_sse_or_json_expectation() {
    let error = resolve_response_representation(
        ResponseRepresentation::Sse,
        Some("application/x-ndjson"),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ContractViolation::UnexpectedRepresentation {
            actual: Representation::Ndjson,
            ..
        }
    ));
    let error = resolve_response_representation(
        ResponseRepresentation::JsonOrSse,
        Some("application/x-ndjson"),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ContractViolation::UnexpectedRepresentation { .. }
    ));
}

#[test]
fn exhaustive_matrix_never_silently_passes_unknown() {
    for source in [
        SourceProtocol::OpenAi,
        SourceProtocol::OpenAiResponses,
        SourceProtocol::OpenAiEmbeddings,
        SourceProtocol::Anthropic,
        SourceProtocol::Gemini,
        SourceProtocol::GeminiStream,
        SourceProtocol::GeminiInteractions,
    ] {
        for target in [
            ProviderProtocol::OpenAi,
            ProviderProtocol::Anthropic,
            ProviderProtocol::Gemini,
        ] {
            for stream in [false, true] {
                for native_responses in [false, true] {
                    match plan_for_source(source, target, stream, native_responses) {
                        Ok(plan) => {
                            assert_ne!(plan.request.mode, BodyMode::Unsupported);
                            assert_ne!(plan.response.mode, BodyMode::Unsupported);
                            if plan.is_byte_exact() {
                                assert!(plan.loss.is_lossless());
                            } else {
                                assert_eq!(plan.fidelity(), FidelityLevel::SemanticEquivalent);
                            }
                        }
                        Err(error) => {
                            assert!(
                                matches!(
                                    error,
                                    ContractViolation::UnsupportedCombination { .. }
                                        | ContractViolation::InvalidStreaming { .. }
                                ),
                                "unexpected rejection: {error}"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn fidelity_minimum_selects_weaker() {
    assert_eq!(
        FidelityLevel::ByteExact.minimum(FidelityLevel::SemanticEquivalent),
        FidelityLevel::SemanticEquivalent
    );
    assert_eq!(
        FidelityLevel::StructureExact.minimum(FidelityLevel::ByteExact),
        FidelityLevel::StructureExact
    );
}

#[test]
fn response_executor_is_selected_only_from_planned_options() {
    let stream_plan = plan_for_source(
        SourceProtocol::OpenAi,
        ProviderProtocol::OpenAi,
        true,
        false,
    )
    .unwrap();
    assert_eq!(
        select_response_executor(&stream_plan, Some("text/event-stream; charset=utf-8"))
            .unwrap(),
        ResponseExecutor::Sse
    );
    assert_eq!(
        select_response_executor(&stream_plan, Some("application/json")).unwrap(),
        ResponseExecutor::JsonSynthesis
    );
    assert_eq!(
        select_response_executor(&stream_plan, Some("application/x-ndjson")).unwrap(),
        ResponseExecutor::Ndjson
    );
    assert!(matches!(
        select_response_executor(&stream_plan, Some("text/plain")).unwrap_err(),
        ContractViolation::UnknownContentType { .. }
    ));
    assert_eq!(
        select_response_executor(&stream_plan, None).unwrap_err(),
        ContractViolation::MissingContentType
    );

    let json_plan = plan_for_source(
        SourceProtocol::OpenAi,
        ProviderProtocol::OpenAi,
        false,
        false,
    )
    .unwrap();
    assert_eq!(
        select_response_executor(&json_plan, Some("application/json")).unwrap(),
        ResponseExecutor::Json
    );
    // 非流操作收到 SSE 不是可合并的表示转换。
    assert!(matches!(
        select_response_executor(&json_plan, Some("text/event-stream")).unwrap_err(),
        ContractViolation::UnexpectedRepresentation { .. }
    ));
}
