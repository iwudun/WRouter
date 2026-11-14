//! config JSON 读取(方案 #14 R1.4,C9 兼容)。
//!
//! 与 Python `domain/configuration/entities.py::AppConfig.from_dict`
//! **逐行为**对齐:键别名(current PascalCase / legacy camelCase /
//! snake_case)、缺省值、数值钳制、truthy 强转、Router 块收集规则。
//! 持久化键名(如 `Providers`/`APIKEYS` 大写形式)是 C9/C5 契约,
//! 不得"规范化"掉。
//!
//! 解析失败(镜像 Python 抛 ValueError/TypeError 的场景)统一返回
//! [`ConfigError`];缺省值行为与 Python 完全一致,用 `wr-infra/tests`
//! 的 fixtures 钉住。

use serde_json::{Map, Value, json};

use crate::python_semantics::{
    get_key_chain, get_truthy_chain, py_bool, py_float, py_int, py_str, try_truthy_chain,
};
use wr_domain::configuration::normalize_theme;
use wr_domain::policy::PolicyEffect;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config JSON 顶层必须是对象(镜像 Python dict() 对非 dict 抛错)")]
    TopLevelNotObject,
    #[error("`{path}`: {reason}")]
    Invalid { path: String, reason: String },
}

impl ConfigError {
    fn invalid(path: impl Into<String>, reason: impl Into<String>) -> Self {
        ConfigError::Invalid {
            path: path.into(),
            reason: reason.into(),
        }
    }
}

/// 错误信息里标出实际生效的键名,便于定位手改配置的问题。
fn key_path(path: &str, first_key: &str) -> String {
    format!("{path}.{first_key}")
}

/// 镜像 Python `CircuitBreakerConfig`。
#[derive(Debug, Clone, PartialEq)]
pub struct CircuitBreakerConfig {
    pub enabled: bool,
    pub failure_threshold: i64,
    pub cooldown_ms: i64,
    pub failure_status_codes: Vec<i64>,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            failure_threshold: 3,
            cooldown_ms: 30_000,
            failure_status_codes: vec![408, 429, 500, 502, 503, 504],
        }
    }
}

impl CircuitBreakerConfig {
    fn from_object(value: Option<&Value>, path: &str) -> Result<Self, ConfigError> {
        let Some(v) = value else {
            return Ok(Self::default());
        };
        let m = as_object(v, path)?;
        Ok(Self {
            enabled: bool_key(m, &["enabled"], true),
            failure_threshold: int_key_clamped(
                m,
                &["failure_threshold"],
                3,
                1,
                &key_path(path, "failure_threshold"),
            )?,
            cooldown_ms: int_key_clamped(
                m,
                &["cooldown_ms"],
                30_000,
                1,
                &key_path(path, "cooldown_ms"),
            )?,
            failure_status_codes: int_list_key(
                m,
                &["failure_status_codes"],
                &[408, 429, 500, 502, 503, 504],
                &key_path(path, "failure_status_codes"),
            )?,
        })
    }
}

/// 镜像 Python `ConcurrencyConfig`。
#[derive(Debug, Clone, PartialEq)]
pub struct ConcurrencyConfig {
    pub enabled: bool,
    pub max_in_flight_per_provider: i64,
    pub queue_timeout_ms: i64,
}

impl Default for ConcurrencyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_in_flight_per_provider: 8,
            queue_timeout_ms: 0,
        }
    }
}

impl ConcurrencyConfig {
    fn from_object(value: Option<&Value>, path: &str) -> Result<Self, ConfigError> {
        let Some(v) = value else {
            return Ok(Self::default());
        };
        let m = as_object(v, path)?;
        Ok(Self {
            enabled: bool_key(m, &["enabled"], true),
            max_in_flight_per_provider: int_key_clamped(
                m,
                &["max_in_flight_per_provider"],
                8,
                1,
                &key_path(path, "max_in_flight_per_provider"),
            )?,
            queue_timeout_ms: int_key_clamped(
                m,
                &["queue_timeout_ms"],
                0,
                0,
                &key_path(path, "queue_timeout_ms"),
            )?,
        })
    }
}

/// 镜像 Python `IdempotencyConfig`。
#[derive(Debug, Clone, PartialEq)]
pub struct IdempotencyConfig {
    pub enabled: bool,
    pub ttl_ms: i64,
    pub max_entries: i64,
    pub header_name: String,
    pub cache_error_responses: bool,
}

impl Default for IdempotencyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            ttl_ms: 300_000,
            max_entries: 1000,
            header_name: "idempotency-key".to_owned(),
            cache_error_responses: false,
        }
    }
}

impl IdempotencyConfig {
    fn from_object(value: Option<&Value>, path: &str) -> Result<Self, ConfigError> {
        let Some(v) = value else {
            return Ok(Self::default());
        };
        let m = as_object(v, path)?;
        // Python: str(value.get("header_name", value.get("headerName", "idempotency-key"))) or "idempotency-key"
        // str(None) === "None"(truthy)—— 该怪癖由 python_semantics::py_str 复刻,测试钉住。
        let default_header = Value::String("idempotency-key".to_owned());
        let header_raw =
            get_key_chain(m, &["header_name"]).unwrap_or(&default_header);
        let header_name = py_str(header_raw).ok_or_else(|| {
            ConfigError::invalid(key_path(path, "header_name"), "无法 str() 化(数组/对象)")
        })?;
        let header_name = if header_name.is_empty() {
            "idempotency-key".to_owned()
        } else {
            header_name
        };
        Ok(Self {
            enabled: bool_key(m, &["enabled"], true),
            ttl_ms: int_key_clamped(
                m,
                &["ttl_ms"],
                300_000,
                1000,
                &key_path(path, "ttl_ms"),
            )?,
            max_entries: int_key_clamped(
                m,
                &["max_entries"],
                1000,
                1,
                &key_path(path, "max_entries"),
            )?,
            header_name,
            cache_error_responses: bool_key(
                m,
                &["cache_error_responses"],
                false,
            ),
        })
    }
}

/// 镜像 Python `HealthConfig`。
#[derive(Debug, Clone, PartialEq)]
pub struct HealthConfig {
    pub enabled: bool,
    pub initial_delay_ms: i64,
    pub interval_ms: i64,
    pub timeout_ms: i64,
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            initial_delay_ms: 5000,
            interval_ms: 30_000,
            timeout_ms: 5000,
        }
    }
}

impl HealthConfig {
    fn from_object(value: Option<&Value>, path: &str) -> Result<Self, ConfigError> {
        let Some(v) = value else {
            return Ok(Self::default());
        };
        let m = as_object(v, path)?;
        Ok(Self {
            enabled: bool_key(m, &["enabled"], true),
            initial_delay_ms: int_key_clamped(
                m,
                &["initial_delay_ms"],
                5000,
                0,
                &key_path(path, "initial_delay_ms"),
            )?,
            interval_ms: int_key_clamped(
                m,
                &["interval_ms"],
                30_000,
                1000,
                &key_path(path, "interval_ms"),
            )?,
            timeout_ms: int_key_clamped(
                m,
                &["timeout_ms"],
                5000,
                500,
                &key_path(path, "timeout_ms"),
            )?,
        })
    }
}

/// 镜像 Python `PrecheckConfig`。
#[derive(Debug, Clone, PartialEq)]
pub struct PrecheckConfig {
    pub enabled: bool,
    pub rate_limit_rules: Vec<Value>,
    pub budget_max_cost_usd: f64,
}

impl Default for PrecheckConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            rate_limit_rules: Vec::new(),
            budget_max_cost_usd: 0.0,
        }
    }
}

impl PrecheckConfig {
    fn from_object(value: Option<&Value>, path: &str) -> Result<Self, ConfigError> {
        let Some(v) = value else {
            return Ok(Self::default());
        };
        let m = as_object(v, path)?;
        let empty_rules = Value::Array(Vec::new());
        let rules_raw = get_truthy_chain(m, &["rate_limit_rules"], &empty_rules);
        let Value::Array(rules) = rules_raw else {
            return Err(ConfigError::invalid(
                key_path(path, "rate_limit_rules"),
                "必须是数组",
            ));
        };
        // Python: [dict(rule) for rule in rules_raw if isinstance(rule, dict)] —— 静默过滤非对象项。
        let rate_limit_rules = rules.iter().filter(|r| r.is_object()).cloned().collect();
        let budget_default = Value::from(0.0);
        let budget_raw = get_key_chain(m, &["budget_max_cost_usd"])
            .unwrap_or(&budget_default);
        let budget = py_float(budget_raw).ok_or_else(|| {
            ConfigError::invalid(key_path(path, "budget_max_cost_usd"), "无法转 float")
        })?;
        Ok(Self {
            enabled: bool_key(m, &["enabled"], false),
            rate_limit_rules,
            budget_max_cost_usd: budget.max(0.0),
        })
    }
}

/// 镜像 Python `MetricsConfig`。
#[derive(Debug, Clone, PartialEq)]
pub struct MetricsConfig {
    pub enabled: bool,
    pub include_provider_health: bool,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            include_provider_health: true,
        }
    }
}

impl MetricsConfig {
    fn from_object(value: Option<&Value>, path: &str) -> Result<Self, ConfigError> {
        let Some(v) = value else {
            return Ok(Self::default());
        };
        let m = as_object(v, path)?;
        Ok(Self {
            enabled: bool_key(m, &["enabled"], true),
            include_provider_health: bool_key(
                m,
                &["include_provider_health"],
                true,
            ),
        })
    }
}

/// 请求级全局重试预算默认值(方案 20261015_01 P2-2)。
pub const DEFAULT_RETRY_BUDGET: u32 = 16;

/// 答案完成后的有界排空上限(方案 20261108_01 §4.2):观察到语义 `Finish`
/// 后继续读上游至多该毫秒数(捕获尾随 usage / 终态帧);到期即按「答案已
/// 完整」落日志,不再等上游阻塞的终态帧。
pub const DEFAULT_STREAM_TERMINAL_GRACE_MS: u64 = 5_000;

/// 流空闲超时默认值(方案 20261108_01 §4.2):连续该毫秒数没有收到任何
/// 上游记录(keep-alive 注释行不算)即收尾,不再等 1h 超龄清扫。
pub const DEFAULT_STREAM_IDLE_TIMEOUT_MS: u64 = 90_000;

/// 镜像 Python `ReliabilityConfig`(P2 可靠性子系统容器)。
#[derive(Debug, Clone, PartialEq)]
pub struct ReliabilityConfig {
    pub circuit_breaker: CircuitBreakerConfig,
    pub concurrency: ConcurrencyConfig,
    pub idempotency: IdempotencyConfig,
    pub health: HealthConfig,
    pub precheck: PrecheckConfig,
    pub metrics: MetricsConfig,
    /// 请求级全局重试预算(20261015_01 P2-2):单个请求跨计划 attempts ×
    /// 候选链 × 同 provider 重试的上游调用总次数上限,归零即终止并回
    /// 兜底响应。三层相乘没有总闸时最坏可到几十次上游调用。
    pub retry_budget: u32,
    /// 20261108_01 §4.2:答案完成(`Finish` 事实)后的有界排空上限(毫秒)。
    /// 0 = 观察到 `Finish` 即落日志(不排空)。
    pub stream_terminal_grace_ms: u64,
    /// 20261108_01 §4.2:流空闲超时(毫秒;连续无任何上游记录即收尾)。
    pub stream_idle_timeout_ms: u64,
}

impl Default for ReliabilityConfig {
    fn default() -> Self {
        Self {
            circuit_breaker: Default::default(),
            concurrency: Default::default(),
            idempotency: Default::default(),
            health: Default::default(),
            precheck: Default::default(),
            metrics: Default::default(),
            retry_budget: DEFAULT_RETRY_BUDGET,
            stream_terminal_grace_ms: DEFAULT_STREAM_TERMINAL_GRACE_MS,
            stream_idle_timeout_ms: DEFAULT_STREAM_IDLE_TIMEOUT_MS,
        }
    }
}

impl ReliabilityConfig {
    fn from_object(value: Option<&Value>, path: &str) -> Result<Self, ConfigError> {
        let Some(v) = value else {
            return Ok(Self::default());
        };
        let m = as_object(v, path)?;
        // 子块解析走 truthiness 回退(Python `value = value or {}`):
        // null/false/空串等假值一律落回默认块,不报错。
        Ok(Self {
            // 只认 `ReliabilityConfig::to_dict` 写出的权威块名。
            circuit_breaker: CircuitBreakerConfig::from_object(
                try_truthy_chain(m, &["circuit_breaker"]),
                &format!("{path}.circuit_breaker"),
            )?,
            concurrency: ConcurrencyConfig::from_object(
                try_truthy_chain(m, &["concurrency"]),
                &format!("{path}.concurrency"),
            )?,
            idempotency: IdempotencyConfig::from_object(
                try_truthy_chain(m, &["idempotency"]),
                &format!("{path}.idempotency"),
            )?,
            health: HealthConfig::from_object(
                try_truthy_chain(m, &["health"]),
                &format!("{path}.health"),
            )?,
            precheck: PrecheckConfig::from_object(
                try_truthy_chain(m, &["precheck"]),
                &format!("{path}.precheck"),
            )?,
            metrics: MetricsConfig::from_object(
                try_truthy_chain(m, &["metrics"]),
                &format!("{path}.metrics"),
            )?,
            // 20261015_01 P2-2:预算下限 1(0 等于拒绝一切上游调用,无意义)。
            retry_budget: int_key_lenient(
                m,
                &["retryBudget"],
                i64::from(DEFAULT_RETRY_BUDGET),
                1,
            ) as u32,
            // 20261108_01 §4.2:grace 允许 0(答案完成即落日志);空闲超时
            // 下限 1s —— 0 会让所有流立即被判超时。
            stream_terminal_grace_ms: int_key_lenient(
                m,
                &["streamTerminalGraceMs"],
                DEFAULT_STREAM_TERMINAL_GRACE_MS as i64,
                0,
            ) as u64,
            stream_idle_timeout_ms: int_key_lenient(
                m,
                &["streamIdleTimeoutMs"],
                DEFAULT_STREAM_IDLE_TIMEOUT_MS as i64,
                1_000,
            ) as u64,
        })
    }
}

/// 镜像 Python `PolicyConfig`(A3 身份访问控制)。
#[derive(Debug, Clone, PartialEq)]
pub struct PolicyConfig {
    pub enabled: bool,
    pub default_effect: PolicyEffect,
    pub rules: Vec<Value>,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            default_effect: PolicyEffect::Allow,
            rules: Vec::new(),
        }
    }
}

impl PolicyConfig {
    fn from_object(value: Option<&Value>, path: &str) -> Result<Self, ConfigError> {
        // Python: 非 dict 一律返回默认块(静默,不报错)。
        let Some(v) = value else {
            return Ok(Self::default());
        };
        let Some(m) = v.as_object() else {
            return Ok(Self::default());
        };
        let default_effect_default = Value::String("allow".to_owned());
        let effect_raw =
            py_str(get_key_chain(m, &["defaultEffect"]).unwrap_or(&default_effect_default));
        let default_effect = PolicyEffect::parse_default_effect(effect_raw.as_deref());
        let rules_default = Value::Array(Vec::new());
        let rules_raw = get_key_chain(m, &["rules"]).unwrap_or(&rules_default);
        let Value::Array(rules) = rules_raw else {
            // Python: list(value.get("rules", [])) 对非可迭代抛 TypeError。
            return Err(ConfigError::invalid(key_path(path, "rules"), "必须是数组"));
        };
        Ok(Self {
            enabled: bool_key(m, &["enabled"], true),
            default_effect,
            rules: rules.clone(),
        })
    }
}

// ---------------------------------------------------------------------------
// 成本节省全局配置(20260905_03 任务二;Rust 新增,无 Python 镜像)
// ---------------------------------------------------------------------------

/// 语义决策(方案 §7.2/§7.3 + 20261101_02 T3-B 修订,写进代码即契约):
/// - 压缩 `mode` 默认 **off**,必须显式开启(auto/full 都是有损风险行为);
/// - 响应缓存 `enabled` 默认 **false**;
/// - 会话粘性/前缀钉扎 20261101_02 T3 起改为**全局开关**(规则域已下线,
///   规则级 stickySession/prefixPin 动作不复存在),默认 **false**;
///   前缀钉扎 TTL 是固定 300s 常量(wr-server session_affinity),不进配置。
///
/// 解析完全宽松:非 dict/键缺失/坏值一律回退默认,不报错(savings 块无
/// Python parity 约束,坏值静默回退优于阻塞整个配置加载)。
#[derive(Debug, Clone, PartialEq)]
pub struct SavingsConfig {
    pub compression: SavingsCompressionConfig,
    pub response_cache: SavingsResponseCacheConfig,
    pub sticky: SavingsStickyConfig,
    /// 20261101_02 T3-B:前缀钉扎全局开关(规则级 prefixPin 动作已删)。
    pub prefix_pin: SavingsPrefixPinConfig,
}

impl Default for SavingsConfig {
    fn default() -> Self {
        Self {
            compression: SavingsCompressionConfig::default(),
            response_cache: SavingsResponseCacheConfig::default(),
            sticky: SavingsStickyConfig::default(),
            prefix_pin: SavingsPrefixPinConfig::default(),
        }
    }
}

/// 压缩全局默认:`mode`("off"|"auto"|"full",off 即总开关)+ auto 档
/// 触发阈值(估算 token 数)。
#[derive(Debug, Clone, PartialEq)]
pub struct SavingsCompressionConfig {
    pub mode: String,
    pub threshold_tokens: i64,
}

impl Default for SavingsCompressionConfig {
    fn default() -> Self {
        Self {
            mode: "off".to_owned(),
            threshold_tokens: 32_000,
        }
    }
}

/// 响应缓存全局默认(内存 LRU + 磁盘两层;门控语义见方案 §4.4)。
#[derive(Debug, Clone, PartialEq)]
pub struct SavingsResponseCacheConfig {
    pub enabled: bool,
    pub memory_max_entries: usize,
    pub memory_max_bytes: usize,
    pub memory_ttl_secs: i64,
    pub disk_max_entries: usize,
    pub disk_ttl_secs: i64,
}

impl Default for SavingsResponseCacheConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            memory_max_entries: 64,
            memory_max_bytes: 5 * 1024 * 1024,
            memory_ttl_secs: 1800,
            disk_max_entries: 1000,
            disk_ttl_secs: 3600,
        }
    }
}

/// 会话粘性全局配置(20261101_02 T3-B:enabled 全局开关 + TTL;
/// 开关原由规则 `stickySession` 决定,规则域下线后提升为全局)。
///
/// 阶段五 C-1 新增 `cache_cold_secs`:粘住条目的「冷判定」窗口(秒)。
/// 绑定存活超过该窗口且**一次 cache_read 都没有**时,判定该绑定不值
/// (解绑落正常链)。0 = 关闭该判据(保持升级前的纯 TTL 粘性语义)。
#[derive(Debug, Clone, PartialEq)]
pub struct SavingsStickyConfig {
    pub enabled: bool,
    pub ttl_secs: i64,
    /// 阶段五 C-1:冷判定窗口(秒);0 = 关闭「值不值」判据。
    pub cache_cold_secs: i64,
}

impl Default for SavingsStickyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            ttl_secs: 900,
            cache_cold_secs: 300,
        }
    }
}

/// 前缀钉扎全局配置(20261101_02 T3-B;TTL 固定 300s 不进配置)。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SavingsPrefixPinConfig {
    pub enabled: bool,
}

/// 宽松 int:键缺失或 `int()` 失败 → default,命中值钳到 `lo` 下限。
fn int_key_lenient(m: &Map<String, Value>, keys: &[&str], default: i64, lo: i64) -> i64 {
    match get_key_chain(m, keys) {
        Some(v) => py_int(v).map_or(default, |n| n.max(lo)),
        None => default,
    }
}

/// 宽松 usize(int 解析 + 非负钳制后转换;容量类字段不接受负值)。
fn usize_key_lenient(m: &Map<String, Value>, keys: &[&str], default: usize) -> usize {
    int_key_lenient(m, keys, default as i64, 0).max(0) as usize
}

impl SavingsCompressionConfig {
    fn from_object(m: &Map<String, Value>) -> Self {
        // mode:未知字符串一律回退 "off"(off 即总开关,坏值宁可不压)。
        let mode = py_str(get_key_chain(m, &["mode"]).unwrap_or(&Value::Null))
            .filter(|s| matches!(s.as_str(), "off" | "auto" | "full"))
            .unwrap_or_else(|| "off".to_owned());
        Self {
            mode,
            threshold_tokens: int_key_lenient(m, &["thresholdTokens"], 32_000, 0),
        }
    }

    fn to_dict(&self) -> Value {
        let mut out = Map::new();
        out.insert("mode".to_owned(), json!(self.mode));
        out.insert("thresholdTokens".to_owned(), json!(self.threshold_tokens));
        Value::Object(out)
    }
}

impl SavingsResponseCacheConfig {
    fn from_object(m: &Map<String, Value>) -> Self {
        Self {
            enabled: bool_key(m, &["enabled"], false),
            memory_max_entries: usize_key_lenient(m, &["memoryMaxEntries"], 64),
            memory_max_bytes: usize_key_lenient(m, &["memoryMaxBytes"], 5 * 1024 * 1024),
            memory_ttl_secs: int_key_lenient(m, &["memoryTtlSecs"], 1800, 0),
            disk_max_entries: usize_key_lenient(m, &["diskMaxEntries"], 1000),
            disk_ttl_secs: int_key_lenient(m, &["diskTtlSecs"], 3600, 0),
        }
    }

    fn to_dict(&self) -> Value {
        let mut out = Map::new();
        out.insert("enabled".to_owned(), json!(self.enabled));
        out.insert(
            "memoryMaxEntries".to_owned(),
            json!(self.memory_max_entries),
        );
        out.insert("memoryMaxBytes".to_owned(), json!(self.memory_max_bytes));
        out.insert("memoryTtlSecs".to_owned(), json!(self.memory_ttl_secs));
        out.insert("diskMaxEntries".to_owned(), json!(self.disk_max_entries));
        out.insert("diskTtlSecs".to_owned(), json!(self.disk_ttl_secs));
        Value::Object(out)
    }
}

impl SavingsStickyConfig {
    fn from_object(m: &Map<String, Value>) -> Self {
        Self {
            enabled: bool_key(m, &["enabled"], false),
            ttl_secs: int_key_lenient(m, &["ttlSecs"], 900, 0),
            // 阶段五 C-1:冷判定窗口(宽松解析,坏值回退默认 300s)。
            cache_cold_secs: int_key_lenient(m, &["cacheColdSecs"], 300, 0),
        }
    }

    fn to_dict(&self) -> Value {
        let mut out = Map::new();
        out.insert("enabled".to_owned(), json!(self.enabled));
        out.insert("ttlSecs".to_owned(), json!(self.ttl_secs));
        out.insert("cacheColdSecs".to_owned(), json!(self.cache_cold_secs));
        Value::Object(out)
    }
}

impl SavingsPrefixPinConfig {
    fn from_object(m: &Map<String, Value>) -> Self {
        Self {
            enabled: bool_key(m, &["enabled"], false),
        }
    }

    fn to_dict(&self) -> Value {
        let mut out = Map::new();
        out.insert("enabled".to_owned(), json!(self.enabled));
        Value::Object(out)
    }
}

impl SavingsConfig {
    /// 非 dict(含缺失)→ 全默认块;子块同款宽松(非 dict → 该子块默认)。
    fn from_object(value: Option<&Value>) -> Self {
        let Some(m) = value.and_then(Value::as_object) else {
            return Self::default();
        };
        Self {
            compression: SavingsCompressionConfig::from_object(
                m.get("compression")
                    .and_then(Value::as_object)
                    .unwrap_or(&Map::new()),
            ),
            response_cache: SavingsResponseCacheConfig::from_object(
                m.get("responseCache")
                    .and_then(Value::as_object)
                    .unwrap_or(&Map::new()),
            ),
            sticky: SavingsStickyConfig::from_object(
                m.get("sticky")
                    .and_then(Value::as_object)
                    .unwrap_or(&Map::new()),
            ),
            prefix_pin: SavingsPrefixPinConfig::from_object(
                m.get("prefixPin")
                    .and_then(Value::as_object)
                    .unwrap_or(&Map::new()),
            ),
        }
    }

    /// camelCase 序列化(键序:compression / responseCache / sticky / prefixPin)。
    fn to_dict(&self) -> Value {
        let mut out = Map::new();
        out.insert("compression".to_owned(), self.compression.to_dict());
        out.insert("responseCache".to_owned(), self.response_cache.to_dict());
        out.insert("sticky".to_owned(), self.sticky.to_dict());
        out.insert("prefixPin".to_owned(), self.prefix_pin.to_dict());
        Value::Object(out)
    }
}

/// Provider 条目:全量 raw dict + 常用键的类型化访问器。最新格式只认
/// `api_base_url` / `api_key` / `accounts`;其余键(含前端扩展字段)原样保留,
/// 旧别名不再读取也不在加载时改写。
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderRecord {
    pub raw: Map<String, Value>,
}

impl ProviderRecord {
    /// 最新键 `api_base_url`;旧 `base_url`/`baseUrl` 不再兜底。
    pub fn base_url(&self) -> Option<String> {
        wr_domain::provider::provider_base_url(&self.raw)
    }

    pub fn api_key(&self) -> Option<String> {
        wr_domain::provider::provider_api_key(&self.raw)
    }

    pub fn name(&self) -> Option<&str> {
        self.raw.get("name").and_then(Value::as_str)
    }

    pub fn protocol(&self) -> Option<&str> {
        self.raw.get("protocol").and_then(Value::as_str)
    }
}

/// 镜像 Python `AppConfig`。
#[derive(Debug, Clone, PartialEq)]
pub struct AppConfig {
    pub version: i64,
    pub preferred_provider: String,
    pub providers: Vec<ProviderRecord>,
    pub api_keys: Vec<Value>,
    pub policies: PolicyConfig,
    pub reliability: ReliabilityConfig,
    /// 20260905_03 成本节省全局配置(压缩/响应缓存/粘性 TTL)。
    pub savings: SavingsConfig,
    /// 20260905_04 自定义模型聚合(`modelGroups` 数组原始 JSON;解析与
    /// 校验在 wr-domain `parse_model_groups`,存储侧原样往返保真)。
    pub model_groups: Vec<Value>,
    pub theme: String,
    /// 20261102_01 P4-3:开机自启开关(桌面壳读它同步 OS 登录项)。
    ///
    /// **此前该字段根本落不了盘**:`AppConfig::to_dict` 只写固定键集,
    /// 前端 `saveConfig` 送来的 `launchAtLogin` 被静默丢弃 → `getConfig`
    /// 永远读不回来 → Electron 壳的「开机自启」开关与 Tauri 壳的
    /// `tauri-plugin-autostart` 同步都会拿到 `false`。
    pub launch_at_login: bool,
    pub proxy: Map<String, Value>,
    pub observability: Map<String, Value>,
    pub router_fallback: Map<String, Value>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: 2,
            preferred_provider: String::new(),
            providers: Vec::new(),
            api_keys: Vec::new(),
            policies: PolicyConfig::default(),
            reliability: ReliabilityConfig::default(),
            savings: SavingsConfig::default(),
            model_groups: Vec::new(),
            theme: "system".to_owned(),
            launch_at_login: false,
            proxy: Map::new(),
            observability: Map::new(),
            router_fallback: Map::new(),
        }
    }
}

impl AppConfig {
    /// 从 JSON 解析(镜像 `AppConfig.from_dict`)。
    pub fn from_json(value: &Value) -> Result<Self, ConfigError> {
        let Some(root) = value.as_object() else {
            return Err(ConfigError::TopLevelNotObject);
        };

        // Python: max(2, int(migrated.get("version", 1)))。
        let version_default = Value::from(1);
        let version_raw = get_key_chain(root, &["version"]).unwrap_or(&version_default);
        let version = py_int(version_raw)
            .ok_or_else(|| ConfigError::invalid("version", "无法转 int"))?
            .max(2);

        // 只认 `to_dict` 写出的权威键 `preferredProvider`；旧 snake_case
        // 键不再读取，按未知旧格式忽略。
        let preferred_provider = match root.get("preferredProvider") {
            Some(v) => py_str_or_empty(v),
            None => String::new(),
        };

        // 只认权威键 `Providers`。
        let empty_array = Value::Array(Vec::new());
        let providers_raw = get_truthy_chain(root, &["Providers"], &empty_array);
        let Value::Array(provider_items) = providers_raw else {
            return Err(ConfigError::invalid("providers", "必须是数组"));
        };
        let mut providers = Vec::with_capacity(provider_items.len());
        for (idx, item) in provider_items.iter().enumerate() {
            providers.push(normalize_provider(item, idx)?);
        }

        // 只认权威键 `APIKEYS`。
        let api_keys_raw = get_truthy_chain(root, &["APIKEYS"], &empty_array);
        let Value::Array(api_keys) = api_keys_raw else {
            return Err(ConfigError::invalid("api_keys", "必须是数组"));
        };
        let api_keys = api_keys.clone();

        // 20261101_02 T3:路由规则下线,Router 块仅读 fallback。
        let router_fallback = collect_router_fallback(root)?;

        // Python: dict(migrated.get("proxy", {})) —— 键存在但为 null 时
        // dict(None) 抛 TypeError,镜像为错误;键缺失才是空对象。
        let proxy = match root.get("proxy") {
            Some(v) => as_object(v, "proxy")?.clone(),
            None => Map::new(),
        };
        // Python: dict(migrated.get("observability") or {}) —— or 链,null 落到 {}。
        let observability = match try_truthy_chain(root, &["observability"]) {
            Some(v) => as_object(v, "observability")?.clone(),
            None => Map::new(),
        };

        // 只认权威键 `theme`；`themePreference` 旧键忽略。
        let theme = match try_truthy_chain(root, &["theme"]) {
            Some(Value::String(s)) => normalize_theme(Some(s)),
            _ => "system",
        }
        .to_owned();

        // 只认权威键 `modelGroups`。
        let model_groups = match root.get("modelGroups") {
            Some(Value::Array(items)) => items.clone(),
            _ => Vec::new(),
        };

        // 只认权威键 `launchAtLogin`。
        let launch_at_login = match root.get("launchAtLogin") {
            Some(Value::Bool(value)) => *value,
            _ => false,
        };

        warn_retired_config_keys(root, provider_items);

        Ok(Self {
            version,
            preferred_provider,
            providers,
            api_keys,
            policies: PolicyConfig::from_object(root.get("policies"), "policies")?,
            reliability: ReliabilityConfig::from_object(root.get("reliability"), "reliability")?,
            // 20260905_03:缺失/非 dict → 全默认(压缩 off、响应缓存关)。
            savings: SavingsConfig::from_object(root.get("savings")),
            model_groups,
            theme,
            launch_at_login,
            proxy,
            observability,
            router_fallback,
        })
    }

    /// 序列化回配置 payload(镜像 `AppConfig.to_dict(include_secrets)`,R1
    /// 遗留的写路径,R4 补)。键序严格照 Python dict 字面量:
    /// `version / preferredProvider / Providers / APIKEYS / policies /
    /// reliability / savings / theme / proxy / observability / Router`
    /// (C5:持久化键名保持大写形式;20261101_02 T3 起 routingRules 键与
    /// Router.rules 不再写出,规则域已下线,Router 仅存 fallback)。
    ///
    /// `include_secrets = false` 时:
    /// - provider 的 `api_key` 为真值 → 掩成 `"***"`(假值/缺失原样保留);
    /// - `APIKEYS` 整体置空数组。
    pub fn to_dict(&self, include_secrets: bool) -> Value {
        let mut root = Map::new();
        root.insert("version".to_owned(), json!(self.version));
        root.insert(
            "preferredProvider".to_owned(),
            json!(self.preferred_provider),
        );
        let providers: Vec<Value> = self
            .providers
            .iter()
            .map(|record| {
                if include_secrets {
                    return Value::Object(record.raw.clone());
                }
                let mut copied = record.raw.clone();
                if py_bool(copied.get("api_key").unwrap_or(&Value::Null)) {
                    copied.insert("api_key".to_owned(), json!("***"));
                }
                Value::Object(copied)
            })
            .collect();
        root.insert("Providers".to_owned(), Value::Array(providers));
        root.insert(
            "APIKEYS".to_owned(),
            if include_secrets {
                Value::Array(self.api_keys.clone())
            } else {
                Value::Array(Vec::new())
            },
        );
        root.insert("policies".to_owned(), self.policies.to_dict());
        root.insert("reliability".to_owned(), self.reliability.to_dict());
        // 20260905_03:reliability 之后、theme 之前(camelCase 键名)。
        root.insert("savings".to_owned(), self.savings.to_dict());
        // 20260905_04:聚合定义原样往返(存储层不做语义解析)。
        root.insert(
            "modelGroups".to_owned(),
            Value::Array(self.model_groups.clone()),
        );
        root.insert("theme".to_owned(), json!(self.theme));
        // 20261102_01 P4-3:开机自启开关(此前无此键 → 开关不落盘)。
        root.insert("launchAtLogin".to_owned(), json!(self.launch_at_login));
        root.insert("proxy".to_owned(), Value::Object(self.proxy.clone()));
        root.insert(
            "observability".to_owned(),
            Value::Object(self.observability.clone()),
        );
        // Router 块:20261101_02 T3 起仅存全局失败回退。
        let mut router = Map::new();
        router.insert(
            "fallback".to_owned(),
            Value::Object(self.router_fallback.clone()),
        );
        root.insert("Router".to_owned(), Value::Object(router));
        Value::Object(root)
    }

    /// 落库 payload 形态(镜像 `SqliteStore.save_config` 写入 `app_config`
    /// 的文本):默认分隔符 + 非 ASCII 原样 + 插入序。
    pub fn to_payload_text(&self, include_secrets: bool) -> String {
        wr_domain::json_py::py_json_dumps(&self.to_dict(include_secrets))
    }
}

/// 各子块的 `to_dict`(逐字段镜像 Python;键序照 dict 字面量)。
impl ReliabilityConfig {
    fn to_dict(&self) -> Value {
        let mut m = Map::new();
        m.insert("circuit_breaker".to_owned(), self.circuit_breaker.to_dict());
        m.insert("concurrency".to_owned(), self.concurrency.to_dict());
        m.insert("idempotency".to_owned(), self.idempotency.to_dict());
        m.insert("health".to_owned(), self.health.to_dict());
        m.insert("precheck".to_owned(), self.precheck.to_dict());
        m.insert("metrics".to_owned(), self.metrics.to_dict());
        m.insert("retryBudget".to_owned(), json!(self.retry_budget));
        // 20261108_01 §4.2:流收尾的两个时间界(答案完成后的排空上限 /
        // 无记录空闲上限),camelCase 与 reliability 其余键同风格。
        m.insert(
            "streamTerminalGraceMs".to_owned(),
            json!(self.stream_terminal_grace_ms),
        );
        m.insert(
            "streamIdleTimeoutMs".to_owned(),
            json!(self.stream_idle_timeout_ms),
        );
        Value::Object(m)
    }
}

impl CircuitBreakerConfig {
    fn to_dict(&self) -> Value {
        let mut m = Map::new();
        m.insert("enabled".to_owned(), json!(self.enabled));
        m.insert(
            "failure_threshold".to_owned(),
            json!(self.failure_threshold),
        );
        m.insert("cooldown_ms".to_owned(), json!(self.cooldown_ms));
        m.insert(
            "failure_status_codes".to_owned(),
            Value::Array(self.failure_status_codes.iter().map(|c| json!(c)).collect()),
        );
        Value::Object(m)
    }
}

impl ConcurrencyConfig {
    fn to_dict(&self) -> Value {
        let mut m = Map::new();
        m.insert("enabled".to_owned(), json!(self.enabled));
        m.insert(
            "max_in_flight_per_provider".to_owned(),
            json!(self.max_in_flight_per_provider),
        );
        m.insert("queue_timeout_ms".to_owned(), json!(self.queue_timeout_ms));
        Value::Object(m)
    }
}

impl IdempotencyConfig {
    fn to_dict(&self) -> Value {
        let mut m = Map::new();
        m.insert("enabled".to_owned(), json!(self.enabled));
        m.insert("ttl_ms".to_owned(), json!(self.ttl_ms));
        m.insert("max_entries".to_owned(), json!(self.max_entries));
        m.insert("header_name".to_owned(), json!(self.header_name));
        m.insert(
            "cache_error_responses".to_owned(),
            json!(self.cache_error_responses),
        );
        Value::Object(m)
    }
}

impl HealthConfig {
    fn to_dict(&self) -> Value {
        let mut m = Map::new();
        m.insert("enabled".to_owned(), json!(self.enabled));
        m.insert("initial_delay_ms".to_owned(), json!(self.initial_delay_ms));
        m.insert("interval_ms".to_owned(), json!(self.interval_ms));
        m.insert("timeout_ms".to_owned(), json!(self.timeout_ms));
        Value::Object(m)
    }
}

impl PrecheckConfig {
    fn to_dict(&self) -> Value {
        let mut m = Map::new();
        m.insert("enabled".to_owned(), json!(self.enabled));
        m.insert(
            "rate_limit_rules".to_owned(),
            Value::Array(self.rate_limit_rules.clone()),
        );
        m.insert(
            "budget_max_cost_usd".to_owned(),
            json!(self.budget_max_cost_usd),
        );
        Value::Object(m)
    }
}

impl MetricsConfig {
    fn to_dict(&self) -> Value {
        let mut m = Map::new();
        m.insert("enabled".to_owned(), json!(self.enabled));
        m.insert(
            "include_provider_health".to_owned(),
            json!(self.include_provider_health),
        );
        Value::Object(m)
    }
}

impl PolicyConfig {
    fn to_dict(&self) -> Value {
        let mut m = Map::new();
        m.insert("enabled".to_owned(), json!(self.enabled));
        m.insert(
            "defaultEffect".to_owned(),
            json!(match self.default_effect {
                PolicyEffect::Allow => "allow",
                PolicyEffect::Deny => "deny",
            }),
        );
        m.insert("rules".to_owned(), Value::Array(self.rules.clone()));
        Value::Object(m)
    }
}

/// Python `str(x or "")`:假值(含缺失)归空串,其余 str() 化;
/// 数组/对象(Python 会给单引号 repr 的场景)归空串 —— 真实配置不出现。
fn py_str_or_empty(v: &Value) -> String {
    if py_bool(v) {
        py_str(v).unwrap_or_default()
    } else {
        String::new()
    }
}

fn as_object<'a>(v: &'a Value, path: &str) -> Result<&'a Map<String, Value>, ConfigError> {
    v.as_object()
        .ok_or_else(|| ConfigError::invalid(path, "必须是对象"))
}

/// Python `bool(value.get(key, default))`:键存在用 truthiness,缺失用 default。
fn bool_key(m: &Map<String, Value>, keys: &[&str], default: bool) -> bool {
    match get_key_chain(m, keys) {
        Some(v) => py_bool(v),
        None => default,
    }
}

/// Python `max(lo, int(value.get(k1, value.get(k2, default))))`。
fn int_key_clamped(
    m: &Map<String, Value>,
    keys: &[&str],
    default: i64,
    lo: i64,
    path: &str,
) -> Result<i64, ConfigError> {
    let default_value = Value::from(default);
    let raw = get_key_chain(m, keys).unwrap_or(&default_value);
    let parsed = py_int(raw).ok_or_else(|| ConfigError::invalid(path, "无法转 int"))?;
    Ok(parsed.max(lo))
}

/// Python `tuple(int(c) for c in (value.get(k1) or value.get(k2) or defaults))`。
fn int_list_key(
    m: &Map<String, Value>,
    keys: &[&str],
    defaults: &[i64],
    path: &str,
) -> Result<Vec<i64>, ConfigError> {
    let defaults_value = Value::Array(defaults.iter().map(|c| Value::from(*c)).collect());
    let raw = get_truthy_chain(m, keys, &defaults_value);
    let Value::Array(items) = raw else {
        return Err(ConfigError::invalid(path, "必须是数组"));
    };
    items
        .iter()
        .map(|item| py_int(item).ok_or_else(|| ConfigError::invalid(path, "元素无法转 int")))
        .collect()
}

/// 读取全局失败回退配置(20261101_02 T3:路由规则下线后 Router 块仅存
/// fallback;只认权威块 `Router.fallback`,旧 `routerFallback` 键忽略)。
fn collect_router_fallback(root: &Map<String, Value>) -> Result<Map<String, Value>, ConfigError> {
    let Some(Value::Object(router)) = root.get("Router") else {
        return Ok(Map::new());
    };
    match try_truthy_chain(router, &["fallback"]) {
        Some(v) => Ok(as_object(v, "Router.fallback")?.clone()),
        None => Ok(Map::new()),
    }
}

/// 旧配置键检测:只用于启动/加载时记**一条**低基数警告,不参与解析。
fn retired_config_keys(
    root: &Map<String, Value>,
    providers: &[Value],
) -> Vec<&'static str> {
    let mut found = Vec::new();
    let mut note = |present: bool, key: &'static str| {
        if present {
            found.push(key);
        }
    };
    for key in [
        "providers",
        "api_keys",
        "API_KEYS",
        "preferred_provider",
        "themePreference",
        "routerFallback",
        "router_fallback",
        "router",
        "model_groups",
        "launch_at_login",
    ] {
        note(root.contains_key(key), key);
    }
    if let Some(savings) = root.get("savings").and_then(Value::as_object) {
        for key in ["response_cache", "prefix_pin"] {
            note(savings.contains_key(key), key);
        }
        if let Some(cache) = savings.get("responseCache").and_then(Value::as_object) {
            for key in [
                "memory_max_entries",
                "memory_max_bytes",
                "memory_ttl_secs",
                "disk_max_entries",
                "disk_ttl_secs",
            ] {
                note(cache.contains_key(key), key);
            }
        }
        if let Some(compression) = savings.get("compression").and_then(Value::as_object) {
            note(compression.contains_key("threshold_tokens"), "threshold_tokens");
        }
        if let Some(sticky) = savings.get("sticky").and_then(Value::as_object) {
            for key in ["ttl_secs", "cache_cold_secs"] {
                note(sticky.contains_key(key), key);
            }
        }
    }
    if let Some(reliability) = root.get("reliability").and_then(Value::as_object) {
        for key in [
            "upstreamCircuitBreaker",
            "upstream_circuit_breaker",
            "upstreamConcurrency",
            "upstream_concurrency",
            "providerHealthCheck",
            "provider_health_check",
            "retry_budget",
        ] {
            note(reliability.contains_key(key), key);
        }
        for block in ["circuit_breaker", "concurrency", "health"] {
            if let Some(map) = reliability.get(block).and_then(Value::as_object) {
                for key in [
                    "failureThreshold",
                    "cooldownMs",
                    "failureStatusCodes",
                    "maxInFlightPerProvider",
                    "queueTimeoutMs",
                    "initialDelayMs",
                    "intervalMs",
                    "timeoutMs",
                ] {
                    note(map.contains_key(key), key);
                }
            }
        }
        if let Some(map) = reliability.get("idempotency").and_then(Value::as_object) {
            for key in [
                "headerName",
                "ttlMs",
                "maxEntries",
                "cacheErrorResponses",
            ] {
                note(map.contains_key(key), key);
            }
        }
        if let Some(map) = reliability.get("precheck").and_then(Value::as_object) {
            for key in ["rateLimitRules", "budgetMaxCostUsd"] {
                note(map.contains_key(key), key);
            }
        }
        if let Some(map) = reliability.get("metrics").and_then(Value::as_object) {
            note(map.contains_key("includeProviderHealth"), "includeProviderHealth");
        }
    }
    for provider in providers.iter().filter_map(Value::as_object) {
        for key in [
            "base_url",
            "baseUrl",
            "baseurl",
            "apiKey",
            "apikey",
            "credentials",
        ] {
            note(provider.contains_key(key), key);
        }
        if let Some(accounts) = provider.get("accounts").and_then(Value::as_array) {
            for account in accounts.iter().filter_map(Value::as_object) {
                note(account.contains_key("apiKey"), "account.apiKey");
                note(account.contains_key("apikey"), "account.apikey");
            }
        }
    }
    found.sort_unstable();
    found.dedup();
    found
}

fn warn_retired_config_keys(root: &Map<String, Value>, providers: &[Value]) {
    let retired = retired_config_keys(root, providers);
    if !retired.is_empty() {
        tracing::warn!(
            target: "wrouter.infra.config",
            retired = ?retired,
            "config_retired_keys_ignored"
        );
    }
}

/// provider 条目必须是对象;其余键原样保留(前端扩展字段不属于版本兼容面)。
fn normalize_provider(item: &Value, idx: usize) -> Result<ProviderRecord, ConfigError> {
    let path = format!("providers[{idx}]");
    let source = as_object(item, &path)?;
    Ok(ProviderRecord {
        raw: source.clone(),
    })
}

/// 方案 20260906_01 §4.8:预设保存期校验。
///
/// **只在写路径调用** —— 读路径([`AppConfig::from_json`])保持宽松,
/// 避免跨版本/手改配置让整个应用起不来(与决策 #7 同源思路)。
/// `gateway_port` 用于自指递归拒绝(决策 #8:代理指向 WRouter 自身监听
/// 端口会无限打环)。
pub fn validate_proxy_presets(
    proxy: &Map<String, Value>,
    gateway_port: u16,
) -> Result<(), ConfigError> {
    let Some(Value::Array(items)) = proxy.get("presets") else {
        return Ok(());
    };
    let mut seen_ids = std::collections::HashSet::new();
    let mut seen_names = std::collections::HashSet::new();
    for (index, item) in items.iter().enumerate() {
        let path = format!("proxy.presets[{index}]");
        let Some(map) = item.as_object() else {
            return Err(ConfigError::invalid(&path, "必须是对象"));
        };
        let text = |keys: &[&str]| -> String {
            keys.iter()
                .find_map(|key| map.get(*key).and_then(Value::as_str))
                .unwrap_or_default()
                .trim()
                .to_owned()
        };
        let id = text(&["id", "presetId"]);
        if id.is_empty() {
            return Err(ConfigError::invalid(&path, "id 不能为空"));
        }
        if id.contains('/') || id.contains('\\') {
            return Err(ConfigError::invalid(
                &path,
                "id 必须是 path-safe 值(不能含 / 或 \\)",
            ));
        }
        if !seen_ids.insert(id.clone()) {
            return Err(ConfigError::invalid(&path, format!("id 重复:{id}")));
        }
        let name = text(&["name", "label"]);
        if name.is_empty() {
            return Err(ConfigError::invalid(&path, "名称不能为空"));
        }
        if !seen_names.insert(name.to_ascii_lowercase()) {
            return Err(ConfigError::invalid(&path, format!("名称已存在:{name}")));
        }
        let Some(preset) = crate::proxy::ProxyPreset::from_value(item) else {
            return Err(ConfigError::invalid(&path, "结构非法"));
        };
        if preset.server.is_empty() {
            return Err(ConfigError::invalid(&path, "代理地址不能为空"));
        }
        let Some((host, port)) = crate::proxy::preset_host_port(&preset) else {
            return Err(ConfigError::invalid(
                &path,
                "代理地址或端口非法(端口须在 1-65535;scheme 仅支持 http/https/socks5/socks5h)",
            ));
        };
        // 决策 #8:loopback + 网关监听端口 = 自指递归,保存期就拒绝。
        if port == gateway_port && crate::proxy::is_loopback_host(&host) {
            return Err(ConfigError::invalid(
                &path,
                format!("代理指向 WRouter 自身监听端口 {port},会造成请求打环"),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
