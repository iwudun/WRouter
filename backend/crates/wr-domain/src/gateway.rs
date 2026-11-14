//! 网关路由与重试策略(镜像 Python `domain/gateway/policies.py` 与
//! `domain/routing/failure_classifier.py`,R2 子集)。
//!
//! 纯函数/常量,无 I/O。三层重试/回退中的「同 provider 重试」与
//! 「候选链」判定在此;「执行计划 attempts」(Router.fallback)随 R3 迁入
//! `routing.rs`。注意熔断判定不含 501(方案 #14 C4):501 在
//! [`RETRY_STATUS_CODES`] 里可重试,但熔断默认 failure_status_codes
//! (wr-infra config 层解析)不含 501。

use crate::provider::Provider;
use serde_json::{Map, Value};

/// 永不重试的状态码(请求本身错误,重试只烧配额)。
pub const NO_RETRY_STATUS_CODES: &[u16] = &[400, 401, 403, 404, 422];

/// 触发同 provider 重试的状态码(408/409/429/5xx,含 501)。
pub const RETRY_STATUS_CODES: &[u16] = &[408, 409, 429, 500, 501, 502, 503, 504];

/// `classifyRouteFailure`:状态码 → 4 类。`status == 0` 是网络层失败
/// (连接/超时/TLS)哨兵,本质是瞬态,归 retryable。
pub fn classify_route_failure(status: u16) -> &'static str {
    match status {
        0 => "retryable",
        429 => "rate-limit",
        408 | 409 => "retryable",
        s if s >= 500 => "server",
        _ => "client",
    }
}

/// 是否对同一 provider 重试(镜像 `should_retry`)。
/// `status == 0` 表示网络层错误(连接失败/读超时)。
pub fn should_retry(status: u16, attempt: u32, max_retries: u32) -> bool {
    if attempt >= max_retries {
        return false;
    }
    if NO_RETRY_STATUS_CODES.contains(&status) {
        return false;
    }
    matches!(
        classify_route_failure(status),
        "rate-limit" | "retryable" | "server"
    )
}

/// 是否放弃当前 provider 候选链回退(镜像 `should_fallback_with_mode`)。
/// `off`/`retry` 模式仅对 rate-limit/retryable/server 回退(4xx client
/// 直接透传给客户端);`model-chain` 对任意 4xx/5xx 回退。
pub fn should_fallback_mode(status: u16, mode: &str) -> bool {
    if status < 400 {
        return false;
    }
    if mode == "model-chain" {
        return true;
    }
    matches!(
        classify_route_failure(status),
        "rate-limit" | "retryable" | "server"
    )
}

/// 指数退避 `1s * 2^n`,上限 30s(沿用参考实现的取值)。
pub fn backoff_delay(attempt: u32) -> f64 {
    (1.0_f64 * 2.0_f64.powi(attempt as i32)).min(30.0)
}

/// Retry-After 解析:秒数或 HTTP-date,返回毫秒;None 表示无/不可解析。
pub fn parse_retry_after_ms(value: Option<&str>) -> Option<f64> {
    let trimmed = value?.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(seconds) = trimmed.parse::<f64>() {
        if seconds >= 0.0 {
            return Some(seconds * 1000.0);
        }
        return None;
    }
    // HTTP-date(RFC 1123,如 "Wed, 21 Oct 2015 07:28:00 GMT")。
    use chrono::{DateTime, Utc};
    let parsed: DateTime<Utc> = chrono::DateTime::parse_from_rfc2822(trimmed).ok()?.into();
    let delta_ms = (parsed - Utc::now()).num_milliseconds();
    Some((delta_ms as f64).max(0.0))
}

/// 下次重试前的等待与来源标签(方案 20261015_01 P1-1;替代
/// `retry_after_delay`,Python 镜像无抖动,登记 parity 偏差):
///
/// - 上游 Retry-After 为正 → **精确遵守**(上游明确的等待指令,不抖动),
///   夹在 [1, 60000] ms(沿用参考实现的取值);
/// - 否则指数退避 × `[0.5, 1.5)` 均匀抖动 —— 同一 provider 抖动时 N 个
///   并发请求不再在同一时刻集中重打(主流实现均带 jitter)。
///
/// 返回 `(等待秒数, 落库 wait_source)`;调用方**先算后睡**,尝试行的
/// wait_ms 与实际等待同源。`rng` 注入 [0,1) 均匀随机(单测固定)。
pub fn retry_wait(
    header_value: Option<&str>,
    attempt: u32,
    rng: &mut impl FnMut() -> f64,
) -> (f64, &'static str) {
    if let Some(ms) = parse_retry_after_ms(header_value)
        && ms > 0.0
    {
        return (ms.clamp(1.0, 60_000.0) / 1000.0, "retry-after");
    }
    (backoff_delay(attempt) * (0.5 + rng()), "backoff")
}

/// 429 的 Retry-After 超过遵守上限(60s)(方案 20261015_01 P1-2):
/// 本账号在窗口期内不可用 —— 原地等满 clamp 的 60s 再打同一把刚被
/// 限流的 key 大概率再 429,白烧 attempt。凭据池冷却已按 Retry-After
/// 全值记录,应直接推进下一候选。同类实现对 >60s 一律不遵守(走退避);
/// 本方案更进一步:换候选而非原地退避。仅 429 消费(503 等保留原语义)。
pub fn retry_after_exceeds_cap(header_value: Option<&str>) -> bool {
    parse_retry_after_ms(header_value).is_some_and(|ms| ms > 60_000.0)
}

/// 凭据类失败(方案 20261015_01 P1-3):当前请求内**推进候选**而非终态
/// 透传 —— 401/403 是账号 key 失效/无权限,凭据池已记冷却,换号/换
/// 供应商有意义(多部署 401 特判、渠道直切同理);
/// 404/422 是模型不存在/请求形状错误,换候选无意义,保持终态。
pub const CREDENTIAL_FAILURE_STATUS_CODES: &[u16] = &[401, 403];

// ---------------------------------------------------------------------------
// 凭据池辅助(方案 20260905_02 K4/K8;Rust 新增能力,无 Python 镜像)
// ---------------------------------------------------------------------------

/// K4 会话键提取(策略 `sticky` 的前置),提取优先级拿到即停:
/// 1. 请求头 `x-claude-code-session-id` → `x-session-id` → `x-request-id`
///    (头匹配大小写不敏感);
/// 2. 兜底:请求体首条 `user` 消息内容字符串化后 SHA-256 前 16 hex;
/// 3. 无 user 消息(或内容为空)→ None。
///
/// 结果只随调用链传递到选号点,不落库。`headers` 为 `(名, 值)` 对;
/// `body` 为请求体对象(messages 形态;其他请求形态返回 None)。
pub fn extract_session_key(
    headers: &[(String, String)],
    body: Option<&Map<String, Value>>,
) -> Option<String> {
    for name in ["x-claude-code-session-id", "x-session-id", "x-request-id"] {
        if let Some((_, value)) = headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            && !value.trim().is_empty()
        {
            return Some(value.trim().to_owned());
        }
    }
    let body = body?;
    let messages = body.get("messages")?.as_array()?;
    let content = messages
        .iter()
        .filter_map(Value::as_object)
        .find(|message| message.get("role").and_then(Value::as_str) == Some("user"))
        .and_then(|message| message.get("content"))?;
    let text = match content {
        Value::String(text) => text.as_str(),
        // 多模态块等非字符串内容:以 JSON 文本近似(Python str() 形态口径)。
        other => return hash_session_fallback(&crate::json_py::py_json_dumps(other)),
    };
    hash_session_fallback(text)
}

/// 兜底哈希:SHA-256 前 16 hex;空串视为无(避免空内容互相误粘)。
fn hash_session_fallback(text: &str) -> Option<String> {
    if text.is_empty() {
        return None;
    }
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(text.as_bytes());
    Some(hex_encode_lower(&digest[..8]))
}

fn hex_encode_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// K2 sticky 加权 rendezvous(Efraimidis-Spirakis)的确定性哈希:
/// `u = SHA-256(session_key || account_id)` 前 8 字节归一 [0,1)。
pub fn sticky_score_u(session_key: &str, account_id: &str) -> f64 {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(session_key.as_bytes());
    hasher.update(account_id.as_bytes());
    let digest = hasher.finalize();
    // 53 位精度,值域 [0,1);不可能为 0 的概率可忽略,powf(1/w) 安全。
    (u64::from_be_bytes(digest[..8].try_into().unwrap()) >> 11) as f64 / (1u64 << 53) as f64
}

// ---------------------------------------------------------------------------
// 成本节省辅助(方案 20260905_03 S6/S7;Rust 新增,无 Python 镜像)
// ---------------------------------------------------------------------------

/// 请求是否携带绑定上游身份的状态引用(previous_response_id / conversation
/// / item 文件等)。缓存、跨候选回退和续写都必须先过这道门。
pub fn has_stateful_reference(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            ["previous_response_id", "conversation", "conversation_id"]
                .iter()
                .any(|key| map.contains_key(*key))
                || map.values().any(has_stateful_reference)
        }
        Value::Array(items) => items.iter().any(has_stateful_reference),
        _ => false,
    }
}

/// S6:网关响应缓存键(精确键,方案 §4.4)。
///
/// 门控:**显式 `temperature === 0` 才可缓存** —— 省略 temperature 视为
/// 「供应商可能非确定性」,不缓存(方案 §7.1:宁可少命中,不可错误命中)。
/// 键 = SHA-256(client_protocol + model + 归一化 messages + temperature +
/// top_p + tools 摘要);工具集变化必须 miss;protocol 参与
/// 键(缓存值是按客户端协议解码后的 JSON,跨协议重放形状不安全)。
pub fn response_cache_key(protocol: &str, payload: &Value) -> Option<String> {
    // 有状态引用绑定供应商/账号/会话:缓存重放无法证明归属,直接不缓存。
    if has_stateful_reference(payload) {
        return None;
    }
    let payload = payload.as_object()?;
    // 显式零:数字 0 / 0.0;字符串 "0" 不算(客户端语义不明,宁不缓存)。
    let is_zero = payload
        .get("temperature")
        .and_then(Value::as_f64)
        .is_some_and(|value| value == 0.0);
    if !is_zero {
        return None;
    }
    let mut parts = serde_json::Map::new();
    parts.insert("protocol".to_owned(), Value::String(protocol.to_owned()));
    if let Some(model) = payload.get("model").and_then(Value::as_str) {
        parts.insert("model".to_owned(), Value::String(model.to_owned()));
    }
    // messages 归一化:serde 解析后的 Value 序列化天然与请求体的空白/键序
    // (BTreeMap)无关;数组语义序保留。
    if let Some(messages) = payload.get("messages") {
        parts.insert("messages".to_owned(), messages.clone());
    }
    if let Some(top_p) = payload.get("top_p") {
        parts.insert("top_p".to_owned(), top_p.clone());
    }
    if let Some(tools) = payload.get("tools") {
        parts.insert("tools".to_owned(), tools.clone());
    }
    let text = serde_json::to_string(&Value::Object(parts)).ok()?;
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(text.as_bytes());
    Some(hex_encode_lower(&digest))
}

/// S7:400 响应正文是否「上下文超限」(方案 §4.5:`RouteFailureClass`
/// 从 400 类拆出 ContextOverflow 的判定依据)。模式覆盖三家方言:
/// Anthropic "prompt is too long" / OpenAI "context_length_exceeded" /
/// Gemini "exceeds the maximum"。仅在 status==400 时由调用方调用。
pub fn is_context_overflow_body(body: &str) -> bool {
    if body.is_empty() {
        return false;
    }
    static PATTERNS: std::sync::OnceLock<Vec<regex::Regex>> = std::sync::OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        [
            r"prompt is too long",
            r"context[_ ]?length(_exceeded)?",
            r"maximum context length",
            r"exceeds the maximum (number of tokens|context)",
            r"too many (input )?tokens",
            r"reduce the (length|size) of (the|your) (prompt|messages)",
            r"input length exceeds",
            r"请求的(上下文|内容)长度超过",
        ]
        .iter()
        .filter_map(|pattern| regex::Regex::new(&format!("(?i){pattern}")).ok())
        .collect()
    });
    patterns.iter().any(|pattern| pattern.is_match(body))
}

/// K8:429 无 `Retry-After` 头时从错误正文提取限流重置时长(毫秒)。
/// 正则族按 reset 文本解析思路:
/// 相对时长短语(en/zh,try again in / retry after / reset in / 频繁-等待)
/// 优先;其次 `reset` 语境下的 ISO-8601 时间戳(取 `时间戳 - now`,只在
/// 未来时有效)。解析失败一律 None,调用方静默跳过。
pub fn parse_rate_limit_reset_ms(body: &str) -> Option<i64> {
    if body.is_empty() {
        return None;
    }
    // 相对时长:数值(可带小数)+ 单位。`(\d+(?:\.\d+)?)\s*(单位)`。
    let duration_re = regex::Regex::new(
        r"(?i)(\d+(?:\.\d+)?)\s*(seconds?|secs?|s|minutes?|mins?|m|hours?|h|秒|分钟|小时)",
    )
    .ok()?;
    // 短语锚定:时长必须出现在限流语境短语附近,避免误吃正文里的
    // 其他数字(如 "used 123 minutes")。
    let anchored = regex::Regex::new(
        r"(?i)(try\s*again\s*in|retry[-\s]*after|resets?\s*in|come\s*back\s*in|wait\s*(?:for)?|请.*?等待|.*?频繁.*?等待|.*?重试.*)",
    )
    .ok()?;
    let unit_ms = |unit: &str| -> f64 {
        let unit = unit.to_ascii_lowercase();
        match unit.as_str() {
            "h" | "hour" | "hours" | "小时" => 3_600_000.0,
            "m" | "min" | "mins" | "minute" | "minutes" | "分钟" => 60_000.0,
            _ => 1_000.0, // s / sec / secs / second / seconds / 秒
        }
    };
    for cap in duration_re.captures_iter(body) {
        let whole = cap.get(0)?.as_str();
        let start = cap.get(0)?.start();
        // 语境短语须出现在时长短语之前的一段窗口内(120 字符);
        // 窗口起点回退到 UTF-8 字符边界(中文正文下 120 字节可能劈开
        // 多字节字符,直接切片会 panic)。
        let raw_from = start.saturating_sub(120);
        let from = (raw_from..=start)
            .find(|&i| body.is_char_boundary(i))
            .unwrap_or(start);
        let prefix = &body[from..start + whole.len()];
        if !anchored.is_match(prefix) {
            continue;
        }
        let amount: f64 = cap[1].parse().ok()?;
        let ms = amount * unit_ms(&cap[2]);
        if ms > 0.0 {
            return Some(ms.round() as i64);
        }
    }
    // ISO-8601 时间戳(reset 语境):`resets at 2026-09-06T12:00:00Z`。
    if regex::Regex::new(r"(?i)reset").ok()?.is_match(body) {
        let iso_re =
            regex::Regex::new(r"\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:\.\d+)?Z?").ok()?;
        if let Some(hit) = iso_re.find(body) {
            let parsed = chrono::DateTime::parse_from_rfc3339(&format!(
                "{}Z",
                hit.as_str().trim_end_matches('Z')
            ))
            .ok()?;
            let delta = parsed.timestamp_millis() - chrono::Utc::now().timestamp_millis();
            if delta > 0 {
                return Some(delta);
            }
        }
    }
    None
}

/// 无可用 provider 的路由错误(镜像 RoutingError)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingError(pub String);

impl std::fmt::Display for RoutingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for RoutingError {}

/// 选择第一个能服务 `model` 的 provider(镜像 `choose_provider`)。
/// 过滤 enabled + 模型白名单(`models` 为空或含 `"*"` 视为任意);
/// `preferred` 排前;无候选时返回 RoutingError。
pub fn choose_provider<'a>(
    providers: &'a [Provider],
    model: &str,
    preferred: &str,
) -> Result<&'a Provider, RoutingError> {
    choose_provider_from(providers.iter(), model, preferred)
}

/// [`choose_provider`] 的引用池变体:调用方可先按熔断状态过滤
/// (镜像 Python `_select_provider` 的 breaker 过滤)。
pub fn choose_provider_from<'a, I>(
    providers: I,
    model: &str,
    preferred: &str,
) -> Result<&'a Provider, RoutingError>
where
    I: IntoIterator<Item = &'a Provider>,
{
    let mut candidates: Vec<&Provider> = providers
        .into_iter()
        .filter(|p| {
            p.enabled
                && (p.models.is_empty()
                    || p.models.iter().any(|m| m == model)
                    || p.models.iter().any(|m| m == "*"))
        })
        .collect();
    if !preferred.is_empty() {
        candidates.sort_by_key(|p| if p.id == preferred { 0 } else { 1 });
    }
    candidates
        .first()
        .copied()
        .ok_or_else(|| RoutingError(format!("No enabled provider supports model: {model}")))
}

// ---------------------------------------------------------------------------
// 熔断器纯状态机(R4.8;镜像 Python domain/gateway/circuit_breaker.py)
// ---------------------------------------------------------------------------

/// 熔断状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerStatus {
    Closed,
    Open,
    HalfOpen,
}

impl BreakerStatus {
    /// Python `CircuitBreakerStatus.value` 形态(`half_open` 带下划线)。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::Open => "open",
            Self::HalfOpen => "half_open",
        }
    }
}

/// 一个 provider 的熔断状态(时间字段是毫秒,与持久化列同名同型)。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CircuitBreakerState {
    pub consecutive_failures: i64,
    pub opened_until_ms: i64,
    pub last_state_change_ms: i64,
}

/// 状态码 → provider 健康档(`provider-health.ts:classifyProviderResponseStatus`)。
/// 408/429/5xx 为 `degraded`,其余有状态码为 `healthy`,无状态码 `unknown`
/// (连接失败由调用方判 `down`)。
pub fn classify_status_to_health(status: Option<i64>) -> &'static str {
    match status {
        None => "unknown",
        Some(code) if code == 408 || code == 429 || code >= 500 => "degraded",
        Some(_) => "healthy",
    }
}

/// 由状态 + 时钟推出当前档:`opened_until_ms` 未到期为 OPEN,已到期为
/// HALF_OPEN(内存里过渡,下一个试探请求解决),从未开闸为 CLOSED。
pub fn compute_status(
    state: &CircuitBreakerState,
    now_ms: i64,
    failure_threshold: i64,
    _cooldown_ms: i64,
) -> BreakerStatus {
    if state.opened_until_ms <= 0 || state.consecutive_failures < failure_threshold {
        return BreakerStatus::Closed;
    }
    if now_ms < state.opened_until_ms {
        return BreakerStatus::Open;
    }
    BreakerStatus::HalfOpen
}

/// 记一次失败并返回**判定后**的状态。
///
/// HALF_OPEN 下失败(或累计到阈值)→ 以 `max(1, cooldown_ms)` 的全新冷却
/// 重开,并把 `last_state_change_ms` 推到当前;仍在 CLOSED 里累加则返回
/// 原状态。
pub fn record_failure(
    state: &mut CircuitBreakerState,
    now_ms: i64,
    failure_threshold: i64,
    cooldown_ms: i64,
) -> BreakerStatus {
    let current = compute_status(state, now_ms, failure_threshold, cooldown_ms);
    state.consecutive_failures += 1;
    if current == BreakerStatus::HalfOpen || state.consecutive_failures >= failure_threshold {
        state.opened_until_ms = now_ms + cooldown_ms.max(1);
        state.last_state_change_ms = now_ms;
        return BreakerStatus::Open;
    }
    current
}

/// 记一次成功:清零失败计数与开闸时刻(试探通过即关闸)。
pub fn record_success(state: &mut CircuitBreakerState, now_ms: i64) {
    state.consecutive_failures = 0;
    state.opened_until_ms = 0;
    if state.last_state_change_ms == 0 {
        state.last_state_change_ms = now_ms;
    }
}

// ---------------------------------------------------------------------------
// 思考强度提取(调用日志「思考强度」徽章;Rust 新增,无 Python 镜像)
// ---------------------------------------------------------------------------

/// 从**客户端原始请求体**提取思考强度,归一为枚举词:
/// `minimal / low / medium / high / xhigh / none`。
///
/// 方言(按入站映射):
/// - OpenAI 系:顶层 `reasoning_effort` 或 Responses 的 `reasoning.effort`
///   字符串直通(小写化);
/// - Anthropic:`thinking.type = enabled` → `budget_tokens` 分档;`disabled`
///   → `none`;`adaptive` → `high`;兜底 `output_config.effort` 直通;
/// - Gemini:`generationConfig.thinkingConfig.thinkingBudget` 同一分档,
///   `<= 0` → `none`。
///
/// 无法识别 → None(列表不显示思考强度徽章)。
pub fn extract_reasoning_effort(protocol: &str, body: &Value) -> Option<String> {
    let map = body.as_object()?;
    let normalized = protocol.trim().to_lowercase();
    if normalized.starts_with("anthropic") {
        if let Some(thinking) = map.get("thinking").filter(|value| value.is_object()) {
            match thinking.get("type").and_then(Value::as_str) {
                Some("disabled") => return Some("none".to_owned()),
                Some("adaptive") => return Some("high".to_owned()),
                Some("enabled") => {
                    let budget = crate::python_semantics::py_int(thinking.get("budget_tokens")?)?;
                    return Some(budget_to_effort(budget));
                }
                _ => return None,
            }
        }
        let effort = map
            .get("output_config")
            .and_then(|config| config.get("effort"))
            .and_then(Value::as_str)?
            .trim()
            .to_lowercase();
        return (!effort.is_empty()).then_some(effort);
    }
    if normalized.starts_with("gemini") {
        let budget = map
            .get("generationConfig")?
            .get("thinkingConfig")?
            .get("thinkingBudget")
            .and_then(crate::python_semantics::py_int)?;
        return Some(if budget <= 0 {
            "none".to_owned()
        } else {
            budget_to_effort(budget)
        });
    }
    // OpenAI 系(chat / responses / 其他兼容方言)。
    let effort = map
        .get("reasoning_effort")
        .or_else(|| map.get("reasoning").and_then(|reasoning| reasoning.get("effort")))
        .and_then(Value::as_str)?
        .trim()
        .to_lowercase();
    (!effort.is_empty()).then_some(effort)
}

/// budget → 档位(阈值:
/// semantic 侧出站思考编码共用,与 `ReasoningEffort::budget_hint` 互逆)。
pub(crate) fn budget_to_effort(budget: i64) -> String {
    let level = if budget <= 5_000 {
        "low"
    } else if budget <= 15_000 {
        "medium"
    } else if budget <= 30_000 {
        "high"
    } else {
        "xhigh"
    };
    level.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make(id: &str, models: &[&str], enabled: bool) -> Provider {
        let mut raw = serde_json::Map::new();
        raw.insert("id".into(), json!(id));
        raw.insert("api_base_url".into(), json!(format!("http://127.0.0.1:1/{id}")));
        if !models.is_empty() {
            raw.insert("models".into(), json!(models));
        }
        raw.insert("enabled".into(), json!(enabled));
        Provider::from_raw(&raw).unwrap()
    }

    #[test]
    fn retry_policy_sets_mirror_python() {
        for status in [400u16, 401, 403, 404, 422] {
            assert!(NO_RETRY_STATUS_CODES.contains(&status));
            assert!(!should_retry(status, 0, 3));
        }
        for status in [408u16, 409, 429, 500, 501, 502, 503, 504] {
            assert!(RETRY_STATUS_CODES.contains(&status));
            assert!(should_retry(status, 0, 3));
        }
        // attempt 预算耗尽。
        assert!(!should_retry(500, 3, 3));
        assert!(should_retry(500, 2, 3));
        // 网络错误哨兵 0 → retryable。
        assert!(should_retry(0, 0, 3));
    }

    #[test]
    fn fallback_mode_semantics() {
        assert!(should_fallback_mode(503, "off"));
        assert!(should_fallback_mode(429, "off"));
        assert!(!should_fallback_mode(400, "off"));
        assert!(!should_fallback_mode(200, "off"));
        assert!(should_fallback_mode(400, "model-chain"));
    }

    #[test]
    fn backoff_is_exponential_capped() {
        assert_eq!(backoff_delay(0), 1.0);
        assert_eq!(backoff_delay(1), 2.0);
        assert_eq!(backoff_delay(3), 8.0);
        assert_eq!(backoff_delay(10), 30.0);
    }

    #[test]
    fn choose_provider_prefers_and_filters() {
        let providers = vec![
            make("a", &["m-golden"], true),
            make("b", &["m-golden"], true),
        ];
        let chosen = choose_provider(&providers, "m-golden", "").unwrap();
        assert_eq!(chosen.id, "a");
        let chosen = choose_provider(&providers, "m-golden", "b").unwrap();
        assert_eq!(chosen.id, "b");
        assert!(choose_provider(&providers, "unknown", "").is_err());
        let disabled = vec![make("a", &["m-golden"], false)];
        assert!(choose_provider(&disabled, "m-golden", "").is_err());
        // 空 models 列表 = 任意模型。
        let wildcard = vec![make("w", &[], true)];
        assert_eq!(choose_provider(&wildcard, "anything", "").unwrap().id, "w");
    }

    #[test]
    fn retry_after_parses_seconds() {
        assert_eq!(parse_retry_after_ms(Some("2")), Some(2000.0));
        assert_eq!(parse_retry_after_ms(Some("junk")), None);
        assert_eq!(parse_retry_after_ms(Some("")), None);
        assert_eq!(parse_retry_after_ms(None), None);
    }

    // ---- 20261015_01 P1-1/P1-2/P1-3 ----------------------------------------

    #[test]
    fn retry_wait_jitters_backoff_but_not_retry_after() {
        // Retry-After 为正 → 精确遵守(clamp [1,60]s),不抖动。
        let mut fixed = || 0.99;
        assert_eq!(
            retry_wait(Some("3"), 0, &mut fixed),
            (3.0, "retry-after")
        );
        // clamp 上限:大值压到 60s。
        assert_eq!(
            retry_wait(Some("300"), 0, &mut fixed),
            (60.0, "retry-after")
        );
        // 无头 → 退避 × [0.5, 1.5):rng=0 折半、rng≈1 折半再半增。
        let mut zero = || 0.0;
        assert_eq!(retry_wait(None, 1, &mut zero), (1.0, "backoff")); // 2 × 0.5
        let mut half = || 0.5;
        assert_eq!(retry_wait(None, 1, &mut half), (2.0, "backoff")); // 2 × 1.0
        let mut one = || 0.999_999;
        let (delay, source) = retry_wait(None, 10, &mut one);
        assert_eq!(source, "backoff");
        assert!(
            (delay - 30.0 * 1.499_999).abs() < 1e-9,
            "{delay}"
        ); // 30 × (0.5 + rng) 上界内
        // Retry-After 头不可解析("junk")→ 落退避分支。
        assert_eq!(
            retry_wait(Some("junk"), 0, &mut half),
            (backoff_delay(0), "backoff")
        );
    }

    #[test]
    fn retry_after_exceeds_cap_only_above_60s() {
        assert!(!retry_after_exceeds_cap(None));
        assert!(!retry_after_exceeds_cap(Some("junk")));
        assert!(!retry_after_exceeds_cap(Some("60")));
        assert!(retry_after_exceeds_cap(Some("61")));
        assert!(retry_after_exceeds_cap(Some("300")));
    }

    #[test]
    fn credential_failures_are_401_and_403_only() {
        assert_eq!(CREDENTIAL_FAILURE_STATUS_CODES, &[401, 403]);
        // 凭据类失败同时属于 NO_RETRY 集(不再原地重试,只推进候选)。
        for status in CREDENTIAL_FAILURE_STATUS_CODES {
            assert!(NO_RETRY_STATUS_CODES.contains(status));
        }
    }

    #[test]
    fn breaker_state_machine_matches_python() {
        // 阈值 3、冷却 1000ms:两次失败仍 CLOSED,第三次开闸。
        let mut state = CircuitBreakerState::default();
        let threshold = 3;
        let cooldown = 1000;
        assert_eq!(
            compute_status(&state, 0, threshold, cooldown),
            BreakerStatus::Closed
        );
        record_failure(&mut state, 10, threshold, cooldown);
        record_failure(&mut state, 20, threshold, cooldown);
        assert_eq!(
            compute_status(&state, 20, threshold, cooldown),
            BreakerStatus::Closed
        );
        record_failure(&mut state, 30, threshold, cooldown);
        assert_eq!(state.consecutive_failures, 3);
        assert_eq!(state.opened_until_ms, 1030);
        assert_eq!(
            compute_status(&state, 1000, threshold, cooldown),
            BreakerStatus::Open
        );
        // 冷却到期 → HALF_OPEN(试探请求放行)。
        assert_eq!(
            compute_status(&state, 1030, threshold, cooldown),
            BreakerStatus::HalfOpen
        );
        // 半开再失败:立即以全新冷却重开。
        record_failure(&mut state, 1100, threshold, cooldown);
        assert_eq!(state.opened_until_ms, 2100);
        assert_eq!(
            compute_status(&state, 1200, threshold, cooldown),
            BreakerStatus::Open
        );
        // 半开成功 → CLOSED(计数清零)。
        record_failure(&mut state, 3000, threshold, cooldown);
        assert_eq!(
            compute_status(&state, 3000, threshold, cooldown),
            BreakerStatus::Open
        );
        record_success(&mut state, 3100);
        assert_eq!(state.consecutive_failures, 0);
        assert_eq!(state.opened_until_ms, 0);
        // Python record_success 只在 last_state_change_ms == 0 时才写当前时刻,
        // 所以上一次开闸留下的 3000 不会被刷新。
        assert_eq!(state.last_state_change_ms, 3000);
        assert_eq!(
            compute_status(&state, 3100, threshold, cooldown),
            BreakerStatus::Closed
        );
    }

    #[test]
    fn breaker_opened_until_zero_never_opens() {
        // opened_until_ms=0 表示从未开闸:即便失败计数超过阈值也判 CLOSED。
        let state = CircuitBreakerState {
            consecutive_failures: 99,
            opened_until_ms: 0,
            last_state_change_ms: 0,
        };
        assert_eq!(compute_status(&state, 5000, 3, 1000), BreakerStatus::Closed);
    }

    #[test]
    fn health_classification_mirrors_python() {
        assert_eq!(classify_status_to_health(None), "unknown");
        assert_eq!(classify_status_to_health(Some(200)), "healthy");
        assert_eq!(classify_status_to_health(Some(404)), "healthy");
        for status in [408, 429, 500, 502, 503] {
            assert_eq!(
                classify_status_to_health(Some(status)),
                "degraded",
                "{status}"
            );
        }
    }

    // ---- 20260905_02 K4:会话键提取(三态) ---------------------------------

    fn header(name: &str, value: &str) -> (String, String) {
        (name.to_owned(), value.to_owned())
    }

    #[test]
    fn session_key_prefers_session_headers_case_insensitively() {
        // 头存在(拿到即停,优先级从高到低)。
        for name in ["x-claude-code-session-id", "x-session-id", "x-request-id"] {
            let headers = vec![header(name, "  sess-1  ")];
            assert_eq!(
                extract_session_key(&headers, None).as_deref(),
                Some("sess-1"),
                "{name}"
            );
            // 大小写不敏感匹配(RouteRequest.header 是大小写敏感的)。
            let mixed = vec![header(&name.to_ascii_uppercase(), "sess-2")];
            assert_eq!(
                extract_session_key(&mixed, None).as_deref(),
                Some("sess-2"),
                "{} 大写变体",
                name
            );
        }
        // 优先级:高优先头存在时忽略低优先头。
        let both = vec![
            header("x-request-id", "lower"),
            header("x-session-id", "higher"),
        ];
        assert_eq!(extract_session_key(&both, None).as_deref(), Some("higher"));
        // 空白头视为缺失,落到下一优先级。
        let blank = vec![header("x-claude-code-session-id", "   ")];
        assert_eq!(extract_session_key(&blank, None), None);
    }

    #[test]
    fn session_key_falls_back_to_first_user_message_hash() {
        let body = json!({
            "messages": [
                {"role": "system", "content": "sys"},
                {"role": "user", "content": "hello world"},
            ],
        });
        let key = extract_session_key(&[], body.as_object()).expect("有 user 消息应有键");
        // SHA-256 前 16 hex(8 字节)。
        assert_eq!(key.len(), 16);
        assert!(
            key.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        // 确定性:同内容同键;首条 user 消息为准(后续 user 不影响)。
        let same = extract_session_key(&[], body.as_object()).unwrap();
        assert_eq!(key, same);
        let mut with_more = body.clone();
        with_more["messages"]
            .as_array_mut()
            .unwrap()
            .push(json!({"role": "user", "content": "tail should not matter"}));
        assert_eq!(
            key,
            extract_session_key(&[], with_more.as_object()).unwrap(),
            "只取首条 user 消息"
        );
        // 非字符串 content(多模态块):以 JSON 文本近似,仍产出键。
        let multimodal = json!({
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "hi"}]},
            ],
        });
        assert_eq!(
            extract_session_key(&[], multimodal.as_object())
                .expect("多模态 content 也应产出键")
                .len(),
            16
        );
    }

    #[test]
    fn session_key_absent_without_headers_or_user_message() {
        // 无 headers、无 body。
        assert_eq!(extract_session_key(&[], None), None);
        // 无 messages 键 / messages 非数组。
        assert_eq!(extract_session_key(&[], json!({}).as_object()), None);
        assert_eq!(
            extract_session_key(&[], json!({"messages": "x"}).as_object()),
            None
        );
        // 只有 assistant / 空 user 消息。
        assert_eq!(
            extract_session_key(
                &[],
                json!({"messages": [{"role": "assistant", "content": "hi"}]}).as_object()
            ),
            None
        );
        assert_eq!(
            extract_session_key(
                &[],
                json!({"messages": [{"role": "user", "content": ""}]}).as_object()
            ),
            None,
            "空内容视为无(避免空会话互相误粘)"
        );
    }

    // ---- 20260905_03 S6/S7:响应缓存键 + 上下文超限判定 ---------------------

    #[test]
    fn response_cache_key_requires_explicit_zero_temperature() {
        let base = json!({"model": "m", "messages": [{"role": "user", "content": "hi"}]});
        // 显式 temperature 0 → 有键;重复请求键稳定。
        let zero = json!({"model": "m", "temperature": 0, "messages": [{"role": "user", "content": "hi"}]});
        let key = response_cache_key("openai", &zero);
        assert!(key.is_some());
        assert_eq!(key.unwrap().len(), 64);
        // 显式 0.0 同样命中。
        let zero_float = json!({"model": "m", "temperature": 0.0, "messages": [{"role": "user", "content": "hi"}]});
        assert_eq!(
            response_cache_key("openai", &zero_float),
            response_cache_key("openai", &zero)
        );
        // 省略 temperature → None(宁可少命中,不可错误命中)。
        assert_eq!(response_cache_key("openai", &base), None);
        // 显式非零 → None。
        let hot = json!({"temperature": 0.7, "messages": []});
        assert_eq!(response_cache_key("openai", &hot), None);
        // 字符串 "0" 不算显式零。
        let string_zero = json!({"temperature": "0", "messages": []});
        assert_eq!(
            response_cache_key("openai", &string_zero),
            None
        );
    }

    #[test]
    fn stateful_reference_disables_response_cache() {
        let payload = json!({
            "model": "m",
            "temperature": 0,
            "previous_response_id": "resp_1",
            "messages": [{"role": "user", "content": "hi"}],
        });
        assert!(response_cache_key("openai", &payload).is_none());
        // 嵌套在 input/业务对象里的会话引用同样不能被缓存重放。
        let nested = json!({
            "model": "m",
            "temperature": 0,
            "input": [{"type": "item_reference", "conversation": {"id": "c1"}}],
        });
        assert!(has_stateful_reference(&nested));
        assert!(response_cache_key("openai_responses", &nested).is_none());
    }

    #[test]
    fn response_cache_key_varies_by_inputs() {
        let zero = |extra: Value| {
            let mut payload = json!({"model": "m", "temperature": 0, "messages": [{"role": "user", "content": "hi"}]});
            if let Value::Object(map) = &mut payload {
                if let Value::Object(extra) = extra {
                    for (key, value) in extra {
                        map.insert(key, value);
                    }
                }
            }
            response_cache_key("openai", &payload)
        };
        // 模型不同 → 键不同。
        assert_ne!(
            zero(json!({"model": "other"})),
            zero(json!({})),
        );
// 工具集变化必须 miss。
        assert_ne!(
            zero(json!({"tools": [{"name": "t"}]})),
            zero(json!({})),
        );
        // top_p 变化 → 键不同。
        assert_ne!(
            zero(json!({"top_p": 0.9})),
            zero(json!({})),
        );
        // 客户端协议参与键(缓存值按协议解码,跨协议不可重放)。
        let payload = json!({"model": "m", "temperature": 0, "messages": []});
        assert_ne!(
            response_cache_key("openai", &payload),
            response_cache_key("anthropic", &payload)
        );
    }

    #[test]
    fn context_overflow_body_matches_provider_dialects() {
        // Anthropic / OpenAI / Gemini 方言 + 中文。
        assert!(is_context_overflow_body(
            "prompt is too long: 189227 tokens > 200000 maximum"
        ));
        assert!(is_context_overflow_body(
            "This model's maximum context length is 8192 tokens. However, your messages resulted in 9000 tokens."
        ));
        assert!(is_context_overflow_body(
            "{\"error\":{\"code\":\"context_length_exceeded\",\"message\":\"...\"}}"
        ));
        assert!(is_context_overflow_body(
            "The request exceeds the maximum number of tokens allowed."
        ));
        assert!(is_context_overflow_body("请求的上下文长度超过限制"));
        // 非超限 400 文本不误判。
        assert!(!is_context_overflow_body("invalid api key"));
        assert!(!is_context_overflow_body(""));
    }

    // ---- 20260905_02 K8:429 reset 文本解析 --------------------------------

    #[test]
    fn reset_text_parses_relative_durations() {
        // en:常见限流短语 + 相对时长。
        assert_eq!(
            parse_rate_limit_reset_ms("Rate limit exceeded. Try again in 30 seconds."),
            Some(30_000)
        );
        assert_eq!(
            parse_rate_limit_reset_ms("Too many requests, retry after 2 minutes"),
            Some(120_000)
        );
        assert_eq!(
            parse_rate_limit_reset_ms("limit reached. Reset in 1.5 hours please"),
            Some(5_400_000)
        );
        // zh:频繁/等待语境。
        assert_eq!(
            parse_rate_limit_reset_ms("请求过于频繁,请等待 60 秒后再试"),
            Some(60_000)
        );
        assert_eq!(
            parse_rate_limit_reset_ms("您请求太频繁了,请等待 5 分钟"),
            Some(300_000)
        );
        // 小数与单位变体。
        assert_eq!(
            parse_rate_limit_reset_ms("please wait 0.5 min and retry"),
            Some(30_000)
        );
        assert_eq!(
            parse_rate_limit_reset_ms("try again in 90 secs"),
            Some(90_000)
        );
    }

    #[test]
    fn reset_text_ignores_numbers_outside_rate_limit_context() {
        // 无限流语境短语的数字不解析(避免误吃正文)。
        assert_eq!(parse_rate_limit_reset_ms("used 123 minutes of quota"), None);
        assert_eq!(
            parse_rate_limit_reset_ms("model gpt-4 has 5 seconds latency"),
            None
        );
        assert_eq!(parse_rate_limit_reset_ms(""), None);
        assert_eq!(parse_rate_limit_reset_ms("no digits here at all"), None);
        // 语境短语存在但后面没有时长数字。
        assert_eq!(
            parse_rate_limit_reset_ms("rate limited, try again later"),
            None
        );
    }

    #[test]
    fn reset_text_parses_future_iso_reset_and_skips_past() {
        // reset 语境 + ISO 时间戳(远未来 → 正 delta)。
        let future = parse_rate_limit_reset_ms("quota resets at 2099-01-01T00:00:00Z");
        assert!(future.is_some_and(|ms| ms > 86_400_000 * 300), "{future:?}");
        // 过去时间戳 → None(只在未来时有效)。
        assert_eq!(
            parse_rate_limit_reset_ms("quota resets at 2001-01-01T00:00:00Z"),
            None
        );
        // ISO 时间戳但无 reset 语境 → 不解析。
        assert_eq!(
            parse_rate_limit_reset_ms("created at 2099-01-01T00:00:00Z"),
            None
        );
    }

    #[test]
    fn sticky_score_u_is_deterministic_and_in_unit_interval() {
        let a = sticky_score_u("sess-1", "acct-1");
        assert_eq!(a, sticky_score_u("sess-1", "acct-1"), "同输入必须同分");
        assert_eq!(
            sticky_score_u("sess-1", "acct-2"),
            sticky_score_u("sess-1", "acct-2")
        );
        assert_ne!(a, sticky_score_u("sess-2", "acct-1"));
        for (key, account) in [("s", "a"), ("长会话键", "账号"), ("", "x")] {
            let u = sticky_score_u(key, account);
            assert!((0.0..1.0).contains(&u), "{u} 应落在 [0,1)");
        }
    }

    #[test]
    fn reasoning_effort_extraction_covers_dialects() {
        // OpenAI:顶层 reasoning_effort 直通(含 Responses 的 reasoning.effort)。
        assert_eq!(
            extract_reasoning_effort("openai", &json!({"reasoning_effort": "High"})),
            Some("high".to_owned())
        );
        assert_eq!(
            extract_reasoning_effort("openai_responses", &json!({"reasoning": {"effort": "low"}})),
            Some("low".to_owned())
        );
        // Anthropic:enabled+budget 分档 / disabled → none / adaptive → high。
        let anthropic = |budget: i64| {
            extract_reasoning_effort(
                "anthropic",
                &json!({"thinking": {"type": "enabled", "budget_tokens": budget}}),
            )
        };
        assert_eq!(anthropic(4_000), Some("low".to_owned()));
        assert_eq!(anthropic(10_000), Some("medium".to_owned()));
        assert_eq!(anthropic(20_000), Some("high".to_owned()));
        assert_eq!(anthropic(64_000), Some("xhigh".to_owned()));
        assert_eq!(
            extract_reasoning_effort(
                "anthropic",
                &json!({"thinking": {"type": "disabled"}})
            ),
            Some("none".to_owned())
        );
        assert_eq!(
            extract_reasoning_effort(
                "anthropic",
                &json!({"thinking": {"type": "adaptive"}})
            ),
            Some("high".to_owned())
        );
        assert_eq!(
            extract_reasoning_effort(
                "anthropic",
                &json!({"output_config": {"effort": "medium"}})
            ),
            Some("medium".to_owned())
        );
        // Gemini:thinkingBudget 分档,0 → none。
        assert_eq!(
            extract_reasoning_effort(
                "gemini_stream",
                &json!({"generationConfig": {"thinkingConfig": {"thinkingBudget": 8_192}}})
            ),
            Some("medium".to_owned())
        );
        assert_eq!(
            extract_reasoning_effort(
                "gemini",
                &json!({"generationConfig": {"thinkingConfig": {"thinkingBudget": 0}}})
            ),
            Some("none".to_owned())
        );
        // 无思考参数 → None(不显示徽章)。
        assert_eq!(extract_reasoning_effort("openai", &json!({"model": "m1"})), None);
        assert_eq!(extract_reasoning_effort("anthropic", &json!({})), None);
    }
}
