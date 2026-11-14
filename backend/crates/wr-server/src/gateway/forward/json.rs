// 非流式转发主体
// 20261013_01 C1 自 forward.rs 纯搬移;经 `use super::*` 复用共享类型。
// 20261106_01 P3-1:消费统一执行计划,原生 Raw 成功响应原字节透传。

use super::prepared::PreparedExchange;
use super::*;
use wr_domain::protocol::contract::ResponseExecutor;

// ---------------------------------------------------------------------------
// 非流式转发
// ---------------------------------------------------------------------------

/// 非流式转发主体。
#[allow(clippy::too_many_arguments)]
pub async fn forward_json(
    deps: &GatewayDeps,
    snapshot: Arc<ConfigSnapshot>,
    protocol: &str,
    mut payload: Value,
    headers: Vec<(String, String)>,
    request_id: &str,
    identity: &GatewayIdentity,
    // 20261016_01 §2.6.3:入口传入的统一计时起点(替代管线内 Instant::now)。
    started: Instant,
    // 20261016_03:入站原始 query(passthrough allowlist / preset 静态
    // query 的指纹源;None = 无指纹语境)。
    inbound_query: Option<String>,
    // P3-1:入站原始正文字节(原生请求面 ByteExact 的权威来源)。
    inbound_raw: Vec<u8>,
) -> Result<ForwardJsonResult, ForwardError> {
    let attempts = new_trail();
    // 原始正文的解析视图(与 inbound_raw 同一原文;在路由/压缩改写前固定,
    // 作为请求面字节决策的基线)。解析失败时退化为当前 payload。
    let inbound_value: Value =
        serde_json::from_slice(&inbound_raw).unwrap_or_else(|_| payload.clone());

    // K4:会话键提取(sticky 选号前置)。在路由引擎改写 headers/payload
    // 之前从**原始请求**提取;头优先、首条 user 消息哈希兜底;只随调用链
    // 传递到选号点,不落库。
    let session_key = wr_domain::gateway::extract_session_key(&headers, payload.as_object());

    // 客户端指纹源(20261016_03):路由引擎会改写 headers(访问控制/
    // 伪影剥离),指纹必须取**路由改写前**的原始入站快照。仅在本函数
    // 内存活,不落库;passthrough 时经白名单按上游线格式过滤后出站。
    let fingerprint_inbound = headers.clone();

    // Claude Desktop 十六进制模型名解码(对普通名字恒等)。
    if let Some(model) = payload.get("model").and_then(Value::as_str) {
        let decoded = decode_claude_app_model(model);
        payload["model"] = Value::String(decoded);
    }
    let mut model = payload
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    // 20260905_04:客户端原始请求名(聚合的 requestedModel 维度)。
    let client_requested_model = model.clone();

    // provider 记录解析失败(镜像 Python providers() 抛 ValueError → 502;
    // 该异常发生在 `_request_with_fallback` 内部,故 except 分支会先落审计行)。
    if let Some(message) = &snapshot.provider_error {
        log_no_provider(
            deps,
            &snapshot,
            request_id,
            protocol,
            &model,
            identity,
            started,
            &[],
            Vec::new(),
            message,
            &payload,
        );
        return Err(ForwardError {
            message: message.clone(),
            attempts: Vec::new(),
        });
    }
    let breaker_config = snapshot.config.reliability.circuit_breaker.clone();
    deps.concurrency
        .sync_config(&snapshot.config.reliability.concurrency);

    // --- provider/model 前缀解析(客户端显式指定 provider) -----------
    let (client_model, client_route_target) = resolve_client_prefix(&snapshot, &model);
    let mut requested_model: Option<String> = None;
    if client_route_target.is_some() {
        requested_model = Some(model.clone());
        payload["model"] = Value::String(client_model.clone());
        model = client_model;
    }

    // --- 路由(20261101_02 T3:规则域下线,目标只来自客户端显式前缀) ---
    let route_target = client_route_target.clone();
    // 聚合请求:requestedModel = 客户端原始请求名(§3.4 双归属的展示维度)。
    if matches!(route_target, Some(RouteModelRef::Aggregate(_))) {
        requested_model = Some(client_requested_model.clone());
    }
    // --- 调用契约(2026-09-26):选择器必须显式 --------------------------
    // 请求名必须命中注册表(`供应商/模型` 或 `wrouter/<聚合名>`);裸模型名/
    // 裸聚合名通道已全部下线,未命中直接 400,不再进入兜底链。客户端已用
    // 显式前缀(route_target Some)时跳过 —— 选择器本身就已指到目标。
    if route_target.is_none()
        && snapshot
            .compiled
            .model_registry
            .resolve(Some(&model))
            .is_none()
    {
        let message = if model.trim().is_empty() {
            "Request body must include a 'model' field.".to_owned()
        } else {
            format!(
                "Model '{model}' is not configured. Call models as 'provider/model' or aggregates as 'wrouter/aggregate-name'."
            )
        };
        deps.record_log_extra(
            &snapshot,
            request_id,
            &RequestLogEvent {
                protocol: protocol.to_owned(),
                model: model.clone(),
                status: 400,
                stream: false,
                duration_ms: elapsed_ms(started),
                provider_attempts: Vec::new(),
                retry_attempts: Vec::new(),
                end_reason: "model_not_found",
                provider: None,
                account: None,
                api_key_id: Some(identity.key_id.clone()),
                first_byte_ms: None,
                error: Some(message.clone()),
            },
            failure_log_extra(deps, &snapshot, identity, &headers, &payload, None, None),
        );
        return Ok(ForwardJsonResult {
            status: 400,
            body: json!({
                "error": {"type": "model_not_found", "message": message, "code": "model_not_found"}
            }),
            headers: Vec::new(),
            raw_body: None,
        });
    }
    let plan = build_fallback_plan(&snapshot, &model);
    // 20260905_03:全局节省动作解析(20261101_02 T3-B:规则层已删)。
    let resolved_savings = resolve_savings(&snapshot.config.savings);
    // S4/S5:token 压缩管线(auto 档按估算 token 阈值触发,full 恒进;
    // off → None)。压缩发生在路由改写之后、出站之前,调用日志与节省
    // 度量消费同一份报告。
    let compression_report = wr_domain::compression::compress_request_body(
        &mut payload,
        resolved_savings.compression_tier,
        snapshot.config.savings.compression.threshold_tokens,
    );
    let savings_extra = compression_extra_map(&compression_report);

    // S7:上下文预检(20260905_03 §4.5)—— 估算一次 prompt token,装不下
    // 的计划候选剔除;全部装不下 → 明确错误,不发起必败上游调用。
    let estimated_tokens = wr_domain::compression::estimate_tokens(&payload);
    let plan = context_prefilter_plan(&snapshot, plan, estimated_tokens);
    if plan.attempts.is_empty() {
        let message = format!(
            "estimated prompt ~{estimated_tokens} tokens exceeds every candidate model's context window"
        );
        deps.record_log_extra(
            &snapshot,
            request_id,
            &RequestLogEvent {
                protocol: protocol.to_owned(),
                model: model.clone(),
                status: 400,
                stream: false,
                duration_ms: elapsed_ms(started),
                provider_attempts: Vec::new(),
                retry_attempts: Vec::new(),
                end_reason: "context_overflow",
                provider: None,
                account: None,
                api_key_id: Some(identity.key_id.clone()),
                first_byte_ms: None,
                error: Some(message.clone()),
            },
            failure_log_extra(deps, &snapshot, identity, &headers, &payload, None, None),
        );
        return Ok(ForwardJsonResult {
            status: 400,
            body: json!({
                "error": {"type": "context_too_long", "message": message, "code": "context_too_long"}
            }),
            headers: Vec::new(),
            raw_body: None,
        });
    }

    // --- API key 模型白名单(R5;2026-09-26 起按完整请求选择器匹配,
    //     canonical 兜底兼容存量裸名条目) ---
    if let Some(message) = api_key_model_error(&snapshot, identity, &client_requested_model)
    {
        deps.record_log_extra(
            &snapshot,
            request_id,
            &RequestLogEvent {
                protocol: protocol.to_owned(),
                model: model.clone(),
                status: 403,
                stream: false,
                duration_ms: elapsed_ms(started),
                provider_attempts: Vec::new(),
                retry_attempts: Vec::new(),
                end_reason: "model_forbidden",
                provider: None,
                account: None,
                api_key_id: Some(identity.key_id.clone()),
                first_byte_ms: None,
                error: Some(message.clone()),
            },
            failure_log_extra(deps, &snapshot, identity, &headers, &payload, None, None),
        );
        return Ok(ForwardJsonResult {
            status: 403,
            body: json!({
                "error": {"type": "model_forbidden", "message": message, "code": "model_forbidden"}
            }),
            headers: Vec::new(),
            raw_body: None,
        });
    }

    // --- precheck 限流 / 预算(R4.8)------------------------------------
    let extra_rules =
        limits::api_key_limits_to_rules(api_key_limits(&snapshot, &identity.key_id).as_ref());
    if let limits::PrecheckResult::Rejected(failure) = deps.prechecker.evaluate(
        &limits::extract_identity(&headers),
        Some(estimate_usage(&payload)),
        0.0,
        &extra_rules,
    ) {
        let mut precheck_extra = serde_json::Map::new();
        precheck_extra.insert("error".to_owned(), json!(failure.message));
        precheck_extra.insert(
            "precheck".to_owned(),
            Value::Object(failure.details.clone()),
        );
        deps.record_log_extra(
            &snapshot,
            request_id,
            &RequestLogEvent {
                protocol: protocol.to_owned(),
                model: model.clone(),
                status: failure.status,
                stream: false,
                duration_ms: elapsed_ms(started),
                provider_attempts: trail_provider_ids(&attempts),
                retry_attempts: trail_values(&attempts),
                end_reason: "precheck_failed",
                provider: None,
                account: None,
                api_key_id: Some(identity.key_id.clone()),
                first_byte_ms: None,
                error: Some(failure.message.clone()),
            },
            RequestLogExtra {
                extra: precheck_extra,
                ..failure_log_extra(deps, &snapshot, identity, &headers, &payload, None, None)
            },
        );
        return Ok(ForwardJsonResult {
            status: failure.status,
            body: failure.to_body(),
            headers: Vec::new(),
            raw_body: None,
        });
    }

    // --- 访问控制策略(A3) ------------------------------------------------
    let principal = access_policy::extract_principal(&headers);
    let decision = snapshot.access_policy.evaluate(
        &principal.key,
        &PolicyResource {
            kind: "model".to_owned(),
            name: model.clone(),
        },
    );
    if decision.effect == access_policy::PolicyEffect::Deny {
        deps.record_log_extra(
            &snapshot,
            request_id,
            &RequestLogEvent {
                protocol: protocol.to_owned(),
                model: model.clone(),
                status: 403,
                stream: false,
                duration_ms: elapsed_ms(started),
                provider_attempts: Vec::new(),
                retry_attempts: Vec::new(),
                end_reason: "policy_denied",
                provider: None,
                account: None,
                api_key_id: Some(identity.key_id.clone()),
                first_byte_ms: None,
                error: Some(format!("access denied by policy '{}'", decision.rule_id)),
            },
            failure_log_extra(deps, &snapshot, identity, &headers, &payload, None, None),
        );
        return Ok(ForwardJsonResult {
            status: 403,
            body: access_policy::policy_denied_body(&decision),
            headers: Vec::new(),
            raw_body: None,
        });
    }

    // --- 幂等重放(R4.4)-------------------------------------------------
    let idem_config = snapshot.config.reliability.idempotency.clone();
    let cache_key = idempotency_header(&headers, &idem_config.header_name)
        .map(|key| idempotency_store_key(&key, protocol, &payload));
    if let Some(store_key) = cache_key.as_deref() {
        let hash = request_hash(&payload, protocol);
        match deps.idempotency.lookup(store_key, &hash).await {
            Err(_) => {
                let mut conflict = serde_json::Map::new();
                conflict.insert("error".to_owned(), json!("idempotency_key_conflict"));
                deps.record_log_extra(
                    &snapshot,
                    request_id,
                    &RequestLogEvent {
                        protocol: protocol.to_owned(),
                        model: model.clone(),
                        status: 409,
                        stream: false,
                        duration_ms: elapsed_ms(started),
                        provider_attempts: trail_provider_ids(&attempts),
                        retry_attempts: trail_values(&attempts),
                        end_reason: "idempotency_conflict",
                        provider: None,
                        account: None,
                        api_key_id: Some(identity.key_id.clone()),
                        first_byte_ms: None,
                        error: Some("idempotency_key_conflict".to_owned()),
                    },
                    RequestLogExtra {
                        extra: conflict,
                        ..failure_log_extra(
                            deps, &snapshot, identity, &headers, &payload, None, None,
                        )
                    },
                );
                return Ok(ForwardJsonResult {
                    status: 409,
                    body: json!({
                        "error": {
                            "message": "Idempotency key was reused with a different request.",
                            "code": "idempotency_key_conflict",
                        }
                    }),
                    headers: Vec::new(),
                    raw_body: None,
                });
            }
            Ok(Some(entry)) if entry.state == EntryState::Completed => {
                // 重放命中:与 Python 一致直接返回缓存体,不再落新日志行。
                return Ok(ForwardJsonResult {
                    status: entry.status_code as u16,
                    body: entry.payload,
                    headers: Vec::new(),
                    raw_body: None,
                });
            }
            Ok(_) => {}
        }
        deps.idempotency.put_pending(store_key, &hash).await;
    }

    // --- 三层回退请求 ----------------------------------------------------
    // 全链耗尽/无候选:先落一条 no_provider 审计行,再把异常外抛给 HTTP 层
    // 转 502(镜像 forwarding.py 的 except 分支顺序)。
    // --- S6:响应缓存精确键命中(20260905_03 §4.4)------------------------
    // 门控:savings.responseCache(全局 enabled + 规则覆盖)且无
    // x-wr-no-cache 旁路;键只在显式 temperature==0 时存在。
    let cache_bypass = headers.iter().any(|(key, value)| {
        key.eq_ignore_ascii_case("x-wr-no-cache") && value.trim().eq_ignore_ascii_case("true")
    });
    // 20260905_04:聚合请求不进响应缓存 —— 精确键含聚合名而候选实际模型
    // 可变,同键不同模型的命中语义不可解释(宁可少命中,不可错误命中)。
    // 生效目标 = 引擎/显式前缀 target,缺失时回退 plan 首 attempt 的解析
    // 目标(直发聚合名且无规则命中时 route_target 为 None,聚合语义仍生效)。
    let effective_target = route_target.as_ref().or_else(|| {
        plan.attempts
            .first()
            .and_then(|attempt| attempt.target.as_ref())
    });
    let is_aggregate_request = matches!(effective_target, Some(RouteModelRef::Aggregate(_)));
    // 聚合规范名(响应 model 回写用;`model` 变量会被逐候选重写污染)。
    let aggregate_name = match effective_target {
        Some(RouteModelRef::Aggregate(reference)) => Some(reference.group.name.clone()),
        _ => None,
    };
    let response_cache_key =
        if resolved_savings.response_cache && !cache_bypass && !is_aggregate_request {
            wr_domain::gateway::response_cache_key(protocol, &payload)
        } else {
            None
        };
    if let Some(cache_key) = &response_cache_key
        && let Some(hit) =
            response_cache_lookup(deps, cache_key, &snapshot.config.savings.response_cache).await
    {
        // 命中:计费 $0,节省金额单列(x-wr-cost-saved),日志照记
// (cacheSource 标记,方案 §4.4)。
        let usage = wr_domain::usage::normalize_usage(hit.payload.get("usage"));
        let cost_saved = hit.cost_usd;
        record_cache_hit_savings(deps, request_id, &usage);
        let mut hit_extra = savings_extra.clone();
        hit_extra.insert("cacheSource".to_owned(), json!("hit"));
        hit_extra.insert("costSavedUsd".to_owned(), json!(cost_saved));
        deps.record_log_extra(
            &snapshot,
            request_id,
            &RequestLogEvent {
                protocol: protocol.to_owned(),
                model: model.clone(),
                status: 200,
                stream: false,
                duration_ms: elapsed_ms(started),
                provider_attempts: trail_provider_ids(&attempts),
                retry_attempts: trail_values(&attempts),
                end_reason: "cache_hit",
                provider: None,
                account: None,
                api_key_id: Some(identity.key_id.clone()),
                first_byte_ms: None,
                error: None,
            },
            RequestLogExtra::default()
                .with_identity(&identity.key_name)
                .with_preview(RequestLogRecorder::call_logs_enabled(&snapshot), &payload)
                .with_usage(usage, 0.0)
                .with_requested_model(requested_model.as_deref().unwrap_or_default(), &model)
                .with_request_meta(
                    Some(&client_label(&headers)),
                    Some("POST"),
                    Some(protocol),
                    Some(&iso_now()),
                )
                .with_extra(hit_extra),
        );
        let age_ms = session_affinity::now_ms().saturating_sub(hit.created_at_ms);
        return Ok(ForwardJsonResult {
            status: 200,
            body: hit.payload,
            headers: vec![
                ("x-wr-cache".to_owned(), "hit".to_owned()),
                ("x-wr-cache-age-ms".to_owned(), age_ms.to_string()),
                ("x-wr-cost-saved".to_owned(), format!("{cost_saved:.6}")),
            ],
            raw_body: None,
        });
    }

    let ForwardSuccess {
        response,
        provider,
        hit_account,
        actual_model,
        proxy_source,
        byte_exact,
    } = match request_with_fallback(
        deps,
        &snapshot,
        &breaker_config,
        request_id,
        protocol,
        &mut payload,
        &model,
        &attempts,
        &plan,
        effective_target,
        session_key.as_deref(),
        &resolved_savings,
        &fingerprint_inbound,
        inbound_query.as_deref(),
        // 阶段四 C-4:请求级计时起点(出站偏移相对它计算)。
        started,
        &inbound_raw,
        &inbound_value,
    )
    .await
    {
        Ok(success) => success,
        Err(ForwardFailure::QueueRejected {
            status,
            message,
            provider,
            account,
        }) => {
            // 20261016_01 A-3:排队拒绝终态(镜像流式 handle_stream_queue_
            // rejected):落一条 queue_timeout 行 + 429 concurrency_limited
            // 信封,不再静默超发。
            deps.record_log_extra(
                &snapshot,
                request_id,
                &RequestLogEvent {
                    protocol: protocol.to_owned(),
                    model: model.clone(),
                    status,
                    stream: false,
                    duration_ms: elapsed_ms(started),
                    provider_attempts: trail_provider_ids(&attempts),
                    retry_attempts: trail_values(&attempts),
                    end_reason: "queue_timeout",
                    provider,
                    account,
                    api_key_id: Some(identity.key_id.clone()),
                    first_byte_ms: None,
                    error: Some(message.clone()),
                },
                failure_log_extra(deps, &snapshot, identity, &headers, &payload, None, None),
            );
            return Ok(ForwardJsonResult {
                status,
                body: json!({
                    "error": {"type": "concurrency_limited", "message": message, "code": "concurrency_limited"}
                }),
                headers: Vec::new(),
                raw_body: None,
            });
        }
        Err(ForwardFailure::Error(error)) => {
            log_no_provider(
                deps,
                &snapshot,
                request_id,
                protocol,
                &model,
                identity,
                started,
                &trail_provider_ids(&attempts),
                trail_values(&attempts),
                &error.message,
                &payload,
            );
            return Err(error);
        }
    };

    // --- 响应表示执行(P3-1)----------------------------------------------
    // 原生 Raw 成功响应 → 原字节透传(不强制 Value 重序列化);聚合只按
    // 协议路径回写 model(R8)。非流面只接受 JSON 表示:SSE 属未规划
    // 表示,不做流聚合(W2-2 定案)。
    let native_raw = byte_exact
        && response.status < 400
        && matches!(
            PreparedExchange::new(protocol, false, &provider)
                .and_then(|exchange| exchange.select_executor(Some(response.content_type()))),
            Ok(ResponseExecutor::Json)
        );
    let (body, raw_body) = if native_raw {
        let mut value = serde_json::from_slice::<Value>(&response.body).unwrap_or_else(|_| {
            decode_upstream_json(protocol, &provider, &response)
                .map(|(value, _)| value)
                .unwrap_or_else(|failure| {
                    let (_, envelope) = failure.envelope(protocol, None);
                    envelope
                })
        });
        let raw = if let Some(name) = &aggregate_name {
            let (changed, _outcome) =
                wr_domain::protocol::correction::apply_model_alias(&mut value, name);
            if changed {
                serde_json::to_vec(&value).unwrap_or_else(|_| response.body.clone())
            } else {
                response.body.clone()
            }
        } else {
            response.body.clone()
        };
        (value, Some(raw))
    } else {
        let (mut decoded, response_report) =
            match decode_upstream_json(protocol, &provider, &response) {
                Ok(decoded) => decoded,
                Err(failure) => {
                    let (status, body) = failure.envelope(protocol, None);
                    deps.record_log_extra(
                        &snapshot,
                        request_id,
                        &RequestLogEvent {
                            protocol: protocol.to_owned(),
                            model: model.clone(),
                            status,
                            stream: false,
                            duration_ms: elapsed_ms(started),
                            provider_attempts: trail_provider_ids(&attempts),
                            retry_attempts: trail_values(&attempts),
                            end_reason: "conversion_failed",
                            provider: Some(provider.id.clone()),
                            account: hit_account.clone(),
                            api_key_id: Some(identity.key_id.clone()),
                            first_byte_ms: None,
                            error: Some(failure.message().to_owned()),
                        },
                        failure_log_extra(
                            deps, &snapshot, identity, &headers, &payload, None, None,
                        ),
                    );
                    return Ok(ForwardJsonResult {
                        status,
                        body,
                        headers: Vec::new(),
                        raw_body: None,
                    });
                }
            };
        record_response_conversion_metrics(deps, &response_report, protocol, &provider);
        if let Some(name) = &aggregate_name {
            wr_domain::protocol::correction::apply_model_alias(&mut decoded, name);
        }
        (decoded, None)
    };
    let attempts_now = trail_provider_ids(&attempts);
    let mut routing_headers = Vec::new();
    // 20261101_02 T3:x-wr-route-reason / x-wr-route-source 随规则域删除
    // (二者只在规则命中时非 default)。
    if !model.is_empty() {
        routing_headers.push(("x-wr-routed-model".to_owned(), model.clone()));
    }
    if !attempts_now.is_empty() {
        routing_headers.push((
            "x-wr-fallback-attempts".to_owned(),
            attempts_now.len().to_string(),
        ));
        if attempts_now.len() > 1 {
            routing_headers.push((
                "x-wr-fallback-failures".to_owned(),
                (attempts_now.len() - 1).to_string(),
            ));
        }
    }
    // K5:命中账号时回传账号 id(legacy 单 key 不加;排障一眼确认用了
    // 哪把 key)。
    if let Some(account_id) = &hit_account {
        routing_headers.push(("x-wr-account".to_owned(), account_id.clone()));
    }
    // --- R4.5/R4.6:日志落库带用量与成本(C3 正文捕获按门开)------------
    let usage = wr_domain::usage::normalize_usage(body.get("usage"));
    let cost_usd = cost_of(&provider, &usage);
    // S6:成功响应入缓存(只由成功响应写入;存解码后的客户端协议 JSON)。
    if response.status < 400
        && let Some(cache_key) = &response_cache_key
    {
        response_cache_store_entry(
            deps,
            cache_key,
            &body,
            cost_usd,
            &model,
            &snapshot.config.savings.response_cache,
        )
        .await;
    }
    // S8:压缩节省记账(仅上游成功;缓存命中路径已单记一笔)。
    if response.status < 400 {
        record_compression_savings(deps, request_id, compression_report.as_ref());
    }
    let response_text = String::from_utf8_lossy(&response.body).into_owned();
    // §3.4 双归属:聚合请求补记实际调用的上游模型(键 `upstream_model`;
    // 非聚合请求 actual_model == model,不产生键,日志形态零变化)。
    let mut log_extra = savings_extra.clone();
    log_extra.insert(
        "usageQuality".to_owned(),
        Value::String(
            if usage.total_tokens > 0 {
                "final"
            } else {
                "unknown"
            }
            .to_owned(),
        ),
    );
    log_extra.extend(upstream_model_extra(
        &actual_model,
        aggregate_name.as_deref().unwrap_or(&model),
    ));
    // 20260906_01 §4.7:代理来源归属(排障「这个请求到底走没走代理」;
    // 凭据已在 describe_proxy_source 里掩掉,决策 #10)。
    log_extra.insert("proxy".to_owned(), Value::String(proxy_source));
    let extra = RequestLogExtra::default()
        .with_identity(&identity.key_name)
        .with_preview(RequestLogRecorder::call_logs_enabled(&snapshot), &payload)
        .with_usage(usage, cost_usd)
        .with_requested_model(requested_model.as_deref().unwrap_or_default(), &model)
        .with_request_meta(
            Some(&client_label(&headers)),
            Some("POST"),
            Some(protocol),
            Some(&iso_now()),
        )
        // 压缩明细(20260905_03 S4/S5):原始/压缩后字符量 + 引擎 skip
        // 全链路透传;未进管线(off)时 extra 为空。
        .with_extra(log_extra)
        .with_capture(
            RequestLogRecorder::capture_enabled(&snapshot)
                && RequestLogRecorder::call_logs_enabled(&snapshot),
            deps.settings.body_capture_max_bytes,
            Some(&payload),
            Some(&response_text),
            Some(header_map(&response.headers)),
        )
        .with_request_headers(&headers)
        .with_trace_items(trail_values(&attempts));
    deps.record_log_extra(
        &snapshot,
        request_id,
        &RequestLogEvent {
            protocol: protocol.to_owned(),
            model: model.clone(),
            status: response.status,
            stream: false,
            duration_ms: elapsed_ms(started),
            provider_attempts: attempts_now,
            retry_attempts: trail_values(&attempts),
            end_reason: "completed",
            provider: Some(provider.id.clone()),
            account: hit_account.clone(),
            api_key_id: Some(identity.key_id.clone()),
            first_byte_ms: None,
            error: None,
        },
        extra,
    );
    // Python 在转发返回后**无条件**再记一次成功(forwarding.py:344):
    // 即便上游回了 4xx,熔断计数也被清零。上面 `_request_with_fallback`
    // 内的 record_response 已按失败码计次,这里保持同样的落库终态。
    deps.breaker
        .record_success(&provider.id, Some(&provider), &breaker_config)
        .await;
    if let Some(store_key) = cache_key.as_deref() {
        deps.idempotency
            .complete(
                store_key,
                &request_hash(&payload, protocol),
                i64::from(response.status),
                &body,
                None,
            )
            .await;
    }
    Ok(ForwardJsonResult {
        status: response.status,
        body,
        headers: routing_headers,
        raw_body,
    })
}

/// 非流式转发 task 化 + 客户端断联检测(2026-10-08)。
///
/// handler 只持 oneshot 接收端等结果。客户端断开 → handler future 被
/// hyper 丢弃 → 接收端随之消失 → 工作任务的 `closed()` 分支被唤醒:
/// 取消转发(上游请求随 future 丢弃一并中止、并发许可随 drop 释放),
/// 并落一条 499 `client_disconnected` 终态行(`record_log` 是在途汇点,
/// 注册表条目随之取走)。此前的行为是 handler 被丢弃后一切静默 ——
/// 在途条目悬挂「处理中」最长 1h(stale 清理才消失),且请求无任何
/// 日志行。
pub fn spawn_forward_json(
    deps: Arc<GatewayDeps>,
    snapshot: Arc<ConfigSnapshot>,
    protocol: String,
    payload: Value,
    request_id: String,
    identity: GatewayIdentity,
    headers: Vec<(String, String)>,
    request_started: Instant,
    inbound_query: Option<String>,
    inbound_raw: Vec<u8>,
) -> tokio::sync::oneshot::Receiver<Result<ForwardJsonResult, ForwardError>> {
    let (mut result_tx, result_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        // 20261016_01 §2.6.3:断联分支与转发管线共用入口时刻。
        let started = request_started;
        // 断联分支的审计事实(模型名 + 脱敏预览)在 payload 被消费前取好;
        // callLogs 关闭时 preview 不构造(20261016_01 B-1)。
        // protocol 另存一份(select 的工作分支仍持有其借用)。
        let abort_protocol = protocol.clone();
        let model = payload
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let preview = if RequestLogRecorder::call_logs_enabled(&snapshot) {
            preview_for(&payload)
        } else {
            None
        };
        tokio::select! {
            result = forward_json(
                &deps,
                snapshot.clone(),
                &protocol,
                payload,
                headers,
                &request_id,
                &identity,
                started,
                inbound_query,
                inbound_raw,
            ) => {
                let _ = result_tx.send(result);
            }
            // 接收端先消失 = 客户端已断开(handler future 被 drop)。
            _ = result_tx.closed() => {
                tracing::info!(
                    target: "wrouter.gateway",
                    request_id = %request_id,
                    status = 499,
                    "gateway_request client_disconnected (non-stream, work cancelled)"
                );
                let mut extra = serde_json::Map::new();
                extra.insert("error".to_owned(), json!("client_disconnected"));
                deps.record_log_extra(
                    &snapshot,
                    &request_id,
                    &RequestLogEvent {
                        protocol: abort_protocol,
                        model,
                        status: 499,
                        stream: false,
                        duration_ms: elapsed_ms(started),
                        provider_attempts: Vec::new(),
                        retry_attempts: Vec::new(),
                        end_reason: "client_disconnected",
                        provider: None,
                        account: None,
                        api_key_id: Some(identity.key_id.clone()),
                        first_byte_ms: None,
                        error: Some("client_disconnected".to_owned()),
                    },
                    RequestLogExtra {
                        preview,
                        ..RequestLogExtra::default()
                    }
                    .with_identity(&identity.key_name)
                    .with_extra(extra),
                );
            }
        }
    });
    result_rx
}

/// 非流式三层回退(镜像 `_request_with_fallback`):
/// 计划 attempts → 候选链 → 同 provider 重试。返回 (响应, provider)。
/// 「从未触达 provider」的 502 审计行(镜像 forwarding.py 的 except 分支:
/// 先落 `no_provider` 行,再把异常外抛给 HTTP 层转 502)。
#[allow(clippy::too_many_arguments)]
pub(super) fn log_no_provider(
    deps: &GatewayDeps,
    snapshot: &Arc<ConfigSnapshot>,
    request_id: &str,
    protocol: &str,
    model: &str,
    identity: &GatewayIdentity,
    started: Instant,
    attempts: &[String],
    retry_attempts: Vec<Value>,
    message: &str,
    payload: &Value,
) {
    let mut no_provider = serde_json::Map::new();
    no_provider.insert("error".to_owned(), json!(message));
    deps.record_log_extra(
        snapshot,
        request_id,
        &RequestLogEvent {
            protocol: protocol.to_owned(),
            model: model.to_owned(),
            status: 502,
            stream: false,
            duration_ms: elapsed_ms(started),
            provider_attempts: attempts.to_vec(),
            retry_attempts,
            end_reason: "no_provider",
            provider: None,
            account: None,
            api_key_id: Some(identity.key_id.clone()),
            first_byte_ms: None,
            error: Some(message.to_owned()),
        },
        RequestLogExtra::default()
            // Python 的异常分支同样带 payload(→ preview)与密钥名。
            // callLogs 关闭时跳过 preview/capture 构造(20261016_01 B-1)。
            .with_identity(&identity.key_name)
            .with_preview(RequestLogRecorder::call_logs_enabled(snapshot), payload)
            .with_extra(no_provider)
            // 2026-10-14:失败行同样按捕获开关落请求正文(详情页排障)。
            .with_capture(
                RequestLogRecorder::capture_enabled(snapshot)
                    && RequestLogRecorder::call_logs_enabled(snapshot),
                deps.settings.body_capture_max_bytes,
                Some(payload),
                None,
                None,
            ),
    );
}

/// 非流式三层回退(镜像 `_request_with_fallback`):
/// 计划 attempts → 候选链 → 同 provider 重试。返回 [`ForwardSuccess`]
/// (响应 + provider + 命中账号 + 实际模型 + 代理来源)。
/// 「从未触达 provider」的 502 审计行(镜像 forwarding.py 的 except 分支:
/// 先落 `no_provider` 行,再把异常外抛给 HTTP 层转 502)。
#[allow(clippy::too_many_arguments)]
pub(super) async fn request_with_fallback(
    deps: &GatewayDeps,
    snapshot: &ConfigSnapshot,
    breaker_config: &CircuitBreakerConfig,
    request_id: &str,
    protocol: &str,
    payload: &mut Value,
    model: &str,
    attempts: &AttemptTrail,
    plan: &RouteExecutionPlan,
    route_target: Option<&RouteModelRef>,
    session_key: Option<&str>,
    resolved: &ResolvedSavings,
    // 20261016_03:客户端指纹源(路由改写前的原始入站头快照 + 原始 query)。
    fingerprint_inbound: &[(String, String)],
    inbound_query: Option<&str>,
    // 阶段四 C-4:请求级计时起点(出站时刻相对它计算 `sent_offset_ms`)。
    started: Instant,
    // P3-1:入站原始字节与其解析视图(原生请求面字节决策基线)。
    inbound_raw: &[u8],
    inbound_value: &Value,
) -> Result<ForwardSuccess, ForwardFailure> {
    let fallback_mode = plan.fallback.mode.as_str().to_owned();
    let mut last_error: Option<String> = None;
    let mut last_fallback_response: Option<ForwardSuccess> = None;
    // 20260905_04:聚合组(sticky 成功回写消费;单模型为 None)。
    let group = match route_target {
        Some(RouteModelRef::Aggregate(reference)) => Some(&reference.group),
        _ => None,
    };
    // P5-2:有状态引用绑定首次实际出站的供应商/账号;跨候选重试会
    // 把无法转移的 previous_response_id/会话引用发给另一上游。
    let stateful_reference = wr_domain::gateway::has_stateful_reference(payload);
    let mut stateful_origin: Option<(String, Option<String>)> = None;

    // P2-2(方案 20261015_01):请求级全局重试预算 —— 三层相乘
    //(计划 attempts × 候选数 × 同 provider 重试)没有总闸时最坏可到
    // 几十次上游调用、全程持有客户端连接。预算跨层共用,归零即停。
    let mut retry_budget = snapshot.config.reliability.retry_budget;
    let budget_total = retry_budget;
    let mut budget_exhausted = false;
    'plan: for plan_attempt in &plan.attempts {
        let attempt_model = plan_attempt
            .model
            .clone()
            .unwrap_or_else(|| model.to_owned());
        let attempt_target = plan_attempt.target.as_ref().or(route_target);
        if let Value::Object(map) = payload {
            map.insert("model".to_owned(), Value::String(attempt_model.clone()));
        }
        // 统一候选管道(20260905_04 §3.2):粘性/钉扎 > 聚合展开 >
        // select_provider,聚合候选每项带自己的实际模型名与注入账号。
        let (candidates, routing_error) = route_candidates_for_attempt(
            deps,
            snapshot,
            breaker_config,
            &attempt_model,
            attempt_target,
            resolved,
            session_key,
            payload,
        )
        .await;
        if candidates.is_empty() {
            // 模型无人服务(如 model-chain 首环未配置):推进下一 attempt;
            // 错误消息透传选路层(502 形态与升级前一致)。
            last_error = Some(routing_error.unwrap_or_else(|| "No provider available".to_owned()));
            continue;
        }

        for (candidate, account_id) in &candidates {
            let provider = &candidate.provider;
            if let Some((sent_provider, sent_account)) = &stateful_origin
                && (sent_provider != &provider.id || sent_account.as_ref() != account_id.as_ref())
            {
                last_error = Some(
                    "stateful reference cannot be retried against a different provider/account"
                        .to_owned(),
                );
                break 'plan;
            }
            // 每次尝试前把请求体 model 重写为该候选的实际模型(仅当与
            // 当前值不同;聚合混编/同名聚合的关键步,方案 §2「故障转移」)。
            if let Value::Object(map) = payload
                && map.get("model").and_then(Value::as_str) != Some(candidate.model.as_str())
            {
                map.insert("model".to_owned(), Value::String(candidate.model.clone()));
            }
            let candidate_model = candidate.model.as_str();
            let account_id = account_id.clone();
            if deps
                .breaker
                .is_open(&provider.id, Some(provider), breaker_config)
            {
                continue;
            }
            // Python 非流路径不检查 acquire.ok —— 历史对齐(R3,见 14 号
            // 文档)。20261016_01 A-3 改为与流式同语义:排队超时/拒绝时
            // 落一条排队尝试行并整体终止(429 concurrency_limited),
            // 不再置 None 后静默超发上游、放大 429 与重试。
            let queue_started = Instant::now();
            let queue_started_at = iso_now();
            let acquire = deps.concurrency.acquire(provider, || false).await;
            // 阶段四 C-4:并发槽位等待时长(拿到许可与被拒都算「等过」)。
            let queued_ms = elapsed_since(queue_started);
            let mut permit = match acquire {
                AcquireOutcome::Acquired(permit) => Some(permit),
                AcquireOutcome::Rejected {
                    status, message, ..
                } => {
                    trail_push(
                        attempts,
                        AttemptRecord {
                            attempt_id: attempt_id_for(&provider.id, 0),
                            provider: provider.id.clone(),
                            model: candidate_model.to_owned(),
                            account: account_id.clone(),
                            status,
                            error: Some(message.clone()),
                            duration_ms: elapsed_ms(queue_started),
                            wait_ms: 0.0,
                            wait_source: None,
                            // 阶段四 C-4:排队拒绝 = 等了但没发出去。
                            queued_ms,
                            sent_offset_ms: None,
                            started_at: queue_started_at,
                            upstream_request: None,
                            upstream_request_headers: None,
                            upstream_response: None,
                            upstream_headers: None,
                            conversion: None,
                            response_truncated: false,
                            response_size: None,
                        },
                    );
                    return Err(ForwardFailure::QueueRejected {
                        status,
                        message,
                        provider: Some(provider.id.clone()),
                        account: account_id.clone(),
                    });
                }
            };
            // 在途状态:拿到许可即进入处理中(等待中 = 准入/排队阶段)。
            if deps.in_flight.mark_processing(request_id) {
                deps.events
                    .publish(events::inflight_changed_event(request_id, "processing"));
            }

            // 客户端指纹(20261016_03):出站 URL query 与请求头按该候选
            // provider 的指纹配置构造(off/无 → 与旧路径逐字节一致)。
            let url = endpoint_with_query(
                protocol,
                provider,
                candidate_model,
                false,
                client_fingerprint::resolve_fingerprint_query(provider, inbound_query).as_deref(),
            );
            let upstream_head =
                upstream_headers_with_fingerprint(provider, Some(fingerprint_inbound));
            // 20260906_01 B4:代理按「供应商 × 目标 URL」决策(供应商可覆盖
            // 全局默认)。`client_for` 的缓存键已含代理决策,连接池天然按
            // 供应商×代理隔离(方案 §2.4),无需新增缓存机制。
            let proxy = GatewayDeps::proxy_decision_for(snapshot, provider, &url);
            let proxy_source = GatewayDeps::proxy_source_label(snapshot, &proxy);
            // P3-1:出站前固定执行计划(与流式同口径);规划拒绝在触达上游
            // 前推进下一候选,不退化为无约束透传。
            let exchange = match PreparedExchange::new(protocol, false, provider) {
                Ok(exchange) => exchange,
                Err(error) => {
                    last_error = Some(format!("protocol plan rejected: {error}"));
                    continue;
                }
            };
            deps.metrics.record_exchange_plan(
                exchange.plan.fidelity().as_str(),
                exchange.plan.request.mode.as_str(),
                exchange.plan.response.mode.as_str(),
                exchange.plan.profile.source.as_str(),
                exchange.plan.target.as_str(),
            );
            let byte_exact = exchange.plan.is_byte_exact();
            let conversion_started = Instant::now();
            let (outbound, conversion_report) =
                match upstream_payload_with_report(protocol, payload, provider) {
                    Ok(converted) => converted,
                    // B6:矩阵 unsupported 格(如 embeddings×anthropic)——
                    // 记录最后错误后推进下一候选;全部不支持时统一 wrouter_error。
                    Err(failure) => {
                        last_error = Some(failure.message().to_owned());
                        continue;
                    }
                };
            // P3-1:请求面字节决策 —— 无修改原字节;原生仅 model 差异定点
            // 替换;其余声明整体重序列化。
            let wire_bytes = request_wire_bytes(
                inbound_raw,
                inbound_value,
                &outbound,
                exchange.plan.request.mode,
            );
            // E1:转换策略/耗时/诊断类别进 metrics 与转换报告(只有类别
            // 与微秒数,不含 payload/secret)。
            let conversion_elapsed_us = conversion_started.elapsed().as_micros() as u64;
            record_conversion_metrics(
                deps,
                &conversion_report,
                protocol,
                provider,
                conversion_elapsed_us,
            );
            let max_retries = provider.max_retries;
            let mut attempt: u32 = 0;
            let mut terminal: Option<(UpstreamResponse, &Provider)> = None;
            // 阶段 D3:attempt id 序号(闭包只读,循环入口写)。
            let attempt_ordinal = Arc::new(std::sync::atomic::AtomicU32::new(0));
            // 阶段 D3:每次尝试的采集槽(captureBodies 开启时才有内容)。
            let capture = new_capture_slot();
            // 20261016_01 B-1:与非流式成功路径同口径 —— callLogs 关闭时
            // 尝试采集一并跳过。
            let capture_enabled = RequestLogRecorder::capture_enabled(&snapshot)
                && RequestLogRecorder::call_logs_enabled(&snapshot);
            let capture_budget = deps.settings.body_capture_max_bytes;
            if let Some(report) = &conversion_report {
                capture_with(&capture, |slot| slot.set_conversion(report));
            }
            capture_with(&capture, |slot| {
                slot.set_request(capture_enabled, capture_budget, &outbound)
            });
            // 上游请求头一并采集(脱敏后落库):排障需要看实际发出的
            // user-agent/指纹头;认证头由 redact_headers 打码。
            capture_with(&capture, |slot| {
                slot.set_request_headers(capture_enabled, &upstream_head)
            });
            // 阶段四 C-4:本次候选「是否已出站」标记(同一候选的重试轮
            // 共用一个原子量)与请求级计时起点(`push_record` 的 `started`
            // 形参是**尝试**起点,算偏移要用请求起点)。
            let attempt_sent = std::sync::atomic::AtomicBool::new(false);
            let request_started = started;
            // 结构化尝试明细(2026-10-08):每个尝试分支结束时落一条,
            // 详情页「重试尝试」表消费(等待时间/失败状态/错误/耗时)。
            let push_record = |status: u16,
                               error: Option<String>,
                               started: Instant,
                               started_at: &str,
                               wait: Option<(f64, &'static str)>| {
                let mut record = AttemptRecord {
                    attempt_id: attempt_id_for(
                        &provider.id,
                        attempt_ordinal.load(std::sync::atomic::Ordering::Relaxed),
                    ),
                    provider: provider.id.clone(),
                    model: candidate_model.to_owned(),
                    account: account_id.clone(),
                    status,
                    error,
                    duration_ms: elapsed_ms(started),
                    wait_ms: retry_wait_ms(wait),
                    wait_source: wait.as_ref().map(|(_, source)| *source),
                    // 阶段四 C-4:排队等待与出站偏移(未出站的尝试为 None)。
                    queued_ms,
                    sent_offset_ms: if attempt_sent.load(std::sync::atomic::Ordering::Relaxed) {
                        Some(offset_ms(request_started, started))
                    } else {
                        None
                    },
                    started_at: started_at.to_owned(),
                    upstream_request: None,
                    upstream_request_headers: None,
                    upstream_response: None,
                    upstream_headers: None,
                    conversion: None,
                    response_truncated: false,
                    response_size: None,
                };
                capture_with(&capture, |slot| slot.take_into(&mut record));
                trail_push(attempts, record);
            };
            while attempt <= max_retries {
                // P2-2:预算归零不再发起新的上游调用(跨计划/候选共用)。
                if retry_budget == 0 {
                    budget_exhausted = true;
                    break 'plan;
                }
                retry_budget -= 1;
                attempt_ordinal.store(attempt, std::sync::atomic::Ordering::Relaxed);
                let attempt_started = Instant::now();
                let attempt_started_at = iso_now();
                // 阶段四 C-4:即将出站 —— 标记本次尝试「已发出」。
                attempt_sent.store(true, std::sync::atomic::Ordering::Relaxed);
                if stateful_reference && stateful_origin.is_none() {
                    stateful_origin = Some((provider.id.clone(), account_id.clone()));
                }
                match deps
                    .upstream
                    .post_json_bytes(provider, &proxy, &url, &upstream_head, &wire_bytes)
                    .await
                {
                    Err(error) => {
                        log_upstream_http_failed(
                            provider,
                            attempt,
                            max_retries,
                            &url,
                            &error,
                            attempt_started,
                        );
                        deps.breaker
                            .record_failure(&provider.id, Some(provider), breaker_config)
                            .await;
                        last_error = Some(error.to_string());
                        // 等待时长与实际睡眠同源(P1-1:先算后睡,退避带抖动)。
                        let will_retry = should_retry(0, attempt, max_retries);
                        let wait = will_retry.then(|| retry_wait_now(None, attempt));
                        push_record(
                            0,
                            Some(last_error.clone().unwrap_or_default()),
                            attempt_started,
                            &attempt_started_at,
                            wait,
                        );
                        if will_retry {
                            // P2-1:退避期间让出并发许可(睡 2-30s 占死槽位
                            // 是重试放大器),睡醒重取 —— acquire 本就支持排队。
                            drop(permit);
                            sleep_wait(wait.expect("will_retry 必有 wait")).await;
                            permit = match deps.concurrency.acquire(provider, || false).await {
                                AcquireOutcome::Acquired(permit) => Some(permit),
                                // 20261016_01 A-3:重取被拒同样终止(对齐流式)。
                                AcquireOutcome::Rejected {
                                    status, message, ..
                                } => {
                                    push_record(
                                        status,
                                        Some(message.clone()),
                                        attempt_started,
                                        &attempt_started_at,
                                        None,
                                    );
                                    return Err(ForwardFailure::QueueRejected {
                                        status,
                                        message,
                                        provider: Some(provider.id.clone()),
                                        account: account_id.clone(),
                                    });
                                }
                            };
                            attempt += 1;
                            continue;
                        }
                        break;
                    }
                    Ok(response) => {
                        // 非流面上游按 JSON 解码;SSE 不是可聚合表示(W2-2),
                        // 因此这里不做流→JSON 转换。此处的 response 保留
                        // 上游原文,D3 捕获到的即是真实上游字节。
                        // 阶段 D3:本次尝试的上游响应体/头(仅采集开启时)。
                        capture_with(&capture, |slot| {
                            slot.set_headers(capture_enabled, &response.headers);
                            slot.set_response(
                                capture_enabled,
                                capture_budget,
                                &String::from_utf8_lossy(&response.body),
                            );
                        });
                        // K3:主候选命中账号时,以最终状态码记录凭据结果
                        // (分档冷却 + Retry-After;fallback 候选不记录)。
                        if let Some(account_id) = &account_id {
                            let status = i64::from(response.status);
                            let body_text = (status == 429)
                                .then(|| String::from_utf8_lossy(&response.body).into_owned());
                            let retry_after_ms = credential_retry_after_ms(
                                status,
                                response.header("retry-after"),
                                body_text.as_deref(),
                            );
                            if response.status >= 400 {
                                // 阶段五 C-2:按**语义类别**记冷却(与流式同
                                // 口径)—— 配额类与瞬时限速分开,作用域随类别分。
                                let error_body = parse_error_body(&response.body);
                                let upstream_error =
                                    classify_upstream_error(response.status, &error_body);
                                let rest_class = wr_domain::rest::classify_rest_class(
                                    response.status,
                                    &String::from_utf8_lossy(&response.body),
                                    &upstream_error.error_type,
                                    upstream_error.code.as_deref(),
                                );
                                deps.credentials.record_outcome_classified(
                                    &provider.id,
                                    account_id,
                                    Some(candidate_model),
                                    status,
                                    wr_domain::rest::FailureSignal::new(rest_class, retry_after_ms),
                                );
                            } else {
                                // 成功:清冷却 + 复位连续失败计数(既有语义不变)。
                                deps.credentials.record_outcome_classified(
                                    &provider.id,
                                    account_id,
                                    Some(candidate_model),
                                    status,
                                    wr_domain::rest::FailureSignal::default(),
                                );
                            }
                        }
                        if response.status < 400 {
                            deps.breaker
                                .record_success(&provider.id, Some(provider), breaker_config)
                                .await;
                            push_record(
                                response.status,
                                None,
                                attempt_started,
                                &attempt_started_at,
                                None,
                            );
                            terminal = Some((response, provider));
                            break;
                        }
                        deps.breaker
                            .record_response(
                                &provider.id,
                                response.status,
                                Some(provider),
                                breaker_config,
                            )
                            .await;
                        // S7:上下文超限(400 正文命中方言)→ 不重试同模型,
                        // 推进下一候选/计划 attempt;响应保留为最终兜底,
                        // 全链失败时仍回到该响应(与 400 终态一致)。
                        if response.status == 400
                            && wr_domain::gateway::is_context_overflow_body(
                                &String::from_utf8_lossy(&response.body),
                            )
                        {
                            last_error = Some(
                                "upstream context overflow (estimated tokens exceed model window)"
                                    .to_owned(),
                            );
                            push_record(
                                response.status,
                                last_error.clone(),
                                attempt_started,
                                &attempt_started_at,
                                None,
                            );
                            last_fallback_response = Some(ForwardSuccess {
                                response,
                                provider: provider.clone(),
                                hit_account: account_id.clone(),
                                actual_model: candidate_model.to_owned(),
                                proxy_source: proxy_source.clone(),
                                byte_exact,
                            });
                            break;
                        }
                        if NO_RETRY_STATUS_CODES.contains(&response.status) {
                            // P1-3:401/403 是凭据类失败(账号 key 失效/无
                            // 权限)—— 凭据池已记 300s 冷却,当前请求内推进
                            // 下一候选(聚合组下一目标等),全候选耗尽时回本
                            // 响应原样透传(与终态一致)。404/422 是模型不
                            // 存在/请求形状错误,换候选无意义,保持终态。
                            if CREDENTIAL_FAILURE_STATUS_CODES.contains(&response.status) {
                                last_error = Some(format!(
                                    "upstream status {} (credential failure, advancing candidates)",
                                    response.status
                                ));
                                push_record(
                                    response.status,
                                    last_error.clone(),
                                    attempt_started,
                                    &attempt_started_at,
                                    None,
                                );
                                last_fallback_response = Some(ForwardSuccess {
                                    response,
                                    provider: provider.clone(),
                                    hit_account: account_id.clone(),
                                    actual_model: candidate_model.to_owned(),
                                    proxy_source: proxy_source.clone(),
                                    byte_exact,
                                });
                                break;
                            }
                            push_record(
                                response.status,
                                None,
                                attempt_started,
                                &attempt_started_at,
                                None,
                            );
                            terminal = Some((response, provider));
                            break;
                        }
                        let retry_after_header = response.header("retry-after");
                        // P1-2:429 带 >60s Retry-After —— 原地等满 clamp 的
                        // 60s 再打同一把刚被限流的 key 大概率再 429(凭据池
                        // 已按 Retry-After 全值冷却该账号),直接推进下一候选。
                        let skip_inplace_retry = response.status == 429
                            && wr_domain::gateway::retry_after_exceeds_cap(retry_after_header);
                        let will_retry = !skip_inplace_retry
                            && should_retry(response.status, attempt, max_retries);
                        // P1-1:先算后睡,落库 wait_ms 与实际等待同源。
                        let wait = will_retry.then(|| retry_wait_now(retry_after_header, attempt));
                        let error_summary = if skip_inplace_retry {
                            let seconds =
                                wr_domain::gateway::parse_retry_after_ms(retry_after_header)
                                    .map(|ms| (ms / 1000.0) as i64)
                                    .unwrap_or(0);
                            format!(
                                "upstream status 429 (Retry-After {seconds}s exceeds 60s cap, advancing candidates)"
                            )
                        } else {
                            format!("upstream status {}", response.status)
                        };
                        push_record(
                            response.status,
                            Some(error_summary.clone()),
                            attempt_started,
                            &attempt_started_at,
                            wait,
                        );
                        if will_retry {
                            // P2-1:退避期间让出并发许可,睡醒重取。
                            drop(permit);
                            sleep_wait(wait.expect("will_retry 必有 wait")).await;
                            permit = match deps.concurrency.acquire(provider, || false).await {
                                AcquireOutcome::Acquired(permit) => Some(permit),
                                // 20261016_01 A-3:重取被拒同样终止(对齐流式)。
                                AcquireOutcome::Rejected {
                                    status, message, ..
                                } => {
                                    push_record(
                                        status,
                                        Some(message.clone()),
                                        attempt_started,
                                        &attempt_started_at,
                                        None,
                                    );
                                    return Err(ForwardFailure::QueueRejected {
                                        status,
                                        message,
                                        provider: Some(provider.id.clone()),
                                        account: account_id.clone(),
                                    });
                                }
                            };
                            attempt += 1;
                            continue;
                        }
                        if should_fallback_mode(response.status, &fallback_mode) {
                            last_error = Some(error_summary);
                            last_fallback_response = Some(ForwardSuccess {
                                response,
                                provider: provider.clone(),
                                hit_account: account_id.clone(),
                                actual_model: candidate_model.to_owned(),
                                proxy_source: proxy_source.clone(),
                                byte_exact,
                            });
                            break;
                        }
                        terminal = Some((response, provider));
                        break;
                    }
                }
            }
            drop(permit);
            if let Some((response, provider)) = terminal {
                // 成功回写(20260905_03 S2):仅最终 status<400 时写粘性/
                // 钉扎绑定(「只由成功响应写入」;account None 也绑 provider)。
                if response.status < 400 {
                    // 阶段五 C-1:粘性条目附加 cache 观测 —— 本次响应的
                    // 上游 usage(归一化后取 cache 命中/写入 token)。
                    // 上游 body 是原始字节,解析一次取 `usage`;Gemini 上游
                    // 的 usageMetadata 不在 `usage` 键下 → 取不到即 0,
                    // 而 Gemini 协议族不参与「值不值」判据,不影响正确性。
                    let upstream_usage = serde_json::from_slice::<Value>(&response.body)
                        .ok()
                        .map(|body| wr_domain::usage::normalize_usage(body.get("usage")))
                        .unwrap_or_else(wr_domain::usage::Usage::empty);
                    bind_savings_targets(
                        deps,
                        resolved,
                        session_key,
                        payload,
                        provider,
                        account_id.as_deref(),
                        &attempt_model,
                        &upstream_usage,
                    );
                    // 20260905_04:聚合 sticky 成功回写(键空间独立)。
                    bind_group_sticky(
                        deps,
                        group,
                        session_key,
                        provider,
                        account_id.as_deref(),
                        candidate_model,
                    );
                }
                return Ok(ForwardSuccess {
                    response,
                    provider: provider.clone(),
                    hit_account: account_id,
                    actual_model: candidate_model.to_owned(),
                    proxy_source,
                    byte_exact,
                });
            }
            // 候选内重试耗尽 → 下一候选(镜像候选链推进)。
        }
        // provider 候选链全部耗尽且无终态响应:由执行计划决定是否再来一轮。
    }

    if budget_exhausted {
        last_error = Some(format!(
            "retry budget exhausted after {budget_total} upstream attempts (reliability.retryBudget={budget_total})"
        ));
    }
    if let Some(success) = last_fallback_response {
        return Ok(success);
    }
    Err(ForwardFailure::Error(ForwardError {
        message: last_error.unwrap_or_else(|| "No provider available".to_owned()),
        attempts: trail_provider_ids(attempts),
    }))
}
