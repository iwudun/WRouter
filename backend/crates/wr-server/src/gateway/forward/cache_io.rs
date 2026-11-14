// 20260905_03 S6/S7:响应缓存查找/写入 + 上下文预检
// 20261013_01 C1 自 forward.rs 纯搬移;经 `use super::*` 复用共享类型。

use super::*;

// ---------------------------------------------------------------------------
// 20260905_03 S6/S7:响应缓存查找/写入 + 上下文预检
// ---------------------------------------------------------------------------

/// S6:响应缓存查找(内存 → 磁盘晋升)。TTL 由配置驱动:内存层用
/// memory_ttl_secs,晋升条目按剩余磁盘 TTL 保留语义近似为 memory TTL。
pub(super) async fn response_cache_lookup(
    deps: &GatewayDeps,
    key: &str,
    config: &wr_infra::config::SavingsResponseCacheConfig,
) -> Option<response_cache::CachedResponse> {
    let now = session_affinity::now_ms();
    if let Some(hit) = deps.response_cache.get(key, now) {
        return Some(hit);
    }
    let disk = deps.store.response_cache_get(key, now).await.ok()??;
    let entry = response_cache::CachedResponse {
        payload: disk.payload.clone(),
        cost_usd: disk.cost_usd,
        model: disk.model,
        created_at_ms: disk.created_at_ms,
    };
    // 晋升内存(读路径;TTL 用内存层默认语义)。
    deps.response_cache
        .put(key, entry.clone(), config.memory_ttl_secs * 1000, now);
    Some(entry)
}

/// S6:成功响应写入两层缓存(内存 + 磁盘;磁盘失败静默降级为仅内存)。
pub(super) async fn response_cache_store_entry(
    deps: &GatewayDeps,
    key: &str,
    payload: &Value,
    cost_usd: f64,
    model: &str,
    config: &wr_infra::config::SavingsResponseCacheConfig,
) {
    let now = session_affinity::now_ms();
    deps.response_cache.put(
        key,
        response_cache::CachedResponse {
            payload: payload.clone(),
            cost_usd,
            model: model.to_owned(),
            created_at_ms: now,
        },
        config.memory_ttl_secs * 1000,
        now,
    );
    let _ = deps
        .store
        .response_cache_put(
            key,
            payload,
            cost_usd,
            model,
            config.disk_ttl_secs,
            config.disk_max_entries,
            now,
        )
        .await;
}

/// S7:计划候选是否「装得下」—— 至少一个服务该模型且启用的 provider
/// 满足:未配置窗口(不设限)或窗口 ≥ 估算 token。无 provider 服务该
/// 模型时保留候选(交给既有选路错误路径)。`target` 绑定 provider 时
/// 只看该 provider。
pub(super) fn attempt_fits_context(
    snapshot: &ConfigSnapshot,
    target: Option<&RouteModelRef>,
    model: &str,
    estimated_tokens: i64,
) -> bool {
    if model.is_empty() {
        return true;
    }
    // 20260905_04:聚合候选只看组内 (providerId, model) 对(方案 §3.4
    // 「上下文预检对聚合候选生效」);组内任一目标装得下即保留 attempt。
    // 窗口按**各目标自己的模型名**查(聚合名不是上游模型)。
    if let Some(RouteModelRef::Aggregate(reference)) = target {
        let fits: Vec<bool> = reference
            .group
            .targets
            .iter()
            .filter(|group_target| group_target.enabled)
            .filter_map(|group_target| {
                snapshot
                    .providers
                    .iter()
                    .find(|provider| provider.id == group_target.provider_id)
                    .filter(|provider| provider.enabled)
                    .map(|provider| {
                        provider
                            .context_windows
                            .get(&group_target.model)
                            .and_then(Value::as_i64)
                            .is_none_or(|window| window >= estimated_tokens)
                    })
            })
            .collect();
        // 无任何可查目标(全禁用/缺失)→ 保留候选,交给既有选路错误路径。
        return fits.is_empty() || fits.iter().any(|ok| *ok);
    }
    let candidates: Vec<&Provider> = match target {
        Some(RouteModelRef::Provider(reference)) => {
            let target_id = reference
                .provider
                .get("id")
                .and_then(Value::as_str)
                .or_else(|| reference.provider.get("name").and_then(Value::as_str))
                .unwrap_or("");
            snapshot
                .providers
                .iter()
                .filter(|provider| provider.id == target_id)
                .collect()
        }
        _ => snapshot.providers.iter().collect(),
    };
    let serving: Vec<&Provider> = candidates
        .into_iter()
        .filter(|provider| provider.enabled && provider_serves_model(provider, model))
        .collect();
    if serving.is_empty() {
        return true;
    }
    serving.iter().any(|provider| {
        provider
            .context_windows
            .get(model)
            .and_then(Value::as_i64)
            .map_or(true, |window| window >= estimated_tokens)
    })
}

/// S7:上下文预检 —— 估算 token 一次,剔除装不下的计划候选(方案 §4.5
/// 候选过滤前估算,整循环只计一次)。
pub(super) fn context_prefilter_plan(
    snapshot: &ConfigSnapshot,
    mut plan: RouteExecutionPlan,
    estimated_tokens: i64,
) -> RouteExecutionPlan {
    if estimated_tokens <= 0 {
        return plan;
    }
    plan.attempts.retain(|attempt| {
        let model = attempt.model.as_deref().unwrap_or_default();
        attempt_fits_context(snapshot, attempt.target.as_ref(), model, estimated_tokens)
    });
    plan
}

/// S8:压缩节省事件记账(有引擎实际生效才记;2026-09-11 起只记 token 不记
/// 金额,estimated_usd_saved 恒 0;fire-and-forget,度量非关键路径,方案 §4.6)。
pub(super) fn record_compression_savings(
    deps: &GatewayDeps,
    request_id: &str,
    report: Option<&wr_domain::compression::CompressionReport>,
) {
    let Some(report) = report else {
        return;
    };
    let tokens_saved = report.tokens_saved_estimate();
    if tokens_saved <= 0 {
        return;
    }
    let event = wr_app::ports::SavingsEventInput {
        request_id: request_id.to_owned(),
        kind: "compression",
        tokens_saved,
        estimated_usd_saved: 0.0,
        engine_breakdown: Some(report.to_value()),
    };
    let store = deps.store.clone();
    tokio::spawn(async move {
        let _ = store.record_savings_event(&event).await;
    });
}

/// S8:响应缓存命中事件记账(tokens = 被免掉的整次用量;2026-09-11 起只记
/// token 不记金额,estimated_usd_saved 恒 0;同样 fire-and-forget)。
pub(super) fn record_cache_hit_savings(
    deps: &GatewayDeps,
    request_id: &str,
    usage: &wr_domain::usage::Usage,
) {
    let event = wr_app::ports::SavingsEventInput {
        request_id: request_id.to_owned(),
        kind: "response_cache",
        tokens_saved: usage.input_tokens + usage.output_tokens,
        estimated_usd_saved: 0.0,
        engine_breakdown: None,
    };
    let store = deps.store.clone();
    tokio::spawn(async move {
        let _ = store.record_savings_event(&event).await;
    });
}

/// K3/K8:凭据冷却用的 `Retry-After` 毫秒值。仅 429/5xx 消费:
/// 上游 `Retry-After` 头(秒数或 HTTP-date)优先;429 无(可解析)头时从
/// 错误正文做 reset 文本解析兜底(K8)。解析失败一律
/// `None` 静默跳过;24h 上限在 `record_outcome_with_retry_after` 侧夹紧。
pub(super) fn credential_retry_after_ms(
    status: i64,
    retry_after: Option<&str>,
    error_body: Option<&str>,
) -> Option<i64> {
    if status != 429 && status < 500 {
        return None;
    }
    if let Some(ms) = wr_domain::gateway::parse_retry_after_ms(retry_after) {
        return Some(ms as i64);
    }
    if status == 429 {
        return wr_domain::gateway::parse_rate_limit_reset_ms(error_body?);
    }
    None
}

/// API key 模型白名单(镜像 `_api_key_model_error` + auth.models_for):
/// key_id 非空时在快照 api_keys 里按 `id==key_id || key==key_id` 找条目
/// (registry 的 key_id = entry.id 或回落 entry.key),取 `models` 内非空
/// 字符串为白名单;白名单非空且不含 model → 拒绝。内部身份/未配置 → 放行。
///
/// 2026-11-07(W3-7):只认完整请求选择器(`供应商/模型` 或
/// `wrouter/<聚合名>`,大小写不敏感);旧版裸模型名/裸聚合名白名单条目
/// 不再兜底匹配。
pub(super) fn api_key_model_error(
    snapshot: &ConfigSnapshot,
    identity: &GatewayIdentity,
    requested: &str,
) -> Option<String> {
    if identity.key_id.is_empty() {
        return None;
    }
    let entry = snapshot.config.api_keys.iter().find_map(|entry| {
        let map = entry.as_object()?;
        let id = map
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        let key = map
            .get("key")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        // registry 派生:key_id = str(entry.id or "").strip() or key。
        let resolved = if id.is_empty() { key } else { id };
        (resolved == identity.key_id).then_some(map)
    });
    let allowed: Vec<String> = entry
        .and_then(|map| map.get("models").and_then(Value::as_array).cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|item| {
            item.as_str()
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
        })
        .collect();
    if allowed.is_empty() {
        return None;
    }
    let requested_lc = requested.trim().to_ascii_lowercase();
    let ok = allowed
        .iter()
        .any(|entry| entry.trim().to_ascii_lowercase() == requested_lc);
    if ok {
        return None;
    }
    Some(format!(
        "Model '{requested}' is not allowed for this API key."
    ))
}
