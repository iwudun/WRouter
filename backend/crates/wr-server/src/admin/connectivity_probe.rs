//! 连通性探针引擎(方案 #14 R5.3;镜像 Python
//! `interfaces/http/handlers/connectivity_probe.py` 全文)。
//!
//! 两个 RPC 面共用本模块:
//! - [`check_connectivity`] ← `checkProviderConnectivity`:对每个
//!   (candidate × protocol × model) 发一次真实 `max_tokens=256` chat
//!   请求(与网关转发同款协议转换器),**5 重判定门**全过才判 supported;
//! - [`probe_candidates`] ← `probeProviderCandidates`:GET
//!   `/v1/models` 抽取模型列表,跨候选合并 capabilities 并选最优候选。
//!
//! HTTP 语义对齐 httpx:`Timeout(15.0, connect=10.0)` → reqwest 总超时
//! 15s + connect 10s;`trust_env=False` → 显式 no_proxy,再按
//! `resolve_proxy_decision` 的显式决策覆盖(custom/system)。
//!
//! 方案 20260906_01 §4.6:探针载荷可带 `proxy: {mode, presetId}` 表达
//! 供应商级覆盖(载荷无 providerId,草稿阶段也要能测);缺省回落全局默认。

use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde_json::{Map, Value, json};

use wr_domain::protocol as protocol_convert;
use wr_domain::provider::{Provider, ProviderProtocol, ProviderProxyOverride, UrlMode};
use wr_domain::python_semantics::{py_bool, py_str};
use wr_infra::proxy::{
    ProxyDecision, ProxyPreset, describe_proxy_source, resolve_proxy_decision_with_override,
};

/// 代理配置 map 上传递供应商级覆盖的保留键(见 [`with_payload_override`])。
/// 全局 `proxy` 段不会有这个键,故不与既有字段冲突。
const PROXY_OVERRIDE_KEY: &str = "providerProxyOverride";

/// 探针输出 token 上限(必须给推理模型留出可见输出预算;与 UI 文案一致)。
pub const PROBE_MAX_TOKENS: i64 = 256;

/// 探针提示词(请模型自证身份,使响应回显 `model` 字段有意义)。
pub const PROBE_PROMPT: &str = "Respond with the single word: ok";

/// 总超时(镜像 `httpx.Timeout(15.0, connect=10.0)` 的总时长)。
const PROBE_TOTAL_TIMEOUT: Duration = Duration::from_secs(15);
/// 连接超时。
const PROBE_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// 候选探测并发上限(镜像 `asyncio.Semaphore(6)`;`buffered` 保序)。
const PROBE_CONCURRENCY: usize = 6;

/// 前端 API format → 网关存储的粗粒度 family。
fn frontend_protocol_family(frontend: &str) -> Option<&'static str> {
    match frontend {
        "openai_chat_completions" | "openai_chat" | "openai" | "openai_responses" => Some("openai"),
        "anthropic_messages" | "anthropic" => Some("anthropic"),
        _ => None,
    }
}

/// 前端 API format → 协议转换器的 client protocol。
fn frontend_protocol_client(frontend: &str) -> Option<&'static str> {
    match frontend {
        "openai_chat_completions" | "openai_chat" | "openai" => Some("openai"),
        "openai_responses" => Some("openai_responses"),
        "anthropic_messages" | "anthropic" => Some("anthropic"),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Python 迭代语义辅助(`x or []` 后直接 for 迭代)
// ---------------------------------------------------------------------------

/// 镜像 `for item in (payload.get(k) or [])`:falsy → 空表;可迭代 →
/// 元素序列(数组→元素、对象→键、字符串→字符);标量 truthy →
/// TypeError(镜像为 Err,调用方落 500)。
fn py_iter(value: Option<&Value>) -> Result<Vec<Value>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    if !py_bool(value) {
        return Ok(Vec::new());
    }
    match value {
        Value::Array(items) => Ok(items.clone()),
        Value::Object(map) => Ok(map.keys().map(|k| Value::String(k.clone())).collect()),
        Value::String(s) => Ok(s.chars().map(|c| Value::String(c.to_string())).collect()),
        other => Err(format!("'{other}' object is not iterable")),
    }
}

/// `payload.get("apiKey") or payload.get("api_key") or ""` → str → strip。
fn pick_api_key(payload: &Map<String, Value>) -> String {
    for key in ["apiKey", "api_key"] {
        let value = payload.get(key);
        if let Some(value) = value
            && py_bool(value)
            && let Some(text) = py_str(value)
        {
            return text.trim().to_owned();
        }
    }
    String::new()
}

/// `[str(item) for item in seq if str(item).strip()]`。
fn str_list(items: Vec<Value>) -> Vec<String> {
    items
        .into_iter()
        .filter_map(|item| {
            py_str(&item)
                .map(|text| text.trim().to_owned())
                .filter(|t| !t.is_empty())
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 端点后缀剥离(镜像 protocol_convert.strip_endpoint_suffix / probe_endpoint)
// ---------------------------------------------------------------------------

/// (suffix, replacement) — 最长者在前,`/v1/chat/completions` 优先于
/// `/chat/completions`;replacement 保留版本段避免重建 `/v1/models` 时重复。
const ENDPOINT_SUFFIX_STRIP_RULES: &[(&str, &str)] = &[
    ("/v1/chat/completions", "/v1"),
    ("/chat/completions", ""),
    ("/v1/responses", "/v1"),
    ("/responses", ""),
    ("/v1/messages", "/v1"),
    ("/messages", ""),
    ("/v1/embeddings", "/v1"),
    ("/embeddings", ""),
    ("/v1/models", "/v1"),
    ("/models", ""),
    ("/v1beta/models", "/v1beta"),
];

/// 剥掉 base_url 尾部的已知上游端点路径,便于重建 `/v1/models`
/// (DeepSeek 例:`https://api.deepseek.com/chat/completions` →
/// `https://api.deepseek.com`)。
pub fn strip_endpoint_suffix(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    for (suffix, replacement) in ENDPOINT_SUFFIX_STRIP_RULES {
        if base.len() >= suffix.len()
            && base[base.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
        {
            let stripped = format!("{}{}", &base[..base.len() - suffix.len()], replacement);
            return stripped.trim_end_matches('/').to_owned();
        }
    }
    base.to_owned()
}

/// `probe_endpoint`:strip 后 base 以 `/v1` 结尾 → `/models`,否则
/// `/v1/models`(REST `POST /api/v1/providers/probe` 用)。
pub fn probe_endpoint(provider: &Provider) -> String {
    let base = strip_endpoint_suffix(&provider.base_url);
    if base.ends_with("/v1") {
        format!("{base}/models")
    } else {
        format!("{base}/v1/models")
    }
}

// ---------------------------------------------------------------------------
// HTTP 客户端(探针专用:总超时 15s + connect 10s,无连接池缓存)
// ---------------------------------------------------------------------------

pub(crate) fn probe_client(proxy: &ProxyDecision) -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .timeout(PROBE_TOTAL_TIMEOUT)
        .connect_timeout(PROBE_CONNECT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(20));
    builder = match proxy {
        ProxyDecision::Direct => builder.no_proxy(),
        ProxyDecision::Url(url) => match reqwest::Proxy::all(url) {
            Ok(proxy) => builder.proxy(proxy),
            Err(_) => builder.no_proxy(),
        },
    };
    builder.build().unwrap_or_else(|_| reqwest::Client::new())
}

/// 每任务解析代理决策(`resolve_proxy_kwargs_for_url` 的 Rust 镜像 +
/// 方案 20260906_01 的供应商级覆盖;proxy_config=None → Python 传
/// proxy_kwargs=None → trust_env=False 直连,与
/// resolve_proxy_decision(None-config) 的 Direct 同值)。
/// 探针与测速共用(见 [`with_payload_override`])。
pub(crate) fn probe_proxy_decision(
    proxy_config: Option<&Map<String, Value>>,
    url: &str,
) -> ProxyDecision {
    let provider_override = ProviderProxyOverride::from_value(
        proxy_config.and_then(|config| config.get(PROXY_OVERRIDE_KEY)),
    );
    resolve_proxy_decision_with_override(proxy_config, provider_override.as_ref(), url)
}

/// 探针用的代理来源标签(方案 §4.6:让用户确认「测的就是将来的走法」)。
pub(crate) fn probe_proxy_label(proxy_config: Option<&Map<String, Value>>, url: &str) -> String {
    let decision = probe_proxy_decision(proxy_config, url);
    describe_proxy_source(proxy_config, &decision)
}

/// 把探针载荷里的 `proxy` 覆盖并入代理配置(方案 §4.6 / 决策 #9)。
///
/// 探针载荷没有 providerId(草稿阶段——新增供应商尚未保存——也要能探测),
/// 所以供应商级代理只能由前端**显式随载荷传**;缺省时原样返回全局配置
/// (= 回落全局默认,向后兼容)。覆盖挂在 [`PROXY_OVERRIDE_KEY`] 保留键上,
/// 并发任务仍只需克隆一个 Arc。
pub(crate) fn with_payload_override(
    proxy_config: Option<&Map<String, Value>>,
    payload: &Map<String, Value>,
) -> Option<Map<String, Value>> {
    let Some(override_value) = payload.get("proxy").filter(|value| value.is_object()) else {
        return proxy_config.cloned();
    };
    let mut merged = proxy_config.cloned().unwrap_or_default();
    merged.insert(PROXY_OVERRIDE_KEY.to_owned(), override_value.clone());
    Some(merged)
}

fn duration_ms(started: Instant) -> f64 {
    (started.elapsed().as_secs_f64() * 1000.0 * 100.0).round() / 100.0
}

// ---------------------------------------------------------------------------
// GET 模型列表探针(_probe_candidate)
// ---------------------------------------------------------------------------

/// GET 候选的模型列表端点;返回
/// `{"ok","status","duration_ms","models","message"}`(snake_case 原文)。
pub async fn probe_candidate(
    candidate: &Value,
    api_key: &str,
    proxy_config: Option<&Map<String, Value>>,
) -> Result<Value, String> {
    let Some(map) = candidate.as_object() else {
        // Python: candidate.get(...) → AttributeError → RPC 500。
        return Err("'candidate' object has no attribute 'get'".to_owned());
    };
    // 探针载荷契约使用 `baseUrl`(前端 fetchProviderModels 同键);这不是
    // 配置文件的 provider 记录,不套用 `api_base_url` 最新配置键。
    let base_raw = ["baseUrl", "base_url"]
        .iter()
        .find_map(|key| map.get(*key).filter(|v| py_bool(v)).and_then(py_str))
        .unwrap_or_default();
    let base_url = strip_endpoint_suffix(&base_raw);
    if base_url.is_empty() {
        return Ok(json!({
            "ok": false, "status": Value::Null, "duration_ms": 0.0,
            "models": [], "message": "No base URL.",
        }));
    }
    let mut urls_to_try: Vec<String> = Vec::new();
    if base_url.ends_with("/v1") {
        urls_to_try.push(format!("{base_url}/models"));
    } else {
        urls_to_try.push(format!("{base_url}/v1/models"));
        urls_to_try.push(format!("{base_url}/models"));
    }
    let mut headers = vec![("accept".to_owned(), "application/json".to_owned())];
    if !api_key.is_empty() {
        headers.push(("authorization".to_owned(), format!("Bearer {api_key}")));
    }

    let started = Instant::now();
    let client = probe_client(&probe_proxy_decision(proxy_config, &base_url));
    let mut models_found: Vec<Value> = Vec::new();
    let mut ok = false;
    // `status: int | None`:尚未收到任何响应时为 null。
    let mut status: Option<i64> = None;
    let mut message = "Endpoint unreachable.".to_owned();

    for url in urls_to_try {
        let request = client
            .get(&url)
            .header("accept", "application/json")
            .header("authorization", format!("Bearer {api_key}"));
        // Python:except (httpx.HTTPError, ValueError) → continue。
        let response = match request.send().await {
            Ok(response) => response,
            Err(_) => continue,
        };
        let code = i64::from(response.status().as_u16());
        status = Some(code);
        if code < 400 {
            ok = true;
            message = "Endpoint responded successfully.".to_owned();
            // resp.json() 失败(或 data 非 dict 的 .get AttributeError)在
            // Python 里被同一 except 捕获 → continue(ok/message 已置位)。
            let body_text = response.text().await.unwrap_or_default();
            let parsed: Result<Value, _> = serde_json::from_str(&body_text);
            let data = match parsed {
                Ok(data) => data,
                Err(_) => continue,
            };
            if let Some(items) = extract_model_items(&data) {
                models_found.extend(items);
                break;
            }
            // `data.get(...)` AttributeError(data 非对象)→ continue。
            continue;
        }
        message = format!("Endpoint returned HTTP {code}.");
    }
    Ok(json!({
        "ok": ok,
        "status": status.map(|s| json!(s)).unwrap_or(Value::Null),
        "duration_ms": duration_ms(started),
        "models": models_found,
        "message": message,
        // 20260906_01 §4.6:本次探测实际走的出口(凭据已掩码)。
        "proxy": probe_proxy_label(proxy_config, &base_url),
    }))
}

/// `items = data.get("data") or data.get("models") or []` 后的逐项抽取:
/// str 直收;dict 取 `id|model|name`(truthy 才收);Python `or` 链的
/// 穿透语义按 py_bool 处理。返回 None 表示 data 非 dict(镜像
/// AttributeError 路径)。
fn extract_model_items(data: &Value) -> Option<Vec<Value>> {
    let map = data.as_object()?;
    let items = ["data", "models"]
        .iter()
        .find_map(|key| map.get(*key).filter(|v| py_bool(v)).cloned());
    let items = items.unwrap_or(Value::Array(Vec::new()));
    match items {
        Value::Array(list) => {
            let mut found = Vec::new();
            for item in list {
                match &item {
                    Value::String(s) => found.push(Value::String(s.clone())),
                    Value::Object(_) => {
                        let m = ["id", "model", "name"]
                            .iter()
                            .find_map(|key| item.get(*key).filter(|v| py_bool(v)).and_then(py_str))
                            .unwrap_or_default();
                        if !m.is_empty() {
                            found.push(Value::String(m));
                        }
                    }
                    _ => {}
                }
            }
            Some(found)
        }
        // dict → 迭代键;str → 迭代字符(Python for 语义)。
        Value::Object(map) => Some(map.keys().map(|k| Value::String(k.clone())).collect()),
        Value::String(s) => Some(s.chars().map(|c| Value::String(c.to_string())).collect()),
        // 数值等不可迭代 → TypeError → except → continue(ok/message 已置位)。
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// 实探一次 chat(_probe_model_protocol)
// ---------------------------------------------------------------------------

/// `max_tokens=PROBE_MAX_TOKENS` 的最小请求体(client protocol 形态)。
///
/// Responses 显式带 `stream: false`:探针读的是非流式响应,而省略该字段
/// 时部分上游(agnes)会按流式处理并回 500 `bad_response_body`——
/// 「invalid character 'd' looking for beginning of value」(SSE 正文当
/// JSON 解析),使原生 Responses 供应商恒判不可用(2026-10-13 实测)。
fn probe_body(client_protocol: &str, model: &str) -> Value {
    if client_protocol == "openai_responses" {
        json!({
            "model": model,
            "input": PROBE_PROMPT,
            "max_output_tokens": PROBE_MAX_TOKENS,
            "stream": false,
        })
    } else {
        json!({
            "model": model,
            "messages": [{"role": "user", "content": PROBE_PROMPT}],
            "max_tokens": PROBE_MAX_TOKENS,
        })
    }
}

/// 输出文本抽取(`_response_output_text`;三种响应形态)。
///
/// 20261030_01 R12:指纹探针复用同一抽取逻辑(三种响应形态一致)。
pub(crate) fn response_output_text(client_protocol: &str, data: &Value) -> String {
    let Some(map) = data.as_object() else {
        return String::new();
    };
    let first_text = |value: &Value| -> String {
        match value {
            Value::String(s) => s.trim().to_owned(),
            Value::Array(items) => {
                for item in items {
                    match item {
                        Value::Object(inner) => {
                            if let Some(text) = inner.get("text").and_then(Value::as_str)
                                && !text.trim().is_empty()
                            {
                                return text.trim().to_owned();
                            }
                        }
                        Value::String(s) if !s.trim().is_empty() => {
                            return s.trim().to_owned();
                        }
                        _ => {}
                    }
                }
                String::new()
            }
            _ => String::new(),
        }
    };
    if client_protocol == "openai_responses" {
        if let Some(output) = map.get("output").and_then(Value::as_array) {
            for item in output {
                if let Some(inner) = item.as_object()
                    && let Some(content) = inner.get("content")
                {
                    let text = first_text(content);
                    if !text.is_empty() {
                        return text;
                    }
                }
            }
        }
        return String::new();
    }
    if client_protocol == "anthropic" {
        return map.get("content").map(first_text).unwrap_or_default();
    }
    // OpenAI chat completions(及其它默认 OpenAI 形态)。
    if let Some(choices) = map.get("choices").and_then(Value::as_array) {
        for choice in choices {
            let Some(choice) = choice.as_object() else {
                continue;
            };
            if let Some(message) = choice.get("message").and_then(Value::as_object)
                && let Some(content) = message.get("content")
            {
                let text = first_text(content);
                if !text.is_empty() {
                    return text;
                }
            }
            let text = choice.get("text").map(first_text).unwrap_or_default();
            if !text.is_empty() {
                return text;
            }
        }
    }
    String::new()
}

/// `_response_has_error`:OpenAI `{"error": {...}}` 或裸 `{"error": "..."}`。
fn response_has_error(data: &Value) -> bool {
    data.as_object()
        .and_then(|map| map.get("error"))
        .is_some_and(|err| err.is_object() || err.is_string())
}

/// `_response_has_usage`:usage 为 dict 且 total_tokens>0,或四键任一 >0。
fn response_has_usage(data: &Value) -> bool {
    let Some(usage) = data
        .as_object()
        .and_then(|m| m.get("usage"))
        .and_then(Value::as_object)
    else {
        return false;
    };
    if usage
        .get("total_tokens")
        .and_then(Value::as_f64)
        .is_some_and(|total| total > 0.0)
    {
        return true;
    }
    for key in [
        "input_tokens",
        "output_tokens",
        "prompt_tokens",
        "completion_tokens",
    ] {
        if usage
            .get(key)
            .and_then(Value::as_f64)
            .is_some_and(|value| value > 0.0)
        {
            return true;
        }
    }
    false
}

/// `_response_usage`:归一化 `{"prompt_tokens","completion_tokens"}`,全 0
/// → null。
fn response_usage(data: &Value) -> Value {
    let Some(usage) = data
        .as_object()
        .and_then(|m| m.get("usage"))
        .and_then(Value::as_object)
    else {
        return Value::Null;
    };
    let pick = |keys: &[&str]| -> i64 {
        for key in keys {
            if let Some(value) = usage
                .get(*key)
                .and_then(wr_domain::python_semantics::py_int)
            {
                return value;
            }
        }
        0
    };
    let prompt = pick(&["prompt_tokens", "input_tokens"]);
    let completion = pick(&["completion_tokens", "output_tokens"]);
    if prompt == 0 && completion == 0 {
        return Value::Null;
    }
    json!({"prompt_tokens": prompt, "completion_tokens": completion})
}

/// `_response_model_echo`:响应声称服务的 model 名(空串 = 不可用)。
fn response_model_echo(data: &Value) -> String {
    match data.as_object().and_then(|m| m.get("model")) {
        Some(value) if !value.is_null() => py_str(value).unwrap_or_default().trim().to_owned(),
        _ => String::new(),
    }
}

/// `_normalize_model_name`:lower+strip;`-_./` 与空白折叠为单个 `-`;
/// 再剥首尾 `-`。
pub fn normalize_model_name(value: &Value) -> String {
    // str(value or ""):falsy(Null/空串/0)先落 "" 再 str()。
    let text = if py_bool(value) {
        py_str(value).unwrap_or_default()
    } else {
        String::new()
    };
    let text = text.trim().to_lowercase();
    if text.is_empty() {
        return String::new();
    }
    let mut collapsed = String::new();
    let mut last_was_sep = false;
    for ch in text.chars() {
        if matches!(ch, '-' | '_' | '.' | '/') || ch.is_whitespace() {
            if !last_was_sep {
                collapsed.push('-');
                last_was_sep = true;
            }
            continue;
        }
        collapsed.push(ch);
        last_was_sep = false;
    }
    collapsed.trim_matches('-').to_owned()
}

/// `_model_echo_matches`:相等,或一侧以另一侧 + "-" 为前缀且余串含数字
/// (容忍 `deepseek-v4-flash` → `deepseek-v4-flash-ga-260731` 带日期后缀,
/// 拒绝 `gpt-4o` ↔ `gpt-4o-mini` 换型号)。
pub fn model_echo_matches(requested: &str, echoed: &str) -> bool {
    if requested.is_empty() || echoed.is_empty() {
        return false;
    }
    let req = normalize_model_name(&Value::String(requested.to_owned()));
    let echo = normalize_model_name(&Value::String(echoed.to_owned()));
    if req == echo {
        return true;
    }
    let has_digit = |text: &str| text.chars().any(|c| c.is_ascii_digit());
    if let Some(rest) = echo.strip_prefix(&format!("{req}-")) {
        return has_digit(rest);
    }
    if let Some(rest) = req.strip_prefix(&format!("{echo}-")) {
        return has_digit(rest);
    }
    false
}

/// 实探一次 chat 请求;返回
/// `{"ok","status","endpoint","message","duration_ms","usage"}`。
/// `url_mode_full`(20261014_01):完整 URL 模式 —— 对 `base_url` 原样发
/// 探测体,端点不做任何拼接;缺省 false 维持 base 模式启发式补全。
/// `client_fingerprint`(20261016_03):载荷里的客户端指纹配置(添加页
/// 直传),实探与转发走同一出站头构造 —— preset 生效(修「客户端校验
/// 失败」类供应商探不通);passthrough 无入站上下文自然降级为仅注入
/// 认证;off 零变化。
pub async fn probe_model_protocol(
    base_url: &str,
    api_key: &str,
    frontend_protocol: &str,
    model: &str,
    proxy_config: Option<&Map<String, Value>>,
    url_mode_full: bool,
    client_fingerprint: Option<&Value>,
) -> Value {
    let Some(family) = frontend_protocol_family(frontend_protocol) else {
        return json!({
            "ok": false, "status": Value::Null, "endpoint": base_url,
            "message": "This API format does not support real chat probing.",
            "duration_ms": 0.0,
        });
    };
    let Some(client_protocol) = frontend_protocol_client(frontend_protocol) else {
        return json!({
            "ok": false, "status": Value::Null, "endpoint": base_url,
            "message": "This API format does not support real chat probing.",
            "duration_ms": 0.0,
        });
    };
    // 直构 Provider 实体(绕过 from_raw 的 id/base_url 校验——探针两者皆缺)。
    // openai_responses 声明原生 Responses 支持,端点保持 /v1/responses。
    let provider = Provider {
        id: "probe".to_owned(),
        name: "probe".to_owned(),
        base_url: base_url.to_owned(),
        api_key: api_key.to_owned(),
        protocol: ProviderProtocol::parse(family).unwrap_or(ProviderProtocol::OpenAi),
        enabled: true,
        models: Vec::new(),
        headers: Map::new(),
        timeout_seconds: 120.0,
        non_stream_timeout_seconds: None,
        max_retries: 0,
        max_in_flight: None,
        protocol_style: (frontend_protocol == "openai_responses")
            .then(|| "openai_responses".to_owned()),
        // 20261014_01:完整 URL 模式随载荷透传,实探与转发同 URL 语义。
        url_mode: if url_mode_full { UrlMode::Full } else { UrlMode::Base },
        billing: Map::new(),
        failure_threshold: None,
        cooldown_ms: None,
        accounts: Vec::new(),
        // 探针直构实体无凭据池语义,保持缺省(First)。
        credential_strategy: wr_domain::provider::ProviderCredentialStrategy::default(),
        pinned_account_id: None,
        context_windows: Default::default(),
        // 20260906_01:探针的代理来自载荷 `proxy` 字段(见 proxy_for),
        // 不经实体字段传递。
        proxy: None,
        // 20261016_03:客户端指纹随载荷透传(宽松解析,非法结构 = off)。
        client_fingerprint: wr_domain::provider::ClientFingerprintOverride::from_value(
            client_fingerprint,
        ),
    };
    let url = protocol_convert::endpoint_with_query(
        client_protocol,
        &provider,
        model,
        false,
        wr_domain::protocol::client_fingerprint::resolve_fingerprint_query(&provider, None)
            .as_deref(),
    );
    // 2026-10-17 脱敏审查:Gemini query 鉴权线的 url 带 `?key=<api_key>`,
    // 回显给管理面的 endpoint 只保留剥掉 query 的形态(凭据不进探测结果;
    // 实际请求仍用完整 url)。
    let endpoint_display = {
        let mut shown = url.clone();
        if let Some(cut) = shown.find(['?', '#']) {
            shown.truncate(cut);
        }
        shown
    };
    let body = match protocol_convert::upstream_payload(
        client_protocol,
        &probe_body(client_protocol, model),
        &provider,
    ) {
        Ok(body) => body,
        // 语义矩阵不支持组合(embeddings×anthropic/gemini 等):探针直接
        // 报不支持,不再把不兼容 payload 发往上游。
        Err(failure) => {
            return json!({
                "ok": false, "status": Value::Null, "endpoint": endpoint_display,
                "message": failure.message().to_owned(),
                "duration_ms": 0.0,
            });
        }
    };
    // 实探与转发同一出站头构造(20261016_03 批3):preset 生效;passthrough
    // 无入站上下文(inbound = None)自然降级为仅注入认证。
    let headers = protocol_convert::upstream_headers_with_fingerprint(&provider, None);

    let started = Instant::now();
    let client = probe_client(&probe_proxy_decision(proxy_config, base_url));
    let mut request = client.post(&url);
    for (key, value) in &headers {
        if let Some(text) = value.as_str() {
            request = request.header(key.as_str(), text);
        }
    }
    // workspace reqwest 未启用 json feature:手动序列化(upstream_headers
    // 已带 content-type: application/json)。序列化失败 → 构造请求失败分支。
    let body_bytes = serde_json::to_vec(&body).unwrap_or_else(|_| b"{}".to_vec());
    request = request.body(body_bytes);

    let result: Result<(Option<i64>, String, Value), String> = async {
        // 错误串经 reqwest_error_message 脱敏(2026-10-17):query 鉴权线
        // 的 key 不得随 reqwest 内嵌 URL 进探测错误消息。
        let response = request
            .send()
            .await
            .map_err(|e| wr_infra::upstream::reqwest_error_message(&e))?;
        let status = i64::from(response.status().as_u16());
        let text = response
            .text()
            .await
            .map_err(|e| wr_infra::upstream::reqwest_error_message(&e))?;
        // 内层 try/except:data 解析失败记 parse_error,流程继续。
        let parse_result: Result<Value, String> =
            serde_json::from_str(&text).map_err(|e| e.to_string());
        let (data, parse_error) = match parse_result {
            Ok(data) => (Some(data), None),
            Err(e) => (None, Some(e)),
        };
        Ok((
            Some(status),
            text,
            json!({"data": data, "parse_error": parse_error}),
        ))
    }
    .await;

    // 以下聚合逻辑与 Python 逐行对应;网络错误(HTTPError)路径单独处理。
    let (status, text, envelope) = match result {
        Ok(parts) => parts,
        Err(exc) => {
            return json!({
                "ok": false, "status": Value::Null, "endpoint": endpoint_display,
                "message": if exc.is_empty() { "Endpoint unreachable.".to_owned() } else { format!("Endpoint unreachable: {exc}") },
                "duration_ms": duration_ms(started),
                "usage": Value::Null,
            });
        }
    };

    let data = envelope.get("data").cloned().unwrap_or(Value::Null);
    let parse_error = envelope
        .get("parse_error")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let status_value = status.map(|s| json!(s)).unwrap_or(Value::Null);
    let status_num = status.unwrap_or(0);

    // error_detail:error.message(dict)→ error(str)→ parse_error →
    // (status≥400 且有 body)text[:200].strip()。
    let mut error_detail = String::new();
    if let Some(err) = data.as_object().and_then(|m| m.get("error")) {
        if let Some(obj) = err.as_object() {
            error_detail = obj
                .get("message")
                .filter(|v| py_bool(v))
                .and_then(py_str)
                .unwrap_or_default()
                .trim()
                .to_owned();
        } else if let Some(text) = err.as_str() {
            error_detail = text.trim().to_owned();
        }
    }
    if error_detail.is_empty() && !parse_error.is_empty() {
        error_detail = parse_error;
    }
    if error_detail.is_empty() && status_num >= 400 && !text.is_empty() {
        error_detail = text.chars().take(200).collect::<String>().trim().to_owned();
    }

    let status_message = if status_num < 400 {
        if error_detail.is_empty() {
            format!("HTTP {status_num}")
        } else {
            format!("HTTP {status_num}: {error_detail}")
        }
    } else if error_detail.is_empty() {
        format!("HTTP {status_num}")
    } else {
        format!("HTTP {status_num}: {error_detail}")
    };

    // 5 重判定门(全过才 ok)。
    let mut ok = status_num < 400;
    if ok && response_has_error(&data) {
        ok = false;
        if error_detail.is_empty() {
            error_detail = "Upstream returned an error object.".to_owned();
        }
    }
    if ok && response_output_text(client_protocol, &data).is_empty() {
        ok = false;
        error_detail =
            "Upstream returned an empty response — the model may not be available.".to_owned();
    }
    if ok && !response_has_usage(&data) {
        ok = false;
        error_detail =
            "Upstream did not report token usage — the model may not have been invoked.".to_owned();
    }
    let echoed = response_model_echo(&data);
    if ok && !echoed.is_empty() && !model_echo_matches(model, &echoed) {
        ok = false;
        error_detail = format!("Upstream echoed model '{echoed}' instead of requested '{model}'.");
    }

    let message = if ok {
        status_message
    } else {
        let tail = if error_detail.is_empty() {
            String::new()
        } else {
            format!(": {error_detail}")
        };
        if status_num >= 400 {
            format!("HTTP {status_num}{tail}")
        } else {
            format!("Model not available{tail}")
        }
    };
    let usage = response_usage(&data);
    // 20260906_01 §4.6:标注实探走的出口(url 随后被 json! 移动,先算标签)。
    let proxy_label = probe_proxy_label(proxy_config, &url);
    json!({
        "ok": ok,
        "status": status_value,
        "endpoint": endpoint_display,
        "message": message,
        "duration_ms": duration_ms(started),
        "usage": usage,
        "proxy": proxy_label,
    })
}

// ---------------------------------------------------------------------------
// checkProviderConnectivity(候选矩阵 + 聚合)
// ---------------------------------------------------------------------------

/// `check_connectivity`:candidate × protocol × model 矩阵,并发 6 保序。
pub async fn check_connectivity(
    payload: &Map<String, Value>,
    proxy_config: Option<&Map<String, Value>>,
) -> Result<Value, String> {
    let api_key = pick_api_key(payload);
    let mut models = str_list(py_iter(payload.get("models"))?);
    if models.is_empty() {
        models = vec!["default".to_owned()];
    }
    let declared_protocols = str_list(py_iter(payload.get("protocols"))?);

    let mut candidates: Vec<(String, Vec<String>, bool)> = Vec::new();
    for raw in py_iter(payload.get("candidates"))? {
        let Some(map) = raw.as_object() else {
            continue; // Python:非 dict 项跳过
        };
        let base_raw = ["baseUrl", "base_url"]
            .iter()
            .find_map(|key| map.get(*key).filter(|v| py_bool(v)).and_then(py_str))
            .unwrap_or_default()
            .trim()
            .to_owned();
        if base_raw.is_empty() {
            continue;
        }
        // 20261014_01:完整 URL 模式随候选透传 —— 探测对用户填写的地址
        // 原样发请求体(不剥尾斜杠、不拼端点);缺省 base 维持剥除。
        let url_mode_full = map
            .get("urlMode")
            .or_else(|| map.get("url_mode"))
            .and_then(Value::as_str)
            .map(|value| UrlMode::parse(value).is_full())
            .unwrap_or(false);
        let base_url = if url_mode_full {
            base_raw
        } else {
            base_raw.trim_end_matches('/').to_owned()
        };
        let protocols = {
            let own = str_list(py_iter(map.get("protocols"))?);
            if own.is_empty() {
                declared_protocols.clone()
            } else {
                own
            }
        };
        candidates.push((base_url, protocols, url_mode_full));
    }

    if candidates.is_empty() {
        return Ok(json!({
            "failed": models.iter().map(|model| json!({
                "message": "No endpoint candidates available.",
                "model": model,
                "protocols": [],
                "supported": false,
            })).collect::<Vec<_>>(),
            "passed": [],
            "probe": {"capabilities": [], "detectedProtocol": Value::Null, "models": [], "protocols": []},
            "results": [],
        }));
    }

    // 任务矩阵:candidate → protocol → model(与 Python 列表推导同序);
    // buffered(6) 保序(镜像 gather)。
    let api_key = std::sync::Arc::new(api_key);
    // 20260906_01 §4.6:载荷里的供应商级代理覆盖并入配置(缺省 = 全局默认)。
    let proxy_ptr: Option<std::sync::Arc<Map<String, Value>>> =
        with_payload_override(proxy_config, payload).map(std::sync::Arc::new);
    // 20261016_03:客户端指纹随载荷透传到每个实探任务(宽松解析在
    // probe_model_protocol 内,非法结构 = off)。
    let fingerprint_ptr: Option<std::sync::Arc<Value>> =
        payload
            .get("clientFingerprint")
            .filter(|value| value.is_object())
            .cloned()
            .map(std::sync::Arc::new);
    let tasks = candidates
        .iter()
        .flat_map(|(base_url, protocols, url_mode_full)| {
            protocols
                .iter()
                .flat_map(|protocol| {
                    models
                        .iter()
                        .map(move |model| {
                            (
                                base_url.clone(),
                                protocol.clone(),
                                model.clone(),
                                *url_mode_full,
                            )
                        })
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();

    let raw_rows: Vec<(String, String, String, Value)> = futures_util::stream::iter(tasks)
        .map(|(base_url, protocol, model, url_mode_full)| {
            let api_key = api_key.clone();
            let proxy_ptr = proxy_ptr.clone();
            let fingerprint_ptr = fingerprint_ptr.clone();
            async move {
                let raw = probe_model_protocol(
                    &base_url,
                    &api_key,
                    &protocol,
                    &model,
                    proxy_ptr.as_deref(),
                    url_mode_full,
                    fingerprint_ptr.as_deref(),
                )
                .await;
                let proto_result = json!({
                    "endpoint": raw.get("endpoint").cloned().unwrap_or(Value::Null),
                    "message": raw.get("message").cloned().unwrap_or(Value::Null),
                    "protocol": protocol,
                    // 20260906_01 §4.6:探测所用出口(直连/系统/预设名)。
                    "proxy": raw.get("proxy").cloned().unwrap_or(Value::Null),
                    "status": raw.get("status").cloned().unwrap_or(Value::Null),
                    "supported": raw.get("ok").and_then(Value::as_bool).unwrap_or(false),
                    "durationMs": raw.get("duration_ms").cloned().unwrap_or(json!(0.0)),
                    "usage": raw.get("usage").cloned().unwrap_or(Value::Null),
                });
                (base_url, protocol, model, proto_result)
            }
        })
        .buffered(PROBE_CONCURRENCY)
        .collect()
        .await;

    // 首个 supported 的 protocol(候选/模型序)。
    let best_protocol = raw_rows
        .iter()
        .find(|(_, _, _, proto_result)| {
            proto_result
                .get("supported")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .map(|(_, protocol, _, _)| Value::String(protocol.clone()))
        .unwrap_or(Value::Null);

    let mut results: Vec<Value> = Vec::new();
    let mut passed: Vec<Value> = Vec::new();
    let mut failed: Vec<Value> = Vec::new();
    let mut ok_models: Vec<String> = Vec::new();
    let mut seen_models: std::collections::HashSet<String> = Default::default();
    let mut capabilities: Vec<Value> = Vec::new();
    let mut probe_protocol_rows: Vec<Value> = Vec::new();
    let mut seen_caps: std::collections::HashSet<(String, String)> = Default::default();

    for (base_url, _protocol, model, proto_result) in raw_rows {
        let supported = proto_result
            .get("supported")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let result_row = json!({
            "message": proto_result.get("message").cloned().unwrap_or(Value::Null),
            "model": model,
            "protocols": [proto_result.clone()],
            "supported": supported,
        });
        let result_row_for_list = result_row.clone();
        results.push(result_row);
        if supported {
            passed.push(result_row_for_list);
            if seen_models.insert(model.clone()) {
                ok_models.push(model);
            }
        } else {
            failed.push(result_row_for_list);
        }
        let protocol_text = proto_result
            .get("protocol")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if seen_caps.insert((base_url.clone(), protocol_text.clone())) {
            capabilities.push(json!({
                "baseUrl": base_url,
                "source": "preset",
                "type": proto_result.get("protocol").cloned().unwrap_or(Value::Null),
            }));
            probe_protocol_rows.push(proto_result);
        }
    }

    Ok(json!({
        "failed": failed,
        "passed": passed,
        "probe": {
            "capabilities": capabilities,
            "detectedProtocol": best_protocol,
            "models": ok_models,
            "protocols": probe_protocol_rows,
        },
        "results": results,
    }))
}

// ---------------------------------------------------------------------------
// probeProviderCandidates(模型列表合并 + 最优候选)
// ---------------------------------------------------------------------------

/// `probe_candidates`:逐候选 GET /v1/models(并发 gather),合并模型与
/// capabilities,选首个 ok 候选为 best。
pub async fn probe_candidates(
    payload: &Map<String, Value>,
    proxy_config: Option<&Map<String, Value>>,
) -> Result<Value, String> {
    let api_key = pick_api_key(payload);
    let models = str_list(py_iter(payload.get("models"))?);
    let _ = &models; // Python 同样只解析不使用(保持签名一致)

    let raw_candidates: Vec<Value> = py_iter(payload.get("candidates"))?;
    if raw_candidates.is_empty() {
        return Ok(json!({
            "candidate": Value::Null,
            "probe": {"capabilities": [], "detectedProtocol": Value::Null, "models": [], "protocols": []},
        }));
    }

    // 非候选(非 dict)元素 → Python AttributeError → 500。
    for candidate in &raw_candidates {
        if !candidate.is_object() {
            return Err("'dict' object has no attribute 'get'".to_owned());
        }
    }

    let api_key = std::sync::Arc::new(api_key);
    // 20260906_01 §4.6:载荷里的供应商级代理覆盖并入配置(缺省 = 全局默认)。
    let proxy_ptr: Option<std::sync::Arc<Map<String, Value>>> =
        with_payload_override(proxy_config, payload).map(std::sync::Arc::new);
    let collected: Vec<Result<Value, String>> =
        futures_util::stream::iter(raw_candidates.iter().cloned())
            .map(|candidate| {
                let api_key = api_key.clone();
                let proxy_ptr = proxy_ptr.clone();
                async move { probe_candidate(&candidate, &api_key, proxy_ptr.as_deref()).await }
            })
            .buffered(PROBE_CONCURRENCY)
            .collect()
            .await;
    let mut results: Vec<Value> = Vec::with_capacity(collected.len());
    for item in collected {
        results.push(item?);
    }

    let mut all_models: Vec<Value> = Vec::new();
    let mut all_capabilities: Vec<Value> = Vec::new();
    let mut detected_protocol = Value::Null;
    let mut best_candidate: Option<Value> = None;
    let mut best_protocols: Vec<Value> = Vec::new();

    for (idx, result) in results.iter().enumerate() {
        if !result.get("ok").and_then(Value::as_bool).unwrap_or(false) {
            continue;
        }
        if let Some(models) = result.get("models").and_then(Value::as_array) {
            all_models.extend(models.clone());
        }
        let candidate = &raw_candidates[idx];
        let map = candidate.as_object().expect("checked above");
        // 2026-10-14 整治:同 probe_candidate 统一走 provider_base_url
        // (原 baseUrl→base_url 链缺 api_base_url;null 键由统一函数按
        // 假值跳过,展示为空串而非 Python 的 "None" 字面)。
        let base_raw = wr_domain::provider::provider_base_url(map).unwrap_or_default();
        let base_url = base_raw.trim_end_matches('/').to_owned();
        let declared = map
            .get("protocols")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut protocols: Vec<Value> = Vec::new();
        for proto in &declared {
            protocols.push(json!({
                "endpoint": base_url,
                "message": if result.get("ok").and_then(Value::as_bool).unwrap_or(false) {
                    "Endpoint responded successfully."
                } else {
                    "Endpoint unreachable."
                },
                "protocol": proto,
                "supported": result.get("ok").and_then(Value::as_bool).unwrap_or(false),
            }));
        }
        // `(declared or [None])[0] if declared else ""`:declared 非空时
        // 首元素原样采用(即使 falsy),空时 ""。
        let capability_type = if declared.is_empty() {
            Value::String(String::new())
        } else {
            declared.first().cloned().unwrap_or(Value::Null)
        };
        all_capabilities.push(json!({
            "baseUrl": base_url,
            "source": "detected",
            "type": capability_type,
        }));
        if best_candidate.is_none() {
            best_candidate = Some(candidate.clone());
            best_protocols = protocols;
            // `(declared or [None])[0] if declared else None`。
            detected_protocol = declared.first().cloned().unwrap_or(Value::Null);
        }
    }

    // list(dict.fromkeys(all_models)):保序去重。
    let mut seen = std::collections::HashSet::new();
    let models_dedup: Vec<Value> = all_models
        .into_iter()
        .filter(|m| seen.insert(m.to_string()))
        .collect();

    Ok(json!({
        "candidate": best_candidate.unwrap_or(Value::Null),
        "probe": {
            "capabilities": all_capabilities,
            "detectedProtocol": detected_protocol,
            "models": models_dedup,
            "protocols": best_protocols,
        },
    }))
}

// ---------------------------------------------------------------------------
// fetchProviderModels(方案 20261017_01:添加模型弹窗「快速获取」)
// ---------------------------------------------------------------------------

/// Gemini 列表分页:单页上限与翻页硬顶(token 防环)。
const MODEL_LIST_PAGE_SIZE: i64 = 1000;
const MODEL_LIST_MAX_PAGES: usize = 50;

/// 列表请求的认证形态(协议驱动)。
#[derive(Clone, Copy, PartialEq)]
enum ListAuth {
    /// `Authorization: Bearer`(OpenAI 系;anthropic 的 401/403 回落)。
    Bearer,
    /// `x-api-key` + `anthropic-version`。
    Anthropic,
    /// `x-goog-api-key`。
    Gemini,
}

impl ListAuth {
    fn headers(self, api_key: &str) -> Vec<(&'static str, String)> {
        match self {
            // 缺密钥时不带认证头(镜像 probe_candidate 的条件注入)。
            Self::Bearer if api_key.is_empty() => Vec::new(),
            Self::Bearer => vec![("authorization", format!("Bearer {api_key}"))],
            Self::Anthropic => vec![
                ("x-api-key", api_key.to_owned()),
                ("anthropic-version", "2023-06-01".to_owned()),
            ],
            Self::Gemini => vec![("x-goog-api-key", api_key.to_owned())],
        }
    }
}

/// 协议 → 模型列表端点候选(依次试,首个 2xx 且可解析即停)。入参
/// `base_url` 已由调用方定形(base 模式剥端点后缀 / full 模式原样)。
fn model_list_candidates(base_url: &str, protocol: &str) -> Vec<String> {
    let base = base_url.trim_end_matches('/');
    // Gemini 家族:含 /v1(含 /v1beta)→ 同版本段下 /models;根 →
// /v1beta/models。
    if protocol.starts_with("gemini") {
        if base.contains("/v1") {
            return vec![format!("{base}/models")];
        }
        return vec![format!("{base}/v1beta/models")];
    }
    // OpenAI/Anthropic 系共用端点链(差异只在认证头)。
    if base.ends_with("/v1") {
        return vec![format!("{base}/models")];
    }
    vec![format!("{base}/v1/models"), format!("{base}/models")]
}

/// 解析 Gemini 分页页:`models[].name` 剥 `models/` 前缀;返回
/// (模型名, nextPageToken)。响应不是分页形态(缺 `models` 数组)时
/// 回落通用抽取(`data[]`/`models[]` 的 id/model/name)。
fn parse_gemini_models_page(page: &Value) -> (Vec<String>, Option<String>) {
    let mut models = Vec::new();
    if let Some(list) = page.get("models").and_then(Value::as_array) {
        for item in list {
            if let Some(name) = item.get("name").and_then(Value::as_str) {
                let name = name.strip_prefix("models/").unwrap_or(name).trim();
                if !name.is_empty() {
                    models.push(name.to_owned());
                }
            }
        }
        let token = page
            .get("nextPageToken")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .map(str::to_owned);
        return (models, token);
    }
    if let Some(items) = extract_model_items(page) {
        for item in items {
            if let Some(name) = item.as_str().map(str::trim).filter(|n| !n.is_empty()) {
                models.push(name.to_owned());
            }
        }
    }
    (models, None)
}

/// 保序去重(镜像 probe_candidates 的 list(dict.fromkeys))。
fn dedup_preserve_order(models: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    models
        .into_iter()
        .filter(|model| seen.insert(model.clone()))
        .collect()
}

/// RPC `fetchProviderModels`(方案 20261017_01):添加模型弹窗按当前
/// 表单/供应商上下文拉取上游全量模型列表。载荷
/// `{baseUrl, protocol, urlMode?, apiKey?, proxy?}`,返回
/// `{"ok","status","models","message","durationMs","proxy"}`。
///
/// 与 [`probe_candidate`] 的差异:协议感知认证(anthropic `x-api-key`,
/// 全链 401/403 后回落 `Bearer`;gemini `x-goog-api-key` + 分页),
/// `ok` 以「拿到非空模型列表」为准(200 空列表/不可解析体对拾取器
/// 同样是失败,message 带原因)。
pub async fn fetch_provider_models(
    payload: &Map<String, Value>,
    proxy_config: Option<&Map<String, Value>>,
) -> Result<Value, String> {
    let base_raw = payload
        .get("baseUrl")
        .or_else(|| payload.get("base_url"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    let protocol = payload
        .get("protocol")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    let url_mode_full = payload
        .get("urlMode")
        .or_else(|| payload.get("url_mode"))
        .and_then(Value::as_str)
        .map(|value| UrlMode::parse(value).is_full())
        .unwrap_or(false);
    let api_key = pick_api_key(payload);
    if base_raw.is_empty() {
        return Ok(json!({
            "ok": false, "status": Value::Null, "models": [],
            "message": "No base URL.", "durationMs": 0.0,
        }));
    }

    // base 模式剥已知端点后缀(与 probe_candidate 同款,DeepSeek 形态
    // 的 base 也能重建列表端点);full 模式地址原样(仅去尾斜杠防 //)
    // —— 完整地址模式的列表端点无权威规则,按同前缀猜。
    let base_url = if url_mode_full {
        base_raw.trim_end_matches('/').to_owned()
    } else {
        strip_endpoint_suffix(&base_raw)
    };
    let merged_proxy = with_payload_override(proxy_config, payload);
    let client = probe_client(&probe_proxy_decision(merged_proxy.as_ref(), &base_url));
    let started = Instant::now();
    let mut status: Option<i64> = None;
    let mut models: Vec<String> = Vec::new();
    let mut message = "Endpoint unreachable.".to_owned();

    if protocol.starts_with("gemini") {
        // 分页拉取:首个非 2xx/不可解析页即止;同 token 回显防环。
        let url = model_list_candidates(&base_url, &protocol)
            .into_iter()
            .next()
            .expect("gemini candidates are never empty");
        let mut page_token = String::new();
        for _ in 0..MODEL_LIST_MAX_PAGES {
            let mut request = client
                .get(&url)
                .header("accept", "application/json")
                .query(&[("pageSize", MODEL_LIST_PAGE_SIZE.to_string())]);
            for (key, value) in ListAuth::Gemini.headers(&api_key) {
                request = request.header(key, value);
            }
            if !page_token.is_empty() {
                request = request.query(&[("pageToken", page_token.clone())]);
            }
            let response = match request.send().await {
                Ok(response) => response,
                Err(_) => break,
            };
            let code = i64::from(response.status().as_u16());
            status = Some(code);
            if code >= 400 {
                message = format!("Endpoint returned HTTP {code}.");
                break;
            }
            let body_text = response.text().await.unwrap_or_default();
            let Ok(page) = serde_json::from_str::<Value>(&body_text) else {
                message = "Endpoint responded but the body was not a model list.".to_owned();
                break;
            };
            let (page_models, next_token) = parse_gemini_models_page(&page);
            models.extend(page_models);
            match next_token {
                Some(token) if token != page_token => page_token = token,
                _ => break,
            }
        }
    } else {
        // 认证遍历:anthropic 先 x-api-key,全链 401/403 → 回落 Bearer
// (兼容只认 Bearer 的中转站);openai 单 Bearer。
        let passes: &[ListAuth] = if protocol.starts_with("anthropic") {
            &[ListAuth::Anthropic, ListAuth::Bearer]
        } else {
            &[ListAuth::Bearer]
        };
        let urls = model_list_candidates(&base_url, &protocol);
        'passes: for auth in passes {
            // 本轮任一 401/403 即视为认证被拒(后续候选可能是 404 等
            // 无关失败,不能覆盖这个事实)→ 下一轮换 Bearer。
            let mut saw_auth_rejection = false;
            for url in &urls {
                let mut request = client.get(url).header("accept", "application/json");
                for (key, value) in auth.headers(&api_key) {
                    request = request.header(key, value);
                }
                let response = match request.send().await {
                    Ok(response) => response,
                    Err(_) => continue,
                };
                let code = i64::from(response.status().as_u16());
                status = Some(code);
                if code >= 400 {
                    message = format!("Endpoint returned HTTP {code}.");
                    if matches!(code, 401 | 403) {
                        saw_auth_rejection = true;
                    }
                    continue;
                }
                let body_text = response.text().await.unwrap_or_default();
                let Ok(data) = serde_json::from_str::<Value>(&body_text) else {
                    message = "Endpoint responded but the body was not a model list.".to_owned();
                    continue;
                };
                if let Some(items) = extract_model_items(&data) {
                    models.extend(items.into_iter().filter_map(|item| {
                        item.as_str()
                            .map(str::trim)
                            .filter(|name| !name.is_empty())
                            .map(str::to_owned)
                    }));
                    break 'passes;
                }
                message = "Endpoint responded but the body was not a model list.".to_owned();
            }
            if !saw_auth_rejection {
                break;
            }
        }
    }

    let models = dedup_preserve_order(models);
    let ok = !models.is_empty();
    if ok {
        message = format!("Fetched {} models.", models.len());
    }

    Ok(json!({
        "ok": ok,
        "status": status.map(|s| json!(s)).unwrap_or(Value::Null),
        "models": models,
        "message": message,
        "durationMs": duration_ms(started),
        // 方案 20260906_01 §4.6 同款:本次获取实际走的出口(凭据已掩码)。
        "proxy": probe_proxy_label(merged_proxy.as_ref(), &base_url),
    }))
}

// ---------------------------------------------------------------------------
// testProxyPreset(方案 20260906_01 §4.9a)
// ---------------------------------------------------------------------------

/// 内置检测目标:连通性探针(204 无正文,最轻)+ 国际两大 LLM 上游
/// (验证代理能承载 TLS/SNI)+ 国内主流 LLM 上游(DeepSeek/智谱/百炼/Kimi;
/// 规则型代理可能只对部分域名走代理,故多测几个,2026-09-13)。
/// 用户可在设置页代理章节追加自定义目标(config.proxy.testTargets 落盘),
/// 检测时经 RPC `targets` 参数透传;此处列表是缺省与「内置」徽标的口径。
const PRESET_TEST_TARGETS: &[&str] = &[
    "https://www.gstatic.com/generate_204",
    "https://api.openai.com",
    "https://api.anthropic.com",
    "https://api.deepseek.com",
    "https://open.bigmodel.cn",
    "https://dashscope.aliyuncs.com",
    "https://api.moonshot.cn",
];

/// 自定义检测目标(RPC `targets` 参数透传):最多 12 个,仅接受 http/https,
/// 去重;缺省/全部非法 → None(回落内置 [`PRESET_TEST_TARGETS`])。
fn parse_custom_targets(payload: &Map<String, Value>) -> Option<Vec<String>> {
    let raw = payload.get("targets")?.as_array()?;
    let mut out: Vec<String> = Vec::new();
    for value in raw {
        let Some(url) = value
            .as_str()
            .map(str::trim)
            .filter(|url| url.starts_with("http://") || url.starts_with("https://"))
        else {
            continue;
        };
        if !out.iter().any(|item| item == url) {
            out.push(url.to_owned());
        }
        if out.len() >= 12 {
            break;
        }
    }
    Some(out).filter(|list| !list.is_empty())
}

/// RPC `testProxyPreset`:用**给定预设**(含设置页里尚未保存的编辑值)
/// 对 [`PRESET_TEST_TARGETS`] 逐个探测。
///
/// 入参:`{"preset": {id?, name?, server, port, username?, password?}}`。
/// 返回:`{"ok", "proxy", "targets": [{"url","ok","status","latencyMs","error"}]}`
/// —— 拿到任何 HTTP 响应即判该目标 `ok`(代理转发成功;404/204 都算通),
/// 连不上代理才是 `false`(多目标探测口径)。
/// `proxy` 是掩码后的来源标签,便于用户确认「测的就是将来的走法」。
pub async fn test_proxy_preset(payload: &Map<String, Value>) -> Result<Value, String> {
    let preset = ProxyPreset::from_value(payload.get("preset").unwrap_or(&Value::Null))
        .ok_or_else(|| "'preset' must be an object with a non-empty id".to_owned())?;
    let proxy_url = preset.to_proxy_url().ok_or_else(|| {
        "preset server/port is invalid (scheme must be http/https/socks5/socks5h, port 1-65535)"
            .to_owned()
    })?;
    let decision = ProxyDecision::Url(proxy_url);
    // 合成一张只含该预设的配置,复用同一套标签逻辑(凭据在标签里被掩掉)。
    let synthetic = json!({
        "presets": [{
            "id": preset.id,
            "name": preset.name,
            "server": preset.server,
            "port": preset.port,
            "username": preset.username,
            "password": preset.password,
        }]
    })
    .as_object()
    .cloned()
    .unwrap_or_default();
    let label = describe_proxy_source(Some(&synthetic), &decision);
    let client = probe_client(&decision);

    let target_urls = parse_custom_targets(payload).unwrap_or_else(|| {
        PRESET_TEST_TARGETS
            .iter()
            .map(|url| url.to_string())
            .collect()
    });
    let mut targets = Vec::with_capacity(target_urls.len());
    let mut ok = true;
    for url in &target_urls {
        let started = Instant::now();
        let row = match client.get(url.as_str()).send().await {
            Ok(response) => json!({
                "url": url,
                "ok": true,
                "status": i64::from(response.status().as_u16()),
                "latencyMs": duration_ms(started),
                "error": Value::Null,
            }),
            Err(error) => {
                ok = false;
                let message = if error.is_timeout() {
                    "Proxy connect timeout.".to_owned()
                } else if error.is_connect() {
                    "Proxy unreachable.".to_owned()
                } else {
                    wr_infra::upstream::reqwest_error_message(&error)
                };
                json!({
                    "url": url,
                    "ok": false,
                    "status": error.status().map(|s| json!(s.as_u16())).unwrap_or(Value::Null),
                    "latencyMs": Value::Null,
                    "error": message,
                })
            }
        };
        targets.push(row);
    }
    Ok(json!({"ok": ok, "proxy": label, "targets": targets}))
}

#[cfg(test)]
mod tests;
