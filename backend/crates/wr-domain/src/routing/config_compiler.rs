//! `compileRouterConfig`(20261101_02 T3 收缩后的保留面)。
//!
//! 路由规则编译(parse_router_rule(s)/改写/绑定作用域/规则诊断)已随
//! 规则域整体下线;本模块只保留仍被网关消费的三件事:
//! 1. Providers → [`ModelRegistry`](模型注册与解析);
//! 2. `modelGroups` 聚合实体解析入注册表(20260905_04);
//! 3. 全局 `router_fallback` 解析 + model-chain 引用未配置模型的剥离
//!    (存量行为:`Router.fallback` 是保留能力,见 20261101_02 §5.3)。
//!
//! 纯函数:收 config 形态的 `Value`,产出 [`CompiledRouterConfig`],永不抛错。

use std::sync::Arc;

use serde_json::{Map, Value};

use crate::python_semantics::{get_truthy_chain, py_bool, try_truthy_chain};
use crate::routing::contracts::{CompiledRouterConfig, RouterFallbackConfig, RouterFallbackMode};
use crate::routing::model_registry::ModelRegistry;

/// `_as_provider_dicts`:config 的 Providers/providers 键取列表中的 dict 项。
fn as_provider_dicts(config: &Map<String, Value>) -> Vec<Arc<Map<String, Value>>> {
    let empty = Value::Null;
    let list = get_truthy_chain(config, &["Providers", "providers"], &empty);
    let Some(items) = list.as_array() else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(Value::as_object)
        .map(|item| Arc::new(item.clone()))
        .collect()
}

/// `compileRouterConfig`:顶层入口。收 config 形态 dict(至少含
/// `Providers`/`providers`、`router_fallback`/`router.fallback`)。
pub fn compile_router_config(config: &Value) -> CompiledRouterConfig {
    let empty_map = Map::new();
    let config_dict = config.as_object().unwrap_or(&empty_map);

    let providers = as_provider_dicts(config_dict);
    // 20260905_04:`modelGroups` / `model_groups` 聚合实体(宽松解析,
    // 非法项跳过;名称小写去重首见胜出,见 parse_model_groups)。
    let model_groups = crate::routing::contracts::parse_model_groups(
        config_dict
            .get("modelGroups")
            .or_else(|| config_dict.get("model_groups")),
    );
    // 2026-09-26 调用契约:gatewayModels 网关级别名通道下线(运行时从未
    // 有 UI 写入),注册表只认 `Provider/模型` 与 `wrouter/<聚合名>`。
    let registry = ModelRegistry::new(providers, &model_groups);

    // 默认 fallback:router_fallback → router.fallback(legacy)→ off。
    let router_block = config_dict.get("router").and_then(Value::as_object);
    let fallback_raw = try_truthy_chain(config_dict, &["router_fallback"])
        .or_else(|| router_block.and_then(|router| router.get("fallback")))
        .filter(|v| py_bool(v));
    let fallback = match fallback_raw {
        Some(raw) => RouterFallbackConfig::from_value(raw),
        None => RouterFallbackConfig::default(),
    };

    // model-chain 引用未配置模型 → 剥离(存量行为:坏引用不进候选链)。
    let fallback = if fallback.mode == RouterFallbackMode::ModelChain {
        RouterFallbackConfig {
            mode: fallback.mode,
            retry_count: fallback.retry_count,
            models: fallback
                .models
                .iter()
                .filter(|m| registry.is_configured(Some(m)))
                .cloned()
                .collect(),
        }
    } else {
        fallback
    };

    CompiledRouterConfig {
        fallback,
        model_registry: registry,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn provider(id: &str, name: &str, models: &[&str]) -> Value {
        json!({"id": id, "name": name, "models": models})
    }

    #[test]
    fn compiles_registry_and_fallback() {
        let config = json!({
            "Providers": [provider("p1", "Primary", &["m1"])],
            "modelGroups": [{"name": "agg", "targets": [{"providerId": "p1", "model": "m1"}]}],
            "router_fallback": {"mode": "retry", "retryCount": 2},
        });
        let compiled = compile_router_config(&config);
        assert!(compiled.model_registry.resolve(Some("Primary/m1")).is_some());
        assert!(compiled.model_registry.resolve(Some("wrouter/agg")).is_some());
        assert_eq!(compiled.fallback.mode, RouterFallbackMode::Retry);
        assert_eq!(compiled.fallback.retry_count, 2);
    }

    #[test]
    fn missing_config_yields_default_fallback() {
        let compiled = compile_router_config(&json!({}));
        assert_eq!(compiled.fallback, RouterFallbackConfig::default());
    }

    #[test]
    fn global_fallback_diagnostics_strip_unconfigured_models() {
        let config = json!({
            "Providers": [provider("p1", "Primary", &["m1"])],
            "router_fallback": {"mode": "model-chain", "retryCount": 0, "models": ["Primary/m1", "ghost"]},
        });
        let compiled = compile_router_config(&config);
        assert_eq!(compiled.fallback.models, vec!["Primary/m1".to_owned()]);
    }
}
