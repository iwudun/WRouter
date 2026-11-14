//! 转发层共享小件:上游 JSON 解码、重试等待、成本/限流/日志 helper。

use super::*;

/// 上游 JSON 响应 → 客户端协议(decode_response 镜像,R3 全分支)。
///
/// P4-5:转换失败返回显式错误,调用方必须转成 502,不能把上游原对象
/// 当成功响应发给客户端。
pub(super) fn decode_upstream_json(
    protocol: &str,
    provider: &Provider,
    response: &UpstreamResponse,
) -> Result<
    (Value, Option<wr_domain::semantic::ConversionReport>),
    wr_domain::protocol::ConversionFailure,
> {
    let text = String::from_utf8_lossy(&response.body).into_owned();
    let value = serde_json::from_str::<Value>(&text)
        .unwrap_or_else(|_| json!({ "error": { "message": text } }));
    domain_protocol::try_decode_response_with_report(response.status, &value, protocol, provider)
}

pub(super) fn parse_error_body(body: &[u8]) -> Value {
    let text = String::from_utf8_lossy(body).into_owned();
    serde_json::from_str::<Value>(&text).unwrap_or_else(|_| json!({ "message": text }))
}

pub(super) fn header_str<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

/// 重试等待时长(方案 20261015_01 P1-1):上游 Retry-After 为正精确遵守,
/// 否则指数退避 × [0.5, 1.5) 抖动。落库 wait_ms 与实际睡眠共用本值
/// (先算后睡),非流式管线同样消费。
pub(super) fn retry_wait_now(retry_after: Option<&str>, attempt: u32) -> (f64, &'static str) {
    let mut rng = crate::gateway::credential_pool::WeightedRng::from_entropy();
    wr_domain::gateway::retry_wait(retry_after, attempt, &mut || rng.next_f64())
}

/// 落库等待时长(2026-10-20):[`retry_wait_now`] 返回**秒**,而契约字段
/// `wait_ms` 与前端 `delayMs` 都是**毫秒** —— 旧写入直接存秒,前端按毫秒
/// 渲染把 1.4s 的退避显示成 `1.4325796 ms`(缩小 1000 倍且带浮点长尾)。
/// 统一换算为毫秒并保留两位小数;睡眠仍消费原秒值,不影响实际等待。
pub(super) fn retry_wait_ms(wait: Option<(f64, &'static str)>) -> f64 {
    wait.map(|(seconds, _)| (seconds * 1000.0 * 100.0).round() / 100.0)
        .unwrap_or(0.0)
}

/// 按**已算好**的等待时长睡眠(P1-1:与落库 wait_ms 同源)。
pub(super) async fn sleep_wait(wait: (f64, &'static str)) {
    tokio::time::sleep(Duration::from_secs_f64(wait.0)).await;
}

/// 轮询客户端断开标志(250ms 一拍):供上游握手等待/重试退避的 select
/// 分支 —— 断开后立即放弃等待,不再空耗上游(2026-10-08)。
pub(super) async fn wait_client_gone(client_disconnected: &std::sync::atomic::AtomicBool) {
    loop {
        if client_disconnected.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// 流式重试等待(带断联中断;时长已由 [`retry_wait_now`] 算好):
/// 返回 false = 等待期间客户端已断开。
pub(super) async fn sleep_wait_or_abort(
    client_disconnected: &std::sync::atomic::AtomicBool,
    wait: (f64, &'static str),
) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(Duration::from_secs_f64(wait.0)) => true,
        _ = wait_client_gone(client_disconnected) => false,
    }
}

/// 首字前断联的 499 终态行(流式管线;`record_log` 是在途汇点,顺带取走
/// 注册表条目并发布事件 —— 不再出现「处理中」悬挂,也不再继续打上游)。
#[allow(clippy::too_many_arguments)]
pub(super) fn record_pre_stream_abort(
    deps: &GatewayDeps,
    snapshot: &Arc<ConfigSnapshot>,
    request_id: &str,
    protocol: &str,
    model: &str,
    identity: &GatewayIdentity,
    started: Instant,
    attempts: &AttemptTrail,
    payload: &Value,
    provider: Option<&str>,
    account: Option<&String>,
    attempt_ms: Option<f64>,
    // 客户端请求头(捕获开启时随失败行落侧表;2026-10-14)。
    client_headers: &[(String, String)],
) {
    let mut extra = serde_json::Map::new();
    extra.insert("error".to_owned(), json!("client_disconnected"));
    deps.record_log_extra(
        snapshot,
        request_id,
        &RequestLogEvent {
            protocol: protocol.to_owned(),
            model: model.to_owned(),
            status: 499,
            stream: true,
            duration_ms: elapsed_ms(started),
            provider_attempts: trail_provider_ids(attempts),
            retry_attempts: trail_values(attempts),
            end_reason: "client_disconnected",
            provider: provider.map(str::to_owned),
            account: account.cloned(),
            api_key_id: Some(identity.key_id.clone()),
            first_byte_ms: None,
            error: Some(match attempt_ms {
                Some(ms) => format!("client_disconnected while awaiting upstream ({ms:.0} ms)"),
                None => "client_disconnected".to_owned(),
            }),
        },
        RequestLogExtra::default()
            .with_identity(&identity.key_name)
            .with_preview(RequestLogRecorder::call_logs_enabled(snapshot), payload)
            .with_extra(extra)
            .with_capture(
                RequestLogRecorder::capture_enabled(snapshot)
                    && RequestLogRecorder::call_logs_enabled(snapshot),
                deps.settings.body_capture_max_bytes,
                Some(payload),
                None,
                None,
            )
            .with_request_headers(client_headers),
    );
}

pub(super) fn elapsed_ms(started: Instant) -> f64 {
    elapsed_since(started)
}

pub(super) fn elapsed_since(instant: Instant) -> f64 {
    (instant.elapsed().as_secs_f64() * 1000.0 * 100.0).round() / 100.0
}

/// 两个时刻之间的偏移(毫秒;`to` 早于 `from` 时为 0)。
///
/// 阶段四 C-4:出站时刻相对**请求起点**的偏移(`sent_offset_ms`)。
/// 用 `saturating_duration_since` 而不是 `duration_since`:后者在
/// `to` 早于 `from` 时会 panic(时钟回拨/测试构造的边界)。
pub(super) fn offset_ms(from: Instant, to: Instant) -> f64 {
    to.saturating_duration_since(from).as_secs_f64() * 1000.0
}

pub(super) fn log_upstream_http_failed(
    provider: &Provider,
    attempt: u32,
    max_retries: u32,
    url: &str,
    error: &UpstreamError,
    started: Instant,
) {
    tracing::warn!(
        target: "wrouter.gateway.proxy",
        provider = %provider.id,
        attempt = attempt + 1,
        total = max_retries + 1,
        endpoint = %url,
        exc_class = "UpstreamError",
        exc = %error,
        elapsed = format_args!("{:.2}", started.elapsed().as_secs_f64()),
        "upstream_http_failed",
    );
}

// ---------------------------------------------------------------------------
// R4 接线用的纯函数(镜像 cost_usage.py / provider_selection.py 的读值口径)
// ---------------------------------------------------------------------------

/// 请求前用量估算(镜像 `cost_usage.estimate_usage`)。
///
/// 粗略 4 字符 1 token,只喂 precheck 计数器,不参与计费。
pub fn estimate_usage(payload: &Value) -> limits::PrecheckEstimate {
    let messages = payload
        .get("messages")
        .or_else(|| payload.get("input"))
        .cloned()
        .unwrap_or(Value::Null);
    let char_count: usize = match &messages {
        Value::String(text) => text.chars().count(),
        Value::Array(items) => items.iter().map(py_repr_len).sum(),
        _ => 0,
    };
    let input_tokens = (char_count / 4) as i64;
    let max_output = ["max_tokens", "max_output_tokens"]
        .iter()
        .find_map(|key| payload.get(*key).filter(|v| !v.is_null()))
        .cloned()
        .unwrap_or(Value::from(0));
    let output_tokens = wr_domain::python_semantics::py_int(&max_output).unwrap_or(0);
    limits::PrecheckEstimate {
        input_tokens,
        output_tokens,
        total_tokens: input_tokens + output_tokens,
        ..Default::default()
    }
}

/// Python `len(str(obj))` 的近似:JSON 文本长度(dict/list 的 str() 形态
/// 与 json.dumps 长度一致,标量走 `str()`)。
pub(super) fn py_repr_len(value: &Value) -> usize {
    match value {
        Value::String(text) => text.chars().count() + 2,
        other => wr_domain::json_py::py_json_dumps(other).chars().count(),
    }
}

/// 单请求成本(镜像 `cost_usage.cost`):按 provider.billing 的百万 token 单价。
pub fn cost_of(provider: &wr_domain::provider::Provider, usage: &wr_domain::usage::Usage) -> f64 {
    let per_million = |key: &str| -> f64 {
        provider
            .billing
            .get(key)
            .and_then(wr_domain::python_semantics::py_float)
            .unwrap_or(0.0)
    };
    let raw = (usage.input_tokens as f64) * per_million("input_per_million")
        + (usage.output_tokens as f64) * per_million("output_per_million");
    ((raw / 1_000_000.0) * 1e10).round_ties_even() / 1e10
}

/// 取该 API key 的 `limits` 对象(per-key 限流规则源)。
pub(super) fn api_key_limits(snapshot: &ConfigSnapshot, key_id: &str) -> Option<Value> {
    if key_id.is_empty() {
        return None;
    }
    snapshot
        .config
        .api_keys
        .iter()
        .filter_map(Value::as_object)
        .find(|map| {
            map.get("id").and_then(Value::as_str).unwrap_or_default() == key_id
                || map.get("key").and_then(Value::as_str).unwrap_or_default() == key_id
        })
        .and_then(|map| map.get("limits"))
        .filter(|value| value.is_object())
        .cloned()
}

/// 客户端标识(镜像 Python:`context.client` 缺失时回退 user-agent)。
///
/// 这里存的是**原始 UA**(审计留痕:调用日志按它检索);用量统计的
/// `client` 维度另有归一(产品名)——见 `wr_domain::protocol::client_identity`。
pub(super) fn client_label(headers: &[(String, String)]) -> String {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("user-agent"))
        .map(|(_, value)| value.clone())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}

/// 上游响应头 → 可落库的 map(同名头保留首个,与 httpx headers dict 同形)。
pub(super) fn header_map(headers: &[(String, String)]) -> serde_json::Map<String, Value> {
    let mut map = serde_json::Map::new();
    for (key, value) in headers {
        map.entry(key.clone()).or_insert_with(|| json!(value));
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_wait_ms_converts_seconds_and_keeps_two_decimals() {
        // retry_wait 的返回单位是秒;落库契约是毫秒(前端 delayMs)。
        assert_eq!(retry_wait_ms(Some((1.4325796, "backoff"))), 1432.58);
        assert_eq!(retry_wait_ms(Some((0.5, "backoff"))), 500.0);
        // 无等待(成功/最终次)记 0。
        assert_eq!(retry_wait_ms(None), 0.0);
    }
}
