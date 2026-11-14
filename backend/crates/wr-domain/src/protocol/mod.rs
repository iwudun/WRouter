//! 协议面纯逻辑(镜像 Python `application/gateway/protocol_convert.py`、
//! `errors.py`、`sse_encoding.py`、`domain/routing/protocol_endpoints.py`、
//! `domain/protocol/*`)。
//!
//! 职责(阶段 E 起,跨协议转换全部收敛到 [`crate::semantic`],方案
//! 20261011_01 E2;本模块不再承载两两转换;20261013_01 B 组把唯一消费
//! 者在 semantic 的 helper 归位彼处):
//! - 「直连转发」路径:上游端点构造、上游请求头、上游错误 →
//!   客户端协议错误信封、Claude Desktop 模型名解码;
//! - [`client_fingerprint`] / [`client_identity`]:出站指纹层(我们以什么身份
//!   出站)与入站调用方归一(谁在打网关)—— 两者都读 UA,但方向相反、互不覆盖;
//! - [`upstream_payload`] / [`decode_response`] / [`endpoint`] 分派:
//!   语义管线接线(门禁 + 原生透传格 + 转换报告);
//! - [`convert`] — vendor 请求参数修正(`vendor_compat.py` 镜像,
//!   传输 hook,非协议转换);
//! - [`gemini`] — Gemini interactions 协议专项 helper
//!   (`interaction_to_gemini` / `build_gemini_contents`,passthrough
//!   分派调用;schema/usage 归一已归位 `semantic::gemini`)。
//!
//! 保留在 protocol/ 的判定依据(20261013_01 B3,固化分工):本文件是
//! wire 机制本体(端点/请求头/错误信封/分派);[`convert`] 由
//! 被 semantic(anthropic/parts)与 wr-server(forward/savings)跨层
//! 共享 —— 三者都不从属某一协议的语义适配器,挪进任一侧都会造成反向
//! 依赖。
//!
//! Python `domain/protocol/usage.py` / `value_objects.py` /
//! `entities.py` 对应的 Rust 落点分别是 [`crate::usage`]、
//! [`crate::provider::client_protocol`]、[`crate::semantic::model`]
//! (R1/R2 既定布局,镜像时效性声明下保持)。
//!
//! C7 注记:tool_call 流式 args 分片重组的保真逻辑在
//! [`crate::semantic::client_stream`]。

pub mod client_fingerprint;
pub mod client_identity;
pub mod contract;
pub mod convert;
pub mod correction;
pub mod gemini;

use serde_json::{Map, Value, json};

use crate::provider::{Provider, ProviderProtocol, UrlMode, client_protocol};
use crate::semantic::SourceProtocol;
use crate::semantic::diagnostic::ConversionReport;

// ---------------------------------------------------------------------------
// Claude Desktop 模型名解码(镜像 sse_encoding.decode_claude_app_model)
// ---------------------------------------------------------------------------

/// Claude Desktop 对非原生模型的十六进制编码:
/// `anthropic/claude-<命名空间>-h<hex>`(命名空间不限,历史值有 `wr`/`ccr`),
/// 解码还原原始模型名并剥掉 `Provider/` 前缀;其余名字原样返回。
pub fn decode_claude_app_model(model: &str) -> String {
    let re = regex::Regex::new(r"(?i)^anthropic/claude-[a-z0-9]+-h([0-9a-f]+)$").unwrap();
    let Some(captures) = re.captures(model) else {
        return model.to_owned();
    };
    let encoded = &captures[1];
    // 奇数长度非法,视为未编码。
    if encoded.len() % 2 != 0 {
        return model.to_owned();
    }
    let Ok(bytes) = hex_decode(encoded) else {
        return model.to_owned();
    };
    let Ok(decoded) = String::from_utf8(bytes) else {
        return model.to_owned();
    };
    let decoded = decoded.trim();
    if decoded.is_empty() {
        return model.to_owned();
    }
    match decoded.split_once('/') {
        Some((_, rest)) => rest.to_owned(),
        None => decoded.to_owned(),
    }
}

fn hex_decode(text: &str) -> Result<Vec<u8>, ()> {
    if !text.len().is_multiple_of(2) {
        return Err(());
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).map_err(|_| ()))
        .collect()
}

// ---------------------------------------------------------------------------
// 上游端点构造(镜像 protocol_convert.endpoint,R3 全量分支)
// ---------------------------------------------------------------------------

fn ends_with_ignore_case(base: &str, suffix: &str) -> bool {
    base.len() >= suffix.len() && base[base.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
}

/// base 末段是否为版本段(`v1`/`v3`/`v4`/`v1beta` 这类):数字开头,
/// 数字后可跟小写字母;`values`/`versions` 等普通词不匹配。
fn ends_with_version_segment(base: &str) -> bool {
    let last = base.rsplit('/').next().unwrap_or("");
    let Some(rest) = last.strip_prefix('v') else {
        return false;
    };
    let mut saw_digit = false;
    let mut saw_alpha = false;
    for c in rest.chars() {
        if c.is_ascii_digit() && !saw_alpha {
            saw_digit = true;
        } else if c.is_ascii_lowercase() {
            saw_alpha = true;
        } else {
            return false;
        }
    }
    saw_digit
}

fn append_endpoint(base: &str, suffix: &str) -> String {
    if ends_with_ignore_case(base, suffix) {
        base.to_owned()
    } else if ends_with_ignore_case(base, "/v1") {
        format!("{base}{suffix}")
    } else if ends_with_version_segment(base) {
        // base 已带版本段(火山方舟 `/api/coding/v3`、智谱 `/api/paas/v4`)
        // → 直接拼端点;再插 `/v1` 会打出 `/v3/v1/...` 的 404(2026-09-13)。
        format!("{base}{suffix}")
    } else {
        format!("{base}/v1{suffix}")
    }
}

/// 计算上游请求 URL(`query` 无指纹语境的退化形式)。
///
/// 端点由**上游形态**决定(provider.protocol + style),不按客户端协议名
/// 逐一枚举分支 —— 任何对话入口(chat / responses / anthropic / gemini /
/// gemini_stream)打到只提供 Responses 的 provider 都必须走 `/responses`,
/// 否则请求体(已按上游语义转换)与端点不匹配,真实上游直接 400。此前
/// 只识别 openai/gemini 两种入口名,anthropic 与 gemini_stream 会误落
/// `/chat/completions`(2026-10-13 实测)。`stream` 仅影响 gemini 原生
/// 形态的 URL 动作。
pub fn endpoint(protocol: &str, provider: &Provider, model: &str, stream: bool) -> String {
    endpoint_with_query(protocol, provider, model, stream, None)
}

/// [`endpoint`] 的指纹变体(方案 20261016_03 §3.4):`query` 为指纹层
/// 静态/透传 query(passthrough allowlist 或 preset 端点键),以 `?`/`&`
/// 追加到端点之后;`None` 时与 [`endpoint`] 完全一致。full URL 模式维持
/// 短路原样 —— 用户填什么打什么,指纹 query 属 base 模式语义。
pub fn endpoint_with_query(
    protocol: &str,
    provider: &Provider,
    model: &str,
    stream: bool,
    query: Option<&str>,
) -> String {
    // 20261014_01:完整 URL 模式 —— 用户填什么打什么,不做任何路径运算
    // (网关转发与实探共用本函数,一处短路全链生效)。Gemini 家族端点必须
    // 拼模型名与 action(`/models/{model}:generateContent`),原样 URL 无法
    // 表达;手改/导入配置可能带出 full+gemini 组合,宽容回落 base 模式而
    // 不是拒绝请求(前端已对 Gemini 格式隐藏该开关,这里是后端保险)。
    if provider.url_mode == UrlMode::Full && provider.protocol != ProviderProtocol::Gemini {
        return provider.base_url.trim().to_owned();
    }
    let base = provider.base_url.trim_end_matches('/');
    let target = provider.protocol;
    let native_responses = provider.serves_openai_responses_natively();
    // interactions 客户端:专用 `/interactions` 端点(仅 gemini 上游可达,
    // 非 gemini 已被组合门禁拒绝)。
    let mut url = if protocol == client_protocol::GEMINI_INTERACTIONS {
        let mut path = format!("{base}/interactions");
        if let Some(query) = gemini_query_key(provider) {
            path.push('?');
            path.push_str(&query);
        }
        path
    } else {
        match target {
            ProviderProtocol::Gemini => {
                let action = if stream || protocol == client_protocol::GEMINI_STREAM {
                    "streamGenerateContent"
                } else {
                    "generateContent"
                };
                let model_name = uri_encode(if model.is_empty() { "unknown" } else { model });
                let path = format!("{base}/models/{model_name}:{action}");
                if !path.contains('?')
                    && let Some(query) = gemini_query_key(provider)
                {
                    return format!("{path}?{query}");
                }
                path
            }
            ProviderProtocol::Anthropic => append_endpoint(base, "/messages"),
            ProviderProtocol::OpenAi => {
                if protocol == client_protocol::OPENAI_EMBEDDINGS {
                    return append_endpoint(base, "/embeddings");
                }
                if native_responses {
                    // A2/A3 + gap5:provider 原生只提供 Responses 端点 —— 所有
                    // 对话入口经语义转换后都打到 `/responses`。
                    return append_endpoint(base, "/responses");
                }
                append_endpoint(base, "/chat/completions")
            }
        }
    };
    if let Some(query) = query.filter(|text| !text.is_empty()) {
        let separator = if url.contains('?') { '&' } else { '?' };
        url.push(separator);
        url.push_str(query);
    }
    url
}

fn gemini_query_key(provider: &Provider) -> Option<String> {
    let key_mode_query = provider
        .headers
        .get("x-wrouter-gemini-key-mode")
        .and_then(Value::as_str)
        .map(|v| v.eq_ignore_ascii_case("query"))
        .unwrap_or(false);
    if !key_mode_query || provider.api_key.is_empty() {
        return None;
    }
    Some(format!("key={}", uri_encode(&provider.api_key)))
}

/// `urllib.parse.quote(value, safe="")` 的子集:保留 RFC 3986 非保留
/// 字符,其余按 UTF-8 百分号编码(大写十六进制,与 Python 一致)。
pub(crate) fn uri_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.as_bytes() {
        let c = *byte;
        let unreserved = c.is_ascii_alphanumeric() || matches!(c, b'-' | b'.' | b'_' | b'~');
        if unreserved {
            out.push(c as char);
        } else {
            out.push_str(&format!("%{c:02X}"));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 上游请求头(镜像 protocol_convert.upstream_headers)
// ---------------------------------------------------------------------------

/// 构造上游请求头(`inbound` 无指纹语境的退化形式)。
pub fn upstream_headers(provider: &Provider) -> Map<String, Value> {
    upstream_headers_with_fingerprint(provider, None)
}

/// [`upstream_headers`] 的指纹变体(方案 20261016_03 §3.2,三级优先级
/// 保持单点构造):
///
/// ```text
/// provider.headers 显式配置  >  客户端指纹(passthrough / preset)  >  自动注入(认证头 / anthropic-version)
/// ```
///
/// `inbound` 为路由改写前的原始入站头快照(小写名),passthrough 模式
/// 的指纹源;preset 模式查内置指纹表([`client_fingerprint::builtin_presets`])。
/// 指纹层以 `or_insert` 并入 —— 不覆盖 provider.headers,又先于认证注入
/// 落位。`inbound: None`(探测面)时 passthrough 自然降级为 off 行为。
/// 无指纹配置的 provider 与 [`upstream_headers`] 逐字节一致(默认 off,
/// golden 零回归)。
pub fn upstream_headers_with_fingerprint(
    provider: &Provider,
    inbound: Option<&[(String, String)]>,
) -> Map<String, Value> {
    let mut headers: Map<String, Value> = Map::new();
    headers.insert(
        "content-type".to_owned(),
        Value::String("application/json".to_owned()),
    );
    for (key, value) in &provider.headers {
        let text = match value.as_str() {
            Some(s) => s.to_owned(),
            None => crate::json_py::py_json_dumps(value),
        };
        headers.insert(key.to_ascii_lowercase(), Value::String(text));
    }
    // 指纹层(client_fingerprint::resolve 已做域过滤 + 红线剥离)。
    for (key, value) in client_fingerprint::resolve_fingerprint_headers(provider, inbound) {
        headers.entry(key).or_insert_with(|| Value::String(value));
    }
    if !provider.api_key.is_empty() {
        match provider.protocol {
            ProviderProtocol::Gemini => {
                let key_mode_query = provider
                    .headers
                    .get("x-wrouter-gemini-key-mode")
                    .and_then(Value::as_str)
                    .map(|v| v.eq_ignore_ascii_case("query"))
                    .unwrap_or(false);
                if !key_mode_query {
                    headers
                        .entry("x-goog-api-key".to_owned())
                        .or_insert_with(|| Value::String(provider.api_key.clone()));
                }
            }
            ProviderProtocol::Anthropic => {
                headers
                    .entry("x-api-key".to_owned())
                    .or_insert_with(|| Value::String(provider.api_key.clone()));
                headers
                    .entry("anthropic-version".to_owned())
                    .or_insert_with(|| Value::String("2023-06-01".to_owned()));
            }
            ProviderProtocol::OpenAi => {
                headers
                    .entry("authorization".to_owned())
                    .or_insert_with(|| Value::String(format!("Bearer {}", provider.api_key)));
            }
        }
    }
    headers
}

// ---------------------------------------------------------------------------
// 上游请求 payload(B6 语义管线接线;原两两转换见 semantic::)
// ---------------------------------------------------------------------------

/// 请求转换失败(矩阵 unsupported 格等):携带语义错误,经
/// [`ConversionFailure::envelope`] 升格为客户端错误信封。
#[derive(Debug, Clone)]
pub struct ConversionFailure {
    pub error: crate::semantic::SemanticError,
}

impl ConversionFailure {
    pub fn message(&self) -> &str {
        &self.error.message
    }

    /// 客户端协议错误信封 `(http_status, body)`。
    pub fn envelope(&self, protocol: &str, attempts: Option<&[String]>) -> (u16, Value) {
        let source = crate::semantic::SourceProtocol::parse(protocol)
            .unwrap_or(crate::semantic::SourceProtocol::OpenAi);
        crate::semantic::encode_error_for(source, &self.error, attempts)
    }
}

/// 转换 + vendor 修正出站请求体(B6 起经语义管线;原生透传格保持
/// clone 保真)。chat-completions 形态(openai 家族非原生 Responses
/// provider)时套用厂商参数修正(A5;幂等 hook,重试逐候选重跑,
/// 输入恒为原始客户端 payload)。
///
/// 失败路径:矩阵 unsupported 格(embeddings×anthropic/gemini、
/// interactions×非 gemini)返回 [`ConversionFailure`]。
pub fn upstream_payload(
    protocol: &str,
    payload: &Value,
    provider: &Provider,
) -> Result<Value, ConversionFailure> {
    upstream_payload_with_report(protocol, payload, provider).map(|(value, _)| value)
}

/// [`upstream_payload`] 的报告变体:额外返回本次转换的策略与诊断
/// (阶段 D 的 semantic conversion report;旧交互路径为 `None`)。
///
/// 诊断只用于 trace/debug,不进入客户端响应(方案 §3.5)。
pub fn upstream_payload_with_report(
    protocol: &str,
    payload: &Value,
    provider: &Provider,
) -> Result<(Value, Option<ConversionReport>), ConversionFailure> {
    let (converted, report) = upstream_payload_impl_with_report(protocol, payload, provider)?;
    if provider.protocol == ProviderProtocol::OpenAi
        && !provider.serves_openai_responses_natively()
        && matches!(
            protocol,
            client_protocol::OPENAI
                | client_protocol::ANTHROPIC
                | client_protocol::OPENAI_RESPONSES
                | client_protocol::GEMINI
                | client_protocol::GEMINI_STREAM
        )
    {
        return Ok((
            convert::apply_vendor_request_fixups(provider, &converted),
            report,
        ));
    }
    Ok((converted, report))
}

fn upstream_payload_impl_with_report(
    protocol: &str,
    payload: &Value,
    provider: &Provider,
) -> Result<(Value, Option<ConversionReport>), ConversionFailure> {
    let source = match crate::semantic::SourceProtocol::parse(protocol) {
        Some(source) => source,
        None => return Ok((payload.clone(), None)),
    };
    let target = provider.protocol;
    let native_responses = provider.serves_openai_responses_natively();
    crate::semantic::ensure_combination_supported(source, target)
        .map_err(|error| ConversionFailure { error })?;
    // 原生透传格:同协议 wire 保真,不经语义模型(方案决策 #3)。
    let passthrough = match (source, target) {
        (SourceProtocol::OpenAi, ProviderProtocol::OpenAi) if !native_responses => true,
        (SourceProtocol::OpenAiResponses, ProviderProtocol::OpenAi) if native_responses => true,
        (SourceProtocol::Anthropic, ProviderProtocol::Anthropic) => true,
        (SourceProtocol::Gemini, ProviderProtocol::Gemini) => true,
        // embeddings:仅 openai 上游经门禁到达,原样透传。
        (SourceProtocol::OpenAiEmbeddings, _) => true,
        // gemini_stream×gemini:同协议流式透传(流式信号在 URL 动作);
        // ×OpenAI/Anthropic 上游自阶段 E 起走语义转换(E2)。
        (SourceProtocol::GeminiStream, ProviderProtocol::Gemini) => true,
        _ => false,
    };
    if passthrough {
        // 透传不是「没有策略」:报告策略显式记为 NativePassthrough。
        return Ok((payload.clone(), Some(ConversionReport::passthrough())));
    }
    // interactions×gemini:interactions API 面的协议专项 helper(浅变换:
    // input → contents 数组,保留其余键)。上游 /interactions 端点本身就吃
    // `input` 形态,不经语义内核(E2 决策:保留为明确标记的协议专项
    // helper,而非伪造语义转换;×非 gemini 已被门禁拒绝)。
    if source == SourceProtocol::GeminiInteractions {
        return Ok((gemini::interaction_to_gemini(payload), None));
    }
    crate::semantic::convert_request(source, target, native_responses, payload)
        .map(|(value, report)| (value, Some(report)))
        .map_err(|error| ConversionFailure { error })
}

// ---------------------------------------------------------------------------
// 20260905_04 §3.3:响应 model 回写(聚合名替换;自旧 relay.rs 迁入)
// ---------------------------------------------------------------------------

/// 响应 JSON 的 model 字段回写(非流式响应体 / A6 合成支路解码体)。
///
/// R8(方案 20261106_01 §8.2、F-08):模型别名**只改协议指定的 model
/// 路径**,绝不递归进业务对象。覆盖三家方言的协议位置 —— openai chat
/// 顶层 `model`、anthropic `message.model`(message_start)、responses
/// `response.model`(事件信封)与顶层 `model`(响应对象)、gemini
/// `modelVersion`。工具参数 / metadata / 用户内容里同名的 `model` 键
/// 一律不动(旧实现递归回写会污染业务字段)。
///
/// 返回是否发生修改,供调用方决定是否需要重序列化(未修改保持原字节)。
pub fn rewrite_json_model_fields(value: &mut Value, model: &str) -> bool {
    let Value::Object(map) = value else {
        return false;
    };
    let mut changed = false;
    if map.get("model").is_some_and(Value::is_string) {
        changed |= set_model_if_different(map, "model", model);
    }
    if map.get("modelVersion").is_some_and(Value::is_string) {
        changed |= set_model_if_different(map, "modelVersion", model);
    }
    for key in ["message", "response"] {
        if let Some(Value::Object(nested)) = map.get_mut(key)
            && nested.get("model").is_some_and(Value::is_string)
        {
            changed |= set_model_if_different(nested, "model", model);
        }
    }
    changed
}

fn set_model_if_different(map: &mut Map<String, Value>, key: &str, model: &str) -> bool {
    if map.get(key).and_then(Value::as_str) == Some(model) {
        return false;
    }
    map.insert(key.to_owned(), Value::String(model.to_owned()));
    true
}

/// 输出 SSE 帧文本的 model 回写(流式泵出口;逐 chunk O(1) 级):
/// 按空行切分事件块,`data:` 行解析 JSON → [`rewrite_json_model_fields`]
/// → 原分隔符重组。`[DONE]`/解析失败/纯文本 data 原样返回;**协议路径
/// 未命中修改的 data 行保持原字节**(不无谓重序列化)。未设置覆盖时本函数
/// 不会被调用(零成本)。
pub fn rewrite_sse_frame_model(frame: &str, model: &str) -> String {
    if !frame.contains("\"model\"") && !frame.contains("\"modelVersion\"") {
        return frame.to_owned();
    }
    let mut out = String::with_capacity(frame.len());
    for (block_index, block) in frame.split("\n\n").enumerate() {
        if block_index > 0 {
            out.push_str("\n\n");
        }
        if block.trim().is_empty() {
            out.push_str(block);
            continue;
        }
        let mut rewritten_lines: Vec<String> = Vec::new();
        for line in block.split('\n') {
            let Some(data) = line.strip_prefix("data:") else {
                rewritten_lines.push(line.to_owned());
                continue;
            };
            let trimmed = data.trim();
            if trimmed.is_empty() || trimmed == "[DONE]" {
                rewritten_lines.push(line.to_owned());
                continue;
            }
            match serde_json::from_str::<Value>(trimmed) {
                Ok(mut value) => {
                    if rewrite_json_model_fields(&mut value, model) {
                        rewritten_lines
                            .push(format!("data: {}", crate::json_py::py_json_dumps(&value)));
                    } else {
                        rewritten_lines.push(line.to_owned());
                    }
                }
                Err(_) => rewritten_lines.push(line.to_owned()),
            }
        }
        out.push_str(&rewritten_lines.join("\n"));
    }
    out
}

/// NDJSON 单行的 model 回写(原生 Raw + 聚合别名):协议路径命中才重序列化,
/// 否则原样返回。`[DONE]`/非 JSON 行原样保留。
pub fn rewrite_ndjson_line_model(line: &str, model: &str) -> String {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed == "[DONE]" {
        return line.to_owned();
    }
    match serde_json::from_str::<Value>(trimmed) {
        Ok(mut value) => {
            if rewrite_json_model_fields(&mut value, model) {
                crate::json_py::py_json_dumps(&value)
            } else {
                line.to_owned()
            }
        }
        Err(_) => line.to_owned(),
    }
}

// ---------------------------------------------------------------------------
// 非流式响应解码(镜像 protocol_convert.decode_response,R3.8)
// ---------------------------------------------------------------------------

/// 上游 JSON 响应 → 客户端协议形态(B6 语义管线接线)。
///
/// `status` 为上游状态;`value` 为已解析 JSON(解析失败时调用方传
/// `{"error": {"message": <text>}}`,镜像 Python 的 except 分支)。
/// 错误路径统一重写为客户端协议信封(与转发层共享行为)。
///
/// P4-5(F-09):转换失败必须返回 [`ConversionFailure`],不能把另一种
/// 协议的对象当成功响应。旧 [`decode_response`] 包装仅保留给纯转换
/// 测试/历史调用;生产转发使用本函数。
pub fn try_decode_response(
    status: u16,
    value: &Value,
    protocol: &str,
    provider: &Provider,
) -> Result<Value, ConversionFailure> {
    try_decode_response_with_report(status, value, protocol, provider).map(|(value, _)| value)
}

/// P5-3:与 [`try_decode_response`] 相同,但把响应面转换报告一并返回
/// (策略/诊断用于指标;不改变客户端信封)。
pub fn try_decode_response_with_report(
    status: u16,
    value: &Value,
    protocol: &str,
    provider: &Provider,
) -> Result<(Value, Option<crate::semantic::ConversionReport>), ConversionFailure> {
    if status >= 400 {
        let body_for_classification = if value.is_object() {
            value.clone()
        } else {
            json!({"message": crate::python_semantics::py_str(value).unwrap_or_default()})
        };
        let upstream_error = classify_upstream_error(status, &body_for_classification);
        let (_, envelope) = format_error_for_protocol(protocol, &upstream_error, None);
        return Ok((envelope, None));
    }
    let Some(source) = SourceProtocol::parse(protocol) else {
        return Ok((value.clone(), None));
    };
    let target = provider.protocol;
    // G-7:非流响应面与请求面使用同一能力门禁;不再等到底层编码器
    // 偶然失败才拒绝 unsupported 组合。
    if let Err(error) = crate::semantic::ensure_combination_supported(source, target) {
        return Err(ConversionFailure { error });
    }
    let native_responses = provider.serves_openai_responses_natively();
    // 原生透传格(clone 保真)与阶段 C 收敛面(gemini_stream 响应面)。
    let passthrough = match (source, target) {
        (SourceProtocol::OpenAi, ProviderProtocol::OpenAi) if !native_responses => true,
        (SourceProtocol::OpenAiResponses, ProviderProtocol::OpenAi) if native_responses => true,
        (SourceProtocol::Anthropic, ProviderProtocol::Anthropic) => true,
        (SourceProtocol::Gemini, ProviderProtocol::Gemini) => true,
        (SourceProtocol::GeminiStream, ProviderProtocol::Gemini) => true,
        (SourceProtocol::OpenAiEmbeddings, _) => true,
        (SourceProtocol::GeminiInteractions, ProviderProtocol::Gemini) => false,
        (SourceProtocol::GeminiInteractions, _) => true,
        _ => false,
    };
    if passthrough {
        return Ok((value.clone(), None));
    }
    crate::semantic::convert_response(source, target, native_responses, value)
        .map(|(converted, report)| (converted, Some(report)))
        .map_err(|error| ConversionFailure { error })
}

// ---------------------------------------------------------------------------
// 上游错误 → 客户端协议错误信封(镜像 errors.py,R2.6)
// ---------------------------------------------------------------------------

/// 归一化上游错误描述(镜像 `UpstreamError`)。
#[derive(Debug, Clone, PartialEq)]
pub struct UpstreamError {
    pub status: u16,
    pub message: String,
    pub error_type: String,
    pub code: Option<String>,
    pub param: Option<String>,
}

/// 状态码 → 默认错误 type(镜像 `_STATUS_TYPE_MAP`)。
pub fn status_type(status: u16) -> Option<&'static str> {
    Some(match status {
        400 => "invalid_request_error",
        401 => "authentication_error",
        403 => "permission_denied_error",
        404 => "not_found_error",
        408 => "request_timeout",
        409 => "conflict_error",
        413 => "request_too_large",
        422 => "invalid_request_error",
        429 => "rate_limit_error",
        500 => "server_error",
        502 => "bad_gateway",
        503 => "service_unavailable",
        504 => "gateway_timeout",
        _ => return None,
    })
}

/// 从上游状态 + 响应体构建归一化错误(镜像 `classify_upstream_error`)。
/// 兼容 OpenAI `{"error":{...}}` / Anthropic `{"type":"error",...}` /
/// Gemini `{"error":{"code","status","message"}}` / 裸 `{"message"}` /
/// `{"error": "<string>"}` / `{"detail": "..."}`。
pub fn classify_upstream_error(status: u16, body: &Value) -> UpstreamError {
    let error_field = body.get("error").filter(|v| v.is_object() || v.is_string());
    let mut code: Option<String> = None;
    let mut param: Option<String> = None;
    let mut message = match error_field {
        Some(Value::Object(error_obj)) => {
            code = error_obj.get("code").map(stringify_value);
            param = error_obj.get("param").map(stringify_value);
            str_field(error_obj, "message")
                .or_else(|| str_field(error_obj, "reason"))
                .unwrap_or_default()
        }
        Some(Value::String(text)) => text.clone(),
        _ => {
            // Python 兜底:message = str(body.get("message") or
            // body.get("detail") or body.get("error") or "")。
            let pick = |key: &str| {
                body.get(key)
                    .filter(|v| crate::python_semantics::py_bool(v))
            };
            pick("message")
                .or_else(|| pick("detail"))
                .or_else(|| pick("error"))
                .map(|v| match crate::python_semantics::py_str(v) {
                    Some(text) => text,
                    None => crate::json_py::py_json_dumps(v),
                })
                .unwrap_or_default()
        }
    };
    let mut error_type = match error_field {
        Some(Value::Object(error_obj)) => str_field(error_obj, "type")
            .or_else(|| str_field(error_obj, "status"))
            .unwrap_or_default(),
        _ => String::new(),
    };

    if message.is_empty() {
        message = format!("Upstream returned HTTP {status}");
    }
    if error_type.is_empty() {
        error_type = status_type(status)
            .unwrap_or(if status >= 500 {
                "upstream_error"
            } else {
                "request_error"
            })
            .to_owned();
    }
    UpstreamError {
        status,
        message,
        error_type,
        code,
        param,
    }
}

fn str_field(map: &Map<String, Value>, key: &str) -> Option<String> {
    match map.get(key) {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

fn stringify_value(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => crate::json_py::py_json_dumps(other),
    }
}

/// 按客户端协议渲染错误信封,返回 `(http_status, body)`
/// (B7 起委托 `semantic::envelope`,五种信封线级形态不变)。
/// `attempts` 非空时附加 `wrouter_attempts`(A4)。
pub fn format_error_for_protocol(
    protocol: &str,
    error: &UpstreamError,
    attempts: Option<&[String]>,
) -> (u16, Value) {
    let source = SourceProtocol::parse(protocol).unwrap_or(SourceProtocol::OpenAi);
    crate::semantic::encode_error_for(
        source,
        &crate::semantic::SemanticError::from_upstream(error),
        attempts,
    )
}

/// 流式错误事件名(镜像 `sse_encoding.encode_sse_error`)。
/// anthropic / gemini 用 `error`,openai_responses 用 `response.failed`,
/// openai 系无 event 行。
pub fn sse_error_event_name(protocol: &str) -> Option<&'static str> {
    match protocol {
        "anthropic" | "gemini" | "gemini_stream" => Some("error"),
        "openai_responses" => Some("response.failed"),
        _ => None,
    }
}

/// usage 助手再导出(semantic 适配器与网关的统一入口)。
pub use crate::usage::Usage;

#[cfg(test)]
mod endpoint_tests {
    use super::*;

    fn provider(base_url: &str) -> Provider {
        Provider {
            id: "p".to_owned(),
            name: "p".to_owned(),
            base_url: base_url.to_owned(),
            api_key: String::new(),
            protocol: ProviderProtocol::OpenAi,
            enabled: true,
            models: Vec::new(),
            headers: Map::new(),
            timeout_seconds: 120.0,
            non_stream_timeout_seconds: None,
            max_retries: 0,
            max_in_flight: None,
            protocol_style: None,
            url_mode: UrlMode::Base,
            billing: Map::new(),
            failure_threshold: None,
            cooldown_ms: None,
            accounts: Vec::new(),
            credential_strategy: crate::provider::ProviderCredentialStrategy::default(),
            pinned_account_id: None,
            context_windows: Default::default(),
            proxy: None,
            client_fingerprint: None,
        }
    }

    #[test]
    fn versioned_base_appends_without_extra_v1() {
        // 火山方舟 /api/coding/v3:真实接受 /chat/completions,插 /v1 会 404
        // (2026-09-13 实测);智谱 /api/paas/v4 同理。
        assert_eq!(
            endpoint(
                "openai",
                &provider("https://ark.cn-beijing.volces.com/api/coding/v3"),
                "m",
                false
            ),
            "https://ark.cn-beijing.volces.com/api/coding/v3/chat/completions"
        );
        assert_eq!(
            endpoint(
                "openai",
                &provider("https://open.bigmodel.cn/api/paas/v4"),
                "m",
                false
            ),
            "https://open.bigmodel.cn/api/paas/v4/chat/completions"
        );
        // anthropic 家族同规则。
        let mut anthropic = provider("https://ark.cn-beijing.volces.com/api/coding/v3");
        anthropic.protocol = ProviderProtocol::Anthropic;
        assert_eq!(
            endpoint("anthropic", &anthropic, "m", false),
            "https://ark.cn-beijing.volces.com/api/coding/v3/messages"
        );
        // v1beta 形态同样视为已带版本。
        assert_eq!(
            endpoint(
                "openai",
                &provider("https://host.example/v1beta"),
                "m",
                false
            ),
            "https://host.example/v1beta/chat/completions"
        );
    }

    #[test]
    fn unversioned_and_plain_bases_keep_v1_insertion() {
        // 无版本 base:维持既有 /v1 插入语义。
        assert_eq!(
            endpoint("openai", &provider("https://api.deepseek.com"), "m", false),
            "https://api.deepseek.com/v1/chat/completions"
        );
        // /v1 结尾:直接拼端点(原行为)。
        assert_eq!(
            endpoint("openai", &provider("https://api.openai.com/v1"), "m", false),
            "https://api.openai.com/v1/chat/completions"
        );
        // 普通单词(v 开头非版本,如 "values")不误判。
        assert_eq!(
            endpoint(
                "openai",
                &provider("https://host.example/values"),
                "m",
                false
            ),
            "https://host.example/values/v1/chat/completions"
        );
    }

    /// 2026-10-13:端点由上游形态决定,不能按客户端协议名逐一枚举 —— 任何
    /// 对话入口打到原生 Responses 供应商都必须走 `/responses`(此前
    /// anthropic / gemini_stream 会误落 `/chat/completions`,真实上游 400)。
    #[test]
    fn every_client_hits_responses_for_native_responses_provider() {
        let mut native = provider("https://upstream.example/v1");
        native.protocol_style = Some("openai_responses".to_owned());
        for client in [
            "openai",
            "openai_responses",
            "anthropic",
            "gemini",
            "gemini_stream",
        ] {
            assert_eq!(
                endpoint(client, &native, "m", false),
                "https://upstream.example/v1/responses",
                "client {client} must target /responses"
            );
        }
        // 流式与非流式同端点(流式信号在请求体 stream 字段)。
        assert_eq!(
            endpoint("anthropic", &native, "m", true),
            "https://upstream.example/v1/responses"
        );
    }

    /// 非原生 Responses 的 openai 供应商:所有对话入口走 chat completions;
    /// embeddings 走 /embeddings;anthropic 供应商走 /messages。
    #[test]
    fn non_native_openai_provider_endpoints() {
        let openai = provider("https://upstream.example/v1");
        for client in [
            "openai",
            "openai_responses",
            "anthropic",
            "gemini",
            "gemini_stream",
        ] {
            assert_eq!(
                endpoint(client, &openai, "m", false),
                "https://upstream.example/v1/chat/completions",
                "client {client} must target /chat/completions"
            );
        }
        assert_eq!(
            endpoint("openai_embeddings", &openai, "m", false),
            "https://upstream.example/v1/embeddings"
        );
        let mut anthropic = provider("https://upstream.example/v1");
        anthropic.protocol = ProviderProtocol::Anthropic;
        for client in ["openai", "openai_responses", "anthropic", "gemini_stream"] {
            assert_eq!(
                endpoint(client, &anthropic, "m", false),
                "https://upstream.example/v1/messages",
                "client {client} must target /messages"
            );
        }
    }

    // ---- 20261014_01:完整 URL 模式(endpoint 短路) --------------------

    #[test]
    fn full_url_mode_returns_base_verbatim() {
        // 原样调用:无版本段、无已知后缀的自定义路径不再被插 /v1。
        let mut full = provider("https://gw.example.com/llm/claude");
        full.url_mode = UrlMode::Full;
        assert_eq!(
            endpoint("anthropic", &full, "m", false),
            "https://gw.example.com/llm/claude"
        );
        // 尾斜杠与 query 是用户语义,任何一层不得改写。
        let mut slash_query = provider("https://gw.example.com/llm/claude/?edition=us");
        slash_query.url_mode = UrlMode::Full;
        assert_eq!(
            endpoint("openai", &slash_query, "m", false),
            "https://gw.example.com/llm/claude/?edition=us"
        );
    }

    #[test]
    fn full_url_mode_gemini_family_falls_back_to_computed_path() {
        // Gemini 家族端点必须拼模型名与 action,full 语义无法表达:宽容
        // 回落 base 模式(前端隐藏开关后的后端保险)。
        let mut full_gemini = provider("https://gw.example.com/llm/gemini");
        full_gemini.protocol = ProviderProtocol::Gemini;
        full_gemini.url_mode = UrlMode::Full;
        assert_eq!(
            endpoint("gemini", &full_gemini, "m-1", false),
            "https://gw.example.com/llm/gemini/models/m-1:generateContent"
        );
        assert_eq!(
            endpoint("gemini", &full_gemini, "m-1", true),
            "https://gw.example.com/llm/gemini/models/m-1:streamGenerateContent"
        );
    }

    // ---- 20261016_03:客户端指纹(endpoint query + upstream headers) ----

    fn fingerprinted(base_url: &str, fingerprint: Value) -> Provider {
        let mut raw = serde_json::json!({
            "id": "prov-x", "api_base_url": base_url, "protocol": "anthropic",
            "api_key": "sk-provider",
            "clientFingerprint": fingerprint,
        });
        Provider::from_raw(raw.as_object_mut().unwrap()).unwrap()
    }

    #[test]
    fn endpoint_query_appends_with_correct_separator() {
        let p = provider("https://up.example/v1");
        let mut anthropic = provider("https://up.example/v1");
        anthropic.protocol = ProviderProtocol::Anthropic;
        assert_eq!(
            endpoint_with_query("anthropic", &anthropic, "m", false, Some("beta=true")),
            "https://up.example/v1/messages?beta=true"
        );
        // 端点已带 query(gemini key 模式):用 & 拼接。
        assert_eq!(
            endpoint_with_query("openai", &p, "m", false, Some("beta=true")),
            "https://up.example/v1/chat/completions?beta=true"
        );
        // None / 空串:与 endpoint 完全一致(现有调用点零改动)。
        assert_eq!(
            endpoint_with_query("anthropic", &p, "m", false, None),
            endpoint("anthropic", &p, "m", false)
        );
        assert_eq!(
            endpoint_with_query("anthropic", &p, "m", false, Some("")),
            endpoint("anthropic", &p, "m", false)
        );
        // full URL 模式:指纹 query 不追加(用户原样语义)。
        let mut full = provider("https://gw.example.com/llm/claude");
        full.url_mode = UrlMode::Full;
        assert_eq!(
            endpoint_with_query("anthropic", &full, "m", false, Some("beta=true")),
            "https://gw.example.com/llm/claude"
        );
    }

    #[test]
    fn fingerprint_off_matches_plain_upstream_headers_byte_for_byte() {
        // 默认 off 零影响:带 None 上下文的变体输出与原函数逐字节一致。
        let p = provider("https://up.example/v1");
        assert_eq!(
            upstream_headers_with_fingerprint(&p, None),
            upstream_headers(&p)
        );
    }

    #[test]
    fn passthrough_never_overrides_explicit_provider_headers() {
        // 三级优先级:provider.headers 显式配置 > 指纹(passthrough)。
        let mut raw = serde_json::json!({
            "id": "prov-x", "api_base_url": "https://up.example/v1", "protocol": "anthropic",
            "api_key": "sk-provider",
            "headers": {"user-agent": "explicit-ua/1.0"},
            "clientFingerprint": {"mode": "passthrough"},
        });
        let p = Provider::from_raw(raw.as_object_mut().unwrap()).unwrap();
        let inbound = vec![
            ("user-agent".to_owned(), "client-ua/9.9".to_owned()),
            ("anthropic-beta".to_owned(), "beta-x".to_owned()),
        ];
        let headers = upstream_headers_with_fingerprint(&p, Some(&inbound));
        assert_eq!(headers["user-agent"], "explicit-ua/1.0");
        assert_eq!(headers["anthropic-beta"], "beta-x");
        // 自动注入的 anthropic-version 不被指纹抢位(入站无该键 → 注入)。
        assert_eq!(headers["anthropic-version"], "2023-06-01");
    }

    #[test]
    fn passthrough_inbound_version_beats_auto_injection() {
        // 指纹 > 自动注入:入站 anthropic-version 优先于 2023-06-01 缺省。
        let mut raw = serde_json::json!({
            "id": "prov-x", "api_base_url": "https://up.example/v1", "protocol": "anthropic",
            "clientFingerprint": {"mode": "passthrough"},
        });
        let p = Provider::from_raw(raw.as_object_mut().unwrap()).unwrap();
        let inbound = vec![("anthropic-version".to_owned(), "2023-06-01".to_owned())];
        let headers = upstream_headers_with_fingerprint(&p, Some(&inbound));
        assert_eq!(headers["anthropic-version"], "2023-06-01");
    }

    #[test]
    fn auth_injection_still_precedes_nothing_after_fingerprint() {
        // 指纹层不得携带/顶替认证:入站 authorization 被红线剥除,注入的
        // x-api-key 保持 provider 凭据。
        let mut raw = serde_json::json!({
            "id": "prov-x", "api_base_url": "https://up.example/v1", "protocol": "anthropic",
            "api_key": "sk-provider",
            "clientFingerprint": {"mode": "passthrough"},
        });
        let p = Provider::from_raw(raw.as_object_mut().unwrap()).unwrap();
        let inbound = vec![
            (
                "authorization".to_owned(),
                "Bearer client-gw-key".to_owned(),
            ),
            ("x-api-key".to_owned(), "sk-client".to_owned()),
        ];
        let headers = upstream_headers_with_fingerprint(&p, Some(&inbound));
        assert_eq!(headers["x-api-key"], "sk-provider");
        assert!(headers.get("authorization").is_none());
    }

    #[test]
    fn preset_headers_merge_and_explicit_config_wins() {
        let p = fingerprinted(
            "https://up.example/v1",
            serde_json::json!({"mode": "preset", "presetId": "zcode"}),
        );
        let headers = upstream_headers_with_fingerprint(&p, None);
        assert_eq!(
            headers["user-agent"],
            "ZCode/3.15.3 ai-sdk/provider-utils/4.0.39 runtime/node.js/24"
        );
        assert_eq!(headers["x-zcode-agent"], "glm");
        // anthropic-version:预设不带 → 自动注入落位。
        assert_eq!(headers["anthropic-version"], "2023-06-01");

        // provider.headers 覆盖预设单键(UA 跟版本的用户通道)。
        let mut raw = serde_json::json!({
            "id": "prov-x", "api_base_url": "https://up.example/v1", "protocol": "anthropic",
            "headers": {"User-Agent": "ZCode/9.9.9 custom"},
            "clientFingerprint": {"mode": "preset", "presetId": "zcode"},
        });
        let p2 = Provider::from_raw(raw.as_object_mut().unwrap()).unwrap();
        let headers2 = upstream_headers_with_fingerprint(&p2, None);
        assert_eq!(headers2["user-agent"], "ZCode/9.9.9 custom");
        assert_eq!(headers2["x-zcode-agent"], "glm");
    }

    #[test]
    fn preset_query_flows_into_endpoint_for_anthropic() {
        let p = fingerprinted(
            "https://up.example/v1",
            serde_json::json!({"mode": "preset", "presetId": "claude-code"}),
        );
        let preset = client_fingerprint::find_preset("claude-code").unwrap();
        let query = client_fingerprint::preset_query(preset, &p);
        assert_eq!(query.as_deref(), Some("beta=true"));
        assert_eq!(
            endpoint_with_query("anthropic", &p, "m", false, query.as_deref()),
            "https://up.example/v1/messages?beta=true"
        );
    }
}

#[cfg(test)]
mod claude_app_model_tests {
    use super::decode_claude_app_model;

    fn hex(text: &str) -> String {
        text.as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    #[test]
    fn decodes_any_namespace_and_strips_provider_prefix() {
        let encoded = hex("openai/gpt-5");
        for namespace in ["wr1", "ccr2", "local", "myrouter"] {
            let model = format!("anthropic/claude-{namespace}-h{encoded}");
            assert_eq!(decode_claude_app_model(&model), "gpt-5", "{model}");
        }
    }

    #[test]
    fn leaves_plain_and_malformed_names_untouched() {
        for name in [
            "claude-sonnet-4-6",
            "anthropic/claude-sonnet-4-6",
            "anthropic/claude-wr1-habc",
            "anthropic/claude-wr1-hZZZ",
        ] {
            assert_eq!(decode_claude_app_model(name), name);
        }
    }
}
