//! 聚合模型候选展开(20260905_04 §3.2;Rust 新增,无 Python 镜像)。
//!
//! 聚合与单模型共用同一条候选管道(方案 §2「解析路径」):聚合解析产物
//! 归一为「(provider, model) 候选序列」,交给 forward.rs 既有的
//! candidates 循环逐个尝试;单模型路径不进本模块。
//!
//! 展开规则:
//! ```text
//! targets.filter(enabled)
//!   → 按 (priority ASC, 配置顺序) 分层排序
//!   → 层内按 strategy:
//!       first    → 层内全部保留为序列(严格主备,依次尝试)
//!       weighted → 层内加权随机抽一个(累计减权,复用方案 02 实现口径)
//!       sticky   → 会话粘性命中且目标健康 → 置顶;剔除同 provider 其余
//!                  重复目标(防同一供应商经多条关联被重复重试)
//!   → 逐目标过健康门(provider 存在 + enabled + 未熔断)
//!   → 全部不可用 → 返回空序列,调用方 fail-open 回现有全池选择
//! ```
//! weighted 的跨层语义:每层抽一个进入序列 —— 层是主备关系,层内才是
//! 负载均衡;只配一层时即纯负载均衡(方案 §5.3)。

use wr_domain::provider::Provider;
use wr_domain::routing::contracts::{ModelGroup, ModelGroupStrategy, ModelGroupTarget};
use wr_infra::config::CircuitBreakerConfig;

use super::breaker::CircuitBreaker;

/// 展开后的路由候选:provider + **该候选应发往上游的实际模型名**
/// (每次尝试前用它重写请求体 model 字段,方案 §2「故障转移」)。
#[derive(Debug, Clone)]
pub struct RouteCandidate {
    pub provider: Provider,
    pub model: String,
}

/// 纯序列构造(不做健康过滤;测试可控)。
fn sequence_for_strategy<'a>(
    group: &'a ModelGroup,
    sticky_hit: Option<&'a ModelGroupTarget>,
    rng: &mut impl FnMut() -> f64,
) -> Vec<&'a ModelGroupTarget> {
    // sticky:命中目标置顶 + 剔除同 provider 其余重复目标,其余按 first 序。
    if group.strategy == ModelGroupStrategy::Sticky
        && let Some(hit) = sticky_hit
    {
        let mut sequence = vec![hit];
        sequence.extend(
            group
                .targets
                .iter()
                .filter(|target| target.enabled)
                .filter(|target| !std::ptr::eq(*target, hit))
                .filter(|target| target.provider_id != hit.provider_id),
        );
        return sequence;
    }
    // (priority ASC, 配置顺序) 稳定分层。
    let mut enabled: Vec<&ModelGroupTarget> =
        group.targets.iter().filter(|target| target.enabled).collect();
    enabled.sort_by_key(|target| target.priority);
    if group.strategy != ModelGroupStrategy::Weighted {
        return enabled;
    }
    // weighted:逐层(相同 priority 为一层)加权随机抽一个,层序保持。
    let mut sequence: Vec<&ModelGroupTarget> = Vec::new();
    let mut layer_start = 0;
    while layer_start < enabled.len() {
        let priority = enabled[layer_start].priority;
        let mut layer_end = layer_start;
        while layer_end < enabled.len() && enabled[layer_end].priority == priority {
            layer_end += 1;
        }
        let layer = &enabled[layer_start..layer_end];
        if let Some(picked) = pick_weighted(layer, rng) {
            sequence.push(picked);
        }
        layer_start = layer_end;
    }
    sequence
}

/// 层内加权随机(累计减权口径,与方案 02 凭据池 weighted 同一实现家族):
/// 权重和 ≤0 → 取层内第一个;否则 `r × total` 落点取目标。
fn pick_weighted<'a>(
    layer: &[&'a ModelGroupTarget],
    rng: &mut impl FnMut() -> f64,
) -> Option<&'a ModelGroupTarget> {
    if layer.is_empty() {
        return None;
    }
    let total: f64 = layer.iter().map(|target| target.weight.max(0.0)).sum();
    if total <= 0.0 {
        return layer.first().copied();
    }
    let mut point = rng().clamp(0.0, 1.0) * total;
    for target in layer {
        point -= target.weight.max(0.0);
        if point < 0.0 {
            return Some(target);
        }
    }
    layer.last().copied()
}

/// 展开聚合候选序列:策略排序 → 解析 (providerId, model) → 健康门
/// (provider 存在 + enabled + 未熔断)。全部被过滤 → 空序列(调用方
/// fail-open 回现有全池 `choose_provider_from` 语义,方案 §5.4)。
///
/// 聚合请求无 provider 级 fallback 追加(targets 本身就是
/// 候选序,方案 §6「候选序列与现有 fallback 语义叠加」)。
pub fn expand_group_candidates<'a>(
    group: &'a ModelGroup,
    providers: &[Provider],
    breaker: &CircuitBreaker,
    breaker_config: &CircuitBreakerConfig,
    sticky_hit: Option<&'a ModelGroupTarget>,
    rng: &mut impl FnMut() -> f64,
) -> Vec<RouteCandidate> {
    let mut candidates = Vec::new();
    for target in sequence_for_strategy(group, sticky_hit, rng) {
        let Some(provider) = providers
            .iter()
            .find(|provider| provider.id == target.provider_id)
        else {
            continue;
        };
        if !provider.enabled
            || breaker.is_open(&provider.id, Some(provider), breaker_config)
        {
            continue;
        }
        candidates.push(RouteCandidate {
            provider: provider.clone(),
            model: target.model.clone(),
        });
    }
    candidates
}

/// sticky 键空间(方案 §3.2:`model-group:{group_id}:{session_key}`;
/// 复用 SessionAffinityTable 存储,复合键不与裸会话键碰撞)。
pub fn group_sticky_key(group_id: &str, session_key: &str) -> String {
    format!("model-group:{group_id}:{session_key}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(id: &str, enabled: bool) -> Provider {
        let mut raw = serde_json::Map::new();
        raw.insert("id".to_owned(), serde_json::json!(id));
        raw.insert("name".to_owned(), serde_json::json!(id));
        raw.insert(
            "api_base_url".to_owned(),
            serde_json::json!(format!("https://{id}.test")),
        );
        raw.insert("protocol".to_owned(), serde_json::json!("openai"));
        raw.insert("enabled".to_owned(), serde_json::json!(enabled));
        Provider::from_raw(&raw).unwrap()
    }

    fn target(provider_id: &str, model: &str, weight: f64, priority: i64) -> ModelGroupTarget {
        ModelGroupTarget {
            provider_id: provider_id.to_owned(),
            model: model.to_owned(),
            weight,
            priority,
            enabled: true,
        }
    }

    fn group(strategy: ModelGroupStrategy, targets: Vec<ModelGroupTarget>) -> ModelGroup {
        ModelGroup {
            id: "g1".to_owned(),
            name: "agg".to_owned(),
            enabled: true,
            strategy,
            targets,
        }
    }

    fn disabled_breaker() -> CircuitBreaker {
        CircuitBreaker::new(None)
    }

    fn breaker_config() -> CircuitBreakerConfig {
        CircuitBreakerConfig {
            enabled: false,
            ..Default::default()
        }
    }

    /// 固定序列 rng(测试可控)。
    fn fixed_rng(values: &[f64]) -> impl FnMut() -> f64 + '_ {
        let mut index = 0;
        move || {
            let value = values[index.min(values.len() - 1)];
            index += 1;
            value
        }
    }

    #[test]
    fn first_strategy_keeps_layered_sequence() {
        let group = group(
            ModelGroupStrategy::First,
            vec![
                target("p2", "m-b", 1.0, 0),
                target("p1", "m-a", 1.0, 5),
                target("p3", "m-c", 1.0, 0),
            ],
        );
        let providers = vec![
            provider("p1", true),
            provider("p2", true),
            provider("p3", true),
        ];
        let candidates = expand_group_candidates(
            &group,
            &providers,
            &disabled_breaker(),
            &breaker_config(),
            None,
            &mut fixed_rng(&[0.5]),
        );
        // priority 分层(0 层保持配置序),5 排后。
        let models: Vec<_> = candidates.iter().map(|c| c.model.as_str()).collect();
        assert_eq!(models, vec!["m-b", "m-c", "m-a"]);
    }

    #[test]
    fn weighted_picks_one_per_layer_by_cumulative_weight() {
        let group = group(
            ModelGroupStrategy::Weighted,
            vec![
                target("p1", "m-a", 3.0, 0),
                target("p2", "m-b", 1.0, 0),
                target("p3", "m-c", 1.0, 1),
            ],
        );
        let providers = vec![
            provider("p1", true),
            provider("p2", true),
            provider("p3", true),
        ];
        // r=0.9 × total(4) = 3.6 → 减 3 剩 0.6 → 减 1 < 0 → 命中 p2;
        // 第二层只有 p3,恒命中。
        let candidates = expand_group_candidates(
            &group,
            &providers,
            &disabled_breaker(),
            &breaker_config(),
            None,
            &mut fixed_rng(&[0.9]),
        );
        let models: Vec<_> = candidates.iter().map(|c| c.model.as_str()).collect();
        assert_eq!(models, vec!["m-b", "m-c"]);
        // r=0.1 × 4 = 0.4 → 减 3 < 0 → 命中 p1。
        let candidates = expand_group_candidates(
            &group,
            &providers,
            &disabled_breaker(),
            &breaker_config(),
            None,
            &mut fixed_rng(&[0.1]),
        );
        assert_eq!(candidates[0].model, "m-a");
    }

    #[test]
    fn weighted_zero_total_falls_to_first_in_layer() {
        let group = group(
            ModelGroupStrategy::Weighted,
            vec![
                ModelGroupTarget {
                    weight: 0.0,
                    ..target("p1", "m-a", 0.0, 0)
                },
                ModelGroupTarget {
                    weight: 0.0,
                    ..target("p2", "m-b", 0.0, 0)
                },
            ],
        );
        let providers = vec![provider("p1", true), provider("p2", true)];
        let candidates = expand_group_candidates(
            &group,
            &providers,
            &disabled_breaker(),
            &breaker_config(),
            None,
            &mut fixed_rng(&[0.5]),
        );
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].model, "m-a");
    }

    #[test]
    fn health_gate_filters_disabled_and_missing_providers() {
        let group = group(
            ModelGroupStrategy::First,
            vec![
                target("p-off", "m-a", 1.0, 0),
                target("ghost", "m-b", 1.0, 0),
                ModelGroupTarget {
                    enabled: false,
                    ..target("p1", "m-c", 1.0, 0)
                },
                target("p1", "m-d", 1.0, 0),
            ],
        );
        let providers = vec![provider("p-off", false), provider("p1", true)];
        let candidates = expand_group_candidates(
            &group,
            &providers,
            &disabled_breaker(),
            &breaker_config(),
            None,
            &mut fixed_rng(&[0.5]),
        );
        let models: Vec<_> = candidates.iter().map(|c| c.model.as_str()).collect();
        assert_eq!(models, vec!["m-d"], "disabled 目标/禁用与缺失 provider 全过滤");
    }

    #[test]
    fn all_filtered_yields_empty_for_fail_open() {
        let group = group(
            ModelGroupStrategy::First,
            vec![target("ghost", "m-a", 1.0, 0)],
        );
        let candidates = expand_group_candidates(
            &group,
            &[provider("p1", true)],
            &disabled_breaker(),
            &breaker_config(),
            None,
            &mut fixed_rng(&[0.5]),
        );
        assert!(candidates.is_empty(), "空序列 = 调用方 fail-open 信号");
    }

    #[test]
    fn sticky_hit_goes_first_and_dedupes_same_provider() {
        let group = group(
            ModelGroupStrategy::Sticky,
            vec![
                target("p1", "m-a", 1.0, 0),
                target("p2", "m-x", 1.0, 0),
                target("p1", "m-b", 1.0, 9),
            ],
        );
        let providers = vec![provider("p1", true), provider("p2", true)];
        // 粘性命中 p1/m-b:置顶,且同 provider 的 m-a 被剔除。
        let candidates = expand_group_candidates(
            &group,
            &providers,
            &disabled_breaker(),
            &breaker_config(),
            Some(&group.targets[2]),
            &mut fixed_rng(&[0.5]),
        );
        let models: Vec<_> = candidates.iter().map(|c| c.model.as_str()).collect();
        assert_eq!(models, vec!["m-b", "m-x"]);
    }

    #[test]
    fn group_sticky_key_namespace() {
        assert_eq!(
            group_sticky_key("g1", "sess"),
            "model-group:g1:sess".to_owned()
        );
    }
}
