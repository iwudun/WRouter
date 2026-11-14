//! OpenAI Chat Completions 协议适配器(B3)。
//!
//! Inbound:chat 请求 → [`SemanticRequest`];[`SemanticResponse`] →
//! chat completion 信封。
//! Outbound:语义请求 → chat 请求体;chat completion JSON → 语义响应。
//!
//! 旧两两转换(`convert.rs`/`mod.rs` 内联分支)的行为以语义路径复现;
//! 有意变更逐项标注 `// 意图变更:` 并登记 B 阶段实施记录。

use serde_json::{Map, Value, json};

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
    ContentPart, FinishReason, GenerationOptions, IdOrigin, Message, ReasoningBlock,
    ReasoningEffort, ResponseCandidate, Role, SemanticRequest, SemanticResponse, SourceProtocol,
    StreamPreference, ToolArguments, ToolCall, ToolCallId, ToolChoice, ToolDefinition, ToolResult,
    Usage,
};
use crate::semantic::parts::{
    opt_str, parts_from_chat_content, preserve_extra_keys, schema_or_empty,
};

// ---------------------------------------------------------------------------
// Inbound:客户端 openai chat
// ---------------------------------------------------------------------------

pub struct Inbound;

/// decode 阶段建模的顶层键(其余进 opaque)。
const MODELED_KEYS: &[&str] = &[
    "model",
    "messages",
    "tools",
    "tool_choice",
    "max_tokens",
    "max_completion_tokens",
    "temperature",
    "top_p",
    "stop",
    "stream",
    "parallel_tool_calls",
];

impl InboundAdapter for Inbound {
    fn protocol(&self) -> SourceProtocol {
        SourceProtocol::OpenAi
    }

    fn capabilities(&self) -> AdapterCapabilities {
        CHAT_NONSTREAM
    }

    fn decode_request(&self, raw: &Value) -> Result<Decoded<SemanticRequest>, SemanticError> {
        let mut report = ConversionReport::semantic();
        let mut request = SemanticRequest::new(
            SourceProtocol::OpenAi,
            opt_str(raw.get("model")).unwrap_or_default(),
        );
        request.generation = decode_generation(raw);
        request.stream = decode_stream(raw);
        request.messages = decode_messages(raw.get("messages"), &mut report);
        request.tools = decode_tools(raw.get("tools"));
        request.tool_choice = raw.get("tool_choice").map(decode_tool_choice);
        collect_opaque(raw, MODELED_KEYS, &mut request.opaque);
        Ok(Decoded::new(request, report))
    }

    fn encode_response(
        &self,
        response: &SemanticResponse,
    ) -> Result<Decoded<Value>, SemanticError> {
        let mut report = ConversionReport::semantic();
        Ok(Decoded::new(
            encode_chat_completion(response, &mut report),
            report,
        ))
    }

    fn encode_error(&self, error: &SemanticError, attempts: Option<&[String]>) -> (u16, Value) {
        encode_error(SourceProtocol::OpenAi, error, attempts)
    }
}

/// chat 生成参数解码(reasoning_effort/thinking 双写 opaque 供
/// openai 家族透传保真)。
pub(super) fn decode_generation(raw: &Value) -> GenerationOptions {
    let mut generation = GenerationOptions {
        temperature: raw.get("temperature").and_then(py_float),
        top_p: raw.get("top_p").and_then(py_float),
        // max_completion_tokens 是 OpenAI 现行键(max_tokens 已废弃);
        // 两者同现时以 completion 为准。此前只读 max_tokens,仅发
        // max_completion_tokens 的客户端(ZCode 实测)语义层拿到 None,
        // anthropic 出站回落 4096 默认,输出被静默截断。
        max_tokens: raw
            .get("max_completion_tokens")
            .and_then(py_int)
            .or_else(|| raw.get("max_tokens").and_then(py_int)),
        stop_sequences: decode_stop(raw.get("stop")),
        parallel_tool_calls: raw.get("parallel_tool_calls").and_then(Value::as_bool),
        reasoning_effort: opt_str(raw.get("reasoning_effort")).map(|s| ReasoningEffort::parse(&s)),
        thinking: raw.get("thinking").filter(|v| v.is_object()).map(|t| {
            crate::semantic::model::ThinkingOptions {
                budget_tokens: t
                    .get("budget_tokens")
                    .and_then(py_int)
                    .or_else(|| t.get("thinkingBudget").and_then(py_int)),
                include_thoughts: t.get("include_thoughts").and_then(Value::as_bool),
            }
        }),
        ..GenerationOptions::default()
    };
    // 家族透传保真:reasoning_effort / thinking 原值随 opaque 重放。
    if let Some(effort) = raw.get("reasoning_effort") {
        generation.opaque.push(crate::semantic::model::OpaqueField {
            path: "reasoning_effort".to_owned(),
            value: effort.clone(),
        });
    }
    if let Some(thinking) = raw.get("thinking") {
        generation.opaque.push(crate::semantic::model::OpaqueField {
            path: "thinking".to_owned(),
            value: thinking.clone(),
        });
    }
    generation
}

pub(super) fn decode_stop(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(items)) => items.iter().filter_map(py_str).collect(),
        _ => Vec::new(),
    }
}

pub(super) fn decode_stream(raw: &Value) -> StreamPreference {
    match raw.get("stream") {
        Some(Value::Null) | None => StreamPreference::NotSpecified,
        Some(v) => StreamPreference::Explicit(py_bool(v)),
    }
}

fn decode_messages(messages: Option<&Value>, report: &mut ConversionReport) -> Vec<Message> {
    let Some(Value::Array(items)) = messages else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let Some(map) = item.as_object() else {
            report.push(
                DiagnosticCode::OpaquePreserved,
                format!("messages[{index}]"),
                "非对象消息以 opaque 保留".to_owned(),
            );
            continue;
        };
        let raw_role = map.get("role").and_then(Value::as_str).unwrap_or("");
        let role = match raw_role {
            "system" => Role::System,
            "developer" => Role::Developer,
            "assistant" => Role::Assistant,
            "tool" => Role::Tool,
            "user" => Role::User,
            other => {
                let mut message = Message::new(Role::User);
                if !other.is_empty() {
                    message
                        .extensions
                        .insert("openai", "raw_role", json!(other));
                }
                out.push(message);
                continue;
            }
        };
        let mut message = Message::new(role);
        message.content = parts_from_chat_content(map.get("content").unwrap_or(&Value::Null));
        if role == Role::Assistant {
            if let Some(text) = opt_str(map.get("reasoning_content")).filter(|s| !s.is_empty()) {
                message.reasoning = Some(ReasoningBlock {
                    text,
                    signature: None,
                    opaque: Vec::new(),
                });
            }
            if let Some(Value::Array(calls)) = map.get("tool_calls") {
                for call in calls {
                    if let Some(tool_call) = decode_tool_call(call) {
                        message.tool_calls.push(tool_call);
                    }
                }
            }
        }
        if role == Role::Tool {
            message.tool_result = Some(ToolResult {
                call_ref: opt_str(map.get("tool_call_id")).map(|local| ToolCallId {
                    local,
                    origin: IdOrigin::Source,
                }),
                resolved_by_name: None,
                // 缺失 content 兜底空串(镜像旧实现)。
                content: map.get("content").cloned().unwrap_or(json!("")),
                is_error: false,
                extensions: Default::default(),
            });
        }
        message.name = opt_str(map.get("name"));
        preserve_extra_keys(
            item,
            &[
                "role",
                "content",
                "reasoning_content",
                "tool_calls",
                "tool_call_id",
                "name",
            ],
            &mut message.extensions,
            report,
            &format!("messages[{index}]"),
        );
        out.push(message);
    }
    out
}

fn decode_tool_call(call: &Value) -> Option<ToolCall> {
    let function = call.get("function").cloned().unwrap_or(json!({}));
    let name = opt_str(function.get("name")).unwrap_or_default();
    let arguments = match function.get("arguments") {
        Some(Value::String(raw)) if !raw.is_empty() => ToolArguments::Raw(raw.clone()),
        Some(Value::String(_)) | None | Some(Value::Null) => ToolArguments::Parsed(json!({})),
        // 意图变更:对象形态 arguments 不再折叠为 `{}`(旧实现静默丢参)。
        Some(other) => ToolArguments::Parsed(other.clone()),
    };
    Some(ToolCall {
        id: ToolCallId {
            local: opt_str(call.get("id")).unwrap_or_default(),
            origin: IdOrigin::Source,
        },
        name,
        arguments,
        extensions: Default::default(),
    })
}

pub(super) fn decode_tools(tools: Option<&Value>) -> Vec<ToolDefinition> {
    let Some(Value::Array(items)) = tools else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let inner = item.get("function").unwrap_or(item);
            let name = opt_str(inner.get("name"))?;
            Some(ToolDefinition {
                name,
                description: opt_str(item.get("description"))
                    .or_else(|| opt_str(inner.get("description"))),
                parameters_schema: schema_or_empty(
                    inner
                        .get("parameters")
                        .filter(|v| py_bool(v))
                        .or_else(|| inner.get("input_schema").filter(|v| py_bool(v))),
                ),
                extensions: Default::default(),
            })
        })
        .collect()
}

pub(super) fn decode_tool_choice(value: &Value) -> ToolChoice {
    match value {
        Value::String(s) => match s.as_str() {
            "auto" => ToolChoice::Auto,
            "none" => ToolChoice::None,
            "required" => ToolChoice::Required,
            other => ToolChoice::Opaque(json!(other)),
        },
        Value::Object(_) => {
            if value.get("type").and_then(Value::as_str) == Some("function") {
                // chat 嵌套形态 {type:function, function:{name}} 与
                // responses 扁平形态 {type:function, name} 双读。
                let name = value
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(py_str)
                    .or_else(|| value.get("name").and_then(py_str))
                    .unwrap_or_default();
                ToolChoice::Named { name }
            } else {
                ToolChoice::Opaque(value.clone())
            }
        }
        other => ToolChoice::Opaque(other.clone()),
    }
}

/// 顶层未建模键 → opaque(供家族转换重放)。
pub(super) fn collect_opaque(
    raw: &Value,
    modeled: &[&str],
    opaque: &mut Vec<crate::semantic::model::OpaqueField>,
) {
    let Some(map) = raw.as_object() else {
        return;
    };
    for (key, value) in map {
        if !modeled.contains(&key.as_str()) {
            opaque.push(crate::semantic::model::OpaqueField {
                path: key.clone(),
                value: value.clone(),
            });
        }
    }
}

/// 语义响应 → chat completion 信封(线级形态对齐旧三条生产路径;
/// finish 统一走映射表,见模块头注)。
pub(super) fn encode_chat_completion(
    response: &SemanticResponse,
    _report: &mut ConversionReport,
) -> Value {
    let mut choices = Vec::new();
    for candidate in &response.candidates {
        choices.push(encode_chat_choice(response, Some(candidate)));
    }
    if choices.is_empty() {
        choices.push(encode_chat_choice(response, None));
    }
    let mut envelope = Map::new();
    envelope.insert(
        "id".to_owned(),
        json!(
            response
                .id
                .clone()
                .unwrap_or_else(|| "chatcmpl_wrouter".to_owned())
        ),
    );
    envelope.insert("object".to_owned(), json!("chat.completion"));
    envelope.insert(
        "model".to_owned(),
        json!(response.model.clone().unwrap_or_default()),
    );
    envelope.insert("choices".to_owned(), Value::Array(choices));
    // P5-3:未观察到 usage 时不写全零对象(unknown ≠ 0)。目标编码器在
    // 流式路径同样只在有真实用量时下发 usage。
    if let Some(usage) = response.usage {
        let input = Usage::or_zero(usage.input);
        let output = Usage::or_zero(usage.output);
        let total = usage.total.unwrap_or(input + output);
        let mut usage_json = Map::new();
        usage_json.insert("prompt_tokens".to_owned(), json!(input));
        usage_json.insert("completion_tokens".to_owned(), json!(output));
        usage_json.insert("total_tokens".to_owned(), json!(total));
        // 缓存计数只在非零时透出(流式客户端一直带 `prompt_tokens_details`;非流
        // 式此前整个丢掉,导致调用日志 cache_read 恒 0)。零值不写键,黄金基线
        // 形状零变化(2026-10-13)。
        if let Some(cache_read) = usage.cache_read.filter(|value| *value != 0) {
            usage_json.insert(
                "prompt_tokens_details".to_owned(),
                json!({"cached_tokens": cache_read}),
            );
        }
        envelope.insert("usage".to_owned(), Value::Object(usage_json));
    }
    // gemini 方言透传:safetyRatings 顶层保留(镜像旧 gemini_response_to_openai)。
    if let Some(ratings) = response
        .first_candidate()
        .and_then(|c| c.extensions.get("gemini", "safetyRatings"))
    {
        envelope.insert("safetyRatings".to_owned(), ratings.clone());
    }
    Value::Object(envelope)
}

fn encode_chat_choice(response: &SemanticResponse, candidate: Option<&ResponseCandidate>) -> Value {
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut refusal: Option<String> = None;
    let mut tool_calls: Vec<Value> = Vec::new();
    if let Some(candidate) = candidate {
        for part in &candidate.content {
            match part {
                ContentPart::Text { text: part } => text.push_str(part),
                ContentPart::Refusal { text: part } => refusal = Some(part.clone()),
                _ => {}
            }
        }
        reasoning = candidate
            .reasoning
            .as_ref()
            .map(|r| r.text.clone())
            .unwrap_or_default();
        tool_calls = candidate.tool_calls.iter().map(encode_tool_call).collect();
    }
    if refusal.is_none() && candidate.is_none_or(|candidate| candidate.index == 0) {
        refusal = response.refusal.clone();
    }
    let finish_reason = if let Some(incomplete) = &response.incomplete {
        incomplete.as_responses_str().to_owned()
    } else if !tool_calls.is_empty() {
        // gemini 路径的 tool_calls 覆盖语义,统一保留(上游常报 STOP)。
        "tool_calls".to_owned()
    } else if refusal.is_some() {
        "refusal".to_owned()
    } else {
        finish::to_openai_chat(
            candidate
                .and_then(|candidate| candidate.finish.as_ref())
                .or(response.finish.as_ref())
                .unwrap_or(&FinishReason::Stop),
        )
    };
    let mut message = Map::new();
    message.insert("role".to_owned(), json!("assistant"));
    // 空 content 走 ""(旧 anthropic 主路径形态;gemini 路径的 null
    // 形态为有意收敛,登记实施记录)。
    message.insert("content".to_owned(), json!(text));
    if !reasoning.is_empty() {
        message.insert("reasoning_content".to_owned(), json!(reasoning));
    }
    if let Some(refusal) = refusal {
        message.insert("refusal".to_owned(), json!(refusal));
    }
    if !tool_calls.is_empty() {
        message.insert("tool_calls".to_owned(), Value::Array(tool_calls));
    }
    json!({
        "index": candidate.map(|candidate| candidate.index).unwrap_or(0),
        "message": Value::Object(message),
        "finish_reason": finish_reason,
    })
}

/// 工具调用 → openai `tool_calls` 条目(arguments 字符串形态)。
pub(super) fn encode_tool_call(call: &ToolCall) -> Value {
    json!({
        "id": if call.id.local.is_empty() { Value::Null } else { json!(call.id.local) },
        "type": "function",
        "function": {
            "name": if call.name.is_empty() { Value::Null } else { json!(call.name) },
            "arguments": call.arguments.as_raw_string(),
        },
    })
}

// ---------------------------------------------------------------------------
// Outbound:上游 openai chat
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
        _ctx: &OutboundContext,
    ) -> Result<Decoded<Value>, SemanticError> {
        let mut report = request.report();
        let body = encode_chat_request(request, &mut report);
        Ok(Decoded::new(body, report))
    }

    fn decode_response(
        &self,
        raw: &Value,
        _ctx: &OutboundContext,
    ) -> Result<Decoded<SemanticResponse>, SemanticError> {
        if !raw.is_object() {
            return Err(SemanticError::unsupported(
                "Chat Completions 响应不是 JSON 对象,无法按 chat 语义解码",
            ));
        }
        let mut report = ConversionReport::semantic();
        Ok(Decoded::new(
            decode_chat_completion(raw, &mut report),
            report,
        ))
    }
}

/// 语义请求 → chat 请求体(源协议分支复现旧转换的透传策略:
/// anthropic/openai 源重放 opaque,responses/gemini 源为白名单体)。
pub(super) fn encode_chat_request(
    request: &SemanticRequest,
    report: &mut ConversionReport,
) -> Value {
    let mut body = Map::new();
    body.insert("model".to_owned(), json!(request.model));
    let mut messages: Vec<Value> = Vec::new();
    // 意图变更(源 anthropic):system 不再散落 messages,统一升格
    // messages[0](与旧 anthropic_request_to_openai 的插入位一致);
    // 顶层 system 键不再残留在 chat 体(旧实现 payload 拷贝残留)。
    for message in &request.messages {
        // tool_result 可能挂在 user 消息上(anthropic 形态),同样走
        // tool 消息编码。
        if message.role == Role::Tool || message.tool_result.is_some() {
            messages.push(encode_tool_message(message));
            continue;
        }
        messages.push(encode_chat_message(message, request.source, report));
    }
    body.insert("messages".to_owned(), Value::Array(messages));
    let default_max = matches!(
        request.source,
        SourceProtocol::OpenAiResponses | SourceProtocol::Anthropic
    );
    if default_max || request.generation.max_tokens.is_some() {
        body.insert(
            "max_tokens".to_owned(),
            json!(request.generation.max_tokens.unwrap_or(4096)),
        );
    }
    if let Some(temperature) = request.generation.temperature {
        body.insert("temperature".to_owned(), json!(temperature));
    }
    if let Some(top_p) = request.generation.top_p {
        body.insert("top_p".to_owned(), json!(top_p));
    }
    if !request.generation.stop_sequences.is_empty() {
        // 意图变更:stop 统一为数组形态(旧家族转换按原样透传字符串)。
        body.insert("stop".to_owned(), json!(request.generation.stop_sequences));
    }
    if let Some(parallel) = request.generation.parallel_tool_calls {
        body.insert("parallel_tool_calls".to_owned(), json!(parallel));
    }
    match request.stream {
        StreamPreference::Explicit(value) => {
            body.insert("stream".to_owned(), json!(value));
        }
        StreamPreference::NotSpecified => {}
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
                            "function": {
                                "name": tool.name,
                                "description": tool.description.clone().unwrap_or_default(),
                                "parameters": if tool.parameters_schema.is_null() { json!({}) } else { tool.parameters_schema.clone() },
                            },
                        })
                    })
                    .collect(),
            ),
        );
    }
    if let Some(choice) = &request.tool_choice {
        body.insert("tool_choice".to_owned(), encode_tool_choice(choice));
    }
    // 语义思考编码:非 openai 家族源(anthropic thinking/responses
    // reasoning.effort/gemini thinkingConfig)此前整体丢失思考意图——
    // anthropic 源原样重放 `thinking`(chat 线格式无此键,严格上游 400),
    // responses/gemini 白名单体直接丢弃。统一按语义换算 reasoning_effort。
    encode_chat_reasoning(request, &mut body);
    // 家族源(opaque 重放)先行;语义键已被上面占用,重放跳过建模键
    // (collect 阶段已剔除;reasoning_effort/thinking 双写项在此重放)。
    if matches!(
        request.source,
        SourceProtocol::OpenAi | SourceProtocol::Anthropic
    ) {
        for field in &request.opaque {
            if request.source == SourceProtocol::Anthropic
                && NON_CHAT_REPLAY_KEYS.contains(&field.path.as_str())
            {
                report.push(
                    DiagnosticCode::DroppedField,
                    field.path.clone(),
                    "Anthropic 方言键不重放进 Chat 体(thinking 已按语义编码为 reasoning_effort)"
                        .to_owned(),
                );
                continue;
            }
            body.insert(field.path.clone(), field.value.clone());
        }
    } else {
        for field in &request.opaque {
            report.push(
                DiagnosticCode::DroppedField,
                field.path.clone(),
                "目标协议白名单外键丢弃(旧实现静默)".to_owned(),
            );
        }
    }
    Value::Object(body)
}

/// Anthropic 方言键:anthropic 源不重放进 Chat 出站体的顶层键。
/// `thinking` 的意图已按语义换算为 `reasoning_effort`;`metadata`/`top_k`/
/// `anthropic_version` 为 chat 线格式不存在的键,重放只会在严格校验的
/// 上游 400(与 anthropic 出站的 NON_ANTHROPIC_REPLAY_KEYS 同性质)。
const NON_CHAT_REPLAY_KEYS: &[&str] = &["thinking", "metadata", "top_k", "anthropic_version"];

/// 语义思考配置 → chat `reasoning_effort`。
///
/// 优先级与 anthropic 出站一致:显式 budget(thinking.budget_tokens)按
/// `gateway::budget_to_effort` 正向分档 > 语义 effort 直出。`Off` 省略
/// (chat 线格式无跨上游统一的关闭表达,缺省即无推理)。
fn encode_chat_reasoning(request: &SemanticRequest, body: &mut Map<String, Value>) {
    let derived = request
        .generation
        .thinking
        .as_ref()
        .and_then(|thinking| thinking.budget_tokens)
        .map(crate::gateway::budget_to_effort);
    let effort = derived.or_else(|| {
        request
            .generation
            .reasoning_effort
            .as_ref()
            .and_then(|effort| match effort {
                ReasoningEffort::Off => None,
                other => Some(other.as_raw()),
            })
    });
    if let Some(effort) = effort {
        body.insert("reasoning_effort".to_owned(), json!(effort));
    }
}

fn encode_tool_choice(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => json!("auto"),
        // 意图变更:None/Required 编码为合法 openai 值(旧实现 "none"
        // 原样透传、"any"→"auto")。
        ToolChoice::None => json!("none"),
        ToolChoice::Required => json!("required"),
        ToolChoice::Named { name } => json!({"type": "function", "function": {"name": name}}),
        ToolChoice::Opaque(value) => value.clone(),
    }
}

fn encode_chat_message(
    message: &Message,
    source: SourceProtocol,
    report: &mut ConversionReport,
) -> Value {
    let role = match message.role {
        Role::System => "system",
        Role::Developer => "developer",
        Role::User | Role::Tool => "user",
        Role::Assistant => "assistant",
    };
    let mut out = Map::new();
    // 非 standard 角色回放原值(镜像旧 role 透传)。
    if let Some(raw_role) = message
        .extensions
        .get("openai", "raw_role")
        .and_then(Value::as_str)
    {
        out.insert("role".to_owned(), json!(raw_role));
    } else {
        out.insert("role".to_owned(), json!(role));
    }
    out.insert(
        "content".to_owned(),
        if message.content.len() == 1 {
            match &message.content[0] {
                ContentPart::Text { text } => json!(text),
                other => json!([encode_chat_part(other, report)]),
            }
        } else if message.content.is_empty() {
            // 空 content 走 ""(旧 responses→chat 形态;统一字符串化,
            // anthropic 源的 null 形态为有意收敛)。
            json!("")
        } else {
            Value::Array(
                message
                    .content
                    .iter()
                    .map(|part| encode_chat_part(part, report))
                    .collect(),
            )
        },
    );
    if let Some(reasoning) = &message.reasoning
        && !reasoning.text.is_empty()
    {
        // 意图变更(源 anthropic):thinking 不再折叠进 content 文本,
        // 统一走 reasoning_content(与响应面一致)。
        out.insert("reasoning_content".to_owned(), json!(reasoning.text));
    }
    if !message.tool_calls.is_empty() {
        out.insert(
            "tool_calls".to_owned(),
            Value::Array(message.tool_calls.iter().map(encode_tool_call).collect()),
        );
    }
    if let Some(name) = &message.name {
        out.insert("name".to_owned(), json!(name));
    }
    replay_message_extensions(message, source, &mut out, report);
    Value::Object(out)
}

fn encode_chat_part(part: &ContentPart, report: &mut ConversionReport) -> Value {
    match part {
        ContentPart::Text { text } => json!({"type": "text", "text": text}),
        ContentPart::ImageRef { url } => json!({"type": "image_url", "image_url": {"url": url}}),
        ContentPart::ImageInline {
            media_type,
            data_base64,
        } => json!({
            "type": "image_url",
            "image_url": {"url": format!("data:{};base64,{}", media_type.clone().unwrap_or_else(|| "image/png".to_owned()), data_base64)},
        }),
        // 目标协议不可表达的多模态:保留原值并以诊断显式化(不伪造文本)。
        ContentPart::Audio { .. } | ContentPart::File { .. } => {
            report.push(
                DiagnosticCode::DowngradedContent,
                "content",
                "chat 目标不表达 audio/file 块,以 opaque 保留".to_owned(),
            );
            match part {
                ContentPart::Audio {
                    media_type,
                    data_base64,
                } => json!({
                    "type": "audio", "media_type": media_type, "data_base64": data_base64,
                }),
                ContentPart::File {
                    file_id,
                    filename,
                    data_base64,
                } => json!({
                    "type": "file", "file_id": file_id, "filename": filename, "data_base64": data_base64,
                }),
                _ => unreachable!(),
            }
        }
        ContentPart::Refusal { text } => json!({"type": "refusal", "refusal": text}),
        ContentPart::Opaque { value, .. } => value.clone(),
    }
}

fn encode_tool_message(message: &Message) -> Value {
    let result = message.tool_result.as_ref();
    let call_id = result
        .and_then(|r| r.call_ref.as_ref())
        .map(|id| id.local.clone())
        .unwrap_or_default();
    json!({
        "role": "tool",
        "tool_call_id": if call_id.is_empty() { Value::Null } else { json!(call_id) },
        "content": result.map(|r| r.content.clone()).unwrap_or(Value::Null),
    })
}

/// 家族源的消息级扩展重放(镜像旧 payload 拷贝的消息级未知键丢弃:
/// 默认仅记诊断;source==Anthropic 时不重放,保持旧重建语义)。
fn replay_message_extensions(
    message: &Message,
    _source: SourceProtocol,
    _out: &mut Map<String, Value>,
    report: &mut ConversionReport,
) {
    for block in message.extensions.blocks() {
        for key in block.fields.keys() {
            if let Some(suffix) = key.strip_prefix("extra/") {
                report.push(
                    DiagnosticCode::DroppedField,
                    format!("messages[].{suffix}"),
                    "消息级未识别键不重放(镜像旧实现重建语义)".to_owned(),
                );
            }
        }
    }
}

/// chat completion JSON → 语义响应(outbound decode)。
pub(super) fn decode_chat_completion(
    raw: &Value,
    report: &mut ConversionReport,
) -> SemanticResponse {
    let mut response = SemanticResponse::new();
    response.id = opt_str(raw.get("id"));
    response.model = opt_str(raw.get("model"));
    response.usage = Some(decode_usage(raw.get("usage")));
    if let Some(Value::Array(choices)) = raw.get("choices") {
        for (position, choice) in choices.iter().enumerate() {
            let index = choice
                .get("index")
                .and_then(Value::as_u64)
                .map(|index| index as usize)
                .unwrap_or(position);
            let mut candidate = crate::semantic::model::ResponseCandidate::new(index);
            let message = choice.get("message").cloned().unwrap_or(json!({}));
            candidate.content =
                parts_from_chat_content(message.get("content").unwrap_or(&Value::Null));
            if let Some(refusal) = opt_str(message.get("refusal")).filter(|s| !s.is_empty()) {
                if !candidate
                    .content
                    .iter()
                    .any(|part| matches!(part, ContentPart::Refusal { .. }))
                {
                    candidate.content.push(ContentPart::Refusal {
                        text: refusal.clone(),
                    });
                }
                response.refusal = Some(refusal);
            }
            if let Some(text) = opt_str(message.get("reasoning_content")).filter(|s| !s.is_empty())
            {
                candidate.reasoning = Some(ReasoningBlock {
                    text,
                    signature: None,
                    opaque: Vec::new(),
                });
            }
            if let Some(Value::Array(calls)) = message.get("tool_calls") {
                for call in calls {
                    if let Some(tool_call) = decode_tool_call(call) {
                        candidate.tool_calls.push(tool_call);
                    }
                }
            }
            candidate.finish = Some(finish::from_openai(
                choice.get("finish_reason").and_then(Value::as_str),
                report,
                &format!("choices[{index}].finish_reason"),
            ));
            response.candidates.push(candidate);
        }
    }
    response.finish = response
        .first_candidate()
        .and_then(|c| c.finish.clone())
        .or(Some(FinishReason::Stop));
    response
}

pub(super) fn decode_usage(value: Option<&Value>) -> Usage {
    let mut usage = Usage::reported();
    let Some(map) = value else {
        return usage;
    };
    // 负值视为未报告:个别兼容端点以负数表达 cache 折扣,透传会污染
    // 命中率/成本算术(所有 usage 解析点统一防御)。
    let nn = |v: Option<i64>| v.filter(|n| *n >= 0);
    usage.input = nn(map.get("prompt_tokens").and_then(py_int));
    usage.output = nn(map.get("completion_tokens").and_then(py_int));
    usage.total = nn(map.get("total_tokens").and_then(py_int));
    usage.reasoning = nn(map
        .get("completion_tokens_details")
        .and_then(|d| d.get("reasoning_tokens"))
        .and_then(py_int));
    usage.cache_read = nn(map
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(py_int));
    usage
}
