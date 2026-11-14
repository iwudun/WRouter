//! Anthropic Messages 协议适配器(B4)。
//!
//! Inbound:`system` + content blocks 请求 → [`SemanticRequest`];
//! [`SemanticResponse`] → Anthropic Message 信封。
//! Outbound:语义请求 → Anthropic 请求体;Message JSON → 语义响应。
//!
//! thinking signature / redacted_thinking / cache_control 块原值经
//! Opaque/扩展保留(契约 §3.5);stop reason 统一走映射表(fix:
//! 旧实现 length 等英文值原样透传)。

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
    ContentPart, FinishReason, IdOrigin, Message, ReasoningBlock, Role, SemanticRequest,
    SemanticResponse, SourceProtocol, StreamPreference, ToolArguments, ToolCall, ToolCallId,
    ToolChoice, ToolDefinition, ToolResult, Usage,
};
use crate::semantic::parts::{joined_text, opt_str};
use crate::semantic::tool_names::{self, Profile};

// ---------------------------------------------------------------------------
// 阶段五 C-3:assistant prefill 续写(断流后把已收的那半句发回上游)
// ---------------------------------------------------------------------------

/// 在**原始** Anthropic 请求体上拼装 assistant prefill(续写请求)。
///
/// Anthropic 的续写语义:把「已经收到的那半句」作为最后一条 assistant
/// 消息发回,模型会**从那里接着写**,而不是重头再来 —— 客户端因此拿到
/// 一条完整回复而不是半句话。
///
/// - `messages` 末条已是 assistant → 把 prefill 文本**并入**该条
///   (不新增消息:两条相邻的 assistant 消息语义含糊,模型可能当成
///   两轮对话);
/// - 否则 → 追加一条 assistant 消息(prefill 必须是最后一条才能生效);
/// - 形态不合法(非对象 / 无 `messages` / `messages` 非数组 / 末条
///   content 形态未知)→ `None`,调用方据此**放弃续写**。
///
/// ⚠️ 本函数只处理 Anthropic wire 形态;OpenAI/Gemini 没有官方续写
/// 语义(照抄会变成「假续写」,模型把 prefill 当新消息重新回答)。
pub fn append_assistant_prefill(payload: &Value, partial: &str) -> Option<Value> {
    if partial.is_empty() {
        return None;
    }
    let mut next = payload.clone();
    let messages = next.as_object_mut()?.get_mut("messages")?.as_array_mut()?;
    match messages.last_mut() {
        Some(last) if last.get("role").and_then(Value::as_str) == Some("assistant") => {
            let content = last.get_mut("content")?;
            match content {
                Value::String(text) => {
                    let mut merged = text.clone();
                    merged.push_str(partial);
                    *content = Value::String(merged);
                }
                Value::Array(blocks) => {
                    blocks.push(json!({"type": "text", "text": partial}));
                }
                _ => return None,
            }
        }
        _ => messages.push(json!({
            "role": "assistant",
            "content": [{"type": "text", "text": partial}],
        })),
    }
    Some(next)
}

/// 阶段五 C-3:请求是否开启了 extended thinking。
///
/// Anthropic 的 extended thinking 与 assistant prefill **互斥**(代码外
/// 事实,未联网核实)→ 保守策略:带 thinking 的请求不续写。
pub fn has_thinking(payload: &Value) -> bool {
    payload
        .get("thinking")
        .is_some_and(|thinking| !thinking.is_null() && thinking != &Value::Bool(false))
}

// ---------------------------------------------------------------------------
// Inbound:客户端 anthropic
// ---------------------------------------------------------------------------

pub struct Inbound;

const MODELED_KEYS: &[&str] = &[
    "model",
    "system",
    "messages",
    "tools",
    "tool_choice",
    "max_tokens",
    "temperature",
    "top_p",
    "stop_sequences",
    "thinking",
    "stream",
];

impl InboundAdapter for Inbound {
    fn protocol(&self) -> SourceProtocol {
        SourceProtocol::Anthropic
    }

    fn capabilities(&self) -> AdapterCapabilities {
        CHAT_NONSTREAM
    }

    fn decode_request(&self, raw: &Value) -> Result<Decoded<SemanticRequest>, SemanticError> {
        let mut report = ConversionReport::semantic();
        let mut request = SemanticRequest::new(
            SourceProtocol::Anthropic,
            opt_str(raw.get("model")).unwrap_or_default(),
        );
        request.generation.max_tokens = raw.get("max_tokens").and_then(py_int);
        request.generation.temperature = raw.get("temperature").and_then(py_float);
        request.generation.top_p = raw.get("top_p").and_then(py_float);
        request.generation.stop_sequences = raw
            .get("stop_sequences")
            .and_then(Value::as_array)
            .map(|items| items.iter().filter_map(py_str).collect())
            .unwrap_or_default();
        if let Some(Value::Object(thinking)) = raw.get("thinking") {
            request.generation.thinking = Some(crate::semantic::model::ThinkingOptions {
                budget_tokens: thinking.get("budget_tokens").and_then(py_int),
                include_thoughts: None,
            });
            // 家族透传保真:thinking 原值随 opaque 重放。
            request
                .generation
                .opaque
                .push(crate::semantic::model::OpaqueField {
                    path: "thinking".to_owned(),
                    value: raw.get("thinking").cloned().unwrap_or(Value::Null),
                });
        }
        request.stream = match raw.get("stream") {
            Some(Value::Null) | None => StreamPreference::NotSpecified,
            Some(v) => StreamPreference::Explicit(py_bool(v)),
        };
        if let Some(system) = raw.get("system").filter(|v| py_bool(v)) {
            request.messages.push(decode_system(system));
        }
        if let Some(Value::Array(items)) = raw.get("messages") {
            for (index, item) in items.iter().enumerate() {
                if let Some(message) =
                    decode_message(item, &mut report, &format!("messages[{index}]"))
                {
                    request.messages.push(message);
                }
            }
        }
        if let Some(Value::Array(tools)) = raw.get("tools") {
            for tool in tools {
                if let Some(name) = opt_str(tool.get("name")) {
                    request.tools.push(ToolDefinition {
                        name,
                        description: opt_str(tool.get("description")),
                        parameters_schema: tool
                            .get("input_schema")
                            .filter(|v| py_bool(v))
                            .cloned()
                            .unwrap_or(json!({})),
                        extensions: Default::default(),
                    });
                }
            }
        }
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
            encode_message_envelope(response, &mut report),
            report,
        ))
    }

    fn encode_error(&self, error: &SemanticError, attempts: Option<&[String]>) -> (u16, Value) {
        encode_error(SourceProtocol::Anthropic, error, attempts)
    }
}

fn decode_system(system: &Value) -> Message {
    let mut message = Message::new(Role::System);
    match system {
        Value::String(text) => message
            .content
            .push(ContentPart::Text { text: text.clone() }),
        Value::Array(blocks) => {
            for block in blocks {
                if block.get("type").and_then(Value::as_str) == Some("text")
                    && block.get("cache_control").is_none()
                {
                    message.content.push(ContentPart::Text {
                        text: opt_str(block.get("text")).unwrap_or_default(),
                    });
                } else {
                    // cache_control 断点等协议特有块原样保留。
                    message.content.push(ContentPart::Opaque {
                        kind_hint: block.get("type").and_then(Value::as_str).map(str::to_owned),
                        value: block.clone(),
                    });
                }
            }
        }
        other => message.content.push(ContentPart::Opaque {
            kind_hint: None,
            value: other.clone(),
        }),
    }
    message
}

fn decode_message(item: &Value, report: &mut ConversionReport, path: &str) -> Option<Message> {
    let map = item.as_object()?;
    let role = match map.get("role").and_then(Value::as_str) {
        Some("assistant") => Role::Assistant,
        Some("user") | None => Role::User,
        other => {
            let mut message = Message::new(Role::User);
            if let Some(other) = other {
                message
                    .extensions
                    .insert("anthropic", "raw_role", json!(other));
            }
            report.push(
                DiagnosticCode::OpaquePreserved,
                format!("{path}.role"),
                "非 standard 角色按 user 处理,原值扩展保留".to_owned(),
            );
            return Some(message);
        }
    };
    let mut message = Message::new(role);
    match map.get("content") {
        Some(Value::Array(blocks)) => {
            for block in blocks {
                decode_block(block, &mut message, report, path);
            }
        }
        Some(Value::String(text)) => message
            .content
            .push(ContentPart::Text { text: text.clone() }),
        Some(Value::Null) | None => {}
        Some(other) => message.content.push(ContentPart::Opaque {
            kind_hint: None,
            value: other.clone(),
        }),
    }
    Some(message)
}

fn decode_block(block: &Value, message: &mut Message, report: &mut ConversionReport, path: &str) {
    let kind = block.get("type").and_then(Value::as_str).unwrap_or("");
    match kind {
        "text" if block.get("cache_control").is_none() => {
            message.content.push(ContentPart::Text {
                text: opt_str(block.get("text")).unwrap_or_default(),
            });
        }
        "thinking" => {
            let reasoning = message.reasoning.get_or_insert_with(|| ReasoningBlock {
                text: String::new(),
                signature: None,
                opaque: Vec::new(),
            });
            reasoning
                .text
                .push_str(&opt_str(block.get("thinking")).unwrap_or_default());
            if reasoning.signature.is_none() {
                reasoning.signature = opt_str(block.get("signature"));
            }
        }
        "redacted_thinking" => {
            report.push(
                DiagnosticCode::OpaquePreserved,
                format!("{path}.redacted_thinking"),
                "redacted_thinking 原样保留".to_owned(),
            );
            message.content.push(ContentPart::Opaque {
                kind_hint: Some("redacted_thinking".to_owned()),
                value: block.clone(),
            });
        }
        "tool_use" => {
            message.tool_calls.push(ToolCall {
                id: ToolCallId {
                    local: opt_str(block.get("id")).unwrap_or_default(),
                    origin: IdOrigin::Source,
                },
                // 上游回流的是名册清洗名,还原原名(非编码形态原样通过)。
                name: tool_names::desanitize(&opt_str(block.get("name")).unwrap_or_default()),
                arguments: ToolArguments::Parsed(
                    block
                        .get("input")
                        .filter(|v| py_bool(v))
                        .cloned()
                        .unwrap_or(json!({})),
                ),
                extensions: Default::default(),
            });
        }
        "tool_result" => {
            message.tool_result = Some(ToolResult {
                call_ref: opt_str(block.get("tool_use_id")).map(|local| ToolCallId {
                    local,
                    origin: IdOrigin::Source,
                }),
                resolved_by_name: None,
                content: block.get("content").cloned().unwrap_or(Value::Null),
                is_error: block
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                extensions: Default::default(),
            });
        }
        "image" => {
            let source = block.get("source").cloned().unwrap_or(Value::Null);
            let source_type = source.get("type").and_then(Value::as_str).unwrap_or("");
            if source_type == "base64" {
                message.content.push(ContentPart::ImageInline {
                    media_type: opt_str(source.get("media_type")),
                    data_base64: opt_str(source.get("data")).unwrap_or_default(),
                });
            } else if source_type == "url" {
                message.content.push(ContentPart::ImageRef {
                    url: opt_str(source.get("url")).unwrap_or_default(),
                });
            } else {
                message.content.push(ContentPart::Opaque {
                    kind_hint: Some("image".to_owned()),
                    value: block.clone(),
                });
            }
        }
        _ => {
            message.content.push(ContentPart::Opaque {
                kind_hint: Some(kind.to_owned()),
                value: block.clone(),
            });
        }
    }
}

fn decode_tool_choice(value: &Value) -> ToolChoice {
    match value {
        Value::String(s) => match s.as_str() {
            "auto" => ToolChoice::Auto,
            "any" | "required" => ToolChoice::Required,
            "none" => ToolChoice::None,
            other => ToolChoice::Opaque(json!(other)),
        },
        Value::Object(_) => match value.get("type").and_then(Value::as_str) {
            Some("auto") => ToolChoice::Auto,
            Some("any") => ToolChoice::Required,
            Some("none") => ToolChoice::None,
            Some("tool") => ToolChoice::Named {
                name: opt_str(value.get("name")).unwrap_or_default(),
            },
            _ => ToolChoice::Opaque(value.clone()),
        },
        other => ToolChoice::Opaque(other.clone()),
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

/// 语义响应 → Anthropic Message 信封。
pub(super) fn encode_message_envelope(
    response: &SemanticResponse,
    report: &mut ConversionReport,
) -> Value {
    let candidate = response.first_candidate();
    let mut blocks: Vec<Value> = Vec::new();
    if let Some(reasoning) = candidate.and_then(|c| c.reasoning.as_ref())
        && !reasoning.text.is_empty()
    {
        let mut block = Map::new();
        block.insert("type".to_owned(), json!("thinking"));
        block.insert("thinking".to_owned(), json!(reasoning.text));
        if reasoning.signature.is_some() {
            // 签名是发送方家族私有凭据;跨协议不迁移(同协议原生路径
            // 不经本编码器)。
            report.push(
                DiagnosticCode::DroppedField,
                "reasoning.signature".to_owned(),
                "跨协议不迁移厂商私有推理签名".to_owned(),
            );
        }
        blocks.push(Value::Object(block));
    }
    let text = joined_text(candidate.map(|c| c.content.as_slice()).unwrap_or_default());
    if !text.is_empty() {
        blocks.push(json!({"type": "text", "text": text}));
    }
    let refusal = response.refusal.clone().or_else(|| {
        candidate.and_then(|candidate| {
            candidate.content.iter().find_map(|part| match part {
                ContentPart::Refusal { text } => Some(text.clone()),
                _ => None,
            })
        })
    });
    if let Some(refusal) = &refusal {
        // Anthropic 没有 refusal content block:文本保留,拒绝语义由
        // stop_reason=refusal 承载,并显式记录降级。
        blocks.push(json!({"type": "text", "text": refusal}));
        report.push(
            DiagnosticCode::DowngradedContent,
            "content",
            "Anthropic 无 refusal 块类型;拒绝文本以 text 块承载,stop_reason=refusal".to_owned(),
        );
    }
    if let Some(candidate) = candidate {
        for call in &candidate.tool_calls {
            let (input, downgraded) = call.arguments.as_object();
            if downgraded {
                report.push(
                    DiagnosticCode::DowngradedContent,
                    "tool_calls",
                    format!("工具 {} 参数非法 JSON → {{\"raw\": s}} 兜底", call.name),
                );
            }
            blocks.push(json!({
                "type": "tool_use",
                "id": if call.id.local.is_empty() { Value::Null } else { json!(call.id.local) },
                "name": if call.name.is_empty() { Value::Null } else { json!(call.name) },
                "input": input,
            }));
        }
    }
    let usage = response.usage.unwrap_or_else(Usage::reported);
    // tool_calls 覆盖语义(镜像旧 gemini→anthropic;chat 上游的
    // tool_calls 原样透传也随之归一为 tool_use)。
    let stop_reason = if candidate.map(|c| !c.tool_calls.is_empty()).unwrap_or(false) {
        finish::to_anthropic(&FinishReason::ToolCall)
    } else if refusal.is_some() {
        finish::to_anthropic(&FinishReason::Refusal)
    } else {
        finish::to_anthropic(response.finish.as_ref().unwrap_or(&FinishReason::Stop))
    };
    let mut usage_json = Map::new();
    usage_json.insert(
        "input_tokens".to_owned(),
        json!(Usage::or_zero(usage.input)),
    );
    usage_json.insert(
        "output_tokens".to_owned(),
        json!(Usage::or_zero(usage.output)),
    );
    // 缓存计数只在非零时透出:缺失时下游(normalize_usage 的「首个非零」
    // 方言链)会把整行 cache_read/cache_write 记成 0(2026-10-13 实测
    // anthropic 客户端缓存读写恒 0)。零值不写键,黄金基线形状零变化。
    if let Some(cache_read) = usage.cache_read.filter(|value| *value != 0) {
        usage_json.insert("cache_read_input_tokens".to_owned(), json!(cache_read));
    }
    if let Some(cache_write) = usage.cache_write.filter(|value| *value != 0) {
        usage_json.insert("cache_creation_input_tokens".to_owned(), json!(cache_write));
    }
    json!({
        "id": response.id.clone().unwrap_or_else(|| "msg_wrouter".to_owned()),
        "type": "message",
        "role": "assistant",
        "model": response.model.clone().unwrap_or_default(),
        "content": blocks,
        "stop_reason": stop_reason,
        "stop_sequence": Value::Null,
        "usage": Value::Object(usage_json),
    })
}

// ---------------------------------------------------------------------------
// Outbound:上游 anthropic
// ---------------------------------------------------------------------------

pub struct Outbound;

impl OutboundAdapter for Outbound {
    fn protocol(&self) -> ProviderProtocol {
        ProviderProtocol::Anthropic
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
            encode_anthropic_request(request, &mut report),
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
                "Anthropic 响应不是 JSON 对象,无法按 Messages 语义解码",
            ));
        }
        let mut report = ConversionReport::semantic();
        Ok(Decoded::new(
            decode_anthropic_message(raw, &mut report),
            report,
        ))
    }
}

/// 占位 id(工具 id 缺失时的 wire 合法兜底,无可匹配语义)。
fn placeholder_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("toolu_regen_{nanos:x}")
}

/// 语义请求 → Anthropic 请求体。
///
/// 源分支策略(镜像旧实现):openai 源重放 opaque(payload 拷贝语义);
/// responses/gemini 源为白名单体(responses 源补 tool_choice 映射,
/// 修复旧实现的静默丢弃)。
fn encode_anthropic_request(request: &SemanticRequest, report: &mut ConversionReport) -> Value {
    let mut body = Map::new();
    body.insert("model".to_owned(), json!(request.model));
    // system:全部 System 消息归并(单条时与旧实现原值一致)。Developer
    // 一并升格:anthropic 无 developer 角色,落 user 会把系统级指令降级
// 为用户轮次(主流实现均映射 system)。
    let system_messages: Vec<&Message> = request
        .messages
        .iter()
        .filter(|m| matches!(m.role, Role::System | Role::Developer))
        .collect();
    match system_messages.as_slice() {
        [] => {}
        [single] => {
            body.insert("system".to_owned(), encode_system_value(single));
        }
        many => {
            // Anthropic wire:system 数组的元素必须是 block 对象;此前复用
            // encode_system_value(单 Text 块返回裸字符串)产出 ["str",…],
            // 上游反序列化直接 422(DeepSeek anthropic 端点实测
            // "expected internally tagged enum TextBlockParam",2026-10-14)。
            let joined: Vec<Value> = many
                .iter()
                .flat_map(|m| m.content.iter())
                .map(encode_system_part)
                .collect();
            body.insert("system".to_owned(), Value::Array(joined));
            report.push(
                DiagnosticCode::MergedContent,
                "messages[system]",
                "多条 system 归并为 system 数组(旧实现仅取首条,其余静默丢弃)".to_owned(),
            );
        }
    }
    let mut messages: Vec<Value> = Vec::new();
    for message in &request.messages {
        if matches!(message.role, Role::System | Role::Developer) {
            continue;
        }
        messages.push(encode_anthropic_message(message, report));
    }
    body.insert("messages".to_owned(), Value::Array(messages));
    let max_tokens = request.generation.max_tokens.unwrap_or(4096);
    body.insert("max_tokens".to_owned(), json!(max_tokens));
    let thinking_enabled = encode_anthropic_thinking(request, max_tokens, &mut body, report);
    // Anthropic 约束:thinking 开启时 temperature 须为 1 且不可自定义
    // top_p——省略(上游缺省即 1)并记诊断,避免 openai 系客户端
// effort+temperature 同发触发 400(回退语义同源)。
    if thinking_enabled {
        for (key, present) in [
            ("temperature", request.generation.temperature.is_some()),
            ("top_p", request.generation.top_p.is_some()),
        ] {
            if present {
                report.push(
                    DiagnosticCode::DroppedField,
                    key,
                    "Anthropic thinking 启用时不接受自定义采样参数,已省略(等效 temperature=1)"
                        .to_owned(),
                );
            }
        }
    } else {
        if let Some(temperature) = request.generation.temperature {
            body.insert("temperature".to_owned(), json!(temperature));
        }
        if let Some(top_p) = request.generation.top_p {
            body.insert("top_p".to_owned(), json!(top_p));
        }
    }
    if !request.generation.stop_sequences.is_empty() {
        body.insert(
            "stop_sequences".to_owned(),
            json!(request.generation.stop_sequences),
        );
    }
    match request.stream {
        StreamPreference::Explicit(true) => {
            body.insert("stream".to_owned(), json!(true));
        }
        // 旧 responses→anthropic 仅在 stream 真值时写入;家族透传由
        // opaque 重放携带(显式 false 亦保留,供家族源)。
        StreamPreference::Explicit(false) if request.source == SourceProtocol::Anthropic => {
            body.insert("stream".to_owned(), json!(false));
        }
        _ => {}
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
                            "name": tool_names::sanitize(&tool.name, Profile::Anthropic),
                            "description": tool.description.clone().unwrap_or_default(),
                            "input_schema": if tool.parameters_schema.is_null() { json!({}) } else { tool.parameters_schema.clone() },
                        })
                    })
                    .collect(),
            ),
        );
    }
    if let Some(choice) = &request.tool_choice {
        let mut encoded = encode_tool_choice(choice);
        // parallel_tool_calls 的 anthropic 表达:tool_choice 内开关。
        if request.generation.parallel_tool_calls == Some(false)
            && let Some(map) = encoded.as_object_mut()
        {
            map.insert("disable_parallel_tool_use".to_owned(), json!(true));
        }
        body.insert("tool_choice".to_owned(), encoded);
    } else if request.generation.parallel_tool_calls == Some(false) {
        body.insert(
            "tool_choice".to_owned(),
            json!({"type": "auto", "disable_parallel_tool_use": true}),
        );
    }
    // 家族源(opaque 重放,镜像 payload 拷贝);白名单源记丢弃诊断。
    if request.source == SourceProtocol::OpenAi {
        for field in &request.opaque {
            if NON_ANTHROPIC_REPLAY_KEYS.contains(&field.path.as_str()) {
                report.push(
                    DiagnosticCode::DroppedField,
                    field.path.clone(),
                    "OpenAI 方言键不重放进 Anthropic 体(thinking/reasoning 已按语义编码,其余为目标线格式不存在的键)".to_owned(),
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
                "白名单体转换不重放顶层键(镜像旧 responses→anthropic)".to_owned(),
            );
        }
    }
    Value::Object(body)
}

/// OpenAI 家族方言键:不重放进 Anthropic 出站体的顶层键。
///
/// `thinking`/`reasoning_effort`/`max_completion_tokens` 已由语义层消费
/// (chat decode_generation),其余为目标线格式不存在的 chat 方言键
/// (采样/日志/服务端策略)。旧实现 payload 拷贝语义全量重放,残缺
/// thinking(如 `{"type":"enabled"}` 无 budget)会被严格上游 400
/// (2026-10-14 everyapi 实测 "enabled.budget_tokens: Field required")。
const NON_ANTHROPIC_REPLAY_KEYS: &[&str] = &[
    "thinking",
    "reasoning",
    "reasoning_effort",
    "enable_thinking",
    "stream_options",
    "max_completion_tokens",
    "n",
    "logprobs",
    "top_logprobs",
    "logit_bias",
    "seed",
    "service_tier",
    "response_format",
    "frequency_penalty",
    "presence_penalty",
    // openai 终端用户标识;anthropic 对应概念是 metadata.user_id,直排
    // 顶层 `user` 键为目标线格式不存在的字段。
    "user",
];

/// 语义思考配置 → Anthropic `thinking` 键。
///
/// 旧实现无此编码:跨族思考意图全靠 opaque 重放的客户端原值。优先级:
/// 显式 `thinking.budget_tokens` > `reasoning_effort` 档位推导(档位阈值
/// 与 `gateway::budget_to_effort` 互逆);`Off` 显式落 disabled 钉住关闭;
/// vendor 私有 effort 值无法推导则省略并记诊断。Anthropic 约束
/// `1024 <= budget_tokens < max_tokens`,越界按边界收敛并记 AdjustedValue。
fn encode_anthropic_thinking(
    request: &SemanticRequest,
    max_tokens: i64,
    body: &mut Map<String, Value>,
    report: &mut ConversionReport,
) -> bool {
    use crate::semantic::model::ReasoningEffort;
    let explicit_off = matches!(
        request.generation.reasoning_effort,
        Some(ReasoningEffort::Off)
    );
    let requested = request
        .generation
        .thinking
        .as_ref()
        .and_then(|thinking| thinking.budget_tokens)
        .or_else(|| {
            request
                .generation
                .reasoning_effort
                .as_ref()
                .and_then(ReasoningEffort::budget_hint)
        });
    match requested {
        Some(target) => {
            let ceiling = max_tokens - 1;
            if ceiling < 1_024 {
                report.push(
                    DiagnosticCode::DroppedField,
                    "thinking".to_owned(),
                    format!("max_tokens={max_tokens} 过小,无法满足 budget_tokens>=1024,思考已省略"),
                );
                return false;
            }
            let budget = target.min(ceiling).max(1_024);
            if budget != target {
                report.push(
                    DiagnosticCode::AdjustedValue,
                    "thinking.budget_tokens".to_owned(),
                    format!(
                        "{target} 按 Anthropic 约束收敛为 {budget}(需 1024<=budget<max_tokens={max_tokens})"
                    ),
                );
            }
            body.insert(
                "thinking".to_owned(),
                json!({"type": "enabled", "budget_tokens": budget}),
            );
            true
        }
        // 显式 Off 钉住关闭;无思考意图不落键(Anthropic 缺省即关)。
        None if explicit_off => {
            body.insert("thinking".to_owned(), json!({"type": "disabled"}));
            false
        }
        None => {
            if let Some(ReasoningEffort::Opaque(raw)) = &request.generation.reasoning_effort {
                report.push(
                    DiagnosticCode::UnmappedEnum,
                    "reasoning_effort".to_owned(),
                    format!("vendor 私有 effort 值 {raw:?} 无法推导 budget_tokens,thinking 省略"),
                );
            }
            false
        }
    }
}

fn encode_system_value(message: &Message) -> Value {
    // 单 Text 块 → 字符串(镜像旧 system 原值形态);其余编码为数组。
    if message.content.len() == 1
        && let ContentPart::Text { text } = &message.content[0]
    {
        return json!(text);
    }
    Value::Array(message.content.iter().map(encode_system_part).collect())
}

/// system 数组的单元素编码:恒为 block 对象(裸字符串只允许出现在
/// 顶层单值形态,数组内不合法,见 encode_anthropic_request 多条分支)。
fn encode_system_part(part: &ContentPart) -> Value {
    match part {
        ContentPart::Text { text } => json!({"type": "text", "text": text}),
        ContentPart::Opaque { value, .. } => value.clone(),
        other => {
            json!({"type": "text", "text": crate::semantic::parts::joined_text(std::slice::from_ref(other))})
        }
    }
}

fn encode_tool_choice(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => json!({"type": "auto"}),
        ToolChoice::Required => json!({"type": "any"}),
        // 意图变更:None 编码为 {"type":"none"}(旧实现 openai 源
        // "none" 字符串原样透传)。
        ToolChoice::None => json!({"type": "none"}),
        ToolChoice::Named { name } => json!({"type": "tool", "name": name}),
        ToolChoice::Opaque(value) => value.clone(),
    }
}

fn encode_anthropic_message(message: &Message, report: &mut ConversionReport) -> Value {
    if message.role == Role::Tool {
        let result = message.tool_result.as_ref();
        return json!({
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": match result
                    .and_then(|r| r.call_ref.as_ref())
                    .map(|id| id.local.as_str())
                {
                    Some(id) if !id.is_empty() => json!(id),
                    _ => {
                        // anthropic 要求非空 string;chat 客户端缺
                        // tool_call_id 时生成占位(仅保 wire 合法)。
                        report.push(
                            DiagnosticCode::RegeneratedId,
                            "messages[tool_result]",
                            "tool_call_id 缺失,生成占位 id(anthropic 要求非空字符串)".to_owned(),
                        );
                        json!(placeholder_id())
                    }
                },
                "content": result.map(|r| r.content.clone()).unwrap_or(json!("")),
            }],
        });
    }
    let mut blocks: Vec<Value> = Vec::new();
    if let Some(reasoning) = &message.reasoning
        && !reasoning.text.is_empty()
    {
        // Claude 校验历史 thinking 块的 signature;openai 系
// reasoning_content 无签名,带上即多轮工具循环 400(同类止血)→ 无签名块省略并记诊断。
//
        if reasoning.signature.is_none() {
            report.push(
                DiagnosticCode::DowngradedContent,
                "messages[thinking]",
                "历史 thinking 块无 signature,为免上游拒绝已省略(openai 系 reasoning_content 无签名)"
                    .to_owned(),
            );
        } else {
            let mut block = Map::new();
            block.insert("type".to_owned(), json!("thinking"));
            block.insert("thinking".to_owned(), json!(reasoning.text));
            block.insert(
                "signature".to_owned(),
                json!(reasoning.signature.clone().unwrap_or_default()),
            );
            blocks.push(Value::Object(block));
        }
    }
    for part in &message.content {
        match part {
            ContentPart::Text { text } if !text.is_empty() => {
                blocks.push(json!({"type": "text", "text": text}));
            }
            ContentPart::Text { .. } => {}
            ContentPart::ImageRef { url } => {
                blocks.push(json!({
                    "type": "image",
                    "source": {"type": "url", "url": url},
                }));
            }
            ContentPart::ImageInline {
                media_type,
                data_base64,
            } => {
                blocks.push(json!({
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": media_type.clone().unwrap_or_else(|| "image/png".to_owned()),
                        "data": data_base64,
                    },
                }));
            }
            ContentPart::Opaque { value, .. } => blocks.push(value.clone()),
            ContentPart::Refusal { text } => {
                blocks.push(json!({"type": "text", "text": text}));
            }
            ContentPart::Audio { .. } | ContentPart::File { .. } => {
                report.push(
                    DiagnosticCode::DowngradedContent,
                    "content",
                    "anthropic 目标不表达 audio/file 块,原值保留".to_owned(),
                );
                blocks.push(match part {
                    ContentPart::Audio { media_type, data_base64 } => json!({
                        "type": "audio", "media_type": media_type, "data_base64": data_base64,
                    }),
                    ContentPart::File { file_id, filename, data_base64 } => json!({
                        "type": "file", "file_id": file_id, "filename": filename, "data_base64": data_base64,
                    }),
                    _ => unreachable!(),
                });
            }
        }
    }
    for call in &message.tool_calls {
        let (input, downgraded) = call.arguments.as_object();
        if downgraded {
            report.push(
                DiagnosticCode::DowngradedContent,
                "tool_calls",
                format!("工具 {} 参数非法 JSON → {{\"raw\": s}} 兜底", call.name),
            );
        }
        // anthropic 要求 id/name 为非空 string;客户端历史缺失时生成占位
        // (仅保 wire 合法)。名字按名册清洗,MCP 名含 `.`/`:` 直发即 400。
        let call_id = if call.id.local.is_empty() {
            report.push(
                DiagnosticCode::RegeneratedId,
                "tool_calls",
                "tool_use id 缺失,生成占位 id".to_owned(),
            );
            placeholder_id()
        } else {
            call.id.local.clone()
        };
        let call_name = if call.name.is_empty() {
            report.push(
                DiagnosticCode::RegeneratedId,
                "tool_calls",
                "tool_use name 缺失,生成占位名".to_owned(),
            );
            tool_names::sanitize("tool_regen", Profile::Anthropic)
        } else {
            tool_names::sanitize(&call.name, Profile::Anthropic)
        };
        blocks.push(json!({
            "type": "tool_use",
            "id": call_id,
            "name": call_name,
            "input": input,
        }));
    }
    let role = match message.role {
        Role::Assistant => "assistant",
        Role::Developer => "user",
        _ => "user",
    };
    json!({
        "role": message
            .extensions
            .get("anthropic", "raw_role")
            .and_then(Value::as_str)
            .unwrap_or(role),
        "content": if blocks.is_empty() { json!("") } else { Value::Array(blocks) },
    })
}

/// Anthropic Message JSON → 语义响应。
pub(super) fn decode_anthropic_message(
    raw: &Value,
    report: &mut ConversionReport,
) -> SemanticResponse {
    let mut response = SemanticResponse::new();
    response.id = opt_str(raw.get("id"));
    response.model = opt_str(raw.get("model"));
    let mut candidate = crate::semantic::model::ResponseCandidate::new(0);
    if let Some(Value::Array(blocks)) = raw.get("content") {
        let mut message = Message::new(Role::Assistant);
        for block in blocks {
            decode_block(block, &mut message, report, "content");
        }
        candidate.content = message.content;
        candidate.reasoning = message.reasoning;
        candidate.tool_calls = message.tool_calls;
    }
    candidate.finish = Some(finish::from_anthropic(
        raw.get("stop_reason").and_then(Value::as_str),
        report,
        "stop_reason",
    ));
    if candidate.finish.as_ref() == Some(&FinishReason::Refusal) {
        let text = candidate
            .content
            .iter()
            .filter_map(|part| match part {
                ContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();
        response.refusal = Some(text);
    }
    response.finish = candidate.finish.clone();
    if let Some(usage) = raw.get("usage") {
        let mut parsed = Usage::reported();
        parsed.input = usage
            .get("input_tokens")
            .and_then(py_int)
            .filter(|v| *v >= 0);
        parsed.output = usage
            .get("output_tokens")
            .and_then(py_int)
            .filter(|v| *v >= 0);
        // 负值视为未报告(个别 anthropic 兼容端点以负数表达 cache 折扣,
        // 透传会污染命中率/成本算术)。
        parsed.cache_read =
            first_non_none(usage, &["cache_read_input_tokens", "cache_read_tokens"])
                .filter(|v| *v >= 0);
        parsed.cache_write = first_non_none(
            usage,
            &["cache_creation_input_tokens", "cache_write_tokens"],
        )
        .filter(|v| *v >= 0);
        response.usage = Some(parsed);
    }
    response.candidates.push(candidate);
    response
}

fn first_non_none(usage: &Value, keys: &[&str]) -> Option<i64> {
    keys.iter().find_map(|key| usage.get(*key).and_then(py_int))
}

#[cfg(test)]
mod prefill_tests {
    use super::*;

    /// 阶段五 C-3:末条不是 assistant → 追加一条 assistant prefill 消息。
    #[test]
    fn prefill_appends_assistant_message_when_last_is_user() {
        let payload = json!({
            "model": "m",
            "messages": [
                {"role": "user", "content": "hi"}
            ],
        });
        let next = append_assistant_prefill(&payload, "半句回复").expect("应可拼装");
        let messages = next["messages"].as_array().expect("messages 数组");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1]["role"], json!("assistant"));
        assert_eq!(messages[1]["content"][0]["text"], json!("半句回复"));
        // 原请求体不被就地修改(每次尝试都基于原始 payload 重拼)。
        assert_eq!(
            payload["messages"].as_array().expect("messages 数组").len(),
            1
        );
    }

    /// 阶段五 C-3:末条已是 assistant → 把 prefill 并入该条,不新增消息。
    #[test]
    fn prefill_merges_into_trailing_assistant_message() {
        let payload = json!({
            "model": "m",
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": [{"type": "text", "text": "前半"}]},
            ],
        });
        let next = append_assistant_prefill(&payload, "后半").expect("应可拼装");
        let messages = next["messages"].as_array().expect("messages 数组");
        assert_eq!(messages.len(), 2, "不新增消息");
        assert_eq!(messages[1]["content"][1]["text"], json!("后半"));
    }

    /// 阶段五 C-3:assistant 消息的 content 是字符串形态 → 直接拼接。
    #[test]
    fn prefill_merges_into_string_content() {
        let payload = json!({
            "messages": [{"role": "assistant", "content": "前半"}],
        });
        let next = append_assistant_prefill(&payload, "后半").expect("应可拼装");
        assert_eq!(next["messages"][0]["content"], json!("前半后半"));
    }

    /// 阶段五 C-3:形态不合法 → 放弃续写(返回 None),不硬拼。
    #[test]
    fn prefill_refuses_malformed_payloads() {
        assert!(
            append_assistant_prefill(&json!({}), "x").is_none(),
            "无 messages"
        );
        assert!(
            append_assistant_prefill(&json!({"messages": "not-an-array"}), "x").is_none(),
            "messages 非数组"
        );
        assert!(
            append_assistant_prefill(
                &json!({"messages": [{"role": "assistant", "content": 7}]}),
                "x"
            )
            .is_none(),
            "末条 assistant 的 content 形态未知"
        );
        assert!(
            append_assistant_prefill(&json!({"messages": []}), "").is_none(),
            "空 prefill 不续写"
        );
    }

    /// 阶段五 C-3:thinking 与 prefill 互斥 → 带 thinking 的请求不续写。
    #[test]
    fn thinking_requests_are_detected() {
        assert!(has_thinking(&json!({
            "thinking": {"type": "enabled", "budget_tokens": 1024}
        })));
        assert!(!has_thinking(&json!({"model": "m"})));
        assert!(!has_thinking(&json!({"thinking": null})));
    }
}

#[cfg(test)]
mod cache_usage_tests {
    use super::*;
    use crate::semantic::model::{ResponseCandidate, SemanticResponse};

    /// 2026-10-13:非流式 anthropic 出站信封必须透出缓存计数,否则
    /// 下游 normalize_usage 的「首个非零」方言链读不到,调用日志的
    /// cache_read/cache_write 恒为 0。
    #[test]
    fn non_stream_envelope_carries_cache_counters() {
        let mut response = SemanticResponse::new();
        response.id = Some("msg_1".to_owned());
        response.model = Some("m".to_owned());
        response.candidates.push(ResponseCandidate::new(0));
        let mut usage = Usage::reported();
        usage.input = Some(100);
        usage.output = Some(5);
        usage.cache_read = Some(64);
        usage.cache_write = Some(16);
        response.usage = Some(usage);

        let mut report = ConversionReport::semantic();
        let envelope = encode_message_envelope(&response, &mut report);
        assert_eq!(envelope["usage"]["cache_read_input_tokens"], json!(64));
        assert_eq!(envelope["usage"]["cache_creation_input_tokens"], json!(16));
        // 黄金基线形状:零值不写键。
        let mut plain = Usage::reported();
        plain.input = Some(3);
        plain.output = Some(5);
        let mut response = SemanticResponse::new();
        response.candidates.push(ResponseCandidate::new(0));
        response.usage = Some(plain);
        let envelope = encode_message_envelope(&response, &mut report);
        assert!(envelope["usage"].get("cache_read_input_tokens").is_none());
        assert!(
            envelope["usage"]
                .get("cache_creation_input_tokens")
                .is_none()
        );
    }
}

/// 思考语义编码 + 方言键重放剔除(2026-10-14 everyapi 400 案回归)。
#[cfg(test)]
mod thinking_encode_tests {
    use super::*;
    use crate::semantic::adapter::InboundAdapter;
    use crate::semantic::openai_chat;

    fn encode_openai(payload: Value) -> (Value, ConversionReport) {
        let decoded = openai_chat::Inbound.decode_request(&payload).unwrap();
        let mut report = decoded.report;
        let body = encode_anthropic_request(&decoded.value, &mut report);
        (body, report)
    }

    fn base_payload() -> Value {
        json!({
            "model": "claude-haiku-5-5",
            "messages": [{"role": "user", "content": "hi"}],
        })
    }

    #[test]
    fn everyapi_regression_effort_derives_budget_and_dialect_keys_dropped() {
        // 2026-10-14 实测(请求 b93028c3):ZCode openai 源带跨方言超集,
        // 残缺 thinking 原样重放 → 严格上游 400
        // "enabled.budget_tokens: Field required"。
        let mut payload = base_payload();
        payload["reasoning_effort"] = json!("high");
        payload["thinking"] = json!({"type": "enabled"});
        payload["enable_thinking"] = json!(true);
        payload["reasoning"] = json!({"effort": "high"});
        payload["stream_options"] = json!({"include_usage": true});
        payload["max_completion_tokens"] = json!(32000);
        let (body, report) = encode_openai(payload);
        // effort high → 档位 budget 30000,合法形状。
        assert_eq!(
            body["thinking"],
            json!({"type": "enabled", "budget_tokens": 30000}),
            "{body}"
        );
        // 方言键一律不进体。
        for key in [
            "reasoning_effort",
            "enable_thinking",
            "reasoning",
            "stream_options",
            "max_completion_tokens",
        ] {
            assert!(
                body.get(key).is_none(),
                "{key} 不应重放进 Anthropic 体: {body}"
            );
        }
        // max_completion_tokens 被语义消费(旧实现回落 4096 默认)。
        assert_eq!(body["max_tokens"], json!(32000));
        // 丢弃可观测(转换报告)。
        let dropped: Vec<&str> = report
            .diagnostics
            .iter()
            .filter(|d| d.code == DiagnosticCode::DroppedField)
            .map(|d| d.path.as_str())
            .collect();
        for key in ["thinking", "enable_thinking", "stream_options", "reasoning"] {
            assert!(dropped.contains(&key), "{key} 应有丢弃诊断: {dropped:?}");
        }
    }

    #[test]
    fn explicit_budget_wins_over_effort_and_clamps_to_max_tokens() {
        let mut payload = base_payload();
        payload["max_tokens"] = json!(4096);
        payload["thinking"] = json!({"type": "enabled", "budget_tokens": 20000});
        let (body, report) = encode_openai(payload);
        assert_eq!(body["thinking"]["budget_tokens"], json!(4095), "{body}");
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.code == DiagnosticCode::AdjustedValue)
        );
    }

    #[test]
    fn tiny_max_tokens_drops_thinking_with_diagnostic() {
        let mut payload = base_payload();
        payload["max_tokens"] = json!(512);
        payload["reasoning_effort"] = json!("high");
        let (body, report) = encode_openai(payload);
        assert!(body.get("thinking").is_none(), "{body}");
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.code == DiagnosticCode::DroppedField && d.path == "thinking")
        );
    }

    #[test]
    fn off_pins_disabled_and_absent_intent_omits_key() {
        let mut payload = base_payload();
        payload["reasoning_effort"] = json!("none");
        let (body, _) = encode_openai(payload);
        assert_eq!(body["thinking"], json!({"type": "disabled"}));
        // 无任何思考意图:不落键(Anthropic 缺省即关)。
        let (body, _) = encode_openai(base_payload());
        assert!(body.get("thinking").is_none(), "{body}");
    }

    #[test]
    fn vendor_xhigh_maps_to_max_tier_unknown_opaque_dropped() {
        let mut payload = base_payload();
        payload["max_tokens"] = json!(64000);
        payload["reasoning_effort"] = json!("xhigh");
        let (body, _) = encode_openai(payload);
        assert_eq!(body["thinking"]["budget_tokens"], json!(60000), "{body}");
        let mut payload = base_payload();
        payload["reasoning_effort"] = json!("yolo");
        let (body, report) = encode_openai(payload);
        assert!(body.get("thinking").is_none(), "{body}");
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.code == DiagnosticCode::UnmappedEnum)
        );
    }
}
