//! 路由层 DTO(20261101_02 T3 收缩后的保留面)。
//!
//! 路由规则(RouterRule/条件/运算符/改写/诊断/RouteRequest/规则级
//! savings 动作块)已整体下线;本模块保留三块仍被网关消费的能力:
//! - 全局回退 [`RouterFallbackConfig`](Router 块仅存 `fallback`);
//! - 模型聚合 [`ModelGroup`] 系(20260905_04)与模型引用 [`RouteModelRef`];
//! - 执行计划 [`RouteExecutionPlan`](三层回退语义 C4 的第一层)。
//!
//! `RouterFallbackConfig::from_value` 逐行镜像 Python `from_dict`
//! (camelCase `retryCount`、非法 mode 回退 off、int() 失败回退 0、
//! models 仅收字符串)。

use std::collections::HashSet;
use std::sync::Arc;

use serde_json::{Value, json};

use crate::python_semantics::py_int;

// ---------------------------------------------------------------------------
// 字面量集合
// ---------------------------------------------------------------------------

/// Router 回退模式(Python `RouterFallbackMode`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouterFallbackMode {
    Off,
    Retry,
    ModelChain,
}

impl RouterFallbackMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Retry => "retry",
            Self::ModelChain => "model-chain",
        }
    }

    /// `value.get("mode", "off")`,不在三值内一律 `off`
    /// (镜像 Python:任何非字符串/未知字符串都回退)。
    pub fn from_value(value: Option<&Value>) -> Self {
        match value.and_then(Value::as_str) {
            Some("retry") => Self::Retry,
            Some("model-chain") => Self::ModelChain,
            _ => Self::Off,
        }
    }
}

/// 失败分类(Python `RouteFailureClass`);分类函数在
/// [`crate::routing::failure_classifier`]。
///
/// **语义变更登记(20260905_03 §7.6,偏差变更)**:新增
/// [`RouteFailureClass::ContextOverflow`],从 400(client)类中拆出 ——
/// 触发条件 = status 400 且错误正文命中上下文超限方言(见
/// `gateway::is_context_overflow_body`);model-chain 语义为「沿链找首个
/// 窗口足够的模型,不重试同模型」。Python 侧无此分类,Rust 网关自用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteFailureClass {
    Client,
    RateLimit,
    Retryable,
    Server,
    /// 上下文超限(20260905_03 S7 新增;status 400 的子类)。
    ContextOverflow,
}

// ---------------------------------------------------------------------------
// 模型聚合(20260905_04 §3.1;Rust 新增,无 Python 镜像)
// ---------------------------------------------------------------------------

/// 聚合组内选择策略(方案 §3.2:first 严格主备 / weighted 层内加权随机 /
/// sticky 会话粘上次成功目标)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelGroupStrategy {
    First,
    Weighted,
    Sticky,
}

impl ModelGroupStrategy {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::First => "first",
            Self::Weighted => "weighted",
            Self::Sticky => "sticky",
        }
    }

    /// 宽松解析:未知字符串/非字符串 → `first`(与 fallback mode 回退
    /// off 同风格的显式默认;策略枚举表 + 测试防漂移,方案 §2「必抄防坑」)。
    pub fn parse(value: Option<&Value>) -> Self {
        match value.and_then(Value::as_str) {
            Some("weighted") => Self::Weighted,
            Some("sticky") => Self::Sticky,
            _ => Self::First,
        }
    }
}

/// 聚合目标:精确 `(providerId, model)` 对(方案 §2:首期不做正则/标签关联)。
#[derive(Debug, Clone, PartialEq)]
pub struct ModelGroupTarget {
    pub provider_id: String,
    pub model: String,
    /// 默认 1,weighted 策略消费;负值夹紧为 0。
    pub weight: f64,
    /// 默认 0,数值小者优先(priority 分层)。
    pub priority: i64,
    /// 默认 true。
    pub enabled: bool,
}

impl ModelGroupTarget {
    /// 宽松解析:非 dict / providerId 或 model 空白 → None(跳过该项);
    /// weight 非法 → 1;priority 非法 → 0;enabled 仅收真布尔(缺失 → true)。
    pub fn from_value(value: &Value) -> Option<Self> {
        let map = value.as_object()?;
        let provider_id = map
            .get("providerId")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())?
            .to_owned();
        let model = map
            .get("model")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())?
            .to_owned();
        let weight = map
            .get("weight")
            .and_then(crate::python_semantics::py_float)
            .filter(|v| v.is_finite() && *v >= 0.0)
            .unwrap_or(1.0);
        let priority = map
            .get("priority")
            .and_then(crate::python_semantics::py_int)
            .unwrap_or(0);
        let enabled = match map.get("enabled") {
            Some(v) => crate::python_semantics::py_bool(v),
            None => true,
        };
        Some(Self {
            provider_id,
            model,
            weight,
            priority,
            enabled,
        })
    }

    /// 回写形态(camelCase,与 [`Self::from_value`] 往返)。
    pub fn to_value(&self) -> Value {
        json!({
            "providerId": self.provider_id,
            "model": self.model,
            "weight": self.weight,
            "priority": self.priority,
            "enabled": self.enabled,
        })
    }
}

/// 聚合实体(`config.modelGroups` 数组项;`gatewayModels` 保留纯别名语义)。
#[derive(Debug, Clone, PartialEq)]
pub struct ModelGroup {
    pub id: String,
    /// 对外聚合名(唯一;解析优先级高于 gatewayModels 别名,低于
    /// `Provider/model` 显式选择器,方案 §3.1)。
    pub name: String,
    pub enabled: bool,
    pub strategy: ModelGroupStrategy,
    /// ≥1 条(空的组解析时被跳过)。
    pub targets: Vec<ModelGroupTarget>,
}

impl ModelGroup {
    /// 宽松解析:非 dict / name 空白 / targets 全非法 → None(跳过整组);
    /// id 缺失回退 name(仅用于粘性键空间,不要求全局唯一)。
    pub fn from_value(value: &Value) -> Option<Self> {
        let map = value.as_object()?;
        let name = map
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())?
            .to_owned();
        let targets: Vec<ModelGroupTarget> = match map.get("targets") {
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(ModelGroupTarget::from_value)
                .collect(),
            _ => Vec::new(),
        };
        if targets.is_empty() {
            return None;
        }
        let id = map
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| name.clone());
        let enabled = match map.get("enabled") {
            Some(v) => crate::python_semantics::py_bool(v),
            None => true,
        };
        Some(Self {
            id,
            name,
            enabled,
            strategy: ModelGroupStrategy::parse(map.get("strategy")),
            targets,
        })
    }

    /// 回写形态(camelCase,与 [`Self::from_value`] 往返)。
    pub fn to_value(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "enabled": self.enabled,
            "strategy": self.strategy.as_str(),
            "targets": self.targets.iter().map(ModelGroupTarget::to_value).collect::<Vec<_>>(),
        })
    }
}

/// `modelGroups` 数组解析:非数组 → 空;非法项跳过;名称小写去重保序
/// (首见胜出,与 gatewayModels 别名去重同口径)。
pub fn parse_model_groups(raw: Option<&Value>) -> Vec<ModelGroup> {
    let Some(items) = raw.and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut seen: HashSet<String> = HashSet::new();
    let mut groups = Vec::new();
    for item in items {
        let Some(group) = ModelGroup::from_value(item) else {
            continue;
        };
        if seen.insert(group.name.to_ascii_lowercase()) {
            groups.push(group);
        }
    }
    groups
}

// ---------------------------------------------------------------------------
// 模型引用
// ---------------------------------------------------------------------------

/// 解析到具体 provider 的模型引用(Python `ProviderModelRef`)。
///
/// `provider` 用 `Arc` 指向注册表内的原始 provider dict:编译期的
/// `resolved.provider is target_provider` 身份比较在 Rust 侧以
/// `Arc::ptr_eq` 镜像。
#[derive(Debug, Clone)]
pub struct ProviderModelRef {
    pub canonical_selector: String,
    pub model: String,
    pub provider: Arc<serde_json::Map<String, Value>>,
    pub selector: String,
}

/// 聚合模型引用(20260905_04):聚合名命中时携带整组定义,选路侧
/// 展开为 `(provider, model)` 候选序列。
#[derive(Debug, Clone)]
pub struct AggregateModelRef {
    pub canonical_selector: String,
    pub model: String,
    pub group: ModelGroup,
}

/// `RouteModelRef = ProviderModelRef | AggregateModelRef`(2026-09-26 调用
/// 契约:网关级别名通道随裸模型名一并下线)。
#[derive(Debug, Clone)]
pub enum RouteModelRef {
    Provider(ProviderModelRef),
    /// 聚合名命中(20260905_04 §3.1;新枚举分支)。
    Aggregate(AggregateModelRef),
}

impl RouteModelRef {
    /// Python `ref.kind`("provider");聚合为 Rust 新增 "aggregate"。
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Provider(_) => "provider",
            Self::Aggregate(_) => "aggregate",
        }
    }

    pub fn model(&self) -> &str {
        match self {
            Self::Provider(r) => &r.model,
            Self::Aggregate(r) => &r.model,
        }
    }
}

// ---------------------------------------------------------------------------
// 节省档位(20260905_03;全局 savings 消费)
// ---------------------------------------------------------------------------

/// 压缩档位(`savings.compression`):off=禁用;auto=超阈值才压;full=总是。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouterCompressionTier {
    Off,
    Auto,
    Full,
}

impl RouterCompressionTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Auto => "auto",
            Self::Full => "full",
        }
    }
}

// ---------------------------------------------------------------------------
// 全局回退配置
// ---------------------------------------------------------------------------

/// 回退配置上限(mirror `ROUTER_FALLBACK_MAX_RETRY_COUNT`)。
pub const ROUTER_FALLBACK_MAX_RETRY_COUNT: i64 = 9999;

#[derive(Debug, Clone, PartialEq)]
pub struct RouterFallbackConfig {
    pub mode: RouterFallbackMode,
    pub retry_count: i64,
    pub models: Vec<String>,
}

impl Default for RouterFallbackConfig {
    fn default() -> Self {
        Self {
            mode: RouterFallbackMode::Off,
            retry_count: 0,
            models: Vec::new(),
        }
    }
}

impl RouterFallbackConfig {
    /// `to_dict()`:camelCase `retryCount`。
    pub fn to_value(&self) -> Value {
        json!({
            "mode": self.mode.as_str(),
            "retryCount": self.retry_count,
            "models": self.models.iter().map(|m| json!(m)).collect::<Vec<_>>(),
        })
    }

    /// `from_dict`:非 dict → 默认(off/0/[]);mode 非法回退 off;
    /// `int(retryCount)` 失败回退 0;models 仅收字符串元素。
    pub fn from_value(value: &Value) -> Self {
        let Some(map) = value.as_object() else {
            return Self::default();
        };
        let mode = RouterFallbackMode::from_value(map.get("mode"));
        let retry_count = match map.get("retryCount") {
            Some(v) => py_int(v).unwrap_or(0),
            None => 0,
        };
        let models = match map.get("models") {
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(|m| m.as_str().map(str::to_owned))
                .collect(),
            _ => Vec::new(),
        };
        Self {
            mode,
            retry_count,
            models,
        }
    }
}

// ---------------------------------------------------------------------------
// 执行计划
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct RouteAttemptPlan {
    pub index: usize,
    pub model: Option<String>,
    pub target: Option<RouteModelRef>,
}

#[derive(Debug, Clone)]
pub struct RouteExecutionPlan {
    pub attempts: Vec<RouteAttemptPlan>,
    pub fallback: RouterFallbackConfig,
    pub primary_model: Option<String>,
}

// ---------------------------------------------------------------------------
// 编译产物
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CompiledRouterConfig {
    pub fallback: RouterFallbackConfig,
    /// 编译期构造的模型注册表(Python `compiled.model_registry`)。
    pub model_registry: crate::routing::model_registry::ModelRegistry,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fallback_from_value_mirrors_python_from_dict() {
        // 非 dict → 默认 off。
        assert_eq!(
            RouterFallbackConfig::from_value(&json!("junk")),
            RouterFallbackConfig::default()
        );
        // 非法 mode 回退 off;retryCount 宽容 int();models 仅收字符串。
        let config = RouterFallbackConfig::from_value(&json!({
            "mode": "bogus",
            "retryCount": "3",
            "models": ["a", 1, "b"],
        }));
        assert_eq!(config.mode, RouterFallbackMode::Off);
        assert_eq!(config.retry_count, 3);
        assert_eq!(config.models, vec!["a".to_owned(), "b".to_owned()]);
        // int("x") 失败 → 0;float 截断。
        let config = RouterFallbackConfig::from_value(&json!({"retryCount": "x"}));
        assert_eq!(config.retry_count, 0);
        let config = RouterFallbackConfig::from_value(&json!({"retryCount": 2.9}));
        assert_eq!(config.retry_count, 2);
    }

    #[test]
    fn fallback_roundtrips_to_camel_case_json() {
        let config = RouterFallbackConfig {
            mode: RouterFallbackMode::Retry,
            retry_count: 2,
            models: vec!["m1".to_owned()],
        };
        let value = config.to_value();
        assert_eq!(value["retryCount"], json!(2));
        assert_eq!(RouterFallbackConfig::from_value(&value), config);
    }

    // ---- 20260905_04 M1:聚合实体解析 -------------------------------------

    #[test]
    fn model_group_parses_leniently_and_roundtrips() {
        // 合法形态全解析(camelCase;缺省 weight=1/priority=0/enabled=true)。
        let group = ModelGroup::from_value(&json!({
            "id": "g1",
            "name": " smart-router ",
            "strategy": "weighted",
            "targets": [
                {"providerId": "p1", "model": "gpt-4o", "weight": 3},
                {"providerId": "p2", "model": "gpt-4o", "priority": 2, "enabled": false},
            ],
        }))
        .unwrap();
        assert_eq!(group.id, "g1");
        assert_eq!(group.name, "smart-router");
        assert_eq!(group.strategy, ModelGroupStrategy::Weighted);
        assert!(group.enabled, "enabled 缺失 → true");
        assert_eq!(group.targets.len(), 2);
        assert_eq!(group.targets[0].weight, 3.0);
        assert_eq!(group.targets[0].priority, 0);
        assert!(!group.targets[1].enabled);
        // 往返:to_value → from_value 语义等价。
        assert_eq!(ModelGroup::from_value(&group.to_value()).unwrap(), group);

        // 非法/缺失字段宽松回退。
        let junk = ModelGroup::from_value(&json!({
            "name": "g",
            "strategy": "bogus",
            "targets": [{"providerId": "p", "model": "m", "weight": -2, "priority": "x"}],
        }))
        .unwrap();
        assert_eq!(junk.strategy, ModelGroupStrategy::First, "未知策略回退 first");
        assert_eq!(junk.id, "g", "id 缺失回退 name");
        assert_eq!(junk.targets[0].weight, 1.0, "负权重回退 1");
        assert_eq!(junk.targets[0].priority, 0, "非法 priority 回退 0");

        // 整组跳过:name 空白 / targets 空或全非法 / 非 dict。
        assert!(ModelGroup::from_value(&json!({"name": "  ", "targets": [{}]})).is_none());
        assert!(ModelGroup::from_value(&json!({"name": "g", "targets": []})).is_none());
        assert!(ModelGroup::from_value(&json!({"name": "g", "targets": ["x"]})).is_none());
        assert!(ModelGroup::from_value(&json!("g")).is_none());
        // 目标跳过:providerId/model 空白。
        assert!(ModelGroupTarget::from_value(&json!({"providerId": "", "model": "m"})).is_none());
        assert!(ModelGroupTarget::from_value(&json!({"providerId": "p", "model": " "})).is_none());
    }

    #[test]
    fn parse_model_groups_dedupes_names_and_skips_invalid() {
        // 非数组 → 空;名称小写去重首见胜出;非法项跳过(旧配置零迁移)。
        assert!(parse_model_groups(None).is_empty());
        assert!(parse_model_groups(Some(&json!("x"))).is_empty());
        let groups = parse_model_groups(Some(&json!([
            {"name": "Agg", "targets": [{"providerId": "p1", "model": "m1"}]},
            {"name": "agg", "targets": [{"providerId": "p2", "model": "m2"}]},
            {"name": "", "targets": [{"providerId": "p3", "model": "m3"}]},
            {"name": "ok2", "targets": []},
            {"name": "ok3", "targets": [{"providerId": "p4", "model": "m4"}]},
        ])));
        let names: Vec<_> = groups.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, vec!["Agg", "ok3"]);
    }
}
