//! 端点测速引擎（供应商页「管理与测速」入口）。
//!
//! 与 [`super::connectivity_probe`] 的区别：连通性探针发**真实计费的模型
//! 请求**验证可用性；本模块只发轻量匿名 `GET` 测量到目标地址的**网络往返
//! 延迟**，不带 API Key、不触发推理、不消耗余额。
//! 每个 URL 先发一次预热请求（忽略结果，
//! 复用连接 / 绕过首包惩罚），再发一次计时请求并把往返毫秒数作为结果。
//!
//! 出站代理沿用用户在「设置 → 代理」里的配置：每任务经
//! [`resolve_proxy_decision`] 解析（Direct / Url），与探针同款。
//!
//! RPC 面：`testApiEndpoints`（见 [`super::config::dispatch`]）。

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde_json::{Map, Value, json};

use wr_infra::proxy::{ProxyDecision, describe_proxy_source};

/// 未显式传 `timeoutSecs` 时的默认总超时。
const DEFAULT_TIMEOUT_SECS: u64 = 10;
/// 总超时下限（过短会把正常但稍慢的地址误判为超时）。
const MIN_TIMEOUT_SECS: u64 = 2;
/// 总超时上限（避免单个地址挂死整批测速）。
const MAX_TIMEOUT_SECS: u64 = 30;
/// 连接超时上限（总超时更小时随总超时）。
const MAX_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// 并发上限（镜像连通性探针的 `PROBE_CONCURRENCY`；`buffered` 保序）。
const SPEEDTEST_CONCURRENCY: usize = 6;

/// 把入参超时秒数收敛到 `[MIN, MAX]`，缺省取 [`DEFAULT_TIMEOUT_SECS`]。
fn sanitize_timeout(timeout_secs: Option<u64>) -> u64 {
    timeout_secs
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
        .clamp(MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS)
}

/// 构建测速专用客户端：总超时 = `timeout`，连接超时 = `min(timeout, 10s)`，
/// 代理按 [`ProxyDecision`] 装配（镜像 `connectivity_probe::probe_client`）。
fn speedtest_client(proxy: &ProxyDecision, timeout: Duration) -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .timeout(timeout)
        .connect_timeout(timeout.min(MAX_CONNECT_TIMEOUT))
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

/// 归一化：去首尾空白 + 去尾部斜杠（与前端 `normalizeEndpointUrl` 一致）。
fn normalize_url(raw: &str) -> String {
    raw.trim().trim_end_matches('/').to_owned()
}

/// 单个 URL 的测速结果（`{url, latency, status, error}`，snake→camel 无需转换）。
async fn test_one(
    raw_url: String,
    timeout: Duration,
    proxy_config: Option<Arc<Map<String, Value>>>,
) -> Value {
    let normalized = normalize_url(&raw_url);
    if normalized.is_empty() {
        return json!({
            "url": raw_url, "latency": Value::Null, "status": Value::Null,
            "error": "URL 不能为空",
        });
    }

    let parsed = match reqwest::Url::parse(&normalized) {
        Ok(parsed) => parsed,
        Err(err) => {
            return json!({
                "url": normalized, "latency": Value::Null, "status": Value::Null,
                "error": format!("URL 无效: {err}"),
            });
        }
    };
    // 明确只允许 http/https（挡掉 file:// 等本地协议）。
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return json!({
            "url": normalized, "latency": Value::Null, "status": Value::Null,
            "error": "仅支持 http/https 地址",
        });
    }

    // 20260906_01 §4.6:测速与探针共用同一条决策链(含载荷里的供应商级
    // 覆盖),并把实际走的出口标注进结果。
    let proxy_config = proxy_config.as_deref();
    let decision = super::connectivity_probe::probe_proxy_decision(proxy_config, &normalized);
    let proxy_label = describe_proxy_source(proxy_config, &decision);
    let client = speedtest_client(&decision, timeout);

    // 预热请求：忽略结果，仅用于建立/复用连接、绕过 TLS 首包惩罚。
    let _ = client.get(parsed.clone()).send().await;

    // 计时请求：往返毫秒数即测速结果（任何 HTTP 状态都算“可达”）。
    let started = Instant::now();
    match client.get(parsed).send().await {
        Ok(resp) => {
            let latency = started.elapsed().as_millis() as u64;
            let status = resp.status().as_u16();
            json!({
                "url": normalized, "latency": latency, "status": status,
                "error": Value::Null, "proxy": proxy_label,
            })
        }
        Err(err) => {
            let status = err.status().map(|s| s.as_u16());
            let message = if err.is_timeout() {
                "请求超时".to_owned()
            } else if err.is_connect() {
                "连接失败".to_owned()
            } else {
                err.to_string()
            };
            json!({
                "url": normalized,
                "latency": Value::Null,
                "status": status.map(|s| json!(s)).unwrap_or(Value::Null),
                "error": message,
                "proxy": proxy_label,
            })
        }
    }
}

/// `testApiEndpoints`：对一组 URL 并发测速，**按入参顺序**返回结果数组。
///
/// 入参 `payload`：`{ "urls": string[], "timeoutSecs"?: number,
/// "proxy"?: {mode, presetId} }`（`proxy` 为方案 20260906_01 的供应商级
/// 覆盖，缺省回落全局默认）。
/// 返回：`[{ "url", "latency": number|null, "status": number|null,
/// "error": string|null, "proxy"?: string }, ...]`（非法/空 URL 就地记
/// error，不发请求）。
pub async fn test_endpoints(
    payload: &Map<String, Value>,
    proxy_config: Option<&Map<String, Value>>,
) -> Result<Value, String> {
    let urls: Vec<String> = payload
        .get("urls")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    if urls.is_empty() {
        return Ok(Value::Array(Vec::new()));
    }

    let timeout = Duration::from_secs(sanitize_timeout(
        payload.get("timeoutSecs").and_then(Value::as_u64),
    ));
    // 20260906_01 §4.6:载荷里的供应商级代理覆盖并入配置(缺省 = 全局默认)。
    let proxy_ptr: Option<Arc<Map<String, Value>>> =
        super::connectivity_probe::with_payload_override(proxy_config, payload).map(Arc::new);

    // `buffered` 保序：结果数组与入参 urls 一一对应。
    let results: Vec<Value> = futures_util::stream::iter(urls.into_iter())
        .map(|raw_url| {
            let proxy_ptr = proxy_ptr.clone();
            async move { test_one(raw_url, timeout, proxy_ptr).await }
        })
        .buffered(SPEEDTEST_CONCURRENCY)
        .collect()
        .await;

    Ok(Value::Array(results))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let out = runtime.block_on(fut);
        runtime.shutdown_background();
        out
    }

    #[test]
    fn sanitize_timeout_clamps_values() {
        assert_eq!(sanitize_timeout(Some(1)), MIN_TIMEOUT_SECS);
        assert_eq!(sanitize_timeout(Some(999)), MAX_TIMEOUT_SECS);
        assert_eq!(sanitize_timeout(Some(12)), 12);
        assert_eq!(sanitize_timeout(None), DEFAULT_TIMEOUT_SECS);
    }

    #[test]
    fn normalize_url_trims_and_strips_trailing_slashes() {
        assert_eq!(normalize_url("  https://x.example/v1//  "), "https://x.example/v1");
        assert_eq!(normalize_url(""), "");
    }

    #[test]
    fn test_endpoints_handles_empty_list() {
        let payload = json!({ "urls": [] }).as_object().cloned().unwrap();
        let result = block_on(test_endpoints(&payload, None)).expect("empty list should succeed");
        assert_eq!(result, Value::Array(Vec::new()));
    }

    #[test]
    fn test_endpoints_reports_invalid_and_empty_urls_without_network() {
        let payload = json!({ "urls": ["not a url", "", "ftp://x.example"] })
            .as_object()
            .cloned()
            .unwrap();
        let result = block_on(test_endpoints(&payload, None)).expect("invalid inputs still succeed");
        let rows = result.as_array().unwrap();
        assert_eq!(rows.len(), 3);
        assert!(rows[0]["error"].as_str().unwrap().starts_with("URL 无效"));
        assert_eq!(rows[0]["latency"], Value::Null);
        assert_eq!(rows[1]["error"].as_str().unwrap(), "URL 不能为空");
        assert_eq!(rows[2]["error"].as_str().unwrap(), "仅支持 http/https 地址");
        // 保序：url 字段回显（非法/空按归一化后回显）。
        assert_eq!(rows[2]["url"].as_str().unwrap(), "ftp://x.example");
    }
}
