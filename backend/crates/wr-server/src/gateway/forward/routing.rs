// 路由引擎半程 + 会话粘性/前缀钉扎选路
// 20261013_01 C1 自 forward.rs 纯搬移;经 `use super::*` 复用共享类型。

use super::*;

// ---------------------------------------------------------------------------
// 选路辅助(forward_json / forward_stream 共享)
// ---------------------------------------------------------------------------

/// `_resolve_client_prefix`:客户端显式 `provider/model` 选择器分解。
/// 命中 provider 域模型时返回 (规范模型, provider 引用)。
pub fn resolve_client_prefix(
    snapshot: &ConfigSnapshot,
    model: &str,
) -> (String, Option<RouteModelRef>) {
    if model.is_empty() || !model.contains('/') {
        return (model.to_owned(), None);
    }
    match snapshot.compiled.model_registry.resolve(Some(model)) {
        Some(RouteModelRef::Provider(reference)) => (
            reference.model.clone(),
            Some(RouteModelRef::Provider(reference)),
        ),
        _ => (model.to_owned(), None),
    }
}

/// `_build_fallback_plan`:全局 Router.fallback 展开为执行计划
/// (20261101_02 T3:规则级 fallback 已随规则域删除);每个 attempt 内
/// 仍跑 provider 候选链。
pub fn build_fallback_plan(snapshot: &ConfigSnapshot, model: &str) -> RouteExecutionPlan {
    resolve_execution_plan(&snapshot.compiled, Some(model), true)
}

/// `_select_provider`(provider_selection.py 主路径):仅显式 Provider 引用
/// 可选中(2026-09-26 调用契约)。裸模型名全池 fail-open 通道下线 ——
/// 未解析选择器由调用契约前置 400 拦截;目标供应商熔断时不在此拦截,
/// 由候选级 breaker 检查兜底(20261015_01 R1:provider 级 fallback
/// 链已移除,兜底统一走聚合模型)。
pub fn select_provider<'a>(
    _deps: &GatewayDeps,
    snapshot: &'a ConfigSnapshot,
    _breaker_config: &CircuitBreakerConfig,
    model: &str,
    route_target: Option<&RouteModelRef>,
) -> Result<&'a Provider, RoutingError> {
    let Some(RouteModelRef::Provider(reference)) = route_target else {
        return Err(RoutingError(format!(
            "Model '{model}' is not configured. Call models as 'provider/model' or aggregates as 'wrouter/aggregate-name'."
        )));
    };
    let target_id = reference
        .provider
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| reference.provider.get("name").and_then(Value::as_str))
        .unwrap_or("");
    for provider in &snapshot.providers {
        if provider.id == target_id {
            return Ok(provider);
        }
    }
    Err(RoutingError(format!(
        "Routed provider '{target_id}' is not configured."
    )))
}
// ---------------------------------------------------------------------------
// 20260905_03 S2:会话粘性与前缀钉扎选路
// ---------------------------------------------------------------------------

/// 与 `choose_provider_from` 的模型过滤同口径:models 空或含 `*` 视为
/// 服务任意模型(健康门的一部分:钉扎目标必须能服务当前模型)。
pub(super) fn provider_serves_model(provider: &Provider, model: &str) -> bool {
    provider.models.is_empty()
        || provider.models.iter().any(|m| m == model)
        || provider.models.iter().any(|m| m == "*")
}

/// 粘性/钉扎目标的健康门(方案 §7.3:粘性是优化不是承诺):provider
/// 存在 + enabled + 未熔断 + 能服务当前模型;指定账号存在 + enabled +
/// 未冷却。任一失效 → `None`(读取方释放绑定落正常链)。
pub(super) async fn pin_target_checked(
    deps: &GatewayDeps,
    snapshot: &ConfigSnapshot,
    breaker_config: &CircuitBreakerConfig,
    provider_id: &str,
    account_id: Option<&str>,
    model: &str,
) -> Option<(Provider, Option<String>)> {
    let provider = snapshot.providers.iter().find(|p| p.id == provider_id)?;
    if !provider.enabled
        || deps
            .breaker
            .is_open(&provider.id, Some(provider), breaker_config)
    {
        return None;
    }
    if !provider_serves_model(provider, model) {
        return None;
    }
    if let Some(account_id) = account_id {
        let account = provider.accounts.iter().find(|a| a.id == account_id)?;
        if !account.enabled
            || deps
                .credentials
                .is_cooled_down_for_model(&provider.id, account_id, model)
        {
            return None;
        }
    }
    // 指定账号注入;密钥缺失时退化为 legacy 凭据(账号记 None,粘性仍成立)。
    Some(inject_account_credential_for_account(deps, provider.clone(), account_id).await)
}

/// 阶段五 C-1:粘住绑定是否还值得保留。
///
/// 判据前提:该 provider 的协议族**确实上报缓存**(否则 `cache_read`
/// 恒为 0,判据会误杀全部正常粘性)→ 非上报族一律保留。冷判定窗口
/// 来自配置 `savings.sticky.cacheColdSecs`(0 = 关闭判据)。
fn sticky_binding_worth_keeping(
    snapshot: &ConfigSnapshot,
    entry: &AffinityEntry,
    now_ms: i64,
) -> bool {
    let cold_after_ms = snapshot
        .config
        .savings
        .sticky
        .cache_cold_secs
        .saturating_mul(1_000);
    if cold_after_ms <= 0 {
        return true;
    }
    let Some(provider) = snapshot
        .providers
        .iter()
        .find(|provider| provider.id == entry.provider_id)
    else {
        // provider 不在快照里 → 无从判据,交给健康门处理。
        return true;
    };
    if !provider.protocol.reports_cache_usage() {
        return true;
    }
    session_affinity::sticky_cache_worth_keeping(entry, now_ms, cold_after_ms)
}

/// 粘性选路:前缀钉扎 > 会话粘性 > 正常选路。仅当对应动作在
/// [`ResolvedSavings`] 中开启(20261101_02 T3-B:全局开关)才查表;
/// 命中目标过健康门后直接返回 `(provider, account)`,调用方绕过
/// select_provider/策略选号。
///
/// 健康门失败 → 立即释放对应绑定并返回 `None`(落正常链);前缀绑定
/// 不健康时不降级到会话粘性(两表键空间不同,钉扎表已给出失效意见)。
pub(super) async fn resolve_pinned_target(
    deps: &GatewayDeps,
    snapshot: &ConfigSnapshot,
    breaker_config: &CircuitBreakerConfig,
    attempt_model: &str,
    resolved: &ResolvedSavings,
    session_key: Option<&str>,
    payload: &Value,
) -> Option<(Provider, Option<String>)> {
    let now = session_affinity::now_ms();
    // 1. 前缀钉扎:请求体有 cache_control 断点才可能命中。
    if resolved.prefix_pin
        && let Some(key) = prefix_pin_key(payload)
    {
        if let Some(entry) = deps.prefix_pins.prefix_get(&key, now) {
            return match pin_target_checked(
                deps,
                snapshot,
                breaker_config,
                &entry.provider_id,
                entry.account_id.as_deref(),
                attempt_model,
            )
            .await
            {
                Some(pinned) => Some(pinned),
                None => {
                    deps.prefix_pins.prefix_remove(&key);
                    None
                }
            };
        }
        // 无绑定 → 继续尝试会话粘性。
    }
    // 2. 会话粘性:会话键存在才可能命中;命中要求 model 与当前一致。
    if resolved.sticky_session
        && let Some(key) = session_key
        && let Some(entry) = deps.session_affinity.sticky_get(key, now)
    {
        // 阶段五 C-1:「值不值」判据 —— 粘住满冷判定窗口仍零 cache_read
        // (且该 provider 协议族确实上报缓存)→ 释放绑定,落正常链。
        if !sticky_binding_worth_keeping(snapshot, &entry, now) {
            deps.session_affinity.sticky_remove(key);
            return None;
        }
        if entry.model == attempt_model
            && let Some(pinned) = pin_target_checked(
                deps,
                snapshot,
                breaker_config,
                &entry.provider_id,
                entry.account_id.as_deref(),
                attempt_model,
            )
            .await
        {
            return Some(pinned);
        }
        deps.session_affinity.sticky_remove(key);
    }
    None
}

/// 成功回写(方案 §4.1:粘性/钉扎只由成功响应写入):主候选最终
/// status<400 时,各自 key 可用则各写各的;account 为 None 也绑
/// provider(+model),account 存 None。fallback 推进后在新目标上成功
/// → 由调用点在成功处重绑,天然满足「重绑定」语义。
///
/// 阶段五 C-1:粘性写入带上本次 `usage` 的 cache 观测(命中/写入
/// token),供「值不值」判据累计。
pub(super) fn bind_savings_targets(
    deps: &GatewayDeps,
    resolved: &ResolvedSavings,
    session_key: Option<&str>,
    payload: &Value,
    provider: &Provider,
    account_id: Option<&str>,
    model: &str,
    usage: &wr_domain::usage::Usage,
) {
    let now = session_affinity::now_ms();
    if resolved.sticky_session
        && let Some(key) = session_key
    {
        deps.session_affinity.sticky_bind_observed(
            key,
            AffinityEntry {
                provider_id: provider.id.clone(),
                model: model.to_owned(),
                account_id: account_id.map(str::to_owned),
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                hits: 0,
                bound_at_ms: now,
            },
            now,
            usage.cache_read_tokens,
            usage.cache_write_tokens,
        );
    }
    if resolved.prefix_pin
        && let Some(key) = prefix_pin_key(payload)
    {
        deps.prefix_pins.prefix_bind(
            &key,
            PrefixPinEntry {
                provider_id: provider.id.clone(),
                account_id: account_id.map(str::to_owned),
            },
            now,
        );
    }
}

/// 阶段五 C-1:断流/不完整终态时释放粘性、钉扎与组粘性绑定。
///
/// 中断的会话不该继续粘在同一个上游 —— 坏粘性会一直传染给后续回合
/// (方案 §6.1「断流不清除」差距)。健康门失败本来就会释放,这里补的是
/// 「上游中途断流」这条路径(此前直接 return,绑定留着)。
pub(super) fn release_sticky_targets(
    deps: &GatewayDeps,
    resolved: &ResolvedSavings,
    session_key: Option<&str>,
    payload: &Value,
    group: Option<&ModelGroup>,
) {
    if resolved.sticky_session
        && let Some(key) = session_key
    {
        deps.session_affinity.sticky_remove(key);
    }
    if resolved.prefix_pin
        && let Some(key) = prefix_pin_key(payload)
    {
        deps.prefix_pins.prefix_remove(&key);
    }
    if let Some(group) = group
        && group.strategy == ModelGroupStrategy::Sticky
        && let Some(key) = session_key
    {
        deps.group_sticky
            .sticky_remove(&group_sticky_key(&group.id, key));
    }
}

// ---------------------------------------------------------------------------
// 20260905_04:聚合模型选路(§3.2)
// ---------------------------------------------------------------------------

/// 聚合 sticky 策略:查粘性表并过健康门(目标仍在组内且 enabled +
/// provider 存在 + enabled + 未熔断);不健康即释放绑定返回 None
/// (粘性是优化不是承诺,与 20260905_03 S2 同立场)。
pub(super) fn group_sticky_hit(
    deps: &GatewayDeps,
    snapshot: &ConfigSnapshot,
    breaker_config: &CircuitBreakerConfig,
    group: &ModelGroup,
    session_key: Option<&str>,
) -> Option<ModelGroupTarget> {
    if group.strategy != ModelGroupStrategy::Sticky {
        return None;
    }
    let key = session_key?;
    let store_key = group_sticky_key(&group.id, key);
    let now = session_affinity::now_ms();
    let entry = deps.group_sticky.sticky_get(&store_key, now)?;
    let healthy = group.targets.iter().any(|target| {
        target.enabled
            && target.provider_id == entry.provider_id
            && target.model == entry.model
            && snapshot.providers.iter().any(|provider| {
                provider.id == target.provider_id
                    && provider.enabled
                    && !deps
                        .breaker
                        .is_open(&provider.id, Some(provider), breaker_config)
            })
    });
    if healthy {
        return group
            .targets
            .iter()
            .find(|target| target.provider_id == entry.provider_id && target.model == entry.model)
            .cloned();
    }
    deps.group_sticky.sticky_remove(&store_key);
    None
}

/// 上游请求体里的 `model` 名:**厂商侧的裸模型名**,不是路由选择器。
///
/// model-chain 的回退链项在配置里写的是 `Provider/模型` 形态的**内部选择器**
/// (见 `docs/reference/03-网关协议与路由.md` §3.1「选择器必须显式」),它只用于
/// 选路;发给厂商的 `model` 字段必须是厂商认识的裸模型名。
///
/// 主 attempt 天然是裸名(客户端前缀在 `resolve_client_prefix` 已剥),但回退链项
/// 是配置原串 —— 直接发出去等于把路由选择器递给厂商:真实厂商会回
/// `model not found`,回退在生产里等于失效(mock 不校验 model 字段 → 客户端可见
/// 响应看不出来,只有「上游字节」层面的断言才抓得到)。
fn upstream_model_name(attempt_model: &str, attempt_target: Option<&RouteModelRef>) -> String {
    match attempt_target {
        // Provider 引用带的是裸模型名(`ProviderModelRef.model`)。
        Some(RouteModelRef::Provider(reference)) => reference.model.clone(),
        // 聚合:候选各自带实际模型名(见 `expand_group_candidates`),此处不参与。
        _ => attempt_model.to_owned(),
    }
}

/// 组装一次尝试的路由候选序列(方案 §3.2:聚合与单模型共用同一条候选
/// 管道)。聚合请求:targets 展开为多候选(每候选带自己的实际模型名,
/// 逐个注入账号凭据),**无 provider 级 fallback 追加**(R1 已移除)
/// (targets 本身就是候选序,方案 §6);全部目标不可用 → fail-open 回
/// 现有单候选语义。单模型请求:单候选(20261015_01 R1 起)。
#[allow(clippy::too_many_arguments)]
pub(super) async fn route_candidates_for_attempt(
    deps: &GatewayDeps,
    snapshot: &ConfigSnapshot,
    breaker_config: &CircuitBreakerConfig,
    attempt_model: &str,
    attempt_target: Option<&RouteModelRef>,
    resolved: &ResolvedSavings,
    session_key: Option<&str>,
    payload: &Value,
) -> (Vec<(RouteCandidate, Option<String>)>, Option<String>) {
    // 上游请求体的 model 用裸模型名(回退链项是内部选择器,不能发给厂商)。
    let upstream_model = upstream_model_name(attempt_model, attempt_target);
    // 聚合请求跳过储蓄粘性/钉扎选路(其绑定按实际模型记账,与聚合名
    // 口径不一致;组内负载均衡由聚合自己的 sticky 策略承担)。
    let group = match attempt_target {
        Some(RouteModelRef::Aggregate(reference)) => Some(&reference.group),
        _ => None,
    };
    if group.is_none()
        && let Some(pinned) = resolve_pinned_target(
            deps,
            snapshot,
            breaker_config,
            attempt_model,
            resolved,
            session_key,
            payload,
        )
        .await
    {
        // 20261015_01 R1:provider 级 fallback_providers 链已移除,
        // 兜底统一走聚合模型(聚合组顺序即候选序)。
        return (
            vec![(
                RouteCandidate {
                    provider: pinned.0,
                    model: upstream_model,
                },
                pinned.1,
            )],
            None,
        );
    }
    if let Some(group) = group {
        let sticky_hit = group_sticky_hit(deps, snapshot, breaker_config, group, session_key);
        let mut rng = crate::gateway::credential_pool::WeightedRng::from_entropy();
        let expanded = expand_group_candidates(
            group,
            &snapshot.providers,
            &deps.breaker,
            breaker_config,
            sticky_hit.as_ref(),
            &mut || rng.next_f64(),
        );
        if !expanded.is_empty() {
            // 组内候选都是「主候选」:逐个注入账号凭据(与单候选主路径
            // 同口径;账号 id 随候选携带,日志/头/凭据结果记录消费)。
            // 不追加 provider 级 fallback 链(方案 §6;R1 起全系统移除)。
            let mut candidates = Vec::with_capacity(expanded.len());
            for candidate in expanded {
                let (provider, account_id) =
                    inject_account_credential(deps, candidate.provider, session_key, attempt_model)
                        .await;
                candidates.push((
                    RouteCandidate {
                        provider,
                        model: candidate.model,
                    },
                    account_id,
                ));
            }
            return (candidates, None);
        }
        // 2026-09-26 调用契约:聚合目标全部禁用/缺失/熔断 → 明确失败,
        // 不再回退全池选择(裸名 fail-open 已随调用契约下线)。
        return (
            Vec::new(),
            Some(format!(
                "Aggregate '{}' has no available targets (all disabled, missing, or circuit-broken).",
                group.name
            )),
        );
    }
    match select_provider(
        deps,
        snapshot,
        breaker_config,
        attempt_model,
        attempt_target,
    ) {
        Ok(provider) => {
            let (provider, account_id) =
                inject_account_credential(deps, provider.clone(), session_key, attempt_model).await;
            // 20261015_01 R1:fallback_providers 链已移除,见粘性路径注。
            (
                vec![(
                    RouteCandidate {
                        provider,
                        model: upstream_model,
                    },
                    account_id,
                )],
                None,
            )
        }
        // 模型无人服务:透传 RoutingError 消息(502 形态与升级前一致)。
        Err(error) => (Vec::new(), Some(error.0)),
    }
}

/// 聚合成功回写(sticky 策略;与 20260905_03 S2 同口径「只由成功响应
/// 写入」):键 = `model-group:{组id}:{会话键}`,值 = 命中的 (provider,
/// 实际模型, 账号)。
pub(super) fn bind_group_sticky(
    deps: &GatewayDeps,
    group: Option<&ModelGroup>,
    session_key: Option<&str>,
    provider: &Provider,
    account_id: Option<&str>,
    actual_model: &str,
) {
    let Some(group) = group else {
        return;
    };
    if group.strategy != ModelGroupStrategy::Sticky {
        return;
    }
    let Some(key) = session_key else {
        return;
    };
    let store_key = group_sticky_key(&group.id, key);
    let now = session_affinity::now_ms();
    deps.group_sticky.sticky_bind(
        &store_key,
        AffinityEntry {
            provider_id: provider.id.clone(),
            model: actual_model.to_owned(),
            account_id: account_id.map(str::to_owned),
            // 阶段五 C-1:聚合 sticky 不消费缓存收益(统计恒为默认)。
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            hits: 0,
            bound_at_ms: now,
        },
        now,
    );
}

/// 调用日志双归属(方案 §3.4):聚合请求把**实际调用的上游模型**记入
/// payload 键 `upstream_model`(row.model 保持聚合名 → modelLabel 维度;
/// 读取侧以 upstream_model 覆盖 baseModel/计价模型维度)。
pub(super) fn upstream_model_extra(actual_model: &str, request_model: &str) -> Map<String, Value> {
    let mut map = Map::new();
    if !actual_model.is_empty() && actual_model != request_model {
        map.insert("upstream_model".to_owned(), json!(actual_model));
    }
    map
}

/// 压缩报告 → 日志 extra 键(20260905_03 S4/S5):key 固定 `compression`;
/// 未进管线(off / 非 dict body)时返回空 map(with_extra 对空合并不
/// 产生任何键,日志形态与升级前一致)。
pub(super) fn compression_extra_map(
    report: &Option<wr_domain::compression::CompressionReport>,
) -> serde_json::Map<String, Value> {
    let mut map = serde_json::Map::new();
    if let Some(report) = report {
        map.insert("compression".to_owned(), report.to_value());
    }
    map
}
