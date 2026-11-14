//! Gemini 协议适配器(B5)。
//!
//! Inbound:`contents`/`systemInstruction` 请求 → [`SemanticRequest`];
//! [`SemanticResponse`] → generateContent 响应信封。
//! Outbound:语义请求 → Gemini 请求体(镜像 `build_gemini_payload`
//! 的源分支语义);GenerateContentResponse / interactions steps →
//! 语义响应。
//!
//! 响应侧 inlineData/thoughtSignature 提取为修复点(契约 §3.5);
//! openai 源的 system 不上提 systemInstruction(镜像旧实现,有测试钉住)。

use std::collections::HashSet;

use serde_json::{Map, Value, json};

use crate::json_py::py_json_dumps;
use crate::provider::ProviderProtocol;
use crate::python_semantics::{py_bool, py_float, py_int, py_str};
use crate::semantic::adapter::{
    AdapterCapabilities, CHAT_NONSTREAM, Decoded, InboundAdapter, OutboundAdapter, OutboundContext,
};
use crate::semantic::diagnostic::{ConversionReport, DiagnosticCode};
use crate::semantic::envelope::encode_error;
use crate::semantic::error::SemanticError;
use crate::semantic::finish;
use crate::semantic::model::{
    ContentPart, FinishReason, IdOrigin, Message, ReasoningBlock, RequestKind, Role,
    SemanticRequest, SemanticResponse, SourceProtocol, ThinkingOptions, ToolArguments, ToolCall,
    ToolCallId, ToolChoice, ToolDefinition, ToolResult, Usage,
};
use crate::semantic::parts::opt_str;

// ---------------------------------------------------------------------------
// Inbound:客户端 gemini(非流面;gemini_stream 请求面属阶段 C)
// ---------------------------------------------------------------------------

pub struct Inbound;

pub struct InteractionsInbound;

impl InboundAdapter for Inbound {
    fn protocol(&self) -> SourceProtocol {
        SourceProtocol::Gemini
    }

    fn capabilities(&self) -> AdapterCapabilities {
        CHAT_NONSTREAM
    }

    fn decode_request(&self, raw: &Value) -> Result<Decoded<SemanticRequest>, SemanticError> {
        let mut report = ConversionReport::semantic();
        let mut request = SemanticRequest::new(
            SourceProtocol::Gemini,
            opt_str(raw.get("model")).unwrap_or_default(),
        );
        decode_gemini_request(raw, &mut request, &mut report);
        Ok(Decoded::new(request, report))
    }

    fn encode_response(
        &self,
        response: &SemanticResponse,
    ) -> Result<Decoded<Value>, SemanticError> {
        let mut report = ConversionReport::semantic();
        Ok(Decoded::new(
            encode_generate_content_response(response, &mut report),
            report,
        ))
    }

    fn encode_error(&self, error: &SemanticError, attempts: Option<&[String]>) -> (u16, Value) {
        encode_error(SourceProtocol::Gemini, error, attempts)
    }
}

/// gemini_interactions 客户端:仅响应面走语义(Responses 信封,镜像
/// `gemini_interaction_to_openai`);请求面由旧路径承载(阶段 C 收敛)。
impl InboundAdapter for InteractionsInbound {
    fn protocol(&self) -> SourceProtocol {
        SourceProtocol::GeminiInteractions
    }

    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            request_kinds: &[RequestKind::Interactions],
            stream_modes: CHAT_NONSTREAM.stream_modes,
            supports_prefill_continuation: false,
        }
    }

    fn decode_request(&self, _raw: &Value) -> Result<Decoded<SemanticRequest>, SemanticError> {
        Err(SemanticError::unsupported(
            "gemini_interactions 请求面语义适配属后续迁移(当前由既有路径承载)",
        ))
    }

    fn encode_response(
        &self,
        response: &SemanticResponse,
    ) -> Result<Decoded<Value>, SemanticError> {
        let mut report = ConversionReport::semantic();
        Ok(Decoded::new(
            encode_interactions_envelope(response, &mut report),
            report,
        ))
    }

    fn encode_error(&self, error: &SemanticError, attempts: Option<&[String]>) -> (u16, Value) {
        encode_error(SourceProtocol::GeminiInteractions, error, attempts)
    }
}

fn decode_gemini_request(
    raw: &Value,
    request: &mut SemanticRequest,
    report: &mut ConversionReport,
) {
    let generation = &mut request.generation;
    if let Some(config) = raw.get("generationConfig").filter(|v| v.is_object()) {
        generation.temperature = config.get("temperature").and_then(py_float);
        generation.top_p = config.get("topP").and_then(py_float);
        generation.max_tokens = config.get("maxOutputTokens").and_then(py_int);
        generation.stop_sequences = config
            .get("stopSequences")
            .and_then(Value::as_array)
            .map(|items| items.iter().filter_map(py_str).collect())
            .unwrap_or_default();
        if let Some(thinking) = config.get("thinkingConfig").filter(|v| v.is_object()) {
            generation.thinking = Some(ThinkingOptions {
                budget_tokens: thinking.get("thinkingBudget").and_then(py_int),
                include_thoughts: thinking.get("includeThoughts").and_then(Value::as_bool),
            });
        }
        if let Some(config_map) = config.as_object() {
            for key in config_map.keys() {
                if ![
                    "temperature",
                    "topP",
                    "maxOutputTokens",
                    "stopSequences",
                    "thinkingConfig",
                ]
                .contains(&key.as_str())
                {
                    report.push(
                        DiagnosticCode::OpaquePreserved,
                        format!("generationConfig.{key}"),
                        "未建模 generationConfig 键丢弃".to_owned(),
                    );
                }
            }
        }
    }
    if let Some(system) = raw.get("systemInstruction").filter(|v| py_bool(v)) {
        let mut message = Message::new(Role::System);
        parse_gemini_parts(system, &mut message, report);
        request.messages.push(message);
    }
    if let Some(Value::Array(contents)) = raw.get("contents") {
        for (index, content) in contents.iter().enumerate() {
            let role = match content.get("role").and_then(Value::as_str) {
                Some("model") | Some("assistant") => Role::Assistant,
                _ => Role::User,
            };
            let mut message = Message::new(role);
            parse_gemini_parts(content, &mut message, report);
            // functionResponse part → Tool 结果消息。
            let mut result: Option<ToolResult> = None;
            message.content.retain(|part| match part {
                ContentPart::Opaque { kind_hint, value }
                    if kind_hint.as_deref() == Some("functionResponse") =>
                {
                    result = Some(function_response_to_result(value));
                    false
                }
                _ => true,
            });
            if let Some(result) = result {
                let mut tool_message = Message::new(Role::Tool);
                tool_message.tool_result = Some(result);
                request.messages.push(tool_message);
                continue;
            }
            let _ = index;
            request.messages.push(message);
        }
    }
    if let Some(Value::Array(tool_groups)) = raw.get("tools") {
        for group in tool_groups {
            if let Some(Value::Array(declarations)) = group.get("functionDeclarations") {
                for declaration in declarations {
                    if let Some(name) = opt_str(declaration.get("name")) {
                        request.tools.push(ToolDefinition {
                            name,
                            description: opt_str(declaration.get("description")),
                            parameters_schema: declaration
                                .get("parameters")
                                .cloned()
                                .unwrap_or(json!({})),
                            extensions: Default::default(),
                        });
                    }
                }
            }
        }
    }
    if let Some(config) = raw.get("toolConfig") {
        request.tool_choice = config
            .get("functionCallingConfig")
            .map(decode_function_calling_config);
    }
    collect_opaque(raw, &mut request.opaque);
}

/// gemini 请求建模的顶层键(其余进 opaque;各白名单出站编码器统一记
/// DroppedField 丢弃诊断——此前未建模顶层键静默丢失无观测,2026-10-14
/// 审计缺陷 #4)。
const MODELED_KEYS: &[&str] = &[
    "contents",
    "systemInstruction",
    "tools",
    "toolConfig",
    "generationConfig",
];

fn collect_opaque(raw: &Value, opaque: &mut Vec<crate::semantic::model::OpaqueField>) {
    let Some(map) = raw.as_object() else {
        return;
    };
    for (key, value) in map {
        if !MODELED_KEYS.contains(&key.as_str()) {
            opaque.push(crate::semantic::model::OpaqueField {
                path: key.clone(),
                value: value.clone(),
            });
        }
    }
}

fn gemini_parts_to_content(
    content: &Value,
    report: &mut ConversionReport,
) -> (Vec<ContentPart>, Option<ReasoningBlock>, Vec<ToolCall>) {
    let mut message = Message::new(Role::User);
    parse_gemini_parts(content, &mut message, report);
    (message.content, message.reasoning, message.tool_calls)
}

/// gemini parts → 消息内容(functionResponse 以 Opaque 标记交还调用方;
/// thought/thoughtSignature 进 reasoning)。
fn parse_gemini_parts(container: &Value, message: &mut Message, report: &mut ConversionReport) {
    let Some(Value::Array(items)) = container.get("parts") else {
        return;
    };
    for part in items {
        if let Some(call) = part.get("functionCall").filter(|v| py_bool(v)) {
            let mut extensions = crate::semantic::model::Extensions::default();
            if let Some(signature) = opt_str(part.get("thoughtSignature")) {
                extensions.insert("gemini", "thoughtSignature", json!(signature));
            }
            message.tool_calls.push(ToolCall {
                id: ToolCallId {
                    local: opt_str(call.get("id")).unwrap_or_default(),
                    origin: IdOrigin::Source,
                },
                // 上游回流的清洗名 → 原名;客户端原名(非编码形态)恒等通过。
                name: crate::semantic::tool_names::desanitize(
                    &opt_str(call.get("name")).unwrap_or_default(),
                ),
                arguments: ToolArguments::Parsed(
                    call.get("args")
                        .filter(|v| py_bool(v))
                        .cloned()
                        .unwrap_or(json!({})),
                ),
                extensions,
            });
            continue;
        }
        if let Some(response) = part.get("functionResponse") {
            message.content.push(ContentPart::Opaque {
                kind_hint: Some("functionResponse".to_owned()),
                value: response.clone(),
            });
            continue;
        }
        let thought = part.get("thought").map(py_bool).unwrap_or(false);
        if let Some(text) = opt_str(part.get("text")) {
            if thought {
                let reasoning = message.reasoning.get_or_insert_with(|| ReasoningBlock {
                    text: String::new(),
                    signature: None,
                    opaque: Vec::new(),
                });
                reasoning.text.push_str(&text);
                if reasoning.signature.is_none() {
                    reasoning.signature = opt_str(part.get("thoughtSignature"));
                }
            } else if part.get("thoughtSignature").is_some() {
                // 带 signature 的正文 part:原样保留(修复点:签名不再丢)。
                message.content.push(ContentPart::Opaque {
                    kind_hint: Some("text".to_owned()),
                    value: part.clone(),
                });
            } else {
                message.content.push(ContentPart::Text { text });
            }
            continue;
        }
        if let Some(inline) = part
            .get("inlineData")
            .or_else(|| part.get("inline_data"))
            .filter(|v| v.is_object())
        {
            message.content.push(ContentPart::ImageInline {
                media_type: opt_str(inline.get("mimeType")),
                data_base64: opt_str(inline.get("data")).unwrap_or_default(),
            });
            continue;
        }
        if let Some(file) = part
            .get("fileData")
            .or_else(|| part.get("file_data"))
            .filter(|v| v.is_object())
        {
            message.content.push(ContentPart::ImageRef {
                url: opt_str(file.get("fileUri")).unwrap_or_default(),
            });
            continue;
        }
        report.push(
            DiagnosticCode::OpaquePreserved,
            "parts",
            "未识别 part 原样保留".to_owned(),
        );
        message.content.push(ContentPart::Opaque {
            kind_hint: None,
            value: part.clone(),
        });
    }
}

fn function_response_to_result(value: &Value) -> ToolResult {
    ToolResult {
        call_ref: opt_str(value.get("id")).map(|local| ToolCallId {
            local,
            origin: IdOrigin::Source,
        }),
        resolved_by_name: opt_str(value.get("name")),
        content: value.get("response").cloned().unwrap_or(Value::Null),
        is_error: false,
        extensions: Default::default(),
    }
}

fn decode_function_calling_config(config: &Value) -> ToolChoice {
    let mode = opt_str(config.get("mode")).unwrap_or_default();
    match mode.as_str() {
        "NONE" => ToolChoice::None,
        "ANY" => {
            let names: Vec<String> = config
                .get("allowedFunctionNames")
                .and_then(Value::as_array)
                .map(|items| items.iter().filter_map(py_str).collect())
                .unwrap_or_default();
            if let Some(name) = names.into_iter().next() {
                ToolChoice::Named { name }
            } else {
                ToolChoice::Required
            }
        }
        _ => ToolChoice::Auto,
    }
}

/// 语义响应 → generateContent 响应信封(gemini 客户端非流回译;
/// 全新路径,形态按 GenerateContentResponse 规范)。
fn encode_generate_content_response(
    response: &SemanticResponse,
    report: &mut ConversionReport,
) -> Value {
    let mut candidates: Vec<Value> = Vec::new();
    for candidate in &response.candidates {
        let mut parts: Vec<Value> = Vec::new();
        if let Some(reasoning) = &candidate.reasoning
            && !reasoning.text.is_empty()
        {
            let mut part = Map::new();
            part.insert("text".to_owned(), json!(reasoning.text));
            part.insert("thought".to_owned(), json!(true));
            if reasoning.signature.is_some() {
                report.push(
                    DiagnosticCode::DroppedField,
                    "reasoning.signature".to_owned(),
                    "跨协议不迁移厂商私有推理签名".to_owned(),
                );
            }
            parts.push(Value::Object(part));
        }
        for part in &candidate.content {
            match part {
                ContentPart::Text { text } => parts.push(json!({"text": text})),
                ContentPart::ImageInline {
                    media_type,
                    data_base64,
                } => parts.push(json!({
                    "inlineData": {
                        "mimeType": media_type.clone().unwrap_or_else(|| "image/png".to_owned()),
                        "data": data_base64,
                    },
                })),
                ContentPart::ImageRef { url } => parts.push(json!({
                    "fileData": {"fileUri": url},
                })),
                ContentPart::Opaque { value, .. } => parts.push(value.clone()),
                ContentPart::Refusal { text } => parts.push(json!({"text": text})),
                ContentPart::Audio { .. } | ContentPart::File { .. } => {
                    report.push(
                        DiagnosticCode::DowngradedContent,
                        "candidates",
                        "gemini 目标不表达 audio/file 块,原值保留".to_owned(),
                    );
                    parts.push(match part {
                        ContentPart::Audio { media_type, data_base64 } => json!({
                            "audioData": {"mimeType": media_type, "data": data_base64},
                        }),
                        ContentPart::File { file_id, filename, data_base64 } => json!({
                            "fileData": {"fileUri": file_id, "filename": filename, "data": data_base64},
                        }),
                        _ => unreachable!(),
                    });
                }
            }
        }
        if candidate.index == 0
            && let Some(refusal) = &response.refusal
            && !candidate
                .content
                .iter()
                .any(|part| matches!(part, ContentPart::Refusal { .. }))
        {
            parts.push(json!({"text": refusal}));
        }
        for call in &candidate.tool_calls {
            let mut function_call = Map::new();
            if !call.id.local.is_empty() {
                function_call.insert("id".to_owned(), json!(call.id.local));
            }
            function_call.insert("name".to_owned(), json!(call.name));
            let (args, downgraded) = call.arguments.as_object();
            if downgraded {
                report.push(
                    DiagnosticCode::DowngradedContent,
                    "functionCall",
                    format!("工具 {} 参数非法 JSON → {{\"raw\": s}} 兜底", call.name),
                );
            }
            function_call.insert("args".to_owned(), args);
            if let Some(signature) = call.extensions.get("gemini", "thoughtSignature") {
                function_call.insert("thoughtSignature".to_owned(), signature.clone());
            }
            parts.push(json!({"functionCall": Value::Object(function_call)}));
        }
        let mut candidate_json = Map::new();
        candidate_json.insert(
            "content".to_owned(),
            json!({"role": "model", "parts": parts}),
        );
        candidate_json.insert(
            "finishReason".to_owned(),
            json!(finish::to_gemini(
                candidate
                    .finish
                    .as_ref()
                    .or(response.finish.as_ref())
                    .unwrap_or(&FinishReason::Stop)
            )),
        );
        candidates.push(Value::Object(candidate_json));
    }
    let mut body = Map::new();
    body.insert("candidates".to_owned(), Value::Array(candidates));
    if let Some(usage) = response.usage {
        let mut metadata = Map::new();
        if let Some(input) = usage.input {
            metadata.insert("promptTokenCount".to_owned(), json!(input));
        }
        if let Some(output) = usage.output {
            metadata.insert("candidatesTokenCount".to_owned(), json!(output));
        }
        if let Some(total) = usage.total {
            metadata.insert("totalTokenCount".to_owned(), json!(total));
        }
        if let Some(reasoning) = usage.reasoning {
            metadata.insert("thoughtsTokenCount".to_owned(), json!(reasoning));
        }
        if let Some(cache_read) = usage.cache_read {
            metadata.insert("cachedContentTokenCount".to_owned(), json!(cache_read));
        }
        if !metadata.is_empty() {
            body.insert("usageMetadata".to_owned(), Value::Object(metadata));
        }
    }
    if let Some(model) = &response.model
        && !model.is_empty()
    {
        body.insert("modelVersion".to_owned(), json!(model));
    }
    if let Some(id) = &response.id {
        body.insert("responseId".to_owned(), json!(id));
    }
    Value::Object(body)
}

/// 语义响应 → interactions 信封(镜像 `gemini_interaction_to_openai`:
/// 单 message item,steps 顺序映射 content parts)。
fn encode_interactions_envelope(
    response: &SemanticResponse,
    _report: &mut ConversionReport,
) -> Value {
    let candidate = response.first_candidate();
    let mut parts: Vec<Value> = Vec::new();
    if let Some(reasoning) = candidate.and_then(|c| c.reasoning.as_ref())
        && !reasoning.text.is_empty()
    {
        parts.push(json!({"type": "reasoning", "text": reasoning.text}));
    }
    if let Some(candidate) = candidate {
        for part in &candidate.content {
            match part {
                ContentPart::Text { text } => {
                    parts.push(json!({"type": "output_text", "text": text}))
                }
                ContentPart::Opaque { value, .. } => parts.push(value.clone()),
                _ => parts.push(json!({"type": "output_text", "text": ""})),
            }
        }
        for call in &candidate.tool_calls {
            parts.push(json!({
                "type": "function_call",
                "id": if call.id.local.is_empty() { Value::Null } else { json!(call.id.local) },
                "name": if call.name.is_empty() { Value::Null } else { json!(call.name) },
                "arguments": call.arguments.as_raw_string(),
            }));
        }
        if let Some(result) = candidate.extensions.get("gemini", "interaction_result") {
            parts.push(result.clone());
        }
    }
    let usage = response.usage.unwrap_or_else(Usage::reported);
    json!({
        "id": response.id.clone().unwrap_or_else(|| "interaction_wrouter".to_owned()),
        "object": "response",
        "status": json!(response
            .extensions
            .get("gemini", "interaction_status")
            .and_then(Value::as_str)
            .unwrap_or("completed")),
        "output": [{"type": "message", "role": "assistant", "content": parts}],
        "usage": usage_to_dict(&usage),
    })
}

/// 内部五键 usage dict(镜像 `usage_to_dict`,gemini interactions 信封)。
fn usage_to_dict(usage: &Usage) -> Value {
    json!({
        "input_tokens": Usage::or_zero(usage.input),
        "output_tokens": Usage::or_zero(usage.output),
        "total_tokens": Usage::or_zero(usage.total),
        "cache_read_tokens": Usage::or_zero(usage.cache_read),
        "cache_write_tokens": Usage::or_zero(usage.cache_write),
    })
}

// ---------------------------------------------------------------------------
// Outbound:上游 gemini
// ---------------------------------------------------------------------------

pub struct Outbound;

impl OutboundAdapter for Outbound {
    fn protocol(&self) -> ProviderProtocol {
        ProviderProtocol::Gemini
    }

    fn capabilities(&self) -> AdapterCapabilities {
        CHAT_NONSTREAM
    }

    fn encode_request(
        &self,
        request: &SemanticRequest,
        _ctx: &OutboundContext,
    ) -> Result<Decoded<Value>, SemanticError> {
        let mut report = request.report();
        Ok(Decoded::new(
            encode_gemini_request(request, &mut report),
            report,
        ))
    }

    fn decode_response(
        &self,
        raw: &Value,
        ctx: &OutboundContext,
    ) -> Result<Decoded<SemanticResponse>, SemanticError> {
        if !raw.is_object() {
            return Err(SemanticError::unsupported(
                "Gemini 响应不是 JSON 对象,无法按 GenerateContent 语义解码",
            ));
        }
        let mut report = ConversionReport::semantic();
        let response = if ctx.kind == RequestKind::Interactions {
            decode_interaction_steps(raw, &mut report)
        } else {
            decode_gemini_response(raw, &mut report)
        };
        Ok(Decoded::new(response, report))
    }
}

// ---------------------------------------------------------------------------
// Gemini schema 清洗(自 protocol::gemini 归位:唯一消费者是本模块)
// ---------------------------------------------------------------------------

/// `_text`:字符串原样,其余 `json.dumps(v, ensure_ascii=False)`。
fn json_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => py_json_dumps(other),
    }
}

/// `_schema`:剥掉 Gemini 不接受的 JSON-Schema 键;`const` → `enum`;
/// `type: [...]` 取首个非 null 并标 nullable。
fn schema_walk(value: &Value) -> Value {
    if let Value::Array(items) = value {
        return Value::Array(items.iter().map(schema_walk).collect());
    }
    let Some(map) = value.as_object() else {
        return value.clone();
    };
    let skip: HashSet<&str> = [
        "$schema",
        "additionalProperties",
        "propertyNames",
        "exclusiveMinimum",
        "exclusiveMaximum",
        "minProperties",
        "maxProperties",
        "patternProperties",
    ]
    .into_iter()
    .collect();
    let mut result = Map::new();
    let mut nullable = false;
    for (key, item) in map {
        if skip.contains(key.as_str()) {
            continue;
        }
        if key == "const" {
            result.insert("enum".to_owned(), json!([item]));
            continue;
        }
        if key == "type"
            && let Value::Array(values) = item
        {
            let values: Vec<String> = values.iter().map(json_text).collect();
            nullable = values.iter().any(|v| v == "null");
            if let Some(first) = values.iter().find(|v| *v != "null") {
                result.insert("type".to_owned(), json!(first));
            }
            continue;
        }
        result.insert(key.clone(), schema_walk(item));
    }
    if nullable {
        result.insert("nullable".to_owned(), json!(true));
    }
    Value::Object(result)
}

/// `sanitizeGeminiSchema`:入参非对象/缺省时按 `{}` 处理。
fn sanitize_gemini_schema(schema: Option<&Value>) -> Map<String, Value> {
    match schema {
        None => Map::new(),
        Some(v) if v.is_null() => Map::new(),
        Some(v) => match schema_walk(v) {
            Value::Object(map) => map,
            _ => Map::new(),
        },
    }
}

/// 语义请求 → Gemini generateContent 请求体(镜像 build_gemini_payload:
/// openai 源 system 不上提;thinkingConfig 整块在前、标量 setdefault)。
fn encode_gemini_request(request: &SemanticRequest, report: &mut ConversionReport) -> Value {
    let mut contents: Vec<Value> = Vec::new();
    // 工具名回查表(tool 结果按 call id 关联 name,镜像旧 tool_names)。
    let mut tool_names: Map<String, Value> = Map::new();
    for call in request
        .messages
        .iter()
        .flat_map(|message| message.tool_calls.iter())
    {
        if !call.id.local.is_empty() {
            tool_names.insert(call.id.local.clone(), json!(call.name));
        }
    }
    let mut system_instruction: Option<Value> = None;
    for message in &request.messages {
        // 镜像旧实现:仅非 openai 源把 system 上提 systemInstruction;
        // openai 源按普通消息进 contents(role=user,有单测钉住)。
        // Developer 同样升格(gemini 无 developer,落 user 会把系统级
        // 指令降级为用户轮次)。
        if matches!(message.role, Role::System | Role::Developer)
            && request.source != SourceProtocol::OpenAi
        {
            let text = joined_text_or_dump(message);
            system_instruction = Some(json!({"parts": [{"text": text}]}));
            continue;
        }
        let gemini_role = match message.role {
            Role::Assistant => "model",
            _ => "user",
        };
        let mut parts: Vec<Value> = Vec::new();
        if message.role == Role::Tool {
            let result = message.tool_result.as_ref();
            let call_id = result
                .and_then(|r| r.call_ref.as_ref())
                .map(|id| id.local.clone())
                .unwrap_or_default();
            let name = result
                .and_then(|r| r.resolved_by_name.clone())
                .or_else(|| {
                    tool_names
                        .get(&call_id)
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_default();
            let response_value = result.map(|r| r.content.clone()).unwrap_or(json!(""));
            let mut call = Map::new();
            // functionResponse 按 name 与 functionCall 配对:须与发出去的
            // 清洗名一致(表里存的是客户端原名)。
            call.insert(
                "name".to_owned(),
                json!(crate::semantic::tool_names::sanitize(
                    &name,
                    crate::semantic::tool_names::Profile::Gemini
                )),
            );
            call.insert("response".to_owned(), json!({"result": response_value}));
            if !call_id.is_empty() {
                call.insert("id".to_owned(), json!(call_id));
            }
            parts.push(json!({"functionResponse": Value::Object(call)}));
        } else {
            // 旧实现怪癖:content 缺失的 assistant 消息仍带空文本 part。
            if message.content.is_empty() && message.role == Role::Assistant {
                parts.push(json!({"text": ""}));
            }
            for part in &message.content {
                encode_gemini_part(part, &mut parts, message, report);
            }
            if let Some(reasoning) = &message.reasoning {
                let mut part = Map::new();
                part.insert("text".to_owned(), json!(reasoning.text));
                part.insert("thought".to_owned(), json!(true));
                if let Some(signature) = &reasoning.signature {
                    part.insert("thoughtSignature".to_owned(), json!(signature));
                }
                parts.insert(0, Value::Object(part));
            }
            for call in &message.tool_calls {
                let mut function_call = Map::new();
                // 镜像旧实现:id 键恒在(可为空串)。
                function_call.insert("id".to_owned(), json!(call.id.local));
                function_call.insert(
                    "name".to_owned(),
                    json!(crate::semantic::tool_names::sanitize(
                        &call.name,
                        crate::semantic::tool_names::Profile::Gemini
                    )),
                );
                let (args, downgraded) = call.arguments.as_object();
                if downgraded {
                    report.push(
                        DiagnosticCode::DowngradedContent,
                        "functionCall",
                        format!("工具 {} 参数非法 JSON → {{\"raw\": s}} 兜底", call.name),
                    );
                }
                function_call.insert("args".to_owned(), args);
                if let Some(signature) = call.extensions.get("gemini", "thoughtSignature") {
                    function_call.insert("thoughtSignature".to_owned(), signature.clone());
                }
                parts.push(json!({"functionCall": Value::Object(function_call)}));
            }
        }
        if !parts.is_empty() {
            contents.push(json!({"role": gemini_role, "parts": parts}));
        }
    }
    let mut body = Map::new();
    body.insert("contents".to_owned(), Value::Array(contents));
    if let Some(system) = system_instruction {
        body.insert("systemInstruction".to_owned(), system);
    }
    if !request.tools.is_empty() {
        let declarations: Vec<Value> = request
            .tools
            .iter()
            .filter(|tool| !tool.name.is_empty())
            .map(|tool| {
                json!({
                    "name": crate::semantic::tool_names::sanitize(&tool.name, crate::semantic::tool_names::Profile::Gemini),
                    "description": tool.description.clone().unwrap_or_default(),
                    "parameters": Value::Object(sanitize_gemini_schema(Some(&tool.parameters_schema))),
                })
            })
            .collect();
        if !declarations.is_empty() {
            body.insert(
                "tools".to_owned(),
                json!([{"functionDeclarations": declarations}]),
            );
        }
    }
    if let Some(choice) = &request.tool_choice {
        if let Some(config) = encode_tool_config(choice) {
            body.insert("toolConfig".to_owned(), config);
        }
    }
    // generationConfig:thinking 整块在前,标量 setdefault(先键胜出)。
// Gemini 3 分代(_is_gemini_3_or_newer 口径):thinkingLevel
    // 取代 thinkingBudget 且两者互斥;3 系无法真正关闭,降最低档+不展示。
    let model_lower = request.model.to_lowercase();
    let gemini3 = model_lower.contains("gemini-3");
    let g3_flash = gemini3 && model_lower.contains("flash");
    let g3_31pro = model_lower.contains("gemini-3.1-pro");
    let mut generation: Option<Map<String, Value>> = None;
    if let Some(thinking) = &request.generation.thinking {
        let mut config = Map::new();
        if gemini3 {
            // 3 线格式不收 thinkingBudget:显式 budget 只作意图旗标
            // (0/缺省 → 不展示,>0 → 展示)。
            config.insert(
                "includeThoughts".to_owned(),
                json!(thinking.budget_tokens.is_some_and(|budget| budget > 0)),
            );
        } else {
            if let Some(budget) = thinking.budget_tokens {
                config.insert("thinkingBudget".to_owned(), json!(budget));
            }
            if let Some(include) = thinking.include_thoughts {
                config.insert("includeThoughts".to_owned(), json!(include));
            }
        }
        if !config.is_empty() {
            generation = Some(config);
        }
    } else if let Some(effort) = request.generation.reasoning_effort.as_ref() {
        use crate::semantic::model::ReasoningEffort;
        if matches!(effort, ReasoningEffort::Off) {
            generation = Some(if gemini3 {
                Map::from_iter([
                    (
                        "thinkingLevel".to_owned(),
                        json!(if g3_flash { "minimal" } else { "low" }),
                    ),
                    ("includeThoughts".to_owned(), json!(false)),
                ])
            } else {
                Map::from_iter([
                    ("thinkingBudget".to_owned(), json!(0)),
                    ("includeThoughts".to_owned(), json!(false)),
                ])
            });
        } else if let Some(budget) = effort.budget_hint() {
            if gemini3 {
                // 档位 → thinkingLevel:medium 仅 3-flash/3.1-pro 支持
                // (其余改档 high),minimal 仅 3-flash。
                let level = match effort {
                    ReasoningEffort::Minimal => {
                        if g3_flash {
                            "minimal"
                        } else {
                            "low"
                        }
                    }
                    ReasoningEffort::Low => "low",
                    ReasoningEffort::Medium if g3_flash || g3_31pro => "medium",
                    _ => "high",
                };
                generation = Some(Map::from_iter([
                    ("thinkingLevel".to_owned(), json!(level)),
                    ("includeThoughts".to_owned(), json!(true)),
                ]));
            } else {
                // effort 档位 → thinkingBudget(与 anthropic 出站同源推导,
                // ReasoningEffort::budget_hint);2.5 系列上限 32768,越界收敛记诊断。
                let clamped = budget.min(32_768);
                if clamped != budget {
                    report.push(
                        DiagnosticCode::AdjustedValue,
                        "generationConfig.thinkingBudget".to_owned(),
                        format!("{budget} 按 Gemini thinkingBudget 上限收敛为 {clamped}"),
                    );
                }
                generation = Some(Map::from_iter([(
                    "thinkingBudget".to_owned(),
                    json!(clamped),
                )]));
            }
        }
    }
    for (target, value) in [
        (
            "temperature",
            request.generation.temperature.map(|v| json!(v)),
        ),
        ("topP", request.generation.top_p.map(|v| json!(v))),
        (
            "maxOutputTokens",
            request.generation.max_tokens.map(|v| json!(v)),
        ),
    ] {
        if let Some(value) = value {
            let map = generation.get_or_insert_with(Map::new);
            map.entry(target.to_owned()).or_insert(value);
        }
    }
    if let Some(map) = generation {
        body.insert("generationConfig".to_owned(), Value::Object(map));
    }
    for field in &request.opaque {
        report.push(
            DiagnosticCode::DroppedField,
            field.path.clone(),
            "Gemini 目标白名单外键丢弃(镜像旧 build_gemini_payload 全新建模)".to_owned(),
        );
    }
    // parallel_tool_calls 已建模但 gemini 线格式无对应概念,显式诊断
    // (此前静默忽略)。
    if request.generation.parallel_tool_calls.is_some() {
        report.push(
            DiagnosticCode::DroppedField,
            "parallel_tool_calls".to_owned(),
            "Gemini 线格式不表达并行工具开关,已省略".to_owned(),
        );
    }
    Value::Object(body)
}

fn encode_gemini_part(
    part: &ContentPart,
    parts: &mut Vec<Value>,
    message: &Message,
    report: &mut ConversionReport,
) {
    match part {
        ContentPart::Text { text } => parts.push(json!({"text": text})),
        ContentPart::ImageInline {
            media_type,
            data_base64,
        } => parts.push(json!({
            "inlineData": {
                "mimeType": media_type.clone().unwrap_or_else(|| "image/png".to_owned()),
                "data": data_base64,
            },
        })),
        ContentPart::ImageRef { url } => parts.push(json!({
            "fileData": {"fileUri": url},
        })),
        // 镜像旧 part_from_content 默认分支:带 text 字段的未知块按文本
        // 保留,其余丢弃并记诊断。
        ContentPart::Opaque { value, .. } => {
            if let Some(text) = value.get("text") {
                parts.push(json!({"text": py_str(text).unwrap_or_else(|| py_json_dumps(text))}));
            } else {
                report.push(
                    DiagnosticCode::OpaquePreserved,
                    "parts",
                    "无 text 的未知块丢弃(镜像旧实现)".to_owned(),
                );
            }
        }
        ContentPart::Refusal { text } => parts.push(json!({"text": text})),
        ContentPart::Audio { .. } | ContentPart::File { .. } => {
            report.push(
                DiagnosticCode::DowngradedContent,
                "parts",
                "gemini 目标不表达 audio/file 块".to_owned(),
            );
            let _ = message;
        }
    }
}

fn encode_tool_config(choice: &ToolChoice) -> Option<Value> {
    Some(match choice {
        ToolChoice::Auto => json!({"functionCallingConfig": {"mode": "AUTO"}}),
        ToolChoice::None => json!({"functionCallingConfig": {"mode": "NONE"}}),
        ToolChoice::Required => json!({"functionCallingConfig": {"mode": "ANY"}}),
        ToolChoice::Named { name } if !name.is_empty() => {
            json!({"functionCallingConfig": {"mode": "ANY", "allowedFunctionNames": [name]}})
        }
        ToolChoice::Named { .. } => json!({"functionCallingConfig": {"mode": "ANY"}}),
        ToolChoice::Opaque(value) => return encode_tool_config_opaque(value),
    })
}

fn encode_tool_config_opaque(value: &Value) -> Option<Value> {
    // 旧 build_gemini_tool_config 的宽容递归(字符串/对象形态)。
    if let Some(mode) = value.as_str() {
        let mapped = match mode {
            "none" => "NONE",
            "required" | "any" => "ANY",
            _ => "AUTO",
        };
        return Some(json!({"functionCallingConfig": {"mode": mapped}}));
    }
    if let Some(map) = value.as_object() {
        let kind = map.get("type").and_then(Value::as_str).unwrap_or("");
        if kind == "function" || kind == "tool" {
            let name = map
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(Value::as_str)
                .or_else(|| map.get("name").and_then(Value::as_str));
            return Some(match name.filter(|n| !n.is_empty()) {
                Some(name) => {
                    json!({"functionCallingConfig": {"mode": "ANY", "allowedFunctionNames": [name]}})
                }
                None => json!({"functionCallingConfig": {"mode": "ANY"}}),
            });
        }
        let inner = map.get("mode").or_else(|| map.get("type"));
        return inner.and_then(encode_tool_config_opaque);
    }
    None
}

fn joined_text_or_dump(message: &Message) -> String {
    let mut out = String::new();
    for part in &message.content {
        match part {
            ContentPart::Text { text } => out.push_str(text),
            ContentPart::Opaque { value, .. } => {
                if let Some(text) = value.get("text") {
                    out.push_str(&py_str(text).unwrap_or_else(|| py_json_dumps(text)));
                } else {
                    out.push_str(&py_json_dumps(value));
                }
            }
            _ => {}
        }
    }
    out
}

/// GenerateContentResponse → 语义响应(响应侧 inlineData/thoughtSignature
/// 提取为修复点;镜像 parse_gemini_usage 五键归一的 Option 化)。
pub(super) fn decode_gemini_response(
    raw: &Value,
    report: &mut ConversionReport,
) -> SemanticResponse {
    let mut response = SemanticResponse::new();
    response.id = opt_str(raw.get("responseId")).or_else(|| opt_str(raw.get("id")));
    response.model = opt_str(raw.get("modelVersion")).or_else(|| opt_str(raw.get("model")));
    if let Some(Value::Array(candidates)) = raw.get("candidates") {
        for (index, candidate_value) in candidates.iter().enumerate() {
            let mut candidate = crate::semantic::model::ResponseCandidate::new(index);
            let (content, reasoning, tool_calls) = gemini_parts_to_content(
                candidate_value.get("content").unwrap_or(&Value::Null),
                report,
            );
            candidate.content = content;
            candidate.reasoning = reasoning;
            candidate.tool_calls = tool_calls;
            candidate.finish = Some(finish::from_gemini(
                candidate_value.get("finishReason").and_then(Value::as_str),
                report,
                &format!("candidates[{index}].finishReason"),
            ));
            if let Some(Value::Array(ratings)) = candidate_value.get("safetyRatings") {
                candidate.extensions.insert(
                    "gemini",
                    "safetyRatings",
                    Value::Array(ratings.clone()),
                );
            }
            response.candidates.push(candidate);
        }
    }
    response.finish = response
        .first_candidate()
        .and_then(|c| c.finish.clone())
        .or(Some(FinishReason::Stop));
    if let Some(usage) = raw
        .get("usageMetadata")
        .filter(|v| py_bool(v))
        .or_else(|| raw.get("usage_metadata").filter(|v| py_bool(v)))
        .or_else(|| raw.get("usage").filter(|v| py_bool(v)))
    {
        response.usage = Some(parse_gemini_usage(usage));
    }
    response
}

/// Gemini usage 方言 → 语义 Usage(镜像 parse_gemini_usage 的键序,
/// Option 化:缺失即未知,编码侧按 0 兜底)。
fn parse_gemini_usage(usage: &Value) -> Usage {
    // 负值视为未报告(所有 usage 解析点统一防御,防 cache 折扣污染算术)。
    let nn = |v: Option<i64>| v.filter(|n| *n >= 0);
    let mut parsed = Usage::reported();
    parsed.input = nn(usage
        .get("promptTokenCount")
        .or_else(|| usage.get("prompt_token_count"))
        .and_then(py_int));
    parsed.output = nn(usage
        .get("candidatesTokenCount")
        .or_else(|| usage.get("candidates_token_count"))
        .and_then(py_int));
    parsed.total = nn(usage
        .get("totalTokenCount")
        .or_else(|| usage.get("total_token_count"))
        .and_then(py_int));
    parsed.reasoning = nn(usage
        .get("thoughtsTokenCount")
        .or_else(|| usage.get("thoughts_token_count"))
        .and_then(py_int));
    parsed.cache_read = nn(usage
        .get("cachedContentTokenCount")
        .or_else(|| usage.get("cached_content_token_count"))
        .and_then(py_int));
    parsed
}

/// interactions steps JSON → 语义响应(outbound;gemini 上游 ×
/// interactions 客户端)。
pub(super) fn decode_interaction_steps(
    raw: &Value,
    report: &mut ConversionReport,
) -> SemanticResponse {
    let mut response = SemanticResponse::new();
    response.id = opt_str(raw.get("id"));
    response.extensions.insert(
        "gemini",
        "interaction_status",
        json!(opt_str(raw.get("status")).unwrap_or_else(|| "completed".to_owned())),
    );
    // usage 与 generateContent 同源键序(usageMetadata/usage_metadata/usage)。
    if let Some(usage) = raw
        .get("usageMetadata")
        .filter(|v| py_bool(v))
        .or_else(|| raw.get("usage_metadata").filter(|v| py_bool(v)))
        .or_else(|| raw.get("usage").filter(|v| py_bool(v)))
    {
        response.usage = Some(parse_gemini_usage(usage));
    }
    let mut candidate = crate::semantic::model::ResponseCandidate::new(0);
    if let Some(Value::Array(steps)) = raw.get("steps") {
        for step in steps {
            let kind = step
                .get("type")
                .or_else(|| step.get("kind"))
                .and_then(Value::as_str)
                .unwrap_or("");
            match kind {
                "thought" | "reasoning" => {
                    let text = step
                        .get("text")
                        .or_else(|| step.get("content"))
                        .map(|v| py_str(v).unwrap_or_default())
                        .unwrap_or_default();
                    let reasoning = candidate.reasoning.get_or_insert_with(|| ReasoningBlock {
                        text: String::new(),
                        signature: None,
                        opaque: Vec::new(),
                    });
                    reasoning.text.push_str(&text);
                }
                "function_call" | "functionCall" => {
                    candidate.tool_calls.push(ToolCall {
                        id: ToolCallId {
                            local: opt_str(step.get("id")).unwrap_or_default(),
                            origin: IdOrigin::Source,
                        },
                        name: opt_str(step.get("name")).unwrap_or_default(),
                        arguments: ToolArguments::Parsed(
                            step.get("arguments")
                                .filter(|v| py_bool(v))
                                .or_else(|| step.get("args").filter(|v| py_bool(v)))
                                .cloned()
                                .unwrap_or(json!({})),
                        ),
                        extensions: Default::default(),
                    });
                }
                "function_result" | "functionResponse" => {
                    // 镜像旧实现:function_call_output part 留在 message
                    // content 内,由 interactions 信封编码器原样重放。
                    candidate.extensions.insert(
                        "gemini",
                        "interaction_result",
                        json!({
                            "type": "function_call_output",
                            "call_id": step.get("id").cloned().unwrap_or(Value::Null),
                            "output": step
                                .get("output")
                                .cloned()
                                .or_else(|| step.get("result").cloned())
                                .unwrap_or(json!("")),
                        }),
                    );
                }
                _ => {
                    if let Some(text) = step.get("text") {
                        candidate.content.push(ContentPart::Text {
                            text: py_str(text).unwrap_or_default(),
                        });
                    }
                }
            }
        }
    }
    let _ = report;
    response.candidates.push(candidate);
    response
}
