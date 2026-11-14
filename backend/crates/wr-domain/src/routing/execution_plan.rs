//! `createRouteExecutionPlan` + 辅助(镜像 Python `domain/routing/execution_plan.py`)。
//!
//! 产出每次尝试的目标列表;三层回退语义(C4)的第一层:
//! 计划 attempts → 候选链 → 同 provider 重试。
//!
//! `has_request_body == false` 短路为单 attempt(Gemini path-style 从
//! URL 读模型,body 视为未解析 —— upstream parity)。

use serde_json::Value;

use crate::routing::contracts::{
    CompiledRouterConfig, ROUTER_FALLBACK_MAX_RETRY_COUNT, RouteAttemptPlan, RouteExecutionPlan,
    RouterFallbackConfig, RouterFallbackMode,
};
use crate::routing::model_registry::{ModelRegistry, normalize_route_selector};

/// `clamp`:夹到 [min,max];非有限值回退 min(retry_count 已是 i64,
/// 此函数只在 f64 输入时保留 Python 语义)。
fn clamp_finite(value: &Value, min_value: i64, max_value: i64) -> i64 {
    let Some(numeric) = crate::python_semantics::py_float(value) else {
        return min_value;
    };
    if !numeric.is_finite() {
        return min_value;
    }
    (numeric.trunc() as i64).clamp(min_value, max_value)
}

/// `uniqueStrings`:保序去重,丢弃假值。
fn unique_strings(values: impl IntoIterator<Item = Option<String>>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut output = Vec::new();
    for value in values {
        let Some(normalized) = value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty()) else {
            continue;
        };
        if seen.insert(normalized.clone()) {
            output.push(normalized);
        }
    }
    output
}

/// `_route_attempt`:按注册表解析模型目标。
fn route_attempt(
    index: usize,
    model: Option<String>,
    registry: &ModelRegistry,
) -> RouteAttemptPlan {
    let target = registry.resolve(model.as_deref());
    RouteAttemptPlan {
        index,
        model,
        target,
    }
}

/// `createRouteExecutionPlan`:产出有序 attempts。
pub fn create_route_execution_plan(
    fallback: &RouterFallbackConfig,
    has_request_body: bool,
    body_model: Option<&str>,
    primary_model: Option<&str>,
    registry: &ModelRegistry,
) -> RouteExecutionPlan {
    let primary =
        normalize_route_selector(body_model).or_else(|| normalize_route_selector(primary_model));

    // off 模式(或无 body)→ 单 attempt。
    if fallback.mode == RouterFallbackMode::Off || !has_request_body {
        return RouteExecutionPlan {
            attempts: vec![route_attempt(0, primary.clone(), registry)],
            fallback: fallback.clone(),
            primary_model: primary,
        };
    }

    if fallback.mode == RouterFallbackMode::Retry {
        let retry_count = clamp_finite(
            &Value::from(fallback.retry_count),
            0,
            ROUTER_FALLBACK_MAX_RETRY_COUNT,
        );
        let attempts = (0..=retry_count)
            .map(|index| route_attempt(index as usize, primary.clone(), registry))
            .collect();
        return RouteExecutionPlan {
            attempts,
            fallback: fallback.clone(),
            primary_model: primary,
        };
    }

    // mode == model-chain
    let mut inputs: Vec<Option<String>> = vec![primary.clone()];
    inputs.extend(fallback.models.iter().cloned().map(Some));
    let mut chain_models: Vec<Option<String>> =
        unique_strings(inputs).into_iter().map(Some).collect();
    if chain_models.is_empty() {
        // Python `chain_models = [primary]`:primary 可为 None(model 未知)。
        chain_models = vec![primary.clone()];
    }
    let attempts = chain_models
        .into_iter()
        .enumerate()
        .map(|(index, model)| route_attempt(index, model, registry))
        .collect();
    RouteExecutionPlan {
        attempts,
        fallback: fallback.clone(),
        primary_model: primary,
    }
}

/// `resolveExecutionPlan`:从编译配置构造计划的便捷入口。
/// 20261101_02 T3:规则级 fallback 随规则域删除,全局 `Router.fallback`
/// 是唯一回退来源(仍在 provider 级候选链 `max_retries` 之上 —— 候选链
/// 永远是每个 attempt 内的最后防线)。
pub fn resolve_execution_plan(
    compiled: &CompiledRouterConfig,
    body_model: Option<&str>,
    has_request_body: bool,
) -> RouteExecutionPlan {
    create_route_execution_plan(
        &compiled.fallback,
        has_request_body,
        body_model,
        None,
        &compiled.model_registry,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing::config_compiler::compile_router_config;
    use serde_json::json;

    fn fallback(mode: &str, retry_count: i64, models: &[&str]) -> RouterFallbackConfig {
        RouterFallbackConfig {
            mode: match mode {
                "retry" => RouterFallbackMode::Retry,
                "model-chain" => RouterFallbackMode::ModelChain,
                _ => RouterFallbackMode::Off,
            },
            retry_count,
            models: models.iter().map(|m| m.to_string()).collect(),
        }
    }

    fn compiled() -> CompiledRouterConfig {
        compile_router_config(&json!({
            "Providers": [{"id": "p1", "name": "P1", "models": ["m1", "m2"]}],
        }))
    }

    #[test]
    fn off_mode_single_attempt() {
        let plan = create_route_execution_plan(
            &fallback("off", 0, &[]),
            true,
            Some("P1/m1"),
            None,
            &compiled().model_registry,
        );
        assert_eq!(plan.attempts.len(), 1);
        assert_eq!(plan.primary_model.as_deref(), Some("P1/m1"));
        assert_eq!(plan.attempts[0].target.as_ref().unwrap().model(), "m1");
    }

    #[test]
    fn no_body_short_circuits_even_with_chain() {
        let plan = create_route_execution_plan(
            &fallback("model-chain", 0, &["P1/m2"]),
            false,
            Some("P1/m1"),
            None,
            &compiled().model_registry,
        );
        assert_eq!(plan.attempts.len(), 1);
    }

    #[test]
    fn retry_mode_repeats_primary() {
        let plan = create_route_execution_plan(
            &fallback("retry", 2, &[]),
            true,
            Some("P1/m1"),
            None,
            &compiled().model_registry,
        );
        assert_eq!(plan.attempts.len(), 3);
        assert!(
            plan.attempts
                .iter()
                .all(|a| a.model.as_deref() == Some("P1/m1"))
        );
    }

    #[test]
    fn model_chain_walks_unique_models() {
        let plan = create_route_execution_plan(
            &fallback("model-chain", 0, &["P1/m2", "P1/m1", "P1/m2"]),
            true,
            Some("P1/m1"),
            None,
            &compiled().model_registry,
        );
        let models: Vec<_> = plan.attempts.iter().map(|a| a.model.clone()).collect();
        assert_eq!(
            models,
            vec![Some("P1/m1".to_owned()), Some("P1/m2".to_owned()),]
        );
    }

    #[test]
    fn resolve_execution_plan_uses_global_fallback() {
        // 20261101_02 T3:规则级 fallback 已删,全局 Router.fallback 是
        // 唯一来源;model-chain 展开为多 attempt。
        let compiled_json = json!({
            "Providers": [{"id": "p1", "name": "P1", "models": ["m1", "m2"]}],
            "router_fallback": {"mode": "model-chain", "retryCount": 0, "models": ["P1/m2"]},
        });
        let compiled = compile_router_config(&compiled_json);
        let plan = resolve_execution_plan(&compiled, Some("P1/m1"), true);
        assert_eq!(plan.attempts.len(), 2);
    }
}
