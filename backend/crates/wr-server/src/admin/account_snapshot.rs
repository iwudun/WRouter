//! 账号余额/配额快照引擎(方案 #14 R5.3;镜像 Python
//! `application/provider/account_service.py` 与 RPC handler
//! `handlers/provider.py::handle_get_account_snapshots`)。
//!
//! 组成:
//! - 表达式求值子集:JSONPath 简单路径/过滤器、数值/字符串表达式
//!   (含 `"100 - $.used"` 减法形态与数组回退)、resetAt 归一;
//! - http-json 连接器执行:headers/认证头构造、30s 抓取、meter 解析;
//! - 聚合:status 推断(explicit → error/unsupported → warning →
//!   critical/warning 阈值 0.05/0.20 → ok)、severity 取最大、TTL 缓存
//!   (默认 5min、下限 30s、error/unsupported 上限 60s)。
//!
//! **镜像的 Python 缺陷**(14a §2.7 实测确认):账号级
//! `account_config` 为非空 dict 时,Python `fetch_snapshot` 在
//! `account_config.connectors` 处抛 AttributeError → handler 捕获后该
//! 账号被跳过。本实现以 `FetchOutcome::Skipped` 镜像同一可观察行为
//! (前端从不发送 accountConfig,该路径真实数据面不触发)。

use std::time::Duration;

use serde_json::{Map, Value, json};

use wr_domain::python_semantics::{py_bool, py_int, py_str};
use wr_infra::proxy::{ProxyDecision, resolve_proxy_decision};
use wr_infra::sqlite_config::SqliteConfigSource;

use super::AdminState;

use super::account_expression::*;

/// 默认刷新间隔 5min(可被 provider/account 级 `refresh_interval_ms` 覆盖)。
pub const DEFAULT_REFRESH_INTERVAL_MS: i64 = 300_000;
/// 刷新间隔下限 30s。
pub const MIN_REFRESH_INTERVAL_MS: i64 = 30_000;
/// error/unsupported 快照的缓存上限 60s。
pub const ERROR_SNAPSHOT_TTL_MS: i64 = 60_000;

/// 连接器抓取超时(Python `httpx.Timeout(30.0)`)。
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// 连接器执行
// ---------------------------------------------------------------------------

/// 认证头(`_build_auth_headers`):api_key 空或 mode ∈ {"", "none"} → 无;
/// `provider-api-key-raw` → 裸值;其余 → Bearer。
fn build_auth_headers(auth_mode: &str, api_key: &str) -> Vec<(String, String)> {
    if api_key.is_empty() || auth_mode.is_empty() || auth_mode == "none" {
        return Vec::new();
    }
    if auth_mode == "provider-api-key-raw" {
        return vec![("authorization".to_owned(), api_key.to_owned())];
    }
    vec![("authorization".to_owned(), format!("Bearer {api_key}"))]
}

/// `_parse_json_safely`:(payload, error) 二元组镜像。
fn parse_json_safely(content_type: &str, body: &str) -> (Option<Value>, Option<String>) {
    if body.is_empty() {
        return (None, Some("empty response body".to_owned()));
    }
    if !content_type.to_ascii_lowercase().contains("json")
        && !body.trim_start().starts_with('{')
        && !body.trim_start().starts_with('[')
    {
        let shown = if content_type.is_empty() {
            "unknown"
        } else {
            content_type
        };
        return (None, Some(format!("non-JSON content-type: {shown}")));
    }
    match serde_json::from_str(body) {
        Ok(value) => (Some(value), None),
        Err(e) => (None, Some(format!("invalid JSON: {}", e))),
    }
}

/// 解析后的连接器配置(Python `AccountConnectorConfig.from_dict` 子集)。
#[derive(Debug, Clone)]
struct Connector {
    id: String,
    ctype: String,
    auth: String,
    endpoint: String,
    headers: Vec<(String, String)>,
    method: String,
    body: Option<Value>,
    mapping: Map<String, Value>,
}

impl Connector {
    fn from_raw(raw: &Value) -> Option<Self> {
        let map = raw.as_object()?;
        let str_or = |key: &str, default: &str| -> String {
            match map.get(key) {
                Some(v) if py_bool(v) => py_str(v).unwrap_or_else(|| default.to_owned()),
                _ => default.to_owned(),
            }
        };
        let headers = map
            .get("headers")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), py_str(v).unwrap_or_default()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        Some(Self {
            ctype: str_or("type", "http-json"),
            id: str_or("id", ""),
            auth: str_or("auth", "provider-api-key"),
            endpoint: str_or("endpoint", ""),
            headers,
            method: str_or("method", "GET").to_ascii_uppercase(),
            body: map.get("body").cloned().filter(|v| !v.is_null()),
            mapping: map
                .get("mapping")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default(),
        })
    }
}

/// 单个 meter(`AccountMirror` 的 Rust 面;`to_value` 键序与 Python
/// `AccountMeter.to_dict` 一致)。
#[derive(Debug, Clone)]
pub struct MeterValue {
    pub id: String,
    pub label: String,
    pub kind: String,
    pub unit: String,
    pub limit: Option<f64>,
    pub remaining: Option<f64>,
    pub used: Option<f64>,
    pub reset_at: Option<String>,
    pub window: Option<String>,
    pub source: String,
}

impl MeterValue {
    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        map.insert("id".into(), json!(self.id));
        map.insert("label".into(), json!(self.label));
        map.insert("kind".into(), json!(self.kind));
        map.insert("unit".into(), json!(self.unit));
        if let Some(limit) = self.limit {
            map.insert("limit".into(), json!(limit));
        }
        if let Some(remaining) = self.remaining {
            map.insert("remaining".into(), json!(remaining));
        }
        if let Some(used) = self.used {
            map.insert("used".into(), json!(used));
        }
        if let Some(reset_at) = &self.reset_at
            && !reset_at.is_empty()
        {
            map.insert("reset_at".into(), json!(reset_at));
        }
        if let Some(window) = &self.window
            && !window.is_empty()
        {
            map.insert("window".into(), json!(window));
        }
        if !self.source.is_empty() {
            map.insert("source".into(), json!(self.source));
        }
        Value::Object(map)
    }
}

/// 连接器错误(`AccountConnectorError`)。
#[derive(Debug, Clone)]
pub struct ConnectorErrorValue {
    pub source: String,
    pub message: String,
    pub connector_id: String,
}

impl ConnectorErrorValue {
    fn to_value(&self) -> Value {
        let mut map = Map::new();
        map.insert("source".into(), json!(self.source));
        map.insert("message".into(), json!(self.message));
        if !self.connector_id.is_empty() {
            map.insert("connector_id".into(), json!(self.connector_id));
        }
        Value::Object(map)
    }
}

/// 单连接器执行结果(`ConnectorResult`)。
#[derive(Debug, Default, Clone)]
struct ConnectorResult {
    source: String,
    meters: Vec<MeterValue>,
    errors: Vec<ConnectorErrorValue>,
    explicit_status: Option<String>,
    message: Option<String>,
}

/// `_build_meter`:id+label+unit 必须解析成功且 limit/remaining/used 至少
/// 一个命中;remaining 缺失而 limit/used 命中 → remaining = limit - used。
fn build_meter(meter_cfg: &Value, payload: &Value) -> Option<MeterValue> {
    let map = meter_cfg.as_object()?;
    let raw_id = map.get("id").filter(|v| py_bool(v))?;
    let label = map.get("label").filter(|v| py_bool(v))?;
    let unit = eval_string_expression(map.get("unit"), payload).unwrap_or_default();
    if unit.is_empty() {
        return None;
    }
    let limit = eval_number_expression(map.get("limit"), payload);
    let remaining = eval_number_expression(map.get("remaining"), payload);
    let used = eval_number_expression(map.get("used"), payload);
    if limit.is_none() && remaining.is_none() && used.is_none() {
        return None;
    }
    let remaining = match remaining {
        Some(r) => Some(r),
        None => match (limit, used) {
            (Some(l), Some(u)) => Some(l - u),
            _ => None,
        },
    };
    Some(MeterValue {
        id: py_str(raw_id)?,
        label: py_str(label)?,
        kind: map
            .get("kind")
            .filter(|v| py_bool(v))
            .and_then(py_str)
            .unwrap_or_else(|| "balance".to_owned()),
        unit,
        limit,
        remaining,
        used,
        // _normalize_reset_at 对字符串原样返回 → 等价于直接取求值结果。
        reset_at: eval_string_expression(map.get("reset_at"), payload),
        window: eval_string_expression(map.get("window"), payload),
        source: String::new(),
    })
}

/// `_combine_status`:explicit(词表内)→ 无 meters:有 error→"error" 否则
/// "unsupported" → 有 meters 且有 error→"warning" → 任一 meter
/// remaining/limit ≤ 0.05→"critical"、≤ 0.20→"warning" → "ok"。
fn combine_status(explicit: Option<&str>, has_errors: bool, meters: &[MeterValue]) -> String {
    if let Some(explicit) = explicit
        && matches!(
            explicit,
            "ok" | "warning" | "critical" | "error" | "unsupported"
        )
    {
        return explicit.to_owned();
    }
    if meters.is_empty() {
        return if has_errors { "error" } else { "unsupported" }.to_owned();
    }
    if has_errors {
        return "warning".to_owned();
    }
    for meter in meters {
        if let (Some(limit), Some(remaining)) = (meter.limit, meter.remaining)
            && limit != 0.0
            && limit > 0.0
        {
            let ratio = remaining / limit;
            if ratio <= 0.05 {
                return "critical".to_owned();
            }
            if ratio <= 0.20 {
                return "warning".to_owned();
            }
        }
    }
    "ok".to_owned()
}

fn severity(status: &str) -> i32 {
    match status {
        "error" => 5,
        "critical" => 4,
        "warning" => 3,
        "ok" => 1,
        _ => 0,
    }
}

fn iso_now() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// 单连接器执行(`_run_connector`):endpoint 校验 → 抓取 → ≥400 错 →
/// JSON 解析 → meters/status/message 求值。
async fn run_connector(
    connector: &Connector,
    api_key: &str,
    proxy_config: Option<&Map<String, Value>>,
) -> ConnectorResult {
    let mut result = ConnectorResult {
        source: connector.ctype.clone(),
        ..Default::default()
    };
    let url = connector.endpoint.trim().to_owned();
    if url.is_empty() {
        result.errors.push(ConnectorErrorValue {
            source: connector.ctype.clone(),
            message: "missing endpoint".to_owned(),
            connector_id: connector.id.clone(),
        });
        return result;
    }
    if !url.starts_with("http://") && !url.starts_with("https://") {
        result.errors.push(ConnectorErrorValue {
            source: connector.ctype.clone(),
            message: format!("endpoint must be http(s): {url}"),
            connector_id: connector.id.clone(),
        });
        return result;
    }
    let mut headers: Vec<(String, String)> = connector.headers.clone();
    if !headers.iter().any(|(k, _)| k == "accept") {
        headers.push(("accept".to_owned(), "application/json".to_owned()));
    }
    headers.extend(build_auth_headers(&connector.auth, api_key));

    // 抓取(镜像 default_fetcher:状态码 + content-type + body 文本)。
    let decision = resolve_proxy_decision(proxy_config, &url);
    let mut builder = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .connect_timeout(FETCH_TIMEOUT);
    builder = match decision {
        ProxyDecision::Direct => builder.no_proxy(),
        ProxyDecision::Url(proxy) => match reqwest::Proxy::all(&proxy) {
            Ok(proxy) => builder.proxy(proxy),
            Err(_) => builder.no_proxy(),
        },
    };
    let client = builder.build().unwrap_or_else(|_| reqwest::Client::new());
    // workspace reqwest 未启用 json feature:手动序列化;content-type 仅在
    // 连接器头未声明时补(镜像 httpx json= 的行为)。
    let body_bytes = serde_json::to_vec(&connector.body.as_ref().unwrap_or(&json!({})))
        .unwrap_or_else(|_| b"{}".to_vec());
    let request = match connector.method.as_str() {
        "POST" => {
            let mut r = client.post(&url);
            for (key, value) in &headers {
                r = r.header(key.as_str(), value.as_str());
            }
            if !headers
                .iter()
                .any(|(key, _)| key.eq_ignore_ascii_case("content-type"))
            {
                r = r.header("content-type", "application/json");
            }
            r.body(body_bytes)
        }
        _ => {
            let mut r = client.get(&url);
            for (key, value) in &headers {
                r = r.header(key.as_str(), value.as_str());
            }
            r
        }
    };
    let response = match request.send().await {
        Ok(response) => response,
        Err(e) => {
            result.errors.push(ConnectorErrorValue {
                source: connector.ctype.clone(),
                message: format!("network error: {e}"),
                connector_id: connector.id.clone(),
            });
            return result;
        }
    };
    let status_code: i64 = i64::from(response.status().as_u16());
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let body_text = response.text().await.unwrap_or_default();
    if status_code >= 400 {
        result.errors.push(ConnectorErrorValue {
            source: connector.ctype.clone(),
            message: format!("HTTP {status_code}"),
            connector_id: connector.id.clone(),
        });
        return result;
    }
    let (payload, error) = parse_json_safely(&content_type, &body_text);
    if let Some(error) = error {
        result.errors.push(ConnectorErrorValue {
            source: connector.ctype.clone(),
            message: error,
            connector_id: connector.id.clone(),
        });
        return result;
    }
    let payload = payload.unwrap_or(Value::Null);

    let meters = connector
        .mapping
        .get("meters")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for meter_cfg in &meters {
        if !meter_cfg.is_object() {
            continue;
        }
        if let Some(meter) = build_meter(meter_cfg, &payload) {
            result.meters.push(meter);
        }
    }
    if let Some(status_str) = eval_string_expression(connector.mapping.get("status"), &payload)
        && matches!(
            status_str.as_str(),
            "ok" | "warning" | "critical" | "error" | "unsupported"
        )
    {
        result.explicit_status = Some(status_str);
    }
    if let Some(message) = eval_string_expression(connector.mapping.get("message"), &payload)
        && !message.is_empty()
    {
        result.message = Some(message);
    }
    result
}

// ---------------------------------------------------------------------------
// 聚合与快照
// ---------------------------------------------------------------------------

/// `AccountSnapshot` 的 Rust 面(`to_value` 键序与 Python `to_dict` 一致)。
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub provider: String,
    pub status: String,
    pub source: String,
    pub meters: Vec<MeterValue>,
    pub updated_at: String,
    pub credential_id: Option<String>,
    pub credential_label: Option<String>,
    pub message: Option<String>,
    pub next_refresh_at: Option<String>,
    pub errors: Vec<ConnectorErrorValue>,
}

impl Snapshot {
    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        map.insert("provider".into(), json!(self.provider));
        map.insert("status".into(), json!(self.status));
        map.insert("source".into(), json!(self.source));
        map.insert(
            "meters".into(),
            Value::Array(self.meters.iter().map(MeterValue::to_value).collect()),
        );
        map.insert("updated_at".into(), json!(self.updated_at));
        if let Some(credential_id) = &self.credential_id {
            map.insert("credential_id".into(), json!(credential_id));
        }
        if let Some(credential_label) = &self.credential_label {
            map.insert("credential_label".into(), json!(credential_label));
        }
        if let Some(message) = &self.message {
            map.insert("message".into(), json!(message));
        }
        if let Some(next_refresh_at) = &self.next_refresh_at {
            map.insert("next_refresh_at".into(), json!(next_refresh_at));
        }
        if !self.errors.is_empty() {
            map.insert(
                "errors".into(),
                Value::Array(
                    self.errors
                        .iter()
                        .map(ConnectorErrorValue::to_value)
                        .collect(),
                ),
            );
        }
        Value::Object(map)
    }
}

/// `_aggregate_results`:meters 按 id 去重(后连接器覆盖,保首现位置);
/// source 去重计数(>1 → "merged");explicit 取 severity 最大(平局取
/// 首个,镜像 Python max);message = 首 connector.message,否则
/// 「有 error 且无 meters」时取 errors[0].message。
fn aggregate_results(
    provider_id: &str,
    account: &AccountRuntime,
    results: &[ConnectorResult],
) -> Snapshot {
    let mut all_meters: Vec<(String, MeterValue)> = Vec::new();
    let mut errors: Vec<ConnectorErrorValue> = Vec::new();
    let mut sources: Vec<String> = Vec::new();
    let mut explicit_statuses: Vec<String> = Vec::new();
    let mut first_message: Option<String> = None;

    for result in results {
        let source = if result.source.is_empty() {
            "http-json".to_owned()
        } else {
            result.source.clone()
        };
        if !sources.contains(&source) {
            sources.push(source);
        }
        errors.extend(result.errors.iter().cloned());
        if let Some(explicit) = &result.explicit_status {
            explicit_statuses.push(explicit.clone());
        }
        if first_message.is_none() {
            first_message = result.message.clone();
        }
        for meter in &result.meters {
            // dict[id] = meter:已存在则覆盖但保留首现位置。
            if let Some(slot) = all_meters.iter_mut().find(|(id, _)| *id == meter.id) {
                slot.1 = meter.clone();
            } else {
                all_meters.push((meter.id.clone(), meter.clone()));
            }
        }
    }
    let final_explicit =
        explicit_statuses
            .iter()
            .fold(None::<String>, |best, status| match &best {
                Some(current) if severity(current) >= severity(status) => best,
                _ => Some(status.clone()),
            });
    let meters: Vec<MeterValue> = all_meters.into_iter().map(|(_, m)| m).collect();
    let status = combine_status(final_explicit.as_deref(), !errors.is_empty(), &meters);
    let message = first_message.or_else(|| {
        if !errors.is_empty() && meters.is_empty() {
            Some(errors[0].message.clone())
        } else {
            None
        }
    });
    Snapshot {
        provider: provider_id.to_owned(),
        status,
        source: if sources.len() > 1 {
            "merged".to_owned()
        } else {
            sources
                .first()
                .cloned()
                .unwrap_or_else(|| "http-json".to_owned())
        },
        meters,
        updated_at: iso_now(),
        credential_id: Some(account.id.clone()),
        credential_label: if account.label.is_empty() {
            None
        } else {
            Some(account.label.clone())
        },
        message,
        next_refresh_at: None,
        errors,
    }
}

/// provider 级 `account` 配置解析(`AccountConfig.from_dict` 子集)。
#[derive(Debug, Clone, Default)]
pub struct AccountConfigData {
    pub refresh_interval_ms: i64,
    pub connectors: Vec<Value>,
}

fn parse_account_config(value: Option<&Value>) -> AccountConfigData {
    let Some(map) = value.and_then(Value::as_object).filter(|m| !m.is_empty()) else {
        return AccountConfigData {
            refresh_interval_ms: DEFAULT_REFRESH_INTERVAL_MS,
            connectors: Vec::new(),
        };
    };
    let connectors = map
        .get("connectors")
        .and_then(Value::as_array)
        .map(|list| list.iter().filter(|c| c.is_object()).cloned().collect())
        .unwrap_or_default();
    // `value.get("refresh_interval_ms") or value.get("refreshIntervalMs")`,
    // int() 失败回落默认。
    let raw = ["refresh_interval_ms", "refreshIntervalMs"]
        .iter()
        .find_map(|key| map.get(*key).filter(|v| py_bool(v)));
    let refresh = raw.and_then(py_int).unwrap_or(DEFAULT_REFRESH_INTERVAL_MS);
    AccountConfigData {
        refresh_interval_ms: refresh,
        connectors,
    }
}

/// `snapshot_default_ttl_ms`:account 级 dict 覆盖 → provider 级 → 默认;
/// `max(MIN, value)` 下限。
fn snapshot_default_ttl_ms(
    provider_config: &AccountConfigData,
    account_config_dict: Option<&Map<String, Value>>,
) -> i64 {
    let raw = account_config_dict.and_then(|dict| {
        ["refresh_interval_ms", "refreshIntervalMs"]
            .iter()
            .find_map(|key| dict.get(*key).filter(|v| py_bool(v)))
    });
    let value = match raw {
        Some(raw) => py_int(raw).unwrap_or(DEFAULT_REFRESH_INTERVAL_MS),
        None => provider_config.refresh_interval_ms,
    };
    std::cmp::max(MIN_REFRESH_INTERVAL_MS, value)
}

// ---------------------------------------------------------------------------
// fetch_snapshot + RPC handler
// ---------------------------------------------------------------------------

/// 单账号运行态(handler 构造,镜像 `ProviderAccount` 的使用面)。
#[derive(Debug, Clone)]
pub struct AccountRuntime {
    pub id: String,
    pub label: String,
    pub account_config: Map<String, Value>,
}

/// 抓取结果:Ok(snapshot)/Err(()) 表示 Python AttributeError 缺陷路径
/// (account_config 非空)→ handler 按警告跳过该账号。
enum FetchOutcome {
    Snapshot(Box<Snapshot>),
    Skipped,
}

/// `fetch_snapshot`:缓存命中(TTL 内且非强刷)→ 重建返回;无 connectors
/// → unsupported(60s 缓存);否则逐连接器顺序执行 → 聚合 → 按 TTL 落缓存。
async fn fetch_snapshot(
    store: &dyn wr_app::ports::SecretStore,
    provider_id: &str,
    provider_config: &AccountConfigData,
    account: &AccountRuntime,
    api_key: &str,
    force_refresh: bool,
    proxy_config: Option<&Map<String, Value>>,
) -> FetchOutcome {
    // Python 缺陷镜像:非空 account_config dict → AttributeError → 跳过。
    if !account.account_config.is_empty() {
        return FetchOutcome::Skipped;
    }
    let connectors = provider_config.connectors.clone();
    let account_id = if account.id.is_empty() {
        "default".to_owned()
    } else {
        account.id.clone()
    };
    let now_ms = wall_ms();
    if !force_refresh
        && let Ok(Some(cached)) = store.get_account_snapshot(provider_id, &account_id).await
    {
        let expires_at = cached.get("expires_at").and_then(py_int).unwrap_or(0);
        if expires_at > now_ms {
            let payload = cached
                .get("payload")
                .filter(|v| py_bool(v))
                .cloned()
                .unwrap_or(json!({}));
            let meters = payload
                .get("meters")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| {
                            let map = item.as_object()?;
                            Some(MeterValue {
                                id: map.get("id").and_then(py_str).unwrap_or_default(),
                                label: map.get("label").and_then(py_str).unwrap_or_default(),
                                kind: map
                                    .get("kind")
                                    .and_then(py_str)
                                    .unwrap_or_else(|| "balance".to_owned()),
                                unit: map.get("unit").and_then(py_str).unwrap_or_default(),
                                limit: map.get("limit").and_then(Value::as_f64),
                                remaining: map.get("remaining").and_then(Value::as_f64),
                                used: map.get("used").and_then(Value::as_f64),
                                reset_at: map
                                    .get("reset_at")
                                    .and_then(Value::as_str)
                                    .map(str::to_owned),
                                window: map
                                    .get("window")
                                    .and_then(Value::as_str)
                                    .map(str::to_owned),
                                source: map.get("source").and_then(py_str).unwrap_or_default(),
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let errors = payload
                .get("errors")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| {
                            let map = item.as_object()?;
                            Some(ConnectorErrorValue {
                                source: map
                                    .get("source")
                                    .and_then(py_str)
                                    .unwrap_or_else(|| "http-json".to_owned()),
                                message: map.get("message").and_then(py_str).unwrap_or_default(),
                                connector_id: map
                                    .get("connector_id")
                                    .and_then(py_str)
                                    .unwrap_or_default(),
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            return FetchOutcome::Snapshot(Box::new(Snapshot {
                provider: provider_id.to_owned(),
                status: cached
                    .get("status")
                    .and_then(py_str)
                    .unwrap_or_else(|| "unsupported".to_owned()),
                source: cached
                    .get("source")
                    .and_then(py_str)
                    .unwrap_or_else(|| "unsupported".to_owned()),
                meters,
                updated_at: cached
                    .get("fetched_at")
                    .and_then(py_str)
                    .unwrap_or_default(),
                credential_id: Some(account_id.clone()),
                credential_label: Some(account.label.clone()),
                message: payload
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                next_refresh_at: payload
                    .get("next_refresh_at")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                errors,
            }));
        }
    }
    if connectors.is_empty() {
        let snapshot = Snapshot {
            provider: provider_id.to_owned(),
            status: "unsupported".to_owned(),
            source: "unsupported".to_owned(),
            meters: Vec::new(),
            updated_at: iso_now(),
            credential_id: Some(account_id.clone()),
            credential_label: Some(account.label.clone()),
            message: Some("No account connectors configured".to_owned()),
            next_refresh_at: None,
            errors: Vec::new(),
        };
        cache_snapshot(
            store,
            provider_id,
            &account_id,
            &snapshot,
            ERROR_SNAPSHOT_TTL_MS,
        )
        .await;
        return FetchOutcome::Snapshot(Box::new(snapshot));
    }
    let mut results: Vec<ConnectorResult> = Vec::new();
    for raw in &connectors {
        let Some(connector) = Connector::from_raw(raw) else {
            continue; // Python from_dict 非 dict → 异常;此处列表已过滤 dict
        };
        results.push(run_connector(&connector, api_key, proxy_config).await);
    }
    let snapshot = aggregate_results(provider_id, account, &results);
    let is_error = snapshot.status == "error" || snapshot.status == "unsupported";
    let ttl = std::cmp::min(
        snapshot_default_ttl_ms(provider_config, Some(&account.account_config)),
        if is_error {
            ERROR_SNAPSHOT_TTL_MS
        } else {
            24 * 60 * 60 * 1000
        },
    );
    cache_snapshot(store, provider_id, &account_id, &snapshot, ttl).await;
    FetchOutcome::Snapshot(Box::new(snapshot))
}

/// 缓存行写入(`_snapshot_payload` 形态:meters/message/next_refresh_at/errors)。
async fn cache_snapshot(
    store: &dyn wr_app::ports::SecretStore,
    provider_id: &str,
    account_id: &str,
    snapshot: &Snapshot,
    ttl_ms: i64,
) {
    let mut payload = Map::new();
    payload.insert(
        "meters".into(),
        Value::Array(snapshot.meters.iter().map(MeterValue::to_value).collect()),
    );
    payload.insert(
        "message".into(),
        snapshot
            .message
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null),
    );
    payload.insert(
        "next_refresh_at".into(),
        snapshot
            .next_refresh_at
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null),
    );
    payload.insert(
        "errors".into(),
        Value::Array(
            snapshot
                .errors
                .iter()
                .map(ConnectorErrorValue::to_value)
                .collect(),
        ),
    );
    let input = wr_app::ports::AccountSnapshotInput {
        provider_id,
        account_id,
        status: &snapshot.status,
        source: &snapshot.source,
        payload: &payload,
        ttl_ms,
    };
    if let Err(e) = store.upsert_account_snapshot(&input).await {
        tracing::warn!(target: "wrouter.admin", %e, "account_snapshot_cache_write_failed");
    }
}

fn wall_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// RPC `getProviderAccountSnapshots`(handlers/provider.py:40-112 逐行镜像):
/// payload={provider?, options?{forceRefresh?}};provider 空 → 遍历全部
/// provider;每 pid 先 ensure_default_account,有账号行走账号行
/// (api_key=account_secret),否则走 provider.accounts(api_key=secret 或
/// 行内值);单账号失败仅 warning 跳过。
pub(crate) async fn svc_account_snapshots(state: &AdminState, args: &[Value]) -> Result<Value, String> {
    use wr_app::ports::SecretStore as _;
    let payload = args
        .first()
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let provider_id = payload
        .get("provider")
        .filter(|v| py_bool(v))
        .and_then(py_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    // options 真值但非 dict → Python AttributeError → 500。
    let options = match payload.get("options") {
        Some(value) if py_bool(value) => value
            .as_object()
            .cloned()
            .ok_or_else(|| "'str' object has no attribute 'get'".to_owned())?,
        _ => Map::new(),
    };
    let force_refresh = options
        .get("forceRefresh")
        .or_else(|| options.get("force_refresh"))
        .map(py_bool)
        .unwrap_or(false);

    // ids:显式 provider 或全部配置 id(逐字:仅 truthy id)。
    let source = SqliteConfigSource::new(&state.settings.data_dir);
    let config = source.load_app_config().map_err(|e| e.to_string())?;
    let ids: Vec<String> = if !provider_id.is_empty() {
        vec![provider_id]
    } else {
        config
            .providers
            .iter()
            .filter_map(|record| record.raw.get("id").filter(|v| py_bool(v)).and_then(py_str))
            .collect()
    };

    let store = &state.store;
    let proxy_ptr: Option<Map<String, Value>> = Some(
        state
            .deps
            .snapshot()
            .await
            .map(|snapshot| snapshot.config.proxy.clone())
            .unwrap_or_default(),
    );
    let mut results: Vec<Value> = Vec::new();
    for pid in ids {
        if pid.is_empty() {
            continue;
        }
        let accounts = store.list_accounts(&pid).await.unwrap_or_default();
        // provider_dict:load 的原始 Providers 数组里按 id 查(str 比较)。
        let provider_dict = config.providers.iter().find_map(|record| {
            (py_str(record.raw.get("id").unwrap_or(&Value::Null)).unwrap_or_default() == pid)
                .then(|| record.raw.clone())
        });
        let Some(provider_dict) = provider_dict else {
            continue;
        };
        let provider_entity = wr_domain::provider::Provider::from_raw(&provider_dict)
            .map_err(|e| format!("provider '{pid}' invalid: {e}"))?;
        // provider 级 account 配置(Python Provider.from_dict → provider.account)。
        let provider_config = parse_account_config(provider_dict.get("account"));
        if accounts.is_empty() {
            for acc in &provider_entity.accounts {
                let secret = store
                    .account_secret(&pid, &acc.id)
                    .await
                    .unwrap_or_default();
                let api_key = if !secret.is_empty() {
                    secret
                } else {
                    acc.api_key.clone()
                };
                let runtime = AccountRuntime {
                    id: acc.id.clone(),
                    label: acc.label.clone(),
                    account_config: acc.account_config.clone(),
                };
                match fetch_snapshot(
                    store.as_ref(),
                    &pid,
                    &provider_config,
                    &runtime,
                    &api_key,
                    force_refresh,
                    proxy_ptr.as_ref(),
                )
                .await
                {
                    FetchOutcome::Snapshot(snapshot) => results.push(snapshot.to_value()),
                    FetchOutcome::Skipped => {
                        tracing::warn!(target: "wrouter.admin", provider = %pid, account = %acc.id, "snapshot failed: account_config connectors unsupported (mirrors Python AttributeError path)");
                    }
                }
            }
        } else {
            for acc_dict in &accounts {
                let Some(acc_map) = acc_dict.as_object() else {
                    continue;
                };
                // acc_id = str(acc_dict.get("id") or "")(无 strip)。
                let acc_id = acc_map
                    .get("id")
                    .filter(|v| py_bool(v))
                    .and_then(py_str)
                    .unwrap_or_default();
                if acc_id.is_empty() {
                    continue;
                }
                let api_key = store
                    .account_secret(&pid, &acc_id)
                    .await
                    .unwrap_or_default();
                let runtime = AccountRuntime {
                    id: acc_id.clone(),
                    label: acc_map
                        .get("label")
                        .filter(|v| py_bool(v))
                        .and_then(py_str)
                        .unwrap_or_default(),
                    account_config: acc_map
                        .get("account_config")
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default(),
                };
                match fetch_snapshot(
                    store.as_ref(),
                    &pid,
                    &provider_config,
                    &runtime,
                    &api_key,
                    force_refresh,
                    proxy_ptr.as_ref(),
                )
                .await
                {
                    FetchOutcome::Snapshot(snapshot) => results.push(snapshot.to_value()),
                    FetchOutcome::Skipped => {
                        tracing::warn!(target: "wrouter.admin", provider = %pid, account = %acc_id, "snapshot iterate failed: account_config connectors unsupported (mirrors Python AttributeError path)");
                    }
                }
            }
        }
    }
    Ok(Value::Array(results))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_path_resolution() {
        let payload = json!({"a": {"b": {"c": 7}}, "items": [10, 20], "unusual key": "v"});
        assert_eq!(
            resolve_simple_path(&payload, "$").unwrap(),
            Some(payload.clone())
        );
        assert_eq!(
            resolve_simple_path(&payload, "$.a.b.c").unwrap(),
            Some(json!(7))
        );
        assert_eq!(
            resolve_simple_path(&payload, "$[\"unusual key\"]").unwrap(),
            Some(json!("v"))
        );
        assert_eq!(
            resolve_simple_path(&payload, "$.items[1]").unwrap(),
            Some(json!(20))
        );
        assert_eq!(resolve_simple_path(&payload, "$.a.b.z").unwrap(), None);
        assert_eq!(resolve_simple_path(&payload, "$.items[9]").unwrap(), None);
        // 过滤器必须走 resolve_filter。
        assert!(resolve_simple_path(&payload, "$.items[?(@.x)]").is_err());
    }

    #[test]
    fn filter_resolution() {
        let payload = json!({"limits": [
            {"type": "TOKENS_LIMIT", "unit": 3, "used": 10},
            {"type": "OTHER", "unit": 9}
        ]});
        let hit = resolve_filter(
            &payload,
            "$.limits[?(@.type==\"TOKENS_LIMIT\" && @.unit==3)]",
        )
        .unwrap();
        assert_eq!(
            hit,
            Some(json!({"type": "TOKENS_LIMIT", "unit": 3, "used": 10}))
        );
        assert_eq!(
            resolve_filter(&payload, "$.limits[?(@.type==\"NOPE\")]").unwrap(),
            None
        );
        assert!(resolve_filter(&payload, "$.limits[?(@.type==)]").is_err());
    }

    #[test]
    fn number_expression_forms() {
        let payload = json!({"used": 40, "total": 100});
        assert_eq!(eval_number_expression(Some(&json!(5)), &payload), Some(5.0));
        assert_eq!(
            eval_number_expression(Some(&json!("100 - $.used")), &payload),
            Some(60.0)
        );
        assert_eq!(
            eval_number_expression(Some(&json!(["$.missing", 0, "literal"])), &payload),
            Some(0.0)
        );
        assert_eq!(
            eval_number_expression(Some(&json!("$.total")), &payload),
            Some(100.0)
        );
        assert_eq!(
            eval_number_expression(Some(&json!("$.nothing")), &payload),
            None
        );
    }

    #[test]
    fn string_expression_forms() {
        let payload = json!({"reset": 1700000000, "name": "Plan Pro"});
        assert_eq!(
            eval_string_expression(Some(&json!("Plan")), &payload),
            Some("Plan".to_owned())
        );
        assert_eq!(
            eval_string_expression(Some(&json!("$.name")), &payload),
            Some("Plan Pro".to_owned())
        );
        // 数值路径 → str(value)。
        assert_eq!(
            eval_string_expression(Some(&json!("$.reset")), &payload),
            Some("1700000000".to_owned())
        );
        assert_eq!(
            eval_string_expression(Some(&json!(["$.missing", "fallback"])), &payload),
            Some("fallback".to_owned())
        );
    }

    #[test]
    fn reset_at_normalization() {
        assert_eq!(
            normalize_reset_at(Some(&json!(1_700_000_000))),
            Some("2023-11-14T22:13:20Z".to_owned())
        );
        assert_eq!(
            normalize_reset_at(Some(&json!(1_700_000_000_000_i64))),
            Some("2023-11-14T22:13:20Z".to_owned())
        );
        assert_eq!(normalize_reset_at(Some(&json!(0))), None);
        assert_eq!(
            normalize_reset_at(Some(&json!("2024-01-01T00:00:00Z"))),
            Some("2024-01-01T00:00:00Z".to_owned())
        );
    }

    #[test]
    fn meter_requires_id_label_unit_and_number() {
        let payload = json!({"balance": 5.0, "total": 100.0});
        let ok = json!({"id": "m", "label": "M", "unit": "USD",
            "limit": "$.total", "remaining": "$.balance"});
        let meter = build_meter(&ok, &payload).unwrap();
        assert_eq!(meter.limit, Some(100.0));
        assert_eq!(meter.remaining, Some(5.0));
        // 缺 unit → 丢弃。
        assert!(build_meter(&json!({"id": "m", "label": "M"}), &payload).is_none());
        // 无数值命中 → 丢弃。
        assert!(build_meter(&json!({"id": "m", "label": "M", "unit": "USD"}), &payload).is_none());
        // remaining 缺失 → limit - used。
        let derived = build_meter(
            &json!({"id": "m", "label": "M", "unit": "USD", "limit": "$.total", "used": "$.balance"}),
            &payload,
        )
        .unwrap();
        assert_eq!(derived.remaining, Some(95.0));
    }

    #[test]
    fn status_inference_severity_order() {
        let meter = MeterValue {
            id: "m".into(),
            label: "M".into(),
            kind: "balance".into(),
            unit: "USD".into(),
            limit: Some(100.0),
            remaining: Some(3.0),
            used: None,
            reset_at: None,
            window: None,
            source: String::new(),
        };
        // explicit 词表内直接胜出。
        assert_eq!(
            combine_status(Some("warning"), false, std::slice::from_ref(&meter)),
            "warning"
        );
        assert_eq!(
            combine_status(Some("bogus"), false, std::slice::from_ref(&meter)),
            "critical"
        );
        // 无 meters:error / unsupported。
        assert_eq!(combine_status(None, true, &[]), "error");
        assert_eq!(combine_status(None, false, &[]), "unsupported");
        // 有 meters 有 error → warning。
        assert_eq!(
            combine_status(None, true, std::slice::from_ref(&meter)),
            "warning"
        );
        // 阈值:3/100 ≤ 0.05 → critical。
        assert_eq!(
            combine_status(None, false, std::slice::from_ref(&meter)),
            "critical"
        );
        let low = MeterValue {
            remaining: Some(15.0),
            ..meter.clone()
        };
        assert_eq!(
            combine_status(None, false, std::slice::from_ref(&low)),
            "warning"
        );
        let fine = MeterValue {
            remaining: Some(90.0),
            ..meter
        };
        assert_eq!(
            combine_status(None, false, std::slice::from_ref(&fine)),
            "ok"
        );
    }

    #[test]
    fn ttl_defaults_and_minimum() {
        let provider = AccountConfigData {
            refresh_interval_ms: 45_000,
            connectors: Vec::new(),
        };
        assert_eq!(snapshot_default_ttl_ms(&provider, None), 45_000);
        let mut dict = Map::new();
        dict.insert("refreshIntervalMs".into(), json!(120_000));
        assert_eq!(snapshot_default_ttl_ms(&provider, Some(&dict)), 120_000);
        let mut small = Map::new();
        small.insert("refresh_interval_ms".into(), json!(1_000));
        assert_eq!(
            snapshot_default_ttl_ms(&provider, Some(&small)),
            MIN_REFRESH_INTERVAL_MS
        );
        let mut garbage = Map::new();
        garbage.insert("refresh_interval_ms".into(), json!("abc"));
        assert_eq!(
            snapshot_default_ttl_ms(&provider, Some(&garbage)),
            DEFAULT_REFRESH_INTERVAL_MS
        );
    }
}
