//! OpenAI Responses 协议适配器(B3)。
//!
//! Inbound:`input`/`instructions` 请求 → [`SemanticRequest`];
//! [`SemanticResponse`] → Responses 信封。
//! Outbound:语义请求 → Responses 请求体(input items);Responses
//! JSON → 语义响应。
//!
//! reasoning item 的旧折叠语义(pending 缓冲、仅 function_call 消费、
//! 悬空丢弃)在 decode 侧复现并补诊断;`store`/`previous_response_id`/
//! `text.format` 等服务端状态键 opaque 保留 + 丢弃诊断(契约 §3.4)。

use std::collections::HashSet;

use serde_json::{Map, Value, json};

use crate::json_py::py_json_dumps;
use crate::provider::ProviderProtocol;
use crate::python_semantics::{py_bool, py_int, py_str};
use crate::semantic::adapter::{
    AdapterCapabilities, CHAT_NONSTREAM, Decoded, InboundAdapter, OutboundAdapter, OutboundContext,
};
use crate::semantic::diagnostic::{ConversionReport, DiagnosticCode};
use crate::semantic::envelope::encode_error;
use crate::semantic::error::SemanticError;
use crate::semantic::finish;
use crate::semantic::model::{
    ContentPart, FinishReason, IdOrigin, IncompleteReason, Message, ReasoningBlock, Role,
    SemanticRequest, SemanticResponse, SourceProtocol, ToolArguments, ToolCall, ToolCallId,
    ToolResult,
};
use crate::semantic::model::{ToolChoice, Usage};
use crate::semantic::openai_chat::{
    decode_generation, decode_stream, decode_tool_choice, decode_tools, decode_usage,
};
use crate::semantic::parts::{opt_str, preserve_extra_keys};

// ---------------------------------------------------------------------------
// Inbound:客户端 openai_responses
// ---------------------------------------------------------------------------

pub struct Inbound;

const MODELED_KEYS: &[&str] = &[
    "model",
    "input",
    "instructions",
    "max_output_tokens",
    "temperature",
    "top_p",
    "stop",
    "stream",
    "parallel_tool_calls",
    "tools",
    "tool_choice",
    "reasoning",
];

impl InboundAdapter for Inbound {
    fn protocol(&self) -> SourceProtocol {
        SourceProtocol::OpenAiResponses
    }

    fn capabilities(&self) -> AdapterCapabilities {
        CHAT_NONSTREAM
    }

    fn decode_request(&self, raw: &Value) -> Result<Decoded<SemanticRequest>, SemanticError> {
        let mut report = ConversionReport::semantic();
        let mut request = SemanticRequest::new(
            SourceProtocol::OpenAiResponses,
            opt_str(raw.get("model")).unwrap_or_default(),
        );
        request.generation = decode_generation(raw);
        request.generation.max_tokens = raw.get("max_output_tokens").and_then(py_int);
        // 思考意图:reasoning.effort 进语义层(此前仅落 opaque,跨族白名单
        // 转换时被整体丢弃——responses 客户端的推理档位在 anthropic/chat/
        // gemini 上游全部丢失,2026-10-14 审计)。
        if let Some(effort) = raw
            .pointer("/reasoning/effort")
            .and_then(crate::python_semantics::py_str)
        {
            request.generation.reasoning_effort =
                Some(crate::semantic::model::ReasoningEffort::parse(&effort));
        }
        // OpenAI 规定 reasoning 内 effort 与 max_tokens 二选一(effort
        // 优先);语义层只承载 effort,max_tokens 显式记丢弃诊断。
        if raw.pointer("/reasoning/max_tokens").is_some() {
            report.push(
                DiagnosticCode::DroppedField,
                "reasoning.max_tokens".to_owned(),
                "reasoning.effort 已按 OpenAI 语义优先,max_tokens 无跨族表达已丢弃".to_owned(),
            );
        }
        request.stream = decode_stream(raw);
        if let Some(instructions) = raw.get("instructions").filter(|v| !v.is_null()) {
            let mut message = Message::new(Role::System);
            message.content = vec![ContentPart::Text {
                text: py_str(instructions).unwrap_or_else(|| py_json_dumps(instructions)),
            }];
            request.messages.push(message);
        }
        decode_input(raw.get("input"), &mut request.messages, &mut report);
        request.tools = decode_tools(raw.get("tools"));
        request.tool_choice = raw.get("tool_choice").map(decode_tool_choice);
        collect_opaque(raw, &mut request.opaque);
        Ok(Decoded::new(request, report))
    }

    fn encode_response(
        &self,
        response: &SemanticResponse,
    ) -> Result<Decoded<Value>, SemanticError> {
        let mut report = ConversionReport::semantic();
        Ok(Decoded::new(
            encode_responses_envelope(response, &mut report),
            report,
        ))
    }

    fn encode_error(&self, error: &SemanticError, attempts: Option<&[String]>) -> (u16, Value) {
        encode_error(SourceProtocol::OpenAiResponses, error, attempts)
    }
}

/// input items → 消息(reasoning 折叠语义镜像旧实现:pending 缓冲、
/// 仅 function_call 消费、悬空丢弃 + 诊断)。
fn decode_input(input: Option<&Value>, messages: &mut Vec<Message>, report: &mut ConversionReport) {
    let mut pending_reasoning: Vec<String> = Vec::new();
    match input {
        Some(Value::String(text)) => {
            let mut message = Message::new(Role::User);
            message.content = vec![ContentPart::Text { text: text.clone() }];
            messages.push(message);
        }
        Some(Value::Array(items)) => {
            for (index, item) in items.iter().enumerate() {
                match item {
                    Value::String(text) => {
                        let mut message = Message::new(Role::User);
                        message.content = vec![ContentPart::Text { text: text.clone() }];
                        messages.push(message);
                    }
                    Value::Object(map) => {
                        let declared = map.get("type").and_then(Value::as_str).unwrap_or("");
                        // Responses 的 `EasyInputMessage` 允许省略 `type`:只有
                        // role/content 的对象就是一条 message。真实客户端(ZCode
                        // 等)正是这样发的;此前一律落 `other` 分支被丢弃,整个
                        // input 变成空 messages,上游回 400「messages 不能为空」
                        // (2026-10-13 实测)。
                        let kind = if declared.is_empty() && map.contains_key("role") {
                            "message"
                        } else {
                            declared
                        };
                        match kind {
                            "message" => {
                                let mut message = Message::new(decode_role(map));
                                message.content = decode_item_content(
                                    map.get("content"),
                                    &mut pending_reasoning,
                                    report,
                                );
                                message.id = opt_str(map.get("id"));
                                preserve_extra_keys(
                                    item,
                                    &["type", "role", "content", "id"],
                                    &mut message.extensions,
                                    report,
                                    &format!("input[{index}]"),
                                );
                                messages.push(message);
                            }
                            "function_call" => {
                                let mut message = Message::new(Role::Assistant);
                                if !pending_reasoning.is_empty() {
                                    message.reasoning = Some(ReasoningBlock {
                                        text: pending_reasoning.join("\n"),
                                        signature: None,
                                        opaque: Vec::new(),
                                    });
                                    pending_reasoning.clear();
                                }
                                message.id = opt_str(map.get("id"));
                                message.tool_calls.push(ToolCall {
                                    id: ToolCallId {
                                        local: opt_str(map.get("call_id"))
                                            .or_else(|| opt_str(map.get("id")))
                                            .unwrap_or_default(),
                                        origin: IdOrigin::Source,
                                    },
                                    name: opt_str(map.get("name")).unwrap_or_default(),
                                    arguments: match map.get("arguments") {
                                        Some(Value::String(raw)) if !raw.is_empty() => {
                                            ToolArguments::Raw(raw.clone())
                                        }
                                        _ => ToolArguments::Parsed(json!({})),
                                    },
                                    extensions: Default::default(),
                                });
                                messages.push(message);
                            }
                            "function_call_output" => {
                                let mut message = Message::new(Role::Tool);
                                message.tool_result = Some(ToolResult {
                                    call_ref: opt_str(map.get("call_id")).map(|local| ToolCallId {
                                        local,
                                        origin: IdOrigin::Source,
                                    }),
                                    resolved_by_name: None,
                                    content: map.get("output").cloned().unwrap_or(Value::Null),
                                    is_error: false,
                                    extensions: Default::default(),
                                });
                                messages.push(message);
                            }
                            "reasoning" => {
                                let text = reasoning_text_of(map);
                                if !text.is_empty() {
                                    pending_reasoning.push(text);
                                }
                            }
                            other => {
                                report.push(
                                    DiagnosticCode::OpaquePreserved,
                                    format!("input[{index}]"),
                                    format!("未识别 item type {other:?} 丢弃(旧实现静默)"),
                                );
                            }
                        }
                    }
                    other => {
                        report.push(
                            DiagnosticCode::OpaquePreserved,
                            format!("input[{index}]"),
                            "非对象 item 丢弃(旧实现静默)".to_owned(),
                        );
                        let _ = other;
                    }
                }
            }
        }
        Some(Value::Null) | None => {}
        // 旧实现非 list 非字符串 input 静默产出空 messages;此处显式化。
        Some(other) => {
            report.push(
                DiagnosticCode::OpaquePreserved,
                "input",
                "非字符串/数组 input 丢弃(旧实现静默空消息)".to_owned(),
            );
            let _ = other;
        }
    }
    if !pending_reasoning.is_empty() {
        report.push(
            DiagnosticCode::DroppedField,
            "input[reasoning]",
            "悬空 reasoning(无后续 function_call)丢弃(镜像旧实现)".to_owned(),
        );
    }
}

fn decode_role(map: &serde_json::Map<String, Value>) -> Role {
    match map.get("role").and_then(Value::as_str) {
        Some("assistant") => Role::Assistant,
        Some("system") => Role::System,
        Some("developer") => Role::Developer,
        Some("tool") => Role::Tool,
        _ => Role::User,
    }
}

fn decode_item_content(
    content: Option<&Value>,
    _pending: &mut Vec<String>,
    report: &mut ConversionReport,
) -> Vec<ContentPart> {
    match content {
        Some(Value::String(text)) => vec![ContentPart::Text { text: text.clone() }],
        Some(Value::Array(items)) => items
            .iter()
            .map(|part| {
                let kind = part.get("type").and_then(Value::as_str).unwrap_or("");
                match kind {
                    "output_text" | "input_text" | "text" => ContentPart::Text {
                        text: opt_str(part.get("text")).unwrap_or_default(),
                    },
                    _ => {
                        report.push(
                            DiagnosticCode::OpaquePreserved,
                            "content",
                            "未识别 content part 以 opaque 保留".to_owned(),
                        );
                        ContentPart::Opaque {
                            kind_hint: Some(kind.to_owned()),
                            value: part.clone(),
                        }
                    }
                }
            })
            .collect(),
        Some(Value::Null) | None => Vec::new(),
        Some(other) => vec![ContentPart::Opaque {
            kind_hint: None,
            value: other.clone(),
        }],
    }
}

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

/// 语义响应 → Responses 信封(线级形态对齐旧 chat/anthropic 生产路径;
/// gemini 路径的 reasoning 内嵌 message 形态为有意收敛,登记实施记录)。
fn encode_responses_envelope(response: &SemanticResponse, report: &mut ConversionReport) -> Value {
    let candidate = response.first_candidate();
    let text = response.joined_text();
    let refusal = response.refusal.clone().or_else(|| {
        candidate.and_then(|candidate| {
            candidate.content.iter().find_map(|part| match part {
                ContentPart::Refusal { text } => Some(text.clone()),
                _ => None,
            })
        })
    });
    let annotations = candidate
        .and_then(|candidate| candidate.extensions.get("openai", "annotations"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let reasoning_text = candidate
        .and_then(|c| c.reasoning.as_ref())
        .map(|r| r.text.clone())
        .unwrap_or_default();
    let encrypted = candidate
        .and_then(|c| c.reasoning.as_ref())
        .and_then(|reasoning| reasoning.opaque.first());
    let mut output: Vec<Value> = Vec::new();
    if !reasoning_text.is_empty() {
        output.push(build_responses_reasoning_item(
            &format!("rs_{}", uuid_hex()),
            &reasoning_text,
            "completed",
            encrypted,
        ));
    }
    let mut content: Vec<Value> = Vec::new();
    let mut annotations_used = false;
    if let Some(candidate) = candidate {
        for part in &candidate.content {
            match part {
                ContentPart::Text { text } => {
                    let part_annotations = if !annotations_used && !annotations.is_empty() {
                        annotations_used = true;
                        Value::Array(annotations.clone())
                    } else {
                        json!([])
                    };
                    content.push(json!({
                        "type": "output_text", "text": text, "annotations": part_annotations,
                    }));
                }
                ContentPart::Refusal { text } => {
                    content.push(json!({"type": "refusal", "refusal": text}));
                }
                _ => {}
            }
        }
    }
    if content.is_empty() {
        if let Some(refusal) = &refusal {
            content.push(json!({"type": "refusal", "refusal": refusal}));
        } else {
            content.push(json!({"type": "output_text", "text": "", "annotations": []}));
        }
    }
    let item_id = format!("msg_{}", uuid_hex());
    let single_text_part = content.len() == 1
        && content[0]["type"] == json!("output_text")
        && refusal.is_none()
        && !annotations_used;
    output.push(if single_text_part {
        build_responses_message_item(&item_id, &text, "completed")
    } else {
        json!({
            "id": item_id,
            "type": "message",
            "role": "assistant",
            "status": "completed",
            "content": content,
        })
    });
    for call in candidate.map(|c| c.tool_calls.iter()).unwrap_or_default() {
        let fc_id = format!("fc_{}", uuid_hex());
        let call_id = if call.id.local.is_empty() {
            fc_id.clone()
        } else {
            call.id.local.clone()
        };
        output.push(build_responses_function_call_item(
            &fc_id,
            &call_id,
            &call.name,
            &call.arguments.as_raw_string(),
            "completed",
            None,
        ));
    }
    let usage = response.usage.unwrap_or_else(Usage::reported);
    let input = Usage::or_zero(usage.input);
    let out = Usage::or_zero(usage.output);
    let incomplete_reason = response
        .incomplete
        .as_ref()
        .map(|reason| reason.as_responses_str().to_owned())
        .or_else(|| {
            response
                .finish
                .as_ref()
                .map(|finish_reason| finish::to_responses(finish_reason))
        })
        .filter(|reason| reason == "max_output_tokens" || reason == "content_filter");
    let status = if incomplete_reason.is_some() {
        "incomplete"
    } else {
        "completed"
    };
    let mut envelope = Map::new();
    envelope.insert(
        "id".to_owned(),
        json!(
            response
                .id
                .clone()
                .unwrap_or_else(|| "resp_wrouter".to_owned())
        ),
    );
    envelope.insert("object".to_owned(), json!("response"));
    envelope.insert(
        "model".to_owned(),
        json!(response.model.clone().unwrap_or_default()),
    );
    envelope.insert("status".to_owned(), json!(status));
    envelope.insert("output".to_owned(), Value::Array(output));
    envelope.insert("output_text".to_owned(), json!(text));
    envelope.insert(
        "usage".to_owned(),
        normalize_responses_usage(&json!({
            "input_tokens": input,
            "output_tokens": out,
            "total_tokens": usage.total.unwrap_or(input + out),
            "input_tokens_details": {"cached_tokens": Usage::or_zero(usage.cache_read)},
            "output_tokens_details": {"reasoning_tokens": Usage::or_zero(usage.reasoning)},
        })),
    );
    if let Some(reason) = incomplete_reason {
        envelope.insert("incomplete_details".to_owned(), json!({"reason": reason}));
    }
    let _ = report;
    Value::Object(envelope)
}

fn uuid_hex() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

// ---------------------------------------------------------------------------
// Outbound:上游 openai(原生 Responses)
// ---------------------------------------------------------------------------

pub struct Outbound;

impl OutboundAdapter for Outbound {
    fn protocol(&self) -> ProviderProtocol {
        ProviderProtocol::OpenAi
    }

    fn capabilities(&self) -> AdapterCapabilities {
        CHAT_NONSTREAM
    }

    fn encode_request(
        &self,
        request: &SemanticRequest,
        ctx: &OutboundContext,
    ) -> Result<Decoded<Value>, SemanticError> {
        debug_assert!(ctx.native_responses, "responses 出站仅在原生形态调用");
        let mut report = request.report();
        Ok(Decoded::new(
            encode_responses_request(request, &mut report),
            report,
        ))
    }

    fn decode_response(
        &self,
        raw: &Value,
        _ctx: &OutboundContext,
    ) -> Result<Decoded<SemanticResponse>, SemanticError> {
        if !raw.is_object() {
            return Err(SemanticError::unsupported(
                "Responses 响应不是 JSON 对象,无法按 response 语义解码",
            ));
        }
        let mut report = ConversionReport::semantic();
        Ok(Decoded::new(
            decode_responses_json(raw, &mut report),
            report,
        ))
    }
}

/// 语义请求 → Responses 请求体(input items;镜像 chat_request_to_responses)。
fn encode_responses_request(request: &SemanticRequest, report: &mut ConversionReport) -> Value {
    let mut input: Vec<Value> = Vec::new();
    let mut instructions: Option<Value> = None;
    // G-11:历史里 name 为空的 function_call 是已结束且不可执行的调用。
    // 严格 Responses 上游会以 400 拒绝整个历史,因此连同其孤立工具结果
    // 一起拒绝写入(带诊断),而不是照写空串或误删暂缺 name 的流事件。
    let mut dropped_calls: HashSet<String> = HashSet::new();
    for message in &request.messages {
        match message.role {
            Role::System => {
                // 旧实现最后一条 system 胜出;语义路径保持 + 合并诊断。
                let text = joined_text_of(message);
                instructions = Some(json!(text));
                if request
                    .messages
                    .iter()
                    .filter(|m| m.role == Role::System)
                    .count()
                    > 1
                {
                    report.push(
                        DiagnosticCode::MergedContent,
                        "messages[system]",
                        "多条 system 仅 instructions 单通道,末条胜出(镜像旧实现)".to_owned(),
                    );
                }
            }
            Role::Tool => {
                let result = message.tool_result.as_ref();
                let call_id = result
                    .and_then(|r| r.call_ref.as_ref())
                    .map(|id| id.local.clone())
                    .unwrap_or_default();
                if !call_id.is_empty() && dropped_calls.contains(&call_id) {
                    report.push(
                        DiagnosticCode::DroppedField,
                        "messages[tool]",
                        "对应的 function_call 名为空,工具结果随调用一起不写入历史".to_owned(),
                    );
                    continue;
                }
                let output = result
                    .map(|r| match &r.content {
                        Value::String(s) => s.clone(),
                        other => py_json_dumps(other),
                    })
                    .unwrap_or_default();
                input.push(json!({
                    "type": "function_call_output",
                    "call_id": result
                        .and_then(|r| r.call_ref.as_ref())
                        .map(|id| json!(id.local))
                        .unwrap_or(Value::Null),
                    "output": output,
                }));
            }
            Role::Assistant => {
                if let Some(reasoning) = &message.reasoning
                    && !reasoning.text.is_empty()
                {
                    input.push(build_responses_reasoning_input(&reasoning.text));
                }
                for call in &message.tool_calls {
                    if call.name.trim().is_empty() {
                        if !call.id.local.is_empty() {
                            dropped_calls.insert(call.id.local.clone());
                        }
                        report.push(
                            DiagnosticCode::DroppedField,
                            "messages[assistant].tool_calls",
                            "空名 function_call 不写入 Responses 历史(严格上游会拒绝整段)"
                                .to_owned(),
                        );
                        continue;
                    }
                    input.push(json!({
                        "type": "function_call",
                        // Responses 的 `function_call` 输入项里 `id` 是**服务端
                        // 回执 id**(规范要求以 `fc_` 开头),`call_id` 才是
                        // 关联用的调用 id。
                        //
                        // 2026-11-06 修正:旧实现把来源 id(chat 的 `call_`
                        // 开头)同时填进 `id` 与 `call_id` → 命中严格校验
                        // 节点即 400(实测 AgentRouter:
                        // `Invalid 'input[7].id': 'call_yDez…'. Expected an ID
                        // that begins with 'fc'`)。现在 `id` 恒为 `fc_<uuid>`,
                        // 来源 id 只进 `call_id`。
                        //
                        // `id` 缺失时上游也会反序列化 400(2026-10-13 实测
                        // agnes),所以这里**恒生成**而不是省略。
                        "id": format!("fc_{}", uuid_hex()),
                        "call_id": if call.id.local.is_empty() { Value::Null } else { json!(call.id.local) },
                        "name": call.name,
                        "arguments": call.arguments.as_raw_string(),
                    }));
                }
                let text = joined_text_of(message);
                if !text.is_empty() {
                    input.push(json!({
                        "type": "message", "role": "assistant",
                        "content": [{"type": "output_text", "text": text}],
                    }));
                }
            }
            Role::User | Role::Developer => {
                let role = if message.role == Role::Developer {
                    "developer"
                } else {
                    "user"
                };
                let text = joined_text_of(message);
                input.push(json!({
                    "type": "message",
                    "role": role,
                    "content": text,
                }));
            }
        }
    }
    let mut body = Map::new();
    body.insert("model".to_owned(), json!(request.model));
    body.insert("input".to_owned(), Value::Array(input));
    if let Some(instructions) = instructions {
        body.insert("instructions".to_owned(), instructions);
    }
    if let Some(max_tokens) = request.generation.max_tokens {
        body.insert("max_output_tokens".to_owned(), json!(max_tokens));
    }
    if let Some(temperature) = request.generation.temperature {
        body.insert("temperature".to_owned(), json!(temperature));
    }
    if let Some(top_p) = request.generation.top_p {
        body.insert("top_p".to_owned(), json!(top_p));
    }
    if !request.generation.stop_sequences.is_empty() {
        body.insert("stop".to_owned(), json!(request.generation.stop_sequences));
    }
    if let Some(parallel) = request.generation.parallel_tool_calls {
        body.insert("parallel_tool_calls".to_owned(), json!(parallel));
    }
    // 思考意图:reasoning.effort 从语义层表达(旧白名单体整体丢弃,原生
    // responses 上游收不到推理档位)。Off 省略:Responses 线对「关闭」
    // 无跨上游统一表达(none 仅部分新方言识别),缺省交由上游缺省行为。
    if let Some(effort) = &request.generation.reasoning_effort {
        match effort {
            crate::semantic::model::ReasoningEffort::Off => report.push(
                DiagnosticCode::DroppedField,
                "reasoning.effort".to_owned(),
                "effort=none 在 Responses 线无统一关闭表达,已省略(上游按缺省行为)".to_owned(),
            ),
            other => {
                body.insert("reasoning".to_owned(), json!({"effort": other.as_raw()}));
            }
        }
    }
    // stream 必须显式落键:agnes 的 /v1/responses 省略该字段会按流式回
    // SSE,非流式路径把正文当 JSON 解析即炸(`invalid character 'd'`,
    // 2026-10-14 实测)。NotSpecified 显式写 false —— Responses API 的
    // 省略语义本来就是非流式,对规范上游是恒等改写。
    match request.stream {
        crate::semantic::model::StreamPreference::Explicit(value) => {
            body.insert("stream".to_owned(), json!(value));
        }
        crate::semantic::model::StreamPreference::NotSpecified => {
            body.insert("stream".to_owned(), json!(false));
        }
    }
    if !request.tools.is_empty() {
        body.insert(
            "tools".to_owned(),
            Value::Array(
                request
                    .tools
                    .iter()
                    .map(|tool| {
                        json!({
                            "type": "function",
                            "name": tool.name,
                            "description": tool.description.clone().unwrap_or_default(),
                            "parameters": if tool.parameters_schema.is_null() { json!({}) } else { tool.parameters_schema.clone() },
                        })
                    })
                    .collect(),
            ),
        );
    }
    if let Some(choice) = &request.tool_choice {
        body.insert(
            "tool_choice".to_owned(),
            match choice {
                ToolChoice::Named { name } => json!({"type": "function", "name": name}),
                ToolChoice::Auto => json!("auto"),
                ToolChoice::None => json!("none"),
                ToolChoice::Required => json!("required"),
                ToolChoice::Opaque(value) => value.clone(),
            },
        );
    }
    for field in &request.opaque {
        report.push(
            DiagnosticCode::DroppedField,
            field.path.clone(),
            "Responses 服务端状态/白名单外键不跨族表达(契约 §3.4)".to_owned(),
        );
    }
    Value::Object(body)
}

fn joined_text_of(message: &Message) -> String {
    let mut out = String::new();
    for part in &message.content {
        if let ContentPart::Text { text } = part {
            out.push_str(text);
        } else if let ContentPart::Opaque { value, .. } = part
            && let Some(text) = value.get("text").and_then(Value::as_str)
        {
            out.push_str(text);
        }
    }
    out
}

/// 原生 Responses JSON → 语义响应。
pub(super) fn decode_responses_json(
    raw: &Value,
    report: &mut ConversionReport,
) -> SemanticResponse {
    let mut response = SemanticResponse::new();
    response.id = opt_str(raw.get("id"));
    response.model = opt_str(raw.get("model"));
    let mut candidate = crate::semantic::model::ResponseCandidate::new(0);
    let mut refusal_text: Option<String> = None;
    let mut annotations: Vec<Value> = Vec::new();
    if let Some(Value::Array(items)) = raw.get("output") {
        for (index, item) in items.iter().enumerate() {
            let Some(map) = item.as_object() else {
                continue;
            };
            match map.get("type").and_then(Value::as_str).unwrap_or("") {
                "message" => {
                    if let Some(Value::Array(parts)) = map.get("content") {
                        for part in parts {
                            match part.get("type").and_then(Value::as_str).unwrap_or("") {
                                "output_text" | "text" => {
                                    if let Some(text) = opt_str(part.get("text")) {
                                        candidate.content.push(ContentPart::Text { text });
                                    }
                                    if let Some(items) =
                                        part.get("annotations").and_then(Value::as_array)
                                    {
                                        annotations.extend(items.iter().cloned());
                                    }
                                }
                                "refusal" => {
                                    let text = opt_str(part.get("refusal"))
                                        .or_else(|| opt_str(part.get("text")))
                                        .unwrap_or_default();
                                    refusal_text = Some(text.clone());
                                    candidate.content.push(ContentPart::Refusal { text });
                                }
                                _ => {}
                            }
                        }
                    }
                }
                "reasoning" => {
                    let text = reasoning_text_of(map);
                    let encrypted = map.get("encrypted_content").cloned();
                    candidate.reasoning = Some(ReasoningBlock {
                        text,
                        signature: None,
                        opaque: encrypted.into_iter().collect(),
                    });
                    candidate.extensions.insert(
                        "openai",
                        "item_id",
                        json!(map.get("id").cloned().unwrap_or(Value::Null)),
                    );
                }
                "function_call" => {
                    candidate.tool_calls.push(ToolCall {
                        id: ToolCallId {
                            local: opt_str(map.get("call_id"))
                                .or_else(|| opt_str(map.get("id")))
                                .unwrap_or_default(),
                            origin: IdOrigin::Source,
                        },
                        name: opt_str(map.get("name")).unwrap_or_default(),
                        arguments: match map.get("arguments") {
                            Some(Value::String(s)) if !s.is_empty() => {
                                crate::semantic::model::ToolArguments::Raw(s.clone())
                            }
                            _ => crate::semantic::model::ToolArguments::Parsed(json!({})),
                        },
                        extensions: Default::default(),
                    });
                }
                other => {
                    report.push(
                        DiagnosticCode::OpaquePreserved,
                        format!("output[{index}]"),
                        format!("未识别 output item type {other:?} 以 opaque 保留"),
                    );
                    response.opaque.push(crate::semantic::model::OpaqueField {
                        path: format!("output[{index}]"),
                        value: item.clone(),
                    });
                }
            }
        }
    }
    if !annotations.is_empty() {
        candidate
            .extensions
            .insert("openai", "annotations", Value::Array(annotations));
    }
    if let Some(reason) = raw
        .get("incomplete_details")
        .and_then(|d| d.get("reason"))
        .and_then(Value::as_str)
    {
        response.incomplete = IncompleteReason::parse_responses(reason);
    }
    response.refusal = refusal_text;
    response.finish = Some(match &response.incomplete {
        Some(IncompleteReason::MaxOutputTokens) => FinishReason::Length,
        Some(IncompleteReason::ContentFilter) => FinishReason::ContentFilter,
        _ if response.refusal.is_some() => FinishReason::Refusal,
        _ => FinishReason::Stop,
    });
    if let Some(usage) = raw.get("usage") {
        let mut parsed = decode_usage(Some(usage));
        // Responses 命名。
        parsed.input = usage.get("input_tokens").and_then(py_int);
        parsed.output = usage.get("output_tokens").and_then(py_int);
        parsed.total = usage.get("total_tokens").and_then(py_int);
        parsed.reasoning = usage
            .get("output_tokens_details")
            .and_then(|d| d.get("reasoning_tokens"))
            .and_then(py_int);
        parsed.cache_read = usage
            .get("input_tokens_details")
            .and_then(|d| d.get("cached_tokens"))
            .and_then(py_int);
        response.usage = Some(parsed);
    }
    response.candidates.push(candidate);
    if let Some(candidate) = response.candidates.last_mut() {
        candidate.finish = response.finish.clone();
    }
    response
}

// ---------------------------------------------------------------------------
// Responses 信封 item/usage 归一(自 protocol::responses 并入,20261013_01 B1:
// 唯一消费者是本模块;镜像 Python `domain/protocol/responses.py`。
// finish-reason 映射已随阶段 E2 删除,语义版见 [`crate::semantic::finish`])
// ---------------------------------------------------------------------------

/// `normalize_responses_usage`:任意 usage dict → Responses 形态
/// (含嵌套 details)。缺键按 0;`total_tokens` 为 0 时回退 input+output。
fn normalize_responses_usage(usage: &Value) -> Value {
    normalize_responses_usage_obj(usage.as_object())
}

fn normalize_responses_usage_obj(usage: Option<&Map<String, Value>>) -> Value {
    let empty = Map::new();
    let usage = usage.unwrap_or(&empty);
    let int_or = |value: Option<&Value>, default: i64| -> i64 {
        match value {
            None => default,
            Some(v) => py_int(v).unwrap_or(0),
        }
    };
    // input_tokens is not None → 用之;否则 prompt_tokens(缺省 0)。
    let inp = match usage.get("input_tokens") {
        Some(v) if !v.is_null() => py_int(v).unwrap_or(0),
        _ => int_or(usage.get("prompt_tokens"), 0),
    };
    let out = match usage.get("output_tokens") {
        Some(v) if !v.is_null() => py_int(v).unwrap_or(0),
        _ => int_or(usage.get("completion_tokens"), 0),
    };
    let total_raw = int_or(usage.get("total_tokens"), 0);
    let total = if total_raw != 0 { total_raw } else { inp + out };
    // cached:input_tokens_details 为 dict 时取其 cached_tokens;
    // 否则 usage.cache_read_tokens(缺省 0)。Python 在 dict 缺子键时会给
    // int(None) 抛错,这里按 0 兜底(偏差记录见 14 号文档 R3 完成记录)。
    let cached = match usage.get("input_tokens_details") {
        Some(Value::Object(details)) => {
            let v = py_int_or_none(details.get("cached_tokens"));
            v.unwrap_or(0)
        }
        _ => int_or(usage.get("cache_read_tokens"), 0),
    };
    let reasoning = match usage.get("output_tokens_details") {
        Some(Value::Object(details)) => {
            py_int_or_none(details.get("reasoning_tokens")).unwrap_or(0)
        }
        _ => 0,
    };
    json!({
        "input_tokens": inp,
        "output_tokens": out,
        "total_tokens": total,
        "input_tokens_details": {"cached_tokens": cached},
        "output_tokens_details": {"reasoning_tokens": reasoning},
    })
}

fn py_int_or_none(value: Option<&Value>) -> Option<i64> {
    value.filter(|v| !v.is_null()).and_then(py_int)
}

/// Responses `message` 输出 item。
fn build_responses_message_item(item_id: &str, text: &str, status: &str) -> Value {
    json!({
        "id": item_id,
        "type": "message",
        "role": "assistant",
        "status": status,
        "content": [{"type": "output_text", "text": text, "annotations": []}],
    })
}

/// 从 Responses `reasoning` item 取思考文本。接受线格式
/// (`summary`/`content` 列表,元素 `{type:"summary_text"|"reasoning_text",
/// text}`)与旧实现的平铺键(`summary_text`/`reasoning_text`/`content`
/// 字符串)。
fn reasoning_text_of(map: &Map<String, Value>) -> String {
    for key in ["summary", "content"] {
        if let Some(items) = map.get(key).and_then(Value::as_array) {
            let joined = items
                .iter()
                .filter_map(|item| item.get("text").and_then(Value::as_str))
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            if !joined.is_empty() {
                return joined;
            }
        }
    }
    for key in ["summary_text", "reasoning_text", "content"] {
        if let Some(text) = map.get(key).filter(|v| py_bool(v)).and_then(py_str) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return trimmed.to_owned();
            }
        }
    }
    String::new()
}

/// Responses `reasoning` **输入** item。线格式要求 `summary`/`content` 都是
/// 列表;旧实现发的 `summary_text: "<str>"` 会被真实上游拒绝 —— agnes 400
/// 「did not match any variant of untagged enum ResponseInput」、deepseek 422
/// 「invalid type: string, expected a sequence」(2026-10-13 实测)。
/// 思考文本放 `content[].reasoning_text` 而非 `summary[].summary_text`:
/// 前者 agnes/deepseek 都接受,后者 agnes 拒绝。
fn build_responses_reasoning_input(text: &str) -> Value {
    json!({
        "type": "reasoning",
        "id": format!("rs_{}", uuid_hex()),
        "summary": [],
        "content": [{"type": "reasoning_text", "text": text}],
    })
}

/// Responses `reasoning` 输出 item(summary/content 双列表;
/// `encrypted_content` 可选)。
fn build_responses_reasoning_item(
    item_id: &str,
    summary_text: &str,
    status: &str,
    encrypted_content: Option<&Value>,
) -> Value {
    let mut summary: Vec<Value> = Vec::new();
    let mut content: Vec<Value> = Vec::new();
    if !summary_text.is_empty() {
        summary.push(json!({"type": "summary_text", "text": summary_text}));
        content.push(json!({"type": "reasoning", "text": summary_text}));
    }
    if let Some(encrypted) = encrypted_content {
        let part = if encrypted.is_object() {
            let mut obj = encrypted.as_object().unwrap().clone();
            obj.insert("type".to_owned(), json!("encrypted_content"));
            Value::Object(obj)
        } else {
            json!({"type": "encrypted_content", "text": crate::python_semantics::py_str(encrypted).unwrap_or_default()})
        };
        content.push(part);
    }
    let mut result = Map::new();
    result.insert("id".to_owned(), json!(item_id));
    result.insert("type".to_owned(), json!("reasoning"));
    result.insert("summary".to_owned(), Value::Array(summary));
    result.insert("content".to_owned(), Value::Array(content));
    result.insert("status".to_owned(), json!(status));
    if let Some(encrypted) = encrypted_content {
        let stored = if encrypted.is_object() {
            encrypted.clone()
        } else {
            json!({"text": crate::python_semantics::py_str(encrypted).unwrap_or_default()})
        };
        result.insert("encrypted_content".to_owned(), stored);
    }
    Value::Object(result)
}

/// Responses `function_call` 输出 item(`arguments` 是 JSON 字符串)。
fn build_responses_function_call_item(
    item_id: &str,
    call_id: &str,
    name: &str,
    arguments: &str,
    status: &str,
    namespace: Option<&str>,
) -> Value {
    let mut item = Map::new();
    item.insert("id".to_owned(), json!(item_id));
    item.insert("type".to_owned(), json!("function_call"));
    item.insert(
        "call_id".to_owned(),
        json!(if call_id.is_empty() { item_id } else { call_id }),
    );
    item.insert("name".to_owned(), json!(name));
    item.insert("arguments".to_owned(), json!(arguments));
    item.insert("status".to_owned(), json!(status));
    if let Some(ns) = namespace.filter(|s| py_bool(&json!(s))) {
        item.insert("namespace".to_owned(), json!(ns));
    }
    Value::Object(item)
}

#[cfg(test)]
mod envelope_helpers_tests {
    use super::*;

    #[test]
    fn usage_normalization_nested_details() {
        let value = normalize_responses_usage(&json!({
            "prompt_tokens": 3, "completion_tokens": 5,
            "input_tokens_details": {"cached_tokens": 2},
            "output_tokens_details": {"reasoning_tokens": 1},
        }));
        assert_eq!(value["input_tokens"], json!(3));
        assert_eq!(value["total_tokens"], json!(8));
        assert_eq!(value["input_tokens_details"]["cached_tokens"], json!(2));
        assert_eq!(value["output_tokens_details"]["reasoning_tokens"], json!(1));
        // cache_read_tokens 兜底路径。
        let value = normalize_responses_usage(&json!({"cache_read_tokens": 4}));
        assert_eq!(value["input_tokens_details"]["cached_tokens"], json!(4));
    }

    #[test]
    fn items_builders() {
        let item = build_responses_function_call_item("fc_1", "", "get", "{}", "completed", None);
        assert_eq!(item["call_id"], json!("fc_1"));
        let reasoning = build_responses_reasoning_item("rs_1", "think", "completed", None);
        assert_eq!(reasoning["summary"][0]["text"], json!("think"));
        assert_eq!(
            build_responses_message_item("msg_1", "hi", "completed")["type"],
            json!("message")
        );
    }

    #[test]
    fn refusal_and_annotations_survive_responses_roundtrip() {
        let raw = json!({
            "id": "resp_1", "object": "response", "model": "m", "status": "completed",
            "output": [{"type": "message", "role": "assistant", "content": [
                {"type": "output_text", "text": "cited",
                 "annotations": [{"type": "url_citation", "url": "https://example.com"}]},
                {"type": "refusal", "refusal": "cannot comply"}
            ]}],
        });
        let mut report = ConversionReport::semantic();
        let response = decode_responses_json(&raw, &mut report);
        assert_eq!(response.refusal.as_deref(), Some("cannot comply"));
        assert_eq!(response.finish, Some(FinishReason::Refusal));
        let encoded = encode_responses_envelope(&response, &mut report);
        let content = encoded["output"][0]["content"].as_array().expect("content");
        assert_eq!(
            content[0]["annotations"][0]["url"],
            json!("https://example.com")
        );
        assert_eq!(content[1]["type"], json!("refusal"));
        assert_eq!(content[1]["refusal"], json!("cannot comply"));
    }

    /// 2026-10-13:Responses 的 `EasyInputMessage` 允许省略 `type`。真实
    /// 客户端(ZCode)发的就是只有 role/content 的对象;此前一律落
    /// unknown 分支丢弃,整个 input 变空 → 上游 400「messages 不能为空」。
    #[test]
    fn decode_input_accepts_typeless_message_items() {
        let mut messages = Vec::new();
        let mut report = ConversionReport::semantic();
        decode_input(
            Some(&json!([
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "hello"},
                {"type": "message", "role": "user", "content": "again"},
            ])),
            &mut messages,
            &mut report,
        );
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].role, Role::User);
        assert_eq!(messages[1].role, Role::Assistant);
        assert_eq!(messages[2].role, Role::User);
        assert_eq!(joined_text_of(&messages[0]), "hi");
    }

    /// 2026-10-13:reasoning 输入项必须发线格式(`summary`/`content` 列表)。
    /// 旧实现的 `summary_text: "<str>"` 被真实上游拒绝 —— agnes 400、
    /// deepseek 422「invalid type: string, expected a sequence」。
    #[test]
    fn encode_request_emits_wire_format_reasoning_item() {
        let mut request = SemanticRequest::new(SourceProtocol::OpenAi, "m");
        let mut assistant = Message::new(Role::Assistant);
        assistant.reasoning = Some(ReasoningBlock {
            text: "thought".to_owned(),
            signature: None,
            opaque: Vec::new(),
        });
        assistant.tool_calls.push(ToolCall {
            id: ToolCallId {
                local: "call_1".to_owned(),
                origin: IdOrigin::Source,
            },
            name: "f".to_owned(),
            arguments: ToolArguments::Raw("{}".to_owned()),
            extensions: Default::default(),
        });
        request.messages.push(assistant);

        let mut report = ConversionReport::semantic();
        let body = encode_responses_request(&request, &mut report);
        let input = body["input"].as_array().unwrap();
        let reasoning = input
            .iter()
            .find(|item| item["type"] == json!("reasoning"))
            .expect("reasoning item present");
        // summary/content 必须是数组,不能是字符串键。
        assert!(reasoning["summary"].is_array());
        assert!(reasoning["content"].is_array());
        assert!(reasoning.get("summary_text").is_none());
        assert_eq!(reasoning["content"][0]["text"], json!("thought"));
        // function_call 必须带 `id`(真实上游要求)。
        let call = input
            .iter()
            .find(|item| item["type"] == json!("function_call"))
            .expect("function_call item present");
        assert!(call["id"].as_str().is_some_and(|id| !id.is_empty()));
        assert_eq!(call["call_id"], json!("call_1"));
    }

    /// 2026-10-14:stream 必须显式落键。agnes 的 `/v1/responses` 省略
    /// `stream` 会按流式回 SSE,非流式路径解析即炸(500
    /// `invalid character 'd'`);NotSpecified 显式写 false(规范上的
    /// 省略语义),Explicit 原样透传。
    #[test]
    fn encode_request_always_emits_stream_field() {
        let base = |stream: Option<bool>| {
            let mut request = SemanticRequest::new(SourceProtocol::OpenAi, "m");
            if let Some(value) = stream {
                request.stream = crate::semantic::model::StreamPreference::Explicit(value);
            }
            let mut report = ConversionReport::semantic();
            encode_responses_request(&request, &mut report)
        };
        // 未指定 → 显式 false(agnes 硬约束)。
        assert_eq!(base(None)["stream"], json!(false));
        // 显式值原样透传。
        assert_eq!(base(Some(true))["stream"], json!(true));
        assert_eq!(base(Some(false))["stream"], json!(false));
    }

    /// 线格式与旧平铺键都能解出思考文本。
    #[test]
    fn reasoning_text_of_reads_both_shapes() {
        assert_eq!(
            reasoning_text_of(
                json!({"summary": [{"type": "summary_text", "text": "a"}], "content": []})
                    .as_object()
                    .unwrap()
            ),
            "a"
        );
        assert_eq!(
            reasoning_text_of(
                json!({"content": [{"type": "reasoning_text", "text": "b"}]})
                    .as_object()
                    .unwrap()
            ),
            "b"
        );
        assert_eq!(
            reasoning_text_of(json!({"summary_text": "c"}).as_object().unwrap()),
            "c"
        );
    }

    /// 2026-11-06:出站 `function_call` 的 `id` 必须以 `fc_` 开头。
    ///
    /// 旧实现把来源 id(chat 的 `call_` 开头)**同时**填进 `id` 与
    /// `call_id` → 命中严格校验节点即 400(实测 AgentRouter:
    /// `Invalid 'input[7].id': 'call_yDez…'. Expected an ID that begins with
    /// 'fc'`;该校验多后端不一致,前 10 次侥幸通过)。现在 `id` 恒为
    /// `fc_<uuid>`,来源 id 只进 `call_id`。
    #[test]
    fn encode_request_function_call_id_is_fc_prefixed() {
        let mut request = SemanticRequest::new(SourceProtocol::OpenAi, "m");
        let mut assistant = Message::new(Role::Assistant);
        assistant.tool_calls.push(ToolCall {
            id: ToolCallId {
                local: "call_yDezAbc".to_owned(),
                origin: IdOrigin::Source,
            },
            name: "f".to_owned(),
            arguments: ToolArguments::Raw("{}".to_owned()),
            extensions: Default::default(),
        });
        request.messages.push(assistant);

        let mut report = ConversionReport::semantic();
        let body = encode_responses_request(&request, &mut report);
        let call = body["input"]
            .as_array()
            .expect("input 是数组")
            .iter()
            .find(|item| item["type"] == json!("function_call"))
            .expect("function_call item present");
        let id = call["id"].as_str().expect("function_call.id 必须是字符串");
        assert!(
            id.starts_with("fc_"),
            "出站 function_call.id 必须以 fc_ 开头,实际 {id}"
        );
        // 来源 id 只进 call_id(关联键)。
        assert_eq!(call["call_id"], json!("call_yDezAbc"));
    }

    #[test]
    fn empty_function_call_name_is_dropped_with_its_tool_result() {
        let mut request = SemanticRequest::new(SourceProtocol::OpenAi, "m");
        let mut assistant = Message::new(Role::Assistant);
        assistant.tool_calls.push(ToolCall {
            id: ToolCallId {
                local: "call_empty".to_owned(),
                origin: IdOrigin::Source,
            },
            name: String::new(),
            arguments: ToolArguments::Raw("{}".to_owned()),
            extensions: Default::default(),
        });
        request.messages.push(assistant);
        let mut tool = Message::new(Role::Tool);
        tool.tool_result = Some(ToolResult {
            call_ref: Some(ToolCallId {
                local: "call_empty".to_owned(),
                origin: IdOrigin::Source,
            }),
            resolved_by_name: None,
            content: json!("ok"),
            is_error: false,
            extensions: Default::default(),
        });
        request.messages.push(tool);

        let mut report = ConversionReport::semantic();
        let body = encode_responses_request(&request, &mut report);
        let input = body["input"].as_array().expect("input");
        assert!(
            input
                .iter()
                .all(|item| item["type"] != json!("function_call")),
            "{input:?}"
        );
        assert!(
            input
                .iter()
                .all(|item| item["type"] != json!("function_call_output")),
            "孤立工具结果必须随空名调用一起丢弃: {input:?}"
        );
        assert!(
            report
                .diagnostics
                .iter()
                .filter(|d| d.code == DiagnosticCode::DroppedField)
                .count()
                >= 2
        );
    }
}
