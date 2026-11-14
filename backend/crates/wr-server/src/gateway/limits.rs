//! 请求前置限流 / 预算检查(方案 #14 R4.8;镜像 Python
//! `application/gateway/precheck.py`)。
//!
//! 转发前就可以拒掉请求,不消耗上游调用:429(限流)/ 402(预算超支)。
//!
//! 窗口算法是**固定窗口**(不是滑动):桶起点 `now_ms - now_ms % window_ms`,
//! 键 `"{identity}:{scope}:{metric}:{window_start}"`,所以窗口换了自然从 0
//! 重新计数。`InMemoryPrecheckStorage.increment` 里那句
//! `counter.window_start < window_start` 的换桶分支实际不可达(键里已经带
//! 了窗口起点),因此**老桶不会被回收** —— 这个 Python 事实原样保留,
//! 由 R4.11 的条目上限与进程重启兜住。
//!
//! 两个作用域:
//! - `global`:来自 `reliability.precheck.rate_limit_rules`,受
//!   `precheck.enabled` 门控;
//! - `key`:来自 API key 的 `limits`(由转发层用
//!   [`api_key_limits_to_rules`] 组装),**不受 `enabled` 门控** —— 给某个
//!   key 配了限制就生效;计数器按作用域分桶,永不相撞。
//!
//! 失败即返回,**已经吃进窗口的量不回滚**(只有预算那条会回滚)——
//! Python 的 docstring 说「失败不改计数」,与实现不符,以实现为准。

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::{Map, Value, json};
pub use wr_infra::config::PrecheckConfig;

/// 支持的指标名。
pub const VALID_METRICS: &[&str] = &["rpm", "rph", "rpd", "tpm", "tph", "tpd", "ipm"];

/// 规则未给 `window_ms` 时的默认窗口(秒)。
pub const DEFAULT_WINDOW_SECONDS: &[(&str, i64)] = &[
    ("rpm", 60),
    ("rph", 3600),
    ("rpd", 86400),
    ("tpm", 60),
    ("tph", 3600),
    ("tpd", 86400),
    ("ipm", 60),
];

/// 单请求估算用量,喂给 token/image/预算类检查。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PrecheckEstimate {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub image_count: i64,
    pub video_seconds: i64,
    pub estimated_cost_usd: f64,
}

/// 拒绝结果(`kind` ∈ `rate_limit` | `budget`)。
#[derive(Debug, Clone)]
pub struct PrecheckFailure {
    pub kind: &'static str,
    pub status: u16,
    pub code: &'static str,
    pub message: String,
    pub details: Map<String, Value>,
}

impl PrecheckFailure {
    /// 客户端错误体(镜像 forwarding.py:`type` 与 `code` 同值)。
    pub fn to_body(&self) -> Value {
        json!({
            "error": {
                "type": self.code,
                "message": self.message,
                "code": self.code,
            }
        })
    }
}

/// 检查结果:放行(带回写后的估算)或拒绝。
#[derive(Debug, Clone)]
pub enum PrecheckResult {
    Ok(PrecheckEstimate),
    Rejected(PrecheckFailure),
}

/// 窗口计数存储(默认内存;接口留出来,后续可换 Redis)。
pub trait PrecheckStorage: Send + Sync {
    fn increment(&self, key: &str, amount: i64, window_start: i64) -> i64;
    fn add_cost(&self, identity: &str, amount: f64) -> f64;
}

#[derive(Default)]
pub struct InMemoryPrecheckStorage {
    counters: Mutex<HashMap<String, i64>>,
    cost: Mutex<HashMap<String, f64>>,
}

impl PrecheckStorage for InMemoryPrecheckStorage {
    /// 键里已含窗口起点 → 换窗口就是换键,老键不再命中。
    fn increment(&self, key: &str, amount: i64, _window_start: i64) -> i64 {
        let mut guard = self.counters.lock().unwrap_or_else(|e| e.into_inner());
        let value = guard.get(key).copied().unwrap_or(0) + amount;
        guard.insert(key.to_owned(), value);
        value
    }

    fn add_cost(&self, identity: &str, amount: f64) -> f64 {
        let mut guard = self.cost.lock().unwrap_or_else(|e| e.into_inner());
        let value = guard.get(identity).copied().unwrap_or(0.0) + amount;
        guard.insert(identity.to_owned(), value);
        value
    }
}

/// 转发前的限流/预算执行器。
pub struct Prechecker {
    storage: Mutex<Box<dyn PrecheckStorage>>,
    config: Mutex<PrecheckConfig>,
    clock: Box<dyn Fn() -> i64 + Send + Sync>,
}

impl Default for Prechecker {
    fn default() -> Self {
        Self::new(PrecheckConfig::default())
    }
}

impl Prechecker {
    pub fn new(config: PrecheckConfig) -> Self {
        Self {
            storage: Mutex::new(Box::<InMemoryPrecheckStorage>::default()),
            config: Mutex::new(config),
            clock: Box::new(|| chrono::Utc::now().timestamp_millis()),
        }
    }

    /// 覆盖时钟源(测试要能把窗口推进到下一个周期)。
    pub fn with_clock(mut self, clock: impl Fn() -> i64 + Send + Sync + 'static) -> Self {
        self.clock = Box::new(clock);
        self
    }

    pub fn update_config(&self, config: &PrecheckConfig) {
        let mut guard = self.config.lock().unwrap_or_else(|e| e.into_inner());
        *guard = config.clone();
    }

    fn config(&self) -> PrecheckConfig {
        self.config
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn now_ms(&self) -> i64 {
        (self.clock)()
    }

    /// 评估一个来自 `identity` 的请求。
    ///
    /// 成功时计数已提交;失败时**该请求要吃进窗口的量已经写入**(见模块
    /// 文档),只有预算那条会回滚累加值,避免重试叠加。
    pub fn evaluate(
        &self,
        identity: &str,
        estimate: Option<PrecheckEstimate>,
        requested_cost_usd: f64,
        extra_rules: &[Value],
    ) -> PrecheckResult {
        let estimate = estimate.unwrap_or_default();
        let config = self.config();
        let now_ms = self.now_ms();

        if config.enabled
            && let Some(failure) = self.evaluate_rules(
                identity,
                &config.rate_limit_rules,
                &estimate,
                "global",
                now_ms,
            )
        {
            return PrecheckResult::Rejected(failure);
        }
        if !extra_rules.is_empty()
            && let Some(failure) =
                self.evaluate_rules(identity, extra_rules, &estimate, "key", now_ms)
        {
            return PrecheckResult::Rejected(failure);
        }
        if config.budget_max_cost_usd > 0.0 {
            let cost = if requested_cost_usd > 0.0 {
                requested_cost_usd
            } else {
                estimate.estimated_cost_usd
            };
            let cost = cost.max(0.0);
            if cost > 0.0 {
                let cumulative = {
                    let guard = self.storage.lock().unwrap_or_else(|e| e.into_inner());
                    guard.add_cost(identity, cost)
                };
                if cumulative > config.budget_max_cost_usd {
                    let used = cumulative - cost;
                    {
                        let guard = self.storage.lock().unwrap_or_else(|e| e.into_inner());
                        guard.add_cost(identity, -cost);
                    }
                    return PrecheckResult::Rejected(PrecheckFailure {
                        kind: "budget",
                        status: 402,
                        code: "budget_exceeded",
                        message: format!(
                            "Budget exceeded ({cumulative:.6}/{:.6} USD).",
                            config.budget_max_cost_usd
                        ),
                        details: map_of(vec![
                            ("subject", json!(identity)),
                            ("scope", json!("global")),
                            ("limit", json!(config.budget_max_cost_usd)),
                            ("used", json!(used)),
                            ("requested", json!(cost)),
                        ]),
                    });
                }
            }
        }
        PrecheckResult::Ok(estimate)
    }

    fn evaluate_rules(
        &self,
        identity: &str,
        rules: &[Value],
        estimate: &PrecheckEstimate,
        scope: &str,
        now_ms: i64,
    ) -> Option<PrecheckFailure> {
        for rule in rules {
            let Some(map) = rule.as_object() else {
                continue;
            };
            let metric = map
                .get("metric")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_lowercase();
            if !VALID_METRICS.contains(&metric.as_str()) {
                continue;
            }
            let limit = field_int(map, "limit", 0);
            if limit <= 0 {
                continue;
            }
            let default_seconds = DEFAULT_WINDOW_SECONDS
                .iter()
                .find(|(name, _)| *name == metric)
                .map_or(60, |(_, seconds)| *seconds);
            let window_ms = {
                let raw = field_int(map, "window_ms", 0);
                if raw != 0 {
                    raw
                } else {
                    default_seconds * 1000
                }
            };
            let window_start = now_ms - now_ms.rem_euclid(window_ms);
            let amount = amount_for_metric(&metric, estimate);
            if amount <= 0 {
                continue;
            }
            let key = format!("{identity}:{scope}:{metric}:{window_start}");
            let used = {
                let guard = self.storage.lock().unwrap_or_else(|e| e.into_inner());
                guard.increment(&key, amount, window_start)
            };
            if used > limit {
                return Some(PrecheckFailure {
                    kind: "rate_limit",
                    status: 429,
                    code: "rate_limit_exceeded",
                    message: format!(
                        "Rate limit exceeded for {} ({used}/{limit}).",
                        metric.to_uppercase()
                    ),
                    details: map_of(vec![
                        ("subject", json!(identity)),
                        ("scope", json!(scope)),
                        ("metric", json!(metric)),
                        ("window_ms", json!(window_ms)),
                        ("limit", json!(limit)),
                        ("used", json!(used)),
                        ("requested", json!(amount)),
                    ]),
                });
            }
        }
        None
    }
}

/// 指标 → 本次要计的量。
pub fn amount_for_metric(metric: &str, estimate: &PrecheckEstimate) -> i64 {
    match metric {
        "rpm" | "rph" | "rpd" => 1,
        "tpm" | "tph" | "tpd" => estimate.total_tokens.max(0),
        "ipm" => estimate.image_count.max(0),
        _ => 0,
    }
}

/// 把 `ApiKeyConfig.limits` 翻成 precheck 规则(镜像
/// `api_key_limits_to_rules`)。
///
/// 简单周期指标 1:1 映射;自定义窗口用「基础指标 + 显式 window_ms」表达:
/// `maxRequests`+`windowMs` → `rpm`,`maxTokens`+`quotaWindowMs` → `tpm`。
/// 图像限制(ipm/iph/ipd)**故意不翻**——估算器产不出 image_count,
/// UI 收了字段但没有执行效果。
pub fn api_key_limits_to_rules(limits: Option<&Value>) -> Vec<Value> {
    let Some(map) = limits.and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut rules = Vec::new();
    for metric in ["rpm", "rph", "rpd", "tpm", "tph", "tpd"] {
        let limit = field_int(map, metric, 0);
        if limit > 0 {
            rules.push(json!({"metric": metric, "limit": limit}));
        }
    }
    let max_requests = field_int(map, "maxRequests", 0);
    if max_requests > 0 {
        let window_ms = field_int(map, "windowMs", 0);
        rules.push(json!({
            "metric": "rpm",
            "limit": max_requests,
            "window_ms": if window_ms != 0 { window_ms } else { 60_000 },
        }));
    }
    let max_tokens = field_int(map, "maxTokens", 0);
    if max_tokens > 0 {
        let quota_window_ms = field_int(map, "quotaWindowMs", 0);
        rules.push(json!({
            "metric": "tpm",
            "limit": max_tokens,
            "window_ms": if quota_window_ms != 0 { quota_window_ms } else { 86_400_000 },
        }));
    }
    rules
}

/// 限流用的身份键:凭据**截断到 12 字符**(策略匹配用的
/// `extract_principal` 保留全量,两者职责不同)。
pub fn extract_identity(headers: &[(String, String)]) -> String {
    let header = |name: &str| -> Option<String> {
        headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.clone())
    };
    if let Some(key) = header("x-api-key").filter(|value| !value.is_empty()) {
        return format!("key:{}", key.chars().take(12).collect::<String>());
    }
    if let Some(key) = header("api-key").filter(|value| !value.is_empty()) {
        return format!("key:{}", key.chars().take(12).collect::<String>());
    }
    let auth = header("authorization").unwrap_or_default();
    if auth.to_lowercase().starts_with("bearer ") && auth.chars().count() > 7 {
        let tail: String = auth.chars().skip(7).collect();
        let prefix: String = tail.chars().take(12).collect();
        return format!("key:{prefix}");
    }
    "anonymous".to_owned()
}

/// Python `int(rule.get(key) or default)`:假值/缺失/转不动一律走默认。
fn field_int(map: &Map<String, Value>, key: &str, default: i64) -> i64 {
    match map.get(key) {
        Some(value) if wr_domain::python_semantics::py_bool(value) => {
            wr_domain::python_semantics::py_int(value).unwrap_or(default)
        }
        _ => default,
    }
}

fn map_of(entries: Vec<(&str, Value)>) -> Map<String, Value> {
    let mut map = Map::new();
    for (key, value) in entries {
        map.insert(key.to_owned(), value);
    }
    map
}
