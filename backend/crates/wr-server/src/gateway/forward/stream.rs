// 流式转发(relay 管线)
// 20261013_01 C1 自 forward.rs 纯搬移;经 `use super::*` 复用共享类型。
// 20261106_01 P2:无损分帧 + 共享源事实 + 提交/重试边界。

use super::delivery::{Delivery, DeliveryError, EmissionBatch, StreamHandshake};
use super::prepared::{AttemptContext, PreparedExchange};
use super::*;
use wr_domain::protocol::contract::{BodyMode, ResponseExecutor};
use wr_domain::semantic::client_stream::{ClientStream, build_client_stream};
use wr_domain::semantic::source::SourceObserver;
use wr_domain::semantic::stream::StreamAggregator;
use wr_domain::wire::error::WireLimits;
use wr_domain::wire::record::{RawRecord, RecordSource, RecordView};
use wr_domain::wire::{RecordFramer, ndjson::NdjsonFramer, sse::SseFramer};

use super::cache_replay::replay_cached_response;
use super::record_encoder::RecordEncoder;
use super::stream_control::{continuation_gate, record_stream_client_gone};

/// 流式管线的 HTTP 握手 + 正文通道。
pub struct StreamSession {
    pub handshake: tokio::sync::oneshot::Receiver<StreamHandshake>,
    pub body: mpsc::Receiver<Result<Vec<u8>, std::convert::Infallible>>,
}

// ---------------------------------------------------------------------------
// 流式转发
// ---------------------------------------------------------------------------

/// 流式转发:把上游 SSE 经 relay 转换后写入通道。
/// 返回接收端流(axum Body 来源);管线在 spawned task 中执行。
#[allow(clippy::too_many_arguments)]
pub fn spawn_forward_stream(
    deps: Arc<GatewayDeps>,
    snapshot: Arc<ConfigSnapshot>,
    protocol: String,
    mut payload: Value,
    request_id: String,
    identity: GatewayIdentity,
    mut headers: Vec<(String, String)>,
    client_disconnected: Arc<std::sync::atomic::AtomicBool>,
    request_started: Instant,
    // 20261016_03:入站原始 query(指纹源,与非流式同口径)。
    inbound_query: Option<String>,
    // P3-1:入站原始正文字节(原生请求面 ByteExact 的基础)。
    inbound_raw: Vec<u8>,
) -> StreamSession {
    let (tx, rx) = mpsc::channel::<Result<Vec<u8>, std::convert::Infallible>>(8);
    let (handshake_tx, handshake_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let mut delivery = Delivery::new(&tx, handshake_tx);
        let result = run_stream_pipeline(
            &deps,
            snapshot,
            &protocol,
            &mut payload,
            &mut headers,
            &request_id,
            &identity,
            &client_disconnected,
            &mut delivery,
            request_started,
            inbound_query,
            inbound_raw,
        )
        .await;
        if let Err(error) = result
            && error.client_gone
        {
            // 通道关闭 = 客户端断开(镜像 Python 499 记录)。
            tracing::info!(
                target: "wrouter.gateway",
                request_id = %request_id,
                status = 499,
                "gateway_request client_disconnected",
            );
        }
    });
    StreamSession {
        handshake: handshake_rx,
        body: rx,
    }
}

/// 流式管线内部错误:携带"客户端是否已断开"信息。
pub struct StreamPipelineError {
    pub(super) client_gone: bool,
}

/// 流式排队拒绝/排队中断的统一终态处理(20261015_01 P2-1:首取与退避后
/// 重取共用;尝试行由调用侧先行落库):落调用日志行,断联直接返回,
/// 否则发 `concurrency_limited` 信封。调用方随后终止管线(return)。
#[allow(clippy::too_many_arguments)]
async fn handle_stream_queue_rejected(
    deps: &GatewayDeps,
    snapshot: &Arc<ConfigSnapshot>,
    request_id: &str,
    protocol: &str,
    model: &str,
    identity: &GatewayIdentity,
    started: Instant,
    attempts: &AttemptTrail,
    payload: &Value,
    provider: &Provider,
    account_id: &Option<String>,
    headers: &[(String, String)],
    delivery: &mut Delivery<'_>,
    status: u16,
    message: &str,
    was_aborted: bool,
    // 20261016_01 A-1:首字耗时在首字发生时刻一次算定,在途与终态共用
    // 同一值(终态不再经 elapsed_since 重算 —— 旧实现把「首字后剩余时长」
    // 记成了 first_byte_ms,吞吐随之系统性虚高)。
    first_byte_ms: Option<f64>,
) {
    let end_reason = if was_aborted {
        "client_disconnected"
    } else {
        "queue_timeout"
    };
    deps.record_log_extra(
        snapshot,
        request_id,
        &RequestLogEvent {
            protocol: protocol.to_owned(),
            model: model.to_owned(),
            status,
            stream: true,
            duration_ms: elapsed_ms(started),
            provider_attempts: trail_provider_ids(attempts),
            retry_attempts: trail_values(attempts),
            end_reason,
            provider: Some(provider.id.clone()),
            account: account_id.clone(),
            api_key_id: Some(identity.key_id.clone()),
            first_byte_ms,
            error: Some(message.to_owned()),
        },
        failure_log_extra(deps, snapshot, identity, headers, payload, None, None),
    );
    if was_aborted {
        return;
    }
    let envelope = json!({
        "error": {"type": "concurrency_limited", "message": message, "code": "concurrency_limited"}
    });
    let _ = delivery.error(protocol, status, &envelope).await;
}

/// 提交前失败的协议错误:返回真实 HTTP 状态 + 协议错误体。
async fn reject_upstream_error(
    delivery: &mut Delivery<'_>,
    protocol: &str,
    status: u16,
    message: &str,
    attempts: Option<&[String]>,
) -> Result<(), StreamPipelineError> {
    let upstream_error = classify_upstream_error(
        status,
        &json!({"error": {"message": message, "type": "upstream_error"}}),
    );
    let (_, envelope) = format_error_for_protocol(protocol, &upstream_error, attempts);
    delivery
        .error(protocol, status, &envelope)
        .await
        .map_err(|_| StreamPipelineError {
            client_gone: delivery.client_gone(),
        })
}


fn now_epoch_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// 首个内容帧**成功入队后**才记首字;信封帧不计。
fn note_first_byte(
    deps: &GatewayDeps,
    request_id: &str,
    started: Instant,
    first_byte_ms: &mut Option<f64>,
    delivery: &Delivery<'_>,
) {
    if first_byte_ms.is_some() || !delivery.content_committed() {
        return;
    }
    let ttft_ms = elapsed_ms(started);
    *first_byte_ms = Some(ttft_ms);
    if deps.in_flight.mark_first_byte(request_id, ttft_ms) {
        deps.events
            .publish(events::inflight_changed_event(request_id, "first-byte"));
    }
}

/// 20261108_01 §4.1:答案完成里程碑。首次观察到语义 `Finish` 事实时冻结
/// 请求耗时、把在途计时停在答案完成;返回是否本次首次记录(调用方随即
/// 释放并发许可)。
fn note_answer_end(
    deps: &GatewayDeps,
    request_id: &str,
    started: Instant,
    observer: &SourceObserver,
    answer_end: &mut Option<Instant>,
    answer_end_ms: &mut Option<f64>,
) -> bool {
    if answer_end.is_some() || !observer.saw_finish() {
        return false;
    }
    let now = Instant::now();
    let frozen = round_ms(offset_ms(started, now));
    *answer_end = Some(now);
    *answer_end_ms = Some(frozen);
    if deps.in_flight.mark_answer_end(request_id, frozen) {
        deps.events
            .publish(events::inflight_changed_event(request_id, "answer-end"));
    }
    true
}

fn round_ms(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// 心跳记录(SSE 注释行 / NDJSON 空行)不算上游活动:20261108_01 §4.2
/// 的空闲超时必须能穿透「只发 ping、永不发终态帧」的挂连接。
fn record_is_heartbeat(record: &RawRecord) -> bool {
    match record.view() {
        RecordView::Sse(view) => {
            view.data.is_empty() && view.fields.is_empty() && !view.comments.is_empty()
        }
        RecordView::Ndjson(view) => view.blank,
        _ => false,
    }
}

/// 20261108_01 §5.2:答案段计时快照(日志 payload 键;有值才写)。
fn answer_timing(
    started: Instant,
    answer_end: Option<Instant>,
    pump: &ClientStream,
    stream_end: Instant,
) -> AnswerTiming {
    AnswerTiming {
        answer_end_ms: answer_end.map(|end| round_ms(offset_ms(started, end))),
        first_answer_byte_ms: pump
            .answer_origin()
            .map(|origin| round_ms(offset_ms(started, origin))),
        answer_flow_ms: pump.answer_flow_ms().map(round_ms),
        reasoning_decode_ms: pump.reasoning_flow_ms().map(round_ms),
        reasoning_content_bytes: pump.reasoning_content_bytes().map(|bytes| bytes as i64),
        stream_tail_ms: answer_end.map(|end| round_ms(offset_ms(end, stream_end))),
        reasoning_observed: pump.saw_reasoning(),
    }
}


#[allow(clippy::too_many_arguments)]
pub(super) async fn run_stream_pipeline(
    deps: &Arc<GatewayDeps>,
    snapshot: Arc<ConfigSnapshot>,
    protocol: &str,
    payload: &mut Value,
    // 20261101_02 T3:规则改写删除后管线内不再变更 headers,收窄为切片。
    headers: &mut [(String, String)],
    request_id: &str,
    identity: &GatewayIdentity,
    client_disconnected: &Arc<std::sync::atomic::AtomicBool>,
    delivery: &mut Delivery<'_>,
    // 20261016_01 §2.6.3:入口传入的统一计时起点(替代管线内 Instant::now)。
    started: Instant,
    // 20261016_03:入站原始 query(passthrough allowlist / preset 静态
    // query 的指纹源;None = 无指纹语境)。
    inbound_query: Option<String>,
    // P3-1:入站原始正文字节(原生请求面 ByteExact 的基础)。
    inbound_raw: Vec<u8>,
) -> Result<(), StreamPipelineError> {
    let attempts = new_trail();
    // 原始正文解析视图(与 inbound_raw 同一原文;路由/压缩改写前固定)。
    let inbound_value: Value =
        serde_json::from_slice(&inbound_raw).unwrap_or_else(|_| payload.clone());
    let aborted = || client_disconnected.load(std::sync::atomic::Ordering::Relaxed);

    // K4:会话键提取(非流式同构;在路由引擎改写前从原始请求提取,
    // 只随调用链传递到选号点,不落库)。
    let session_key = wr_domain::gateway::extract_session_key(headers, payload.as_object());

    // 客户端指纹源(20261016_03):与非流式同构 —— 路由引擎改写 headers
    // 之前取原始入站快照;仅随管线存活,不落库。
    let fingerprint_inbound = headers.to_vec();

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

    if let Some(message) = &snapshot.provider_error {
        return reject_upstream_error(delivery, protocol, 502, message, None).await;
    }
    let breaker_config = snapshot.config.reliability.circuit_breaker.clone();
    // 20261108_01 §4.2:流收尾的两个时间界 —— 答案完成后的有界排空
    // (`Finish` → 终态帧之间的拖尾)与连续无任何上游记录的空闲上限。
    let terminal_grace =
        Duration::from_millis(snapshot.config.reliability.stream_terminal_grace_ms);
    let idle_timeout =
        Duration::from_millis(snapshot.config.reliability.stream_idle_timeout_ms);
    deps.concurrency
        .sync_config(&snapshot.config.reliability.concurrency);

    // provider/model 前缀解析(镜像 forward_stream)。
    let (client_model, client_route_target) = resolve_client_prefix(&snapshot, &model);
    if client_route_target.is_some() {
        payload["model"] = Value::String(client_model.clone());
        model = client_model;
    }
    // 路由(20261101_02 T3:规则域下线,目标只来自客户端显式前缀)。
    let route_target = client_route_target.clone();
    // 调用契约(2026-09-26,与非流式同构):未解析选择器直接 SSE 400。
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
                stream: true,
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
            failure_log_extra(deps, &snapshot, identity, headers, payload, None, None),
        );
        let _ = reject_upstream_error(delivery, protocol, 400, &message, None).await;
        return Ok(());
    }
    let plan = build_fallback_plan(&snapshot, &model);
    // 20260905_04:生效目标(引擎/显式前缀缺失时回退 plan 首 attempt 解析,
    // 与非流式同口径);聚合标记驱动响应回写/缓存门控/日志维度。组定义按值
    // 持有(plan 随后被 context_prefilter_plan 消费,借用无法存活)。
    let effective_aggregate = match route_target.as_ref().or_else(|| {
        plan.attempts
            .first()
            .and_then(|attempt| attempt.target.as_ref())
    }) {
        Some(RouteModelRef::Aggregate(reference)) => Some(reference.group.clone()),
        _ => None,
    };
    let is_aggregate_model = effective_aggregate.is_some();
    // 聚合规范名(relay/合成帧回写与日志维度用;`model` 会被逐候选重写)。
    let aggregate_name = effective_aggregate.as_ref().map(|group| group.name.clone());
    // 聚合组借用(sticky 成功回写消费;单模型为 None)。
    let group_ref = effective_aggregate.as_ref();
    // 20260905_03:全局节省动作解析(20261101_02 T3-B:规则层已删)。
    let resolved_savings = resolve_savings(&snapshot.config.savings);
    // S4/S5:token 压缩管线(与非流式同构;auto 档按估算 token 阈值触发,
    // full 恒进;off → None)。
    let compression_report = wr_domain::compression::compress_request_body(
        payload,
        resolved_savings.compression_tier,
        snapshot.config.savings.compression.threshold_tokens,
    );
    let savings_extra = compression_extra_map(&compression_report);

    // S7:上下文预检(与非流式同构;全部装不下 → SSE 明确错误)。
    let estimated_tokens = wr_domain::compression::estimate_tokens(payload);
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
                stream: true,
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
            failure_log_extra(deps, &snapshot, identity, headers, payload, None, None),
        );
        let envelope = json!({
            "error": {"type": "context_too_long", "message": message, "code": "context_too_long"}
        });
        let _ = delivery.error(protocol, 400, &envelope).await;
        return Ok(());
    }

    // --- API key 模型白名单(20261104_01 阶段二 B-4)---------------------
    // 与非流式同序(白名单 → precheck → 访问控制),同批接线,避免中间态
    // 出现「先接 precheck、白名单仍可绕过」。流式此前完全没接这条判定
    // → per-key 模型白名单在流式请求上可被绕过。
    // 拒绝只能走 SSE 信封(响应头已发,HTTP 状态恒 200)。
    if let Some(message) = api_key_model_error(&snapshot, identity, &client_requested_model)
    {
        deps.record_log_extra(
            &snapshot,
            request_id,
            &RequestLogEvent {
                protocol: protocol.to_owned(),
                model: model.clone(),
                status: 403,
                stream: true,
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
            failure_log_extra(deps, &snapshot, identity, &headers, payload, None, None),
        );
        let envelope = json!({
            "error": {"type": "model_forbidden", "message": message, "code": "model_forbidden"}
        });
        let _ = delivery.error(protocol, 403, &envelope).await;
        return Ok(());
    }

    // --- precheck 限流 / 预算(20261104_01 阶段二 A-1)-------------------
    // 与非流式同序(json.rs:白名单 → precheck → 访问控制),且必须早于
    // 候选循环 —— 否则会先打上游再拒。流式此前只有一句「默认放行」的
    // 注释 → per-key 的 rpm/rph/rpd/tpm/tph/tpd 在流式请求上完全不生效,
    // 而流式正是 AI 编程 agent 的主场景。
    // 拒绝走 SSE 信封(同 B-4:响应头已发,HTTP 状态恒 200)。
    let extra_rules =
        limits::api_key_limits_to_rules(api_key_limits(&snapshot, &identity.key_id).as_ref());
    if let limits::PrecheckResult::Rejected(failure) = deps.prechecker.evaluate(
        &limits::extract_identity(headers),
        Some(estimate_usage(payload)),
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
                stream: true,
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
                ..failure_log_extra(deps, &snapshot, identity, &headers, payload, None, None)
            },
        );
        let envelope = failure.to_body();
        let _ = delivery.error(protocol, failure.status, &envelope).await;
        return Ok(());
    }

    // --- 访问控制策略(A3):deny 经 SSE 信封下发 --------------------------
    let principal = access_policy::extract_principal(headers);
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
                stream: true,
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
            failure_log_extra(deps, &snapshot, identity, headers, payload, None, None),
        );
        let body = access_policy::policy_denied_body(&decision);
        let _ = delivery.error(protocol, 403, &body).await;
        return Ok(());
    }

    // --- S6:响应缓存精确键命中(20260905_03 §4.4;流式合成 SSE 重放)---
    // 门控与非流式同构;命中帧经 synthesis 逐帧下发,计费 $0。
    let cache_bypass = headers.iter().any(|(key, value)| {
        key.eq_ignore_ascii_case("x-wr-no-cache") && value.trim().eq_ignore_ascii_case("true")
    });
    // 20260905_04:聚合请求不进响应缓存(与非流式同口径)。
    let response_cache_key =
        if resolved_savings.response_cache && !cache_bypass && !is_aggregate_model {
            wr_domain::gateway::response_cache_key(protocol, payload)
        } else {
            None
        };
    if replay_cached_response(
        deps,
        &snapshot,
        protocol,
        payload,
        &model,
        request_id,
        identity,
        headers,
        delivery,
        started,
        &attempts,
        &savings_extra,
        response_cache_key.as_deref(),
    )
    .await?
    {
        return Ok(());
    }

    // --- 计划 attempts → 候选链 → 同 provider 重试(C4) -------------------
    // 注:流式路径不做 should_fallback_mode 门控(镜像 Python)——
    // 候选耗尽即推进下一计划 attempt,失败状态直接走信封/中断分支。
    let mut last_error: Option<String> = None;
    // 全候选耗尽时的真实失败状态(协议规划 400 / 响应表示 502 / 默认 502)。
    let mut last_status: Option<u16> = None;
    let mut success = false;
    // P2-2(方案 20261015_01):请求级全局重试预算(与非流式同语义)。
    let mut retry_budget = snapshot.config.reliability.retry_budget;
    let budget_total = retry_budget;
    // P1-3:凭据类失败(401/403)推进候选时记录的真实握手错误 —— 全候选
    // 耗尽后以真实状态码/错误体回信封(401 不吞成 502)。
    let mut last_handshake_error: Option<(
        u16,
        wr_domain::protocol::UpstreamError,
        String,
        Map<String, Value>,
    )> = None;

    // 阶段五 C-3:断流续写只允许一次(请求级)—— 防止「续写又断流 →
    // 再续写」的循环(每次续写都要多花一次上游额度)。
    let mut continuation_attempted = false;
    // P2-5:有状态引用绑定原上游身份;一旦实际出站,不得跨候选盲试。
    let stateful_reference = wr_domain::gateway::has_stateful_reference(payload);
    let mut stateful_attempted = false;

    'plan: for plan_attempt in &plan.attempts {
        let attempt_model = plan_attempt
            .model
            .clone()
            .unwrap_or_else(|| model.to_owned());
        let attempt_target = plan_attempt.target.as_ref().or(route_target.as_ref());
        if let Value::Object(map) = payload {
            map.insert("model".to_owned(), Value::String(attempt_model.clone()));
        }
        // 统一候选管道(20260905_04 §3.2;与非流式同构):粘性/钉扎 >
        // 聚合展开 > select_provider,聚合候选每项带自己的实际模型名。
        let (candidates, routing_error) = route_candidates_for_attempt(
            deps,
            &snapshot,
            &breaker_config,
            &attempt_model,
            attempt_target,
            &resolved_savings,
            session_key.as_deref(),
            payload,
        )
        .await;
        if candidates.is_empty() {
            // 该 attempt 的模型无人服务(如 model-chain 首环):推进;
            // 错误消息透传选路层。
            last_error =
                Some(routing_error.unwrap_or_else(|| "No streaming provider available".to_owned()));
            continue 'plan;
        }

        for (candidate, candidate_account) in &candidates {
            if stateful_reference && stateful_attempted {
                last_error = Some(
                    "stateful reference cannot be retried against a different provider/account"
                        .to_owned(),
                );
                last_status = Some(502);
                break 'plan;
            }
            let provider = &candidate.provider;
            // 每次尝试前把请求体 model 重写为该候选的实际模型(方案 §2)。
            if let Value::Object(map) = payload
                && map.get("model").and_then(Value::as_str) != Some(candidate.model.as_str())
            {
                map.insert("model".to_owned(), Value::String(candidate.model.clone()));
            }
            let candidate_model = candidate.model.as_str();
            // 账号 id 随候选携带(聚合候选逐个注入;单模型路径 fallback
            // 候选未注入,为 None)。
            let account_id = candidate_account.clone();
            // 候选级计时与结构化明细落点(排队等待也计入尝试耗时)。
            let candidate_started = Instant::now();
            let candidate_started_at = iso_now();
            // 阶段四 C-4:本候选的「并发槽位等待」与「是否已出站」。
            // 两者都要被下面的 `push_record` 闭包读到,而闭包在 acquire
            // **之前**就建好 → 用原子量(闭包持有共享引用,管线跑在
            // spawned task 里,future 必须 Send;`Cell` 不是 Sync,会让
            // future 不可 Send)。排队时长按厘秒存整数(落库两位小数)。
            let queue_wait_cms = std::sync::atomic::AtomicU64::new(0);
            let attempt_sent = std::sync::atomic::AtomicBool::new(false);
            // 请求级计时起点:`push_record` 的 `started` 形参是**尝试**
            // 起点,算「出站相对请求起点的偏移」要用请求起点。
            let request_started = started;
            // 阶段 D3:attempt id 的序号。用原子量是因为 `push_record`
            // 闭包与下面的重试循环都要读它,而重试循环会推进 `attempt`;
            // 循环每轮入口把当前值写进去,闭包只读当前值。
            let candidate_attempt = Arc::new(std::sync::atomic::AtomicU32::new(0));
            // 阶段 D3:每个候选一次采集槽(流式尝试的 SSE 正文在
            // record_log_extra 侧单独捕获,这里带转换报告与上游请求体)。
            let capture = new_capture_slot();
            // 20261016_01 B-1:采集开关与调用日志门取合 —— callLogs 关闭时
            // 行不落库,尝试采集/正文捕获/trace 序列化全部跳过。
            let logs_enabled = RequestLogRecorder::call_logs_enabled(&snapshot);
            let capture_enabled = RequestLogRecorder::capture_enabled(&snapshot) && logs_enabled;
            let capture_budget = deps.settings.body_capture_max_bytes;
            let push_record = |status: u16,
                               error: Option<String>,
                               started: Instant,
                               started_at: &str,
                               wait: Option<(f64, &'static str)>| {
                let mut record = AttemptRecord {
                    attempt_id: attempt_id_for(
                        &provider.id,
                        candidate_attempt.load(std::sync::atomic::Ordering::Relaxed),
                    ),
                    provider: provider.id.clone(),
                    model: candidate_model.to_owned(),
                    account: account_id.clone(),
                    status,
                    error,
                    duration_ms: elapsed_ms(started),
                    wait_ms: retry_wait_ms(wait),
                    wait_source: wait.map(|(_, source)| source),
                    // 阶段四 C-4:排队等待与出站偏移(未出站的尝试为 None)。
                    queued_ms: queue_wait_cms.load(std::sync::atomic::Ordering::Relaxed) as f64
                        / 100.0,
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
                trail_push(&attempts, record);
            };
            if deps
                .breaker
                .is_open(&provider.id, Some(provider), &breaker_config)
            {
                push_record(
                    0,
                    Some("circuit breaker open".to_owned()),
                    candidate_started,
                    &candidate_started_at,
                    None,
                );
                continue;
            }
            // 阶段四 C-4:并发槽位等待计时起点(排队时长从这里算)。
            let queue_started = Instant::now();
            let acquire = deps.concurrency.acquire(provider, aborted).await;
            // 排队等待时长(拿到许可与被拒都算「等过」—— 被拒的那次
            // duration_ms 本就是排队等待时长)。
            queue_wait_cms.store(
                (elapsed_since(queue_started) * 100.0).round() as u64,
                std::sync::atomic::Ordering::Relaxed,
            );
            // 在途状态:拿到许可即进入处理中(等待中 = 准入/排队阶段)。
            if deps.in_flight.mark_processing(request_id) {
                deps.events
                    .publish(events::inflight_changed_event(request_id, "processing"));
            }
            // 20261108_01 §4.1:许可用 Option 持有 —— 答案完成(观察到
            // `Finish`)即提前释放,其后协议排空不再空占槽位;终态路径的
            // `take()` 对已释放许可是无操作。
            let mut permit = match acquire {
                AcquireOutcome::Acquired(permit) => Some(permit),
                AcquireOutcome::Rejected {
                    status,
                    message,
                    aborted: was_aborted,
                } => {
                    // 排队拒绝/排队中断:duration_ms 即排队等待时长。
                    push_record(
                        status,
                        Some(message.clone()),
                        candidate_started,
                        &candidate_started_at,
                        None,
                    );
                    handle_stream_queue_rejected(
                        deps,
                        &snapshot,
                        request_id,
                        protocol,
                        &model,
                        identity,
                        started,
                        &attempts,
                        payload,
                        provider,
                        &account_id,
                        headers,
                        delivery,
                        status,
                        &message,
                        was_aborted,
                        None,
                    )
                    .await;
                    return Ok(());
                }
            };

            // 语义流泵每候选构造一次(E1:上游 SSE → 语义事件 → 客户端帧);
            // 同 provider 重试共享状态。P2:生命周期/用量/统计由 observer
            // 独占,泵只负责目标帧编码。
            let mut pump = build_client_stream(protocol);
            let mut observer = SourceObserver::new(provider.protocol);
            // 20260905_04 §3.3:聚合请求的流式输出帧 model 回写为聚合名
            // (泵序列化出口统一改写;非聚合请求不设置,零行为变化)。
            if let Some(aggregate_name) = &aggregate_name {
                pump.set_output_model(aggregate_name);
            }
            // 首字口径(20261016_01 A-1):存「请求开始 → 首个有效内容」的
            // 固定耗时,而不是首字时刻的时间点 —— 终态直接复用,不再重算。
            let mut first_byte_ms: Option<f64> = None;
            // 20261108_01 §4.1:答案完成里程碑(观察到语义 `Finish`)与冻结
            // 耗时;拖尾排空从其后的有界窗口内继续,但不再计入主指标。
            let mut answer_end: Option<Instant> = None;
            let mut answer_end_ms: Option<f64> = None;
            // A9.7:流式正文捕获累加的原始 SSE 块(镜像 `_response_chunks`;
            // 每帧补回 `\n\n` 块分隔,保持捕获文本是合法 SSE 流)。
            let mut response_chunks: Vec<String> = Vec::new();
            let mut trace_items: Vec<Value> = Vec::new();
            let mut captured_bytes = 0usize;
            let mut observed_bytes = 0usize;
            // S6:上游 SSE 原始字节缓冲(完成时经非流聚合装配入缓存)。
            // 20261016_01 B-3:响应缓存未启用时不再无条件累积完整上游流
            //(长流白复制一份原始字节)。
            // P5-5:缓存装配只累积语义事件,不再复制整条上游原始字节;
            // SSE 原文不是可重放的客户端 JSON。
            // W2-5:聚合预算复用 `response_cache.memory_max_bytes`;超限后
            // 聚合器标记 suppressed,结束时不写缓存(绝不写截断结果)。
            let cache_budget = snapshot.config.savings.response_cache.memory_max_bytes;
            let mut cache_aggregator = response_cache_key
                .is_some()
                .then(|| StreamAggregator::with_budget(cache_budget));
            let mut attempt: u32 = 0;
            let max_retries = provider.max_retries;
            let url = endpoint_with_query(
                protocol,
                provider,
                candidate_model,
                true,
                client_fingerprint::resolve_fingerprint_query(provider, inbound_query.as_deref())
                    .as_deref(),
            );
            let upstream_head =
                upstream_headers_with_fingerprint(provider, Some(&fingerprint_inbound));
            // 20260906_01 B4:与非流式同构 —— 代理按「供应商 × 目标 URL」决策。
            let proxy = GatewayDeps::proxy_decision_for(&snapshot, provider, &url);
            let proxy_source = GatewayDeps::proxy_source_label(&snapshot, &proxy);
            'attempts: loop {
                // P2-3:出站前固定源/目标 profile、正文模式、保真与预期表示。
                // 规划拒绝在触达上游前终止,不消耗重试预算,不退化为透传。
                let exchange = match PreparedExchange::new(protocol, true, provider) {
                    Ok(exchange) => exchange,
                    Err(error) => {
                        last_error = Some(format!("protocol plan rejected: {error}"));
                        last_status = Some(400);
                        push_record(0, last_error.clone(), Instant::now(), &iso_now(), None);
                        break 'attempts;
                    }
                };
                deps.metrics.record_exchange_plan(
                    exchange.plan.fidelity().as_str(),
                    exchange.plan.request.mode.as_str(),
                    exchange.plan.response.mode.as_str(),
                    exchange.plan.profile.source.as_str(),
                    exchange.plan.target.as_str(),
                );
                let attempt_context = AttemptContext::new(
                    request_id,
                    attempt,
                    provider,
                    account_id.as_deref(),
                    candidate_model,
                    &exchange,
                );
                tracing::debug!(
                    target: "wrouter.gateway.proxy",
                    request_id = %attempt_context.request_id,
                    attempt = attempt_context.attempt,
                    plan = %attempt_context.trace_fields(),
                    "attempt_prepared"
                );
                // P2-2:预算归零不再发起新的上游调用(跨计划/候选共用)。
                if retry_budget == 0 {
                    last_error = Some(format!(
                        "retry budget exhausted after {budget_total} upstream attempts (reliability.retryBudget={budget_total})"
                    ));
                    break 'plan;
                }
                retry_budget -= 1;
                candidate_attempt.store(attempt, std::sync::atomic::Ordering::Relaxed);
                observer.begin_attempt(attempt);
                let conversion_started = Instant::now();
                let (outbound, conversion_report) =
                    match upstream_payload_with_report(protocol, payload, provider) {
                        Ok(converted) => converted,
                        // B6:矩阵 unsupported 格 —— 推进下一候选(此时
                        // 流泵尚未输出任何字节,切换安全)。
                        Err(failure) => {
                            last_error = Some(failure.message().to_owned());
                            last_status = Some(400);
                            break 'attempts;
                        }
                    };
                // P3-1:请求面字节决策(与非流式同口径)。
                let wire_bytes = request_wire_bytes(
                    &inbound_raw,
                    &inbound_value,
                    &outbound,
                    exchange.plan.request.mode,
                );
                // E1:转换策略/耗时/诊断类别进 metrics 与转换报告。
                let conversion_elapsed_us = conversion_started.elapsed().as_micros() as u64;
                record_conversion_metrics(
                    deps,
                    &conversion_report,
                    protocol,
                    provider,
                    conversion_elapsed_us,
                );
                // 阶段 D3:本次尝试的转换报告与上游请求体(采集开启时)。
                if let Some(report) = &conversion_report {
                    capture_with(&capture, |slot| slot.set_conversion(report));
                }
                capture_with(&capture, |slot| {
                    slot.set_request(capture_enabled, capture_budget, &outbound)
                });
                // 上游请求头一并采集(脱敏后落库;与非流式同口径)。
                capture_with(&capture, |slot| {
                    slot.set_request_headers(capture_enabled, &upstream_head)
                });
                let attempt_started = Instant::now();
                let attempt_started_at = iso_now();
                // 阶段四 C-4:即将出站 —— 标记本次尝试「已发出」。
                attempt_sent.store(true, std::sync::atomic::Ordering::Relaxed);
                // 等上游响应头期间感知客户端断开(2026-10-08):断开即取消,
                // 不再继续等握手/重试,不烧上游 token。
                stateful_attempted = true;
                let stream_result = tokio::select! {
                    result = deps
                        .upstream
                        .post_stream_bytes(provider, &proxy, &url, &upstream_head, &wire_bytes) =>
                    {
                        Some(result)
                    }
                    _ = wait_client_gone(client_disconnected) => None,
                };
                let Some(stream_result) = stream_result else {
                    delivery.mark_client_gone();
                    record_pre_stream_abort(
                        deps,
                        &snapshot,
                        request_id,
                        protocol,
                        &model,
                        identity,
                        started,
                        &attempts,
                        payload,
                        Some(provider.id.as_str()),
                        account_id.as_ref(),
                        Some(elapsed_ms(attempt_started)),
                        headers,
                    );
                    return Err(StreamPipelineError { client_gone: true });
                };
                match stream_result {
                    Err(error) => {
                        tracing::warn!(target: "wrouter.gateway.proxy", provider = %provider.id, exc_class = "UpstreamError", exc = %error, "upstream_stream_failed");
                        deps.breaker
                            .record_failure(&provider.id, Some(provider), &breaker_config)
                            .await;
                        last_error = Some(error.to_string());
                        if delivery.committed() {
                            // 阶段五 C-1:中途断流 → 释放粘性/钉扎/组粘性
                            // 绑定(坏粘性会一直传染给后续回合)。
                            release_sticky_targets(
                                deps,
                                &resolved_savings,
                                session_key.as_deref(),
                                payload,
                                group_ref,
                            );
                            push_record(
                                0,
                                Some(error.to_string()),
                                attempt_started,
                                &attempt_started_at,
                                None,
                            );
                            deps.record_log_extra(
                                &snapshot,
                                request_id,
                                &RequestLogEvent {
                                    protocol: protocol.to_owned(),
                                    model: model.clone(),
                                    status: 502,
                                    stream: true,
                                    duration_ms: elapsed_ms(started),
                                    provider_attempts: trail_provider_ids(&attempts),
                                    retry_attempts: trail_values(&attempts),
                                    end_reason: "mid_stream_error",
                                    provider: Some(provider.id.clone()),
                                    account: account_id.clone(),
                                    api_key_id: Some(identity.key_id.clone()),
                                    first_byte_ms: first_byte_ms,
                                    error: last_error.clone(),
                                },
                                // 已捕获的部分流正文一并落侧表(排障看得见断流前的内容)。
                                failure_log_extra(
                                    deps,
                                    &snapshot,
                                    identity,
                                    headers,
                                    payload,
                                    Some(&response_chunks.concat()),
                                    None,
                                ),
                            );
                            return Err(StreamPipelineError { client_gone: false });
                        }
                        let will_retry = should_retry(0, attempt, max_retries);
                        // P1-1:先算后睡,落库 wait_ms 与实际等待同源。
                        let wait = will_retry.then(|| retry_wait_now(None, attempt));
                        push_record(
                            0,
                            Some(error.to_string()),
                            attempt_started,
                            &attempt_started_at,
                            wait,
                        );
                        if will_retry {
                            // P2-1:退避期间让出并发许可,睡醒重取;排队被拒
                            // 视同首取拒绝(落库+信封后终止管线)。
                            drop(permit.take());
                            if !sleep_wait_or_abort(
                                client_disconnected,
                                wait.expect("will_retry 必有 wait"),
                            )
                            .await
                            {
                                delivery.mark_client_gone();
                                record_pre_stream_abort(
                                    deps,
                                    &snapshot,
                                    request_id,
                                    protocol,
                                    &model,
                                    identity,
                                    started,
                                    &attempts,
                                    payload,
                                    Some(provider.id.as_str()),
                                    account_id.as_ref(),
                                    None,
                                    headers,
                                );
                                return Err(StreamPipelineError { client_gone: true });
                            }
                            permit = match deps.concurrency.acquire(provider, aborted).await {
                                AcquireOutcome::Acquired(permit) => Some(permit),
                                AcquireOutcome::Rejected {
                                    status,
                                    message,
                                    aborted: was_aborted,
                                } => {
                                    handle_stream_queue_rejected(
                                        deps,
                                        &snapshot,
                                        request_id,
                                        protocol,
                                        &model,
                                        identity,
                                        started,
                                        &attempts,
                                        payload,
                                        provider,
                                        &account_id,
                                        headers,
                                        delivery,
                                        status,
                                        &message,
                                        was_aborted,
                                        first_byte_ms,
                                    )
                                    .await;
                                    return Ok(());
                                }
                            };
                            attempt += 1;
                            continue 'attempts;
                        }
                        break 'attempts;
                    }
                    Ok(mut stream) if stream.status >= 400 => {
                        // 错误体必须先读尽再解析(镜像 aread 注释)。
                        let mut body = Vec::new();
                        while let Some(chunk) = stream.next_chunk().await {
                            match chunk {
                                Ok(bytes) => body.extend_from_slice(&bytes),
                                Err(_) => break,
                            }
                        }
                        let error_body = parse_error_body(&body);
                        let upstream_error = classify_upstream_error(stream.status, &error_body);
                        let record_error = upstream_error.message.clone();
                        // 2026-10-14:错误正文留一份供失败终态行捕获(脱敏在
                        // with_capture 侧做)。
                        let error_body_text = String::from_utf8_lossy(&body).into_owned();
                        // K3:流式仅在**握手期**(响应头已到且状态非 2xx)记录
                        // 凭据结果;SSE 开始后的中途断连不驱动 key 冷却(方案
                        // §3.3)。Retry-After 头优先,429 无头时 K8 正文兜底。
                        if let Some(account_id) = &account_id {
                            let status = i64::from(stream.status);
                            let body_text = (status == 429)
                                .then(|| String::from_utf8_lossy(&body).into_owned());
                            let retry_after_ms = credential_retry_after_ms(
                                status,
                                header_str(&stream.headers, "retry-after"),
                                body_text.as_deref(),
                            );
                            // 阶段五 C-2:按**语义类别**记冷却 —— 配额类错误
                            // 与瞬时限速分开(前者要等几小时,后者等 60s 就够),
                            // 作用域也随类别分(账号级 vs 账号+模型级)。
                            let rest_class = wr_domain::rest::classify_rest_class(
                                stream.status,
                                &error_body_text,
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
                        }
                        deps.breaker
                            .record_response(
                                &provider.id,
                                stream.status,
                                Some(provider),
                                &breaker_config,
                            )
                            .await;
                        // S7:上下文超限(400 正文命中方言)→ 不重试同模型,
                        // 推进下一候选/计划 attempt(与非流式同语义)。
                        if stream.status == 400
                            && wr_domain::gateway::is_context_overflow_body(
                                &String::from_utf8_lossy(&body),
                            )
                        {
                            last_error = Some(
                                "upstream context overflow (estimated tokens exceed model window)"
                                    .to_owned(),
                            );
                            push_record(
                                stream.status,
                                last_error.clone(),
                                attempt_started,
                                &attempt_started_at,
                                None,
                            );
                            break 'attempts;
                        }
                        if NO_RETRY_STATUS_CODES.contains(&stream.status) {
                            push_record(
                                stream.status,
                                Some(record_error.clone()),
                                attempt_started,
                                &attempt_started_at,
                                None,
                            );
                            // P1-3:401/403 是凭据类失败(账号 key 失效/无
                            // 权限)—— 凭据池已记 300s 冷却,当前请求内推进
                            // 下一候选;真实握手错误暂存,全候选耗尽后以真实
                            // 状态码回信封(401 不吞成 502)。404/422 保持终态。
                            if CREDENTIAL_FAILURE_STATUS_CODES.contains(&stream.status) {
                                last_error = Some(format!(
                                    "upstream status {} (credential failure, advancing candidates)",
                                    stream.status
                                ));
                                last_handshake_error = Some((
                                    stream.status,
                                    upstream_error,
                                    error_body_text,
                                    header_map(&stream.headers),
                                ));
                                break 'attempts;
                            }
                            let (_, envelope) =
                                format_error_for_protocol(protocol, &upstream_error, None);
                            deps.record_log_extra(
                                &snapshot,
                                request_id,
                                &RequestLogEvent {
                                    protocol: protocol.to_owned(),
                                    model: model.clone(),
                                    status: stream.status,
                                    stream: true,
                                    duration_ms: elapsed_ms(started),
                                    provider_attempts: trail_provider_ids(&attempts),
                                    retry_attempts: trail_values(&attempts),
                                    end_reason: "upstream_error",
                                    provider: Some(provider.id.clone()),
                                    account: account_id.clone(),
                                    api_key_id: Some(identity.key_id.clone()),
                                    first_byte_ms: first_byte_ms,
                                    error: Some(record_error.clone()),
                                },
                                // 上游错误响应(正文+响应头)一并捕获:失败详情
                                // 页不再只有一行错误摘要(2026-10-14)。
                                failure_log_extra(
                                    deps,
                                    &snapshot,
                                    identity,
                                    headers,
                                    payload,
                                    Some(&error_body_text),
                                    Some(header_map(&stream.headers)),
                                ),
                            );
                            let _ = delivery.error(protocol, stream.status, &envelope).await;
                            return Ok(());
                        }
                        let retry_after_header = header_str(&stream.headers, "retry-after");
                        // P1-2:429 带 >60s Retry-After —— 原地等满 clamp 的
                        // 60s 再打同一把刚被限流的 key 大概率再 429(凭据池
                        // 已按 Retry-After 全值冷却该账号),直接推进下一候选。
                        let skip_inplace_retry = stream.status == 429
                            && wr_domain::gateway::retry_after_exceeds_cap(retry_after_header);
                        let will_retry = !skip_inplace_retry
                            && should_retry(stream.status, attempt, max_retries);
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
                            record_error.clone()
                        };
                        push_record(
                            stream.status,
                            Some(error_summary),
                            attempt_started,
                            &attempt_started_at,
                            wait,
                        );
                        if will_retry {
                            // P2-1:退避期间让出并发许可,睡醒重取。
                            drop(permit.take());
                            if !sleep_wait_or_abort(
                                client_disconnected,
                                wait.expect("will_retry 必有 wait"),
                            )
                            .await
                            {
                                delivery.mark_client_gone();
                                record_pre_stream_abort(
                                    deps,
                                    &snapshot,
                                    request_id,
                                    protocol,
                                    &model,
                                    identity,
                                    started,
                                    &attempts,
                                    payload,
                                    Some(provider.id.as_str()),
                                    account_id.as_ref(),
                                    None,
                                    headers,
                                );
                                return Err(StreamPipelineError { client_gone: true });
                            }
                            permit = match deps.concurrency.acquire(provider, aborted).await {
                                AcquireOutcome::Acquired(permit) => Some(permit),
                                AcquireOutcome::Rejected {
                                    status,
                                    message,
                                    aborted: was_aborted,
                                } => {
                                    push_record(
                                        status,
                                        Some(message.clone()),
                                        Instant::now(),
                                        &iso_now(),
                                        None,
                                    );
                                    handle_stream_queue_rejected(
                                        deps,
                                        &snapshot,
                                        request_id,
                                        protocol,
                                        &model,
                                        identity,
                                        started,
                                        &attempts,
                                        payload,
                                        provider,
                                        &account_id,
                                        headers,
                                        delivery,
                                        status,
                                        &message,
                                        was_aborted,
                                        first_byte_ms,
                                    )
                                    .await;
                                    return Ok(());
                                }
                            };
                            attempt += 1;
                            continue 'attempts;
                        }
                        last_error = Some(format!("upstream status {}", stream.status));
                        break 'attempts;
                    }
                    Ok(mut stream) => {
                        // P2-3:收到响应头后只从已规划执行器中选择;未知/
                        // 错配表示在提交前失败。
                        let content_type = header_str(&stream.headers, "content-type");
                        let executor = match exchange.select_executor(content_type) {
                            Ok(executor) => executor,
                            Err(error) => {
                                last_error =
                                    Some(format!("response representation rejected: {error}"));
                                last_status = Some(502);
                                push_record(
                                    stream.status,
                                    last_error.clone(),
                                    attempt_started,
                                    &attempt_started_at,
                                    None,
                                );
                                break 'attempts;
                            }
                        };

                        // P3-1:原生 Raw 执行器 —— 仅当规划为原生 Raw 且上游表示
                        // 与下游 SSE 一致时原记录透传;NDJSON→SSE 属表示转换,
                        // 仍经重建路径。聚合别名在 Raw 上按协议路径定点回写(R8)。
                        let native_raw = exchange.plan.response.mode == BodyMode::Raw
                            && executor == ResponseExecutor::Sse;
                        // P5-3:非流式已有的归因头在流式提交时同样下发。
                        let mut commit_headers =
                            vec![("x-wr-routed-model".to_owned(), model.clone())];
                        if let Some(account_id) = &account_id {
                            commit_headers.push(("x-wr-account".to_owned(), account_id.clone()));
                        }
                        let attempt_count = trail_provider_ids(&attempts).len();
                        if attempt_count > 0 {
                            commit_headers.push((
                                "x-wr-fallback-attempts".to_owned(),
                                attempt_count.to_string(),
                            ));
                            if attempt_count > 1 {
                                commit_headers.push((
                                    "x-wr-fallback-failures".to_owned(),
                                    (attempt_count - 1).to_string(),
                                ));
                            }
                        }
                        delivery.set_headers(commit_headers);

                        // A6:流式请求收到单 JSON 上游 → 合成帧序列。
                        // 这是规划内的表示转换(JSON → 客户端流),不是透传。
                        if executor == ResponseExecutor::JsonSynthesis {
                            let mut body = Vec::new();
                            let mut read_error: Option<String> = None;
                            loop {
                                // 20261108_01 §4.2:单 JSON 上游同样受空闲
                                // 超时约束 —— 挂死的连接不再无限等待。
                                let next = tokio::time::timeout(idle_timeout, stream.next_chunk()).await;
                                match next {
                                    Ok(Some(chunk)) => match chunk {
                                        Ok(bytes) => body.extend_from_slice(&bytes),
                                        Err(error) => {
                                            read_error = Some(error.to_string());
                                            break;
                                        }
                                    },
                                    Ok(None) => break,
                                    Err(_) => {
                                        read_error = Some(format!(
                                            "upstream JSON body idle timeout ({} ms without data)",
                                            idle_timeout.as_millis()
                                        ));
                                        break;
                                    }
                                }
                            }
                            if let Some(error) = read_error {
                                last_error =
                                    Some(format!("upstream JSON body interrupted: {error}"));
                                last_status = Some(502);
                                push_record(
                                    0,
                                    last_error.clone(),
                                    attempt_started,
                                    &attempt_started_at,
                                    None,
                                );
                                break 'attempts;
                            }
                            let synthetic = UpstreamResponse {
                                status: 200,
                                headers: stream.headers.clone(),
                                body,
                            };
                            let (mut decoded, response_report) =
                                match decode_upstream_json(protocol, provider, &synthetic) {
                                    Ok(decoded) => decoded,
                                    Err(failure) => {
                                        last_error = Some(failure.message().to_owned());
                                        last_status = Some(502);
                                        push_record(
                                            0,
                                            last_error.clone(),
                                            attempt_started,
                                            &attempt_started_at,
                                            None,
                                        );
                                        break 'attempts;
                                    }
                                };
                            record_response_conversion_metrics(
                                deps,
                                &response_report,
                                protocol,
                                provider,
                            );
                            if let Some(aggregate_name) = &aggregate_name {
                                wr_domain::protocol::rewrite_json_model_fields(
                                    &mut decoded,
                                    aggregate_name,
                                );
                            }
                            let stream_usage =
                                wr_domain::usage::normalize_usage(decoded.get("usage"));
                            let frames = match wr_domain::semantic::synthesis::synthesize_client_response_frames(
                                protocol, &decoded,
                            ) {
                                Ok(frames) => frames,
                                Err(error) => {
                                    last_error = Some(format!(
                                        "upstream JSON could not be synthesized into a client stream: {}",
                                        error.message
                                    ));
                                    last_status = Some(502);
                                    push_record(
                                        0,
                                        last_error.clone(),
                                        attempt_started,
                                        &attempt_started_at,
                                        None,
                                    );
                                    break 'attempts;
                                }
                            };
                            if frames.is_empty() {
                                last_error = Some(
                                    "upstream JSON could not be synthesized into a client stream"
                                        .to_owned(),
                                );
                                last_status = Some(502);
                                push_record(
                                    0,
                                    last_error.clone(),
                                    attempt_started,
                                    &attempt_started_at,
                                    None,
                                );
                                break 'attempts;
                            }
                            for frame in frames {
                                if let Err(error) = delivery
                                    .emit(EmissionBatch::from_frames(vec![frame], true))
                                    .await
                                {
                                    // 20261108_01 §4.3:合成帧下发失败同样必须
                                    // 落库(旧实现直接 return → 在途条目泄漏到
                                    // 1h 超龄清扫)。
                                    let client_gone = error == DeliveryError::ClientGone;
                                    let (status, end_reason, message) = if client_gone {
                                        (499, "client_disconnected", "client_disconnected".to_owned())
                                    } else {
                                        (
                                            502,
                                            "mid_stream_error",
                                            format!("client delivery failed: {error:?}"),
                                        )
                                    };
                                    push_record(
                                        0,
                                        Some(message.clone()),
                                        attempt_started,
                                        &attempt_started_at,
                                        None,
                                    );
                                    deps.record_log_extra(
                                        &snapshot,
                                        request_id,
                                        &RequestLogEvent {
                                            protocol: protocol.to_owned(),
                                            model: model.clone(),
                                            status,
                                            stream: true,
                                            duration_ms: elapsed_ms(started),
                                            provider_attempts: trail_provider_ids(&attempts),
                                            retry_attempts: trail_values(&attempts),
                                            end_reason,
                                            provider: Some(provider.id.clone()),
                                            account: account_id.clone(),
                                            api_key_id: Some(identity.key_id.clone()),
                                            first_byte_ms,
                                            error: Some(message),
                                        },
                                        failure_log_extra(
                                            deps,
                                            &snapshot,
                                            identity,
                                            headers,
                                            payload,
                                            Some(&String::from_utf8_lossy(&synthetic.body)),
                                            None,
                                        ),
                                    );
                                    drop(permit.take());
                                    return Err(StreamPipelineError { client_gone });
                                }
                                note_first_byte(
                                    deps,
                                    request_id,
                                    started,
                                    &mut first_byte_ms,
                                    delivery,
                                );
                            }
                            deps.breaker
                                .record_success(&provider.id, Some(provider), &breaker_config)
                                .await;
                            bind_savings_targets(
                                deps,
                                &resolved_savings,
                                session_key.as_deref(),
                                payload,
                                provider,
                                account_id.as_deref(),
                                &attempt_model,
                                &stream_usage,
                            );
                            if let Some(account_id) = &account_id {
                                deps.credentials.record_outcome_classified(
                                    &provider.id,
                                    account_id,
                                    Some(candidate_model),
                                    200,
                                    wr_domain::rest::FailureSignal::default(),
                                );
                            }
                            bind_group_sticky(
                                deps,
                                group_ref,
                                session_key.as_deref(),
                                provider,
                                account_id.as_deref(),
                                candidate_model,
                            );
                            push_record(
                                stream.status,
                                None,
                                attempt_started,
                                &attempt_started_at,
                                None,
                            );
                            let synthesized_body =
                                String::from_utf8_lossy(&synthetic.body).into_owned();
                            if resolved_savings.response_cache
                                && let Some(cache_key) = &response_cache_key
                            {
                                response_cache_store_entry(
                                    deps,
                                    cache_key,
                                    &decoded,
                                    cost_of(provider, &stream_usage),
                                    &model,
                                    &snapshot.config.savings.response_cache,
                                )
                                .await;
                            }
                            record_compression_savings(
                                deps,
                                request_id,
                                compression_report.as_ref(),
                            );
                            deps.record_log_extra(
                                &snapshot,
                                request_id,
                                &RequestLogEvent {
                                    protocol: protocol.to_owned(),
                                    model: model.clone(),
                                    status: 200,
                                    stream: true,
                                    duration_ms: elapsed_ms(started),
                                    provider_attempts: trail_provider_ids(&attempts),
                                    retry_attempts: trail_values(&attempts),
                                    end_reason: "completed",
                                    provider: Some(provider.id.clone()),
                                    account: account_id.clone(),
                                    api_key_id: Some(identity.key_id.clone()),
                                    first_byte_ms,
                                    error: None,
                                },
                                {
                                    let mut log_extra = savings_extra.clone();
                                    log_extra.extend(upstream_model_extra(
                                        candidate_model,
                                        aggregate_name.as_deref().unwrap_or(&model),
                                    ));
                                    log_extra.insert(
                                        "proxy".to_owned(),
                                        Value::String(proxy_source.clone()),
                                    );
                                    log_extra.insert(
                                        "usageQuality".to_owned(),
                                        Value::String(observer.usage_quality().as_str().to_owned()),
                                    );
                                    let mut extra_builder = RequestLogExtra::default()
                                        .with_identity(&identity.key_name)
                                        .with_preview(logs_enabled, payload)
                                        .with_usage(stream_usage, cost_of(provider, &stream_usage))
                                        .with_extra(log_extra)
                                        .with_flow_ms(pump.flow_ms())
                                        // 20261108_01 §5.2:答案段计时键
                                        // (answer_end / 答案 TTFT / answer_flow /
                                        // 思考跨度 / 排空拖尾;有值才写)。
                                        .with_answer_timing(answer_timing(
                                            started,
                                            answer_end,
                                            &pump,
                                            Instant::now(),
                                        ))
                                        .with_request_meta(
                                            Some(&client_label(headers)),
                                            Some("POST"),
                                            Some(protocol),
                                            Some(&iso_now()),
                                        )
                                        .with_capture(
                                            capture_enabled,
                                            deps.settings.body_capture_max_bytes,
                                            Some(payload),
                                            Some(&synthesized_body),
                                            None,
                                        );
                                    if is_aggregate_model {
                                        extra_builder = extra_builder
                                            .with_requested_model(&client_requested_model, &model);
                                    }
                                    extra_builder
                                },
                            );
                            drop(permit.take());
                            return Ok(());
                        }

                        if executor == ResponseExecutor::Json {
                            last_error = Some(
                                "non-stream JSON executor selected in streaming pipeline"
                                    .to_owned(),
                            );
                            last_status = Some(502);
                            push_record(
                                stream.status,
                                last_error.clone(),
                                attempt_started,
                                &attempt_started_at,
                                None,
                            );
                            break 'attempts;
                        }

                        // Sse / Ndjson:两种表示各自独立分帧,不互相冒充。
                        let mut framer: Box<dyn RecordFramer + Send> = match executor {
                            ResponseExecutor::Sse => Box::new(SseFramer::new(
                                WireLimits::default(),
                                RecordSource::upstream(provider.protocol.as_str()),
                            )),
                            ResponseExecutor::Ndjson => Box::new(NdjsonFramer::new(
                                WireLimits::default(),
                                RecordSource::upstream(provider.protocol.as_str()),
                            )),
                            _ => unreachable!("executor filtered above"),
                        };
                        let mut upstream_ok = true;
                        // P1-1:中途错误的等待时长在错误分支先算好(与落库
                        // wait_ms 同源),统一睡眠点消费;EOF 不完整支路没有
                        // 预算过,睡眠点自行兜底重算。
                        let mut pending_wait: Option<(f64, &'static str)> = None;
                        let mut framing_error: Option<String> = None;
                        // 20261108_01:答案完成后客户端离场(按完成记账,不再
                        // 记 499)/ 空闲或 grace 到期。
                        let mut answered_client_gone = false;
                        let mut tail_timed_out = false;
                        // 最后一次**真实记录**的时刻;心跳(SSE 注释 / 空行)
                        // 不续命(§4.2「ping 不算内容」)。
                        let mut last_upstream_activity = Instant::now();
                        loop {
                            // 20261108_01 §4.2:每轮等待受两个界约束 —— 无记录
                            // 空闲上限,以及答案完成后的 grace 剩余时间。任一
                            // 到期即停止排空(不再等上游阻塞的终态帧)。
                            let mut wait =
                                idle_timeout.saturating_sub(last_upstream_activity.elapsed());
                            if let Some(end) = answer_end {
                                wait = wait.min(terminal_grace.saturating_sub(end.elapsed()));
                            }
                            let next_chunk = tokio::select! {
                                chunk = tokio::time::timeout(wait, stream.next_chunk()) => match chunk {
                                    Ok(chunk) => chunk,
                                    Err(_) => {
                                        tail_timed_out = true;
                                        break;
                                    }
                                },
                                _ = wait_client_gone(client_disconnected) => {
                                    delivery.mark_client_gone();
                                    // 答案已完整交付:客户端只是没等簿记帧
                                    // (常见于 agent 读到 finish 即断开),按
                                    // 完成记账;未完成才记 499。
                                    if answer_end.is_some() {
                                        answered_client_gone = true;
                                        break;
                                    }
                                    deps.record_log_extra(
                                        &snapshot,
                                        request_id,
                                        &RequestLogEvent {
                                            protocol: protocol.to_owned(),
                                            model: model.clone(),
                                            status: 499,
                                            stream: true,
                                            duration_ms: elapsed_ms(started),
                                            provider_attempts: trail_provider_ids(&attempts),
                                            retry_attempts: trail_values(&attempts),
                                            end_reason: "client_disconnected",
                                            provider: Some(provider.id.clone()),
                                            account: account_id.clone(),
                                            api_key_id: Some(identity.key_id.clone()),
                                            first_byte_ms: first_byte_ms,
                                            error: Some("client_disconnected".to_owned()),
                                        },
                                        failure_log_extra(
                                            deps,
                                            &snapshot,
                                            identity,
                                            headers,
                                            payload,
                                            Some(&response_chunks.concat()),
                                            None,
                                        ),
                                    );
                                    drop(permit.take());
                                    return Err(StreamPipelineError { client_gone: true });
                                }
                            };
                            match next_chunk {
                                Some(Ok(bytes)) => {
                                    let now = now_epoch_ms();
                                    let records = match framer.feed(&bytes, now) {
                                        Ok(records) => records,
                                        Err(error) => {
                                            framing_error = Some(error.to_string());
                                            break;
                                        }
                                    };
                                    let mut client_gone = false;
                                    for record in records {
                                        if !record_is_heartbeat(&record) {
                                            last_upstream_activity = Instant::now();
                                        }
                                        let observation_only = observer.terminal();
                                        let mut encoder = RecordEncoder {
                                            provider,
                                            observer: &mut observer,
                                            pump: &mut pump,
                                            native_raw,
                                            aggregate_model: aggregate_name.as_deref(),
                                            capture_enabled,
                                            capture_budget,
                                            captured_bytes: &mut captured_bytes,
                                            observed_bytes: &mut observed_bytes,
                                            response_chunks: &mut response_chunks,
                                            trace_items: &mut trace_items,
                                            cache_aggregator: cache_aggregator.as_mut(),
                                        };
                                        match encoder.encode(&record, observation_only) {
                                            Ok(Some(batch)) => {
                                                // 20261108_01 §4.1:答案完成即冻结
                                                // 耗时、释放并发许可;后续排空只做
                                                // 簿记(尾随 usage / 终态帧)。
                                                if note_answer_end(
                                                    deps,
                                                    request_id,
                                                    started,
                                                    &observer,
                                                    &mut answer_end,
                                                    &mut answer_end_ms,
                                                ) {
                                                    drop(permit.take());
                                                }
                                                let has_frames = !batch.frames.is_empty();
                                                if delivery.emit(batch).await.is_err() {
                                                    client_gone = true;
                                                    break;
                                                }
                                                if has_frames {
                                                    observer.mark_committed();
                                                }
                                                note_first_byte(
                                                    deps,
                                                    request_id,
                                                    started,
                                                    &mut first_byte_ms,
                                                    delivery,
                                                );
                                            }
                                            Ok(None) => {
                                                if note_answer_end(
                                                    deps,
                                                    request_id,
                                                    started,
                                                    &observer,
                                                    &mut answer_end,
                                                    &mut answer_end_ms,
                                                ) {
                                                    drop(permit.take());
                                                }
                                            }
                                            Err(error) => {
                                                framing_error = Some(error);
                                                break;
                                            }
                                        }
                                    }
                                    if client_gone {
                                        // 答案已交付:不记 499,走完成收尾。
                                        if answer_end.is_some() {
                                            answered_client_gone = true;
                                            break;
                                        }
                                        record_stream_client_gone(
                                            deps,
                                            &snapshot,
                                            request_id,
                                            protocol,
                                            &model,
                                            identity,
                                            provider,
                                            &account_id,
                                            started,
                                            &attempts,
                                            payload,
                                            headers,
                                            first_byte_ms,
                                            &response_chunks,
                                            capture_enabled,
                                            deps.settings.body_capture_max_bytes,
                                            logs_enabled,
                                        );
                                        drop(permit.take());
                                        return Err(StreamPipelineError { client_gone: true });
                                    }
                                    if framing_error.is_some() {
                                        break;
                                    }
                                    // 终态帧已消费即收尾,不等上游 EOF(中转站
                                    // 终结后可能长期保持连接)。终态后的残余记录
                                    // 仍作为 Received 观察,但不再编码。
                                    if observer.terminal() {
                                        break;
                                    }
                                }
                                Some(Err(error)) => {
                                    tracing::warn!(target: "wrouter.gateway.proxy", provider = %provider.id, exc_class = "UpstreamError", exc = %error, "upstream_stream_failed");
                                    // 20261108_01 §4.2:答案已交付后的尾部断流
                                    // 只是排空失败(缺 usage / 终态帧),不驱动
                                    // 熔断、不重试、不续写;请求按完成记账。
                                    if answer_end.is_some() {
                                        tracing::debug!(
                                            target: "wrouter.gateway.proxy",
                                            request_id = %request_id,
                                            exc = %error,
                                            "stream_tail_error_after_answer"
                                        );
                                        break;
                                    }
                                    deps.breaker
                                        .record_failure(
                                            &provider.id,
                                            Some(provider),
                                            &breaker_config,
                                        )
                                        .await;
                                    last_error = Some(error.to_string());
                                    if let Some(account_id) = &account_id {
                                        deps.credentials.record_transport_failure(
                                            &provider.id,
                                            account_id,
                                            Some(candidate_model),
                                            "upstream stream interrupted",
                                        );
                                    }
                                    let will_retry = !delivery.committed()
                                        && should_retry(0, attempt, max_retries);
                                    let wait = will_retry.then(|| retry_wait_now(None, attempt));
                                    pending_wait = wait;
                                    push_record(
                                        0,
                                        Some(error.to_string()),
                                        attempt_started,
                                        &attempt_started_at,
                                        wait,
                                    );
                                    // 阶段五 C-3(20261106 修订):续写门必须
                                    // 同时满足已提交、有已提交正文、能力与
                                    // 无状态引用;不再依赖原始 partial_text。
                                    if delivery.committed()
                                        && !observer.committed_text().is_empty()
                                        && !observer.continuation_prefix_exceeded()
                                        && continuation_gate(
                                            protocol,
                                            provider,
                                            payload,
                                            continuation_attempted,
                                        )
                                        .is_ok()
                                    {
                                        continuation_attempted = true;
                                        let continuation_payload =
                                            wr_domain::semantic::anthropic::append_assistant_prefill(
                                                &outbound,
                                                observer.committed_text(),
                                            );
                                        if let Some(continuation_payload) = continuation_payload {
                                            let continuation = tokio::select! {
                                                result = deps.upstream.post_stream(
                                                    provider,
                                                    &proxy,
                                                    &url,
                                                    &upstream_head,
                                                    &continuation_payload,
                                                ) => Some(result),
                                                _ = wait_client_gone(client_disconnected) => None,
                                            };
                                            match continuation {
                                                Some(Ok(next_stream))
                                                    if next_stream.status < 400 =>
                                                {
                                                    tracing::info!(
                                                        target: "wrouter.gateway.proxy",
                                                        provider = %provider.id,
                                                        bytes = observer.committed_text().len(),
                                                        "upstream_stream_continued"
                                                    );
                                                    pump.begin_continuation();
                                                    observer.begin_continuation();
                                                    if trace_items.len() < 200 {
                                                        trace_items.push(json!({
                                                            "kind": "continuation",
                                                            "prefill_bytes": observer.committed_text().len(),
                                                        }));
                                                    }
                                                    framer = match executor {
                                                        ResponseExecutor::Sse => {
                                                            Box::new(SseFramer::new(
                                                                WireLimits::default(),
                                                                RecordSource::upstream(
                                                                    provider.protocol.as_str(),
                                                                ),
                                                            ))
                                                        }
                                                        ResponseExecutor::Ndjson => {
                                                            Box::new(NdjsonFramer::new(
                                                                WireLimits::default(),
                                                                RecordSource::upstream(
                                                                    provider.protocol.as_str(),
                                                                ),
                                                            ))
                                                        }
                                                        _ => {
                                                            unreachable!("executor filtered above")
                                                        }
                                                    };
                                                    pending_wait = None;
                                                    stream = next_stream;
                                                    continue;
                                                }
                                                Some(Ok(rejected)) => {
                                                    tracing::warn!(
                                                        target: "wrouter.gateway.proxy",
                                                        provider = %provider.id,
                                                        status = rejected.status,
                                                        "upstream_continuation_rejected"
                                                    );
                                                }
                                                Some(Err(continuation_error)) => {
                                                    tracing::warn!(
                                                        target: "wrouter.gateway.proxy",
                                                        provider = %provider.id,
                                                        exc = %continuation_error,
                                                        "upstream_continuation_failed"
                                                    );
                                                }
                                                None => {
                                                    delivery.mark_client_gone();
                                                    record_stream_client_gone(
                                                        deps,
                                                        &snapshot,
                                                        request_id,
                                                        protocol,
                                                        &model,
                                                        identity,
                                                        provider,
                                                        &account_id,
                                                        started,
                                                        &attempts,
                                                        payload,
                                                        headers,
                                                        first_byte_ms,
                                                        &response_chunks,
                                                        capture_enabled,
                                                        deps.settings.body_capture_max_bytes,
                                                        logs_enabled,
                                                    );
                                                    drop(permit.take());
                                                    return Err(StreamPipelineError {
                                                        client_gone: true,
                                                    });
                                                }
                                            }
                                        }
                                    }
                                    upstream_ok = false;
                                    break;
                                }
                                None => break,
                            }
                        }

                        if let Some(error) = framing_error {
                            if answer_end.is_some() {
                                // 答案已交付:尾部残帧解析失败只影响簿记。
                                tracing::debug!(
                                    target: "wrouter.gateway.proxy",
                                    request_id = %request_id,
                                    error = %error,
                                    "stream_tail_framing_error_after_answer"
                                );
                            } else {
                                last_error = Some(format!("upstream framing error: {error}"));
                                deps.breaker
                                    .record_failure(&provider.id, Some(provider), &breaker_config)
                                    .await;
                                push_record(
                                    0,
                                    last_error.clone(),
                                    attempt_started,
                                    &attempt_started_at,
                                    None,
                                );
                                upstream_ok = false;
                            }
                        }

                        // 20261108_01 §4.2:空闲/grace 到期。答案已完成时按
                        // 完成收尾(缺终态帧只是簿记缺失);未完成时落
                        // 502 stream_incomplete(不再等上游 EOF 或 1h 清扫)。
                        if tail_timed_out {
                            if answer_end.is_some() {
                                tracing::debug!(
                                    target: "wrouter.gateway.proxy",
                                    request_id = %request_id,
                                    grace_ms = terminal_grace.as_millis() as u64,
                                    "stream_terminal_grace_expired"
                                );
                            } else {
                                last_error = Some(format!(
                                    "upstream stream idle timeout ({} ms without data)",
                                    idle_timeout.as_millis()
                                ));
                                push_record(
                                    502,
                                    last_error.clone(),
                                    attempt_started,
                                    &attempt_started_at,
                                    None,
                                );
                                tracing::warn!(
                                    target: "wrouter.gateway.proxy",
                                    provider = %provider.id,
                                    idle_ms = idle_timeout.as_millis() as u64,
                                    "upstream_stream_idle_timeout"
                                );
                            }
                        }

                        // 答案已交付后的尾部异常不影响成功事实(上方 framing /
                        // 传输 / 空闲分支只记诊断)。
                        if answer_end.is_some() && !upstream_ok {
                            upstream_ok = true;
                            last_error = None;
                        }

                        if upstream_ok && answer_end.is_none() && !answered_client_gone {
                            // EOF:收割无空行终止的残片;残片观察但不编码。
                            // (答案完成后的残片不再处理 —— 不影响投影。)
                            let now = now_epoch_ms();
                            match framer.finish(now) {
                                Ok(records) => {
                                    for record in records {
                                        let observation_only = observer.terminal();
                                        let mut encoder = RecordEncoder {
                                            provider,
                                            observer: &mut observer,
                                            pump: &mut pump,
                                            native_raw,
                                            aggregate_model: aggregate_name.as_deref(),
                                            capture_enabled,
                                            capture_budget,
                                            captured_bytes: &mut captured_bytes,
                                            observed_bytes: &mut observed_bytes,
                                            response_chunks: &mut response_chunks,
                                            trace_items: &mut trace_items,
                                            cache_aggregator: cache_aggregator.as_mut(),
                                        };
                                        match encoder.encode(&record, observation_only) {
                                            Ok(Some(batch)) => {
                                                if note_answer_end(
                                                    deps,
                                                    request_id,
                                                    started,
                                                    &observer,
                                                    &mut answer_end,
                                                    &mut answer_end_ms,
                                                ) {
                                                    drop(permit.take());
                                                }
                                                let has_frames = !batch.frames.is_empty();
                                                if delivery.emit(batch).await.is_err() {
                                                    // 20261108_01 §4.3:残片下发
                                                    // 失败同样是客户端断开,必须
                                                    // 落 499 行(不再泄漏在途)。
                                                    record_stream_client_gone(
                                                        deps,
                                                        &snapshot,
                                                        request_id,
                                                        protocol,
                                                        &model,
                                                        identity,
                                                        provider,
                                                        &account_id,
                                                        started,
                                                        &attempts,
                                                        payload,
                                                        headers,
                                                        first_byte_ms,
                                                        &response_chunks,
                                                        capture_enabled,
                                                        deps.settings.body_capture_max_bytes,
                                                        logs_enabled,
                                                    );
                                                    drop(permit.take());
                                                    return Err(StreamPipelineError {
                                                        client_gone: true,
                                                    });
                                                }
                                                if has_frames {
                                                    observer.mark_committed();
                                                }
                                                note_first_byte(
                                                    deps,
                                                    request_id,
                                                    started,
                                                    &mut first_byte_ms,
                                                    delivery,
                                                );
                                            }
                                            Ok(None) => {
                                                if note_answer_end(
                                                    deps,
                                                    request_id,
                                                    started,
                                                    &observer,
                                                    &mut answer_end,
                                                    &mut answer_end_ms,
                                                ) {
                                                    drop(permit.take());
                                                }
                                            }
                                            Err(error) => {
                                                last_error = Some(format!(
                                                    "upstream framing error: {error}"
                                                ));
                                                upstream_ok = false;
                                                break;
                                            }
                                        }
                                    }
                                }
                                Err(error) => {
                                    last_error = Some(format!("upstream framing error: {error}"));
                                    upstream_ok = false;
                                }
                            }
                            if !upstream_ok {
                                push_record(
                                    0,
                                    last_error.clone(),
                                    attempt_started,
                                    &attempt_started_at,
                                    None,
                                );
                            }
                        }

                        // 残片阶段才观察到 Finish 的边界:尾部解析失败同样
                        // 不改写「答案已交付」的成功事实。
                        if answer_end.is_some() && !upstream_ok {
                            upstream_ok = true;
                            last_error = None;
                        }

                        // 20261108_01 §4.1:答案已完成(`Finish` 已观察)时,
                        // 缺协议终态帧不再算「不完整」—— 按完成收尾。
                        if upstream_ok && !observer.complete() && answer_end.is_none() {
                            let violations = observer.take_violations();
                            if violations > 0 {
                                tracing::debug!(target: "wrouter.gateway.proxy", provider = %provider.id, violations, "semantic_stream_events_rejected");
                            }
                            tracing::warn!(target: "wrouter.gateway.proxy", provider = %provider.id, "upstream_stream_incomplete_at_eof");
                            // 20261108_01 §4.2:空闲超时单独归因(错误列看得见
                            // 是「挂连接」而不是模糊的「流不完整」)。
                            let incomplete_reason = if tail_timed_out {
                                last_error.clone().unwrap_or_else(|| {
                                    "upstream_stream_incomplete".to_owned()
                                })
                            } else {
                                "upstream_stream_incomplete".to_owned()
                            };
                            last_error = Some(incomplete_reason.clone());
                            push_record(
                                502,
                                Some(incomplete_reason.clone()),
                                attempt_started,
                                &attempt_started_at,
                                None,
                            );
                            if delivery.committed() {
                                release_sticky_targets(
                                    deps,
                                    &resolved_savings,
                                    session_key.as_deref(),
                                    payload,
                                    group_ref,
                                );
                                let mut client_gone = false;
                                let finalize_frames = if native_raw {
                                    Vec::new()
                                } else {
                                    pump.finalize_incomplete()
                                };
                                for frame in finalize_frames {
                                    if delivery
                                        .emit(EmissionBatch::from_frames(vec![frame], false))
                                        .await
                                        .is_err()
                                    {
                                        client_gone = true;
                                        break;
                                    }
                                }
                                let incomplete_body = response_chunks.concat();
                                let extra = RequestLogExtra::default()
                                    .with_capture(
                                        capture_enabled,
                                        deps.settings.body_capture_max_bytes,
                                        Some(payload),
                                        Some(&incomplete_body),
                                        None,
                                    )
                                    .with_request_headers(headers)
                                    .with_stream_capture_metadata(
                                        observed_bytes,
                                        observed_bytes > captured_bytes,
                                    )
                                    .with_trace_items({
                                        let mut items = trace_attempt_items(&attempts);
                                        items.extend(trace_items.clone());
                                        items
                                    });
                                deps.record_log_extra(
                                    &snapshot,
                                    request_id,
                                    &RequestLogEvent {
                                        protocol: protocol.to_owned(),
                                        model: model.clone(),
                                        status: 502,
                                        stream: true,
                                        duration_ms: elapsed_ms(started),
                                        provider_attempts: trail_provider_ids(&attempts),
                                        retry_attempts: trail_values(&attempts),
                                        end_reason: "stream_incomplete",
                                        provider: Some(provider.id.clone()),
                                        account: account_id.clone(),
                                        api_key_id: Some(identity.key_id.clone()),
                                        first_byte_ms: first_byte_ms,
                                        error: Some(incomplete_reason),
                                    },
                                    extra,
                                );
                                drop(permit.take());
                                return Err(StreamPipelineError { client_gone });
                            }
                            upstream_ok = false;
                        }

                        if upstream_ok {
                            let mut client_gone = false;
                            // 答案后客户端已离场:不再向死连接补终态帧。
                            let finalize_frames = if native_raw || answered_client_gone {
                                Vec::new()
                            } else {
                                pump.finalize_with_usage(observer.usage())
                            };
                            for frame in finalize_frames {
                                if delivery
                                    .emit(EmissionBatch::from_frames(vec![frame], false))
                                    .await
                                    .is_err()
                                {
                                    client_gone = true;
                                    break;
                                }
                            }
                            // 20261108_01:答案已交付时终帧下发失败不改写成功
                            // 事实(客户端只是没等终帧);否则维持 499 语义。
                            if client_gone && answer_end.is_none() {
                                record_stream_client_gone(
                                    deps,
                                    &snapshot,
                                    request_id,
                                    protocol,
                                    &model,
                                    identity,
                                    provider,
                                    &account_id,
                                    started,
                                    &attempts,
                                    payload,
                                    headers,
                                    first_byte_ms,
                                    &response_chunks,
                                    capture_enabled,
                                    deps.settings.body_capture_max_bytes,
                                    logs_enabled,
                                );
                                drop(permit.take());
                                return Err(StreamPipelineError { client_gone: true });
                            }
                            deps.breaker
                                .record_success(&provider.id, Some(provider), &breaker_config)
                                .await;
                            bind_savings_targets(
                                deps,
                                &resolved_savings,
                                session_key.as_deref(),
                                payload,
                                provider,
                                account_id.as_deref(),
                                &attempt_model,
                                &observer.usage(),
                            );
                            if let Some(account_id) = &account_id {
                                deps.credentials.record_outcome_classified(
                                    &provider.id,
                                    account_id,
                                    Some(candidate_model),
                                    200,
                                    wr_domain::rest::FailureSignal::default(),
                                );
                            }
                            bind_group_sticky(
                                deps,
                                group_ref,
                                session_key.as_deref(),
                                provider,
                                account_id.as_deref(),
                                candidate_model,
                            );
                            if resolved_savings.response_cache
                                && let Some(cache_key) = &response_cache_key
                                && let Some(aggregator) = cache_aggregator.take()
                            {
                                if aggregator.is_suppressed() {
                                    tracing::warn!(
                                        target: "wrouter.gateway.proxy",
                                        request_id = %request_id,
                                        protocol,
                                        budget_bytes = cache_budget,
                                        "response_cache_suppressed"
                                    );
                                } else if let Some(source) =
                                    wr_domain::semantic::SourceProtocol::parse(protocol)
                                {
                                    let response = aggregator.into_partial_response();
                                    if let Ok(encoded) =
                                        wr_domain::semantic::synthesis::synthesize_client_response(
                                            source, &response,
                                        )
                                    {
                                        response_cache_store_entry(
                                            deps,
                                            cache_key,
                                            &encoded,
                                            cost_of(provider, &observer.usage()),
                                            &model,
                                            &snapshot.config.savings.response_cache,
                                        )
                                        .await;
                                    }
                                }
                            }
                            record_compression_savings(
                                deps,
                                request_id,
                                compression_report.as_ref(),
                            );
                            let captured_text = response_chunks.concat();
                            capture_with(&capture, |slot| {
                                slot.set_headers(capture_enabled, &stream.headers);
                                slot.set_response(capture_enabled, capture_budget, &captured_text);
                            });
                            push_record(
                                stream.status,
                                None,
                                attempt_started,
                                &attempt_started_at,
                                None,
                            );
                            let stream_usage = observer.usage();
                            deps.record_log_extra(
                                &snapshot,
                                request_id,
                                &RequestLogEvent {
                                    protocol: protocol.to_owned(),
                                    model: model.clone(),
                                    status: 200,
                                    stream: true,
                                    // 20261108_01 §4.1:主耗时冻结在答案完成
                                    // (`Finish`);无 `Finish` 的流回退到终态。
                                    duration_ms: answer_end_ms
                                        .unwrap_or_else(|| elapsed_ms(started)),
                                    provider_attempts: trail_provider_ids(&attempts),
                                    retry_attempts: trail_values(&attempts),
                                    end_reason: "completed",
                                    provider: Some(provider.id.clone()),
                                    account: account_id.clone(),
                                    api_key_id: Some(identity.key_id.clone()),
                                    first_byte_ms: first_byte_ms,
                                    error: None,
                                },
                                {
                                    let mut log_extra = savings_extra.clone();
                                    log_extra.extend(upstream_model_extra(
                                        candidate_model,
                                        aggregate_name.as_deref().unwrap_or(&model),
                                    ));
                                    log_extra.insert(
                                        "proxy".to_owned(),
                                        Value::String(proxy_source.clone()),
                                    );
                                    log_extra.insert(
                                        "usageQuality".to_owned(),
                                        Value::String(observer.usage_quality().as_str().to_owned()),
                                    );
                                    let mut extra_builder = RequestLogExtra::default()
                                        .with_identity(&identity.key_name)
                                        .with_preview(logs_enabled, payload)
                                        .with_usage(stream_usage, cost_of(provider, &stream_usage))
                                        .with_extra(log_extra)
                                        .with_flow_ms(pump.flow_ms())
                                        // 20261108_01 §5.2:答案段计时键
                                        // (answer_end / 答案 TTFT / answer_flow /
                                        // 思考跨度 / 排空拖尾;有值才写)。
                                        .with_answer_timing(answer_timing(
                                            started,
                                            answer_end,
                                            &pump,
                                            Instant::now(),
                                        ))
                                        .with_request_meta(
                                            Some(&client_label(headers)),
                                            Some("POST"),
                                            Some(protocol),
                                            Some(&iso_now()),
                                        )
                                        .with_capture(
                                            capture_enabled,
                                            deps.settings.body_capture_max_bytes,
                                            Some(payload),
                                            Some(&captured_text),
                                            None,
                                        )
                                        .with_request_headers(headers)
                                        .with_stream_capture_metadata(
                                            observed_bytes,
                                            observed_bytes > captured_bytes,
                                        )
                                        .with_trace_items({
                                            let mut items = trace_attempt_items(&attempts);
                                            items.extend(trace_items.clone());
                                            items
                                        });
                                    if is_aggregate_model {
                                        extra_builder = extra_builder
                                            .with_requested_model(&client_requested_model, &model);
                                    }
                                    extra_builder
                                },
                            );
                            let ledger = delivery.ledger();
                            let received = observer.received();
                            let accepted = observer.accepted();
                            tracing::debug!(
                                target: "wrouter.gateway.proxy",
                                request_id = %request_id,
                                queued_frames = ledger.queued_frames,
                                queued_bytes = ledger.queued_bytes,
                                committed_source_end = ledger.committed_source_end,
                                received_frames = received.frames,
                                accepted_events = accepted.events,
                                rejected_events = accepted.rejected_events,
                                usage_quality = observer.usage_quality().as_str(),
                                "stream_delivery_ledger"
                            );
                            success = true;
                            drop(permit.take());
                            break 'plan;
                        }

                        // relay 输出已开始则不得原地重试(C4);
                        // 未开始则按同 provider 重试预算继续。
                        if delivery.committed() {
                            release_sticky_targets(
                                deps,
                                &resolved_savings,
                                session_key.as_deref(),
                                payload,
                                group_ref,
                            );
                            let mut client_gone = false;
                            let finalize_frames = if native_raw {
                                Vec::new()
                            } else {
                                pump.finalize_incomplete()
                            };
                            for frame in finalize_frames {
                                if delivery
                                    .emit(EmissionBatch::from_frames(vec![frame], false))
                                    .await
                                    .is_err()
                                {
                                    client_gone = true;
                                    break;
                                }
                            }
                            deps.record_log_extra(
                                &snapshot,
                                request_id,
                                &RequestLogEvent {
                                    protocol: protocol.to_owned(),
                                    model: model.clone(),
                                    status: 502,
                                    stream: true,
                                    duration_ms: elapsed_ms(started),
                                    provider_attempts: trail_provider_ids(&attempts),
                                    retry_attempts: trail_values(&attempts),
                                    end_reason: "mid_stream_error",
                                    provider: Some(provider.id.clone()),
                                    account: account_id.clone(),
                                    api_key_id: Some(identity.key_id.clone()),
                                    first_byte_ms: first_byte_ms,
                                    error: last_error.clone(),
                                },
                                failure_log_extra(
                                    deps,
                                    &snapshot,
                                    identity,
                                    headers,
                                    payload,
                                    Some(&response_chunks.concat()),
                                    None,
                                ),
                            );
                            drop(permit.take());
                            return Err(StreamPipelineError { client_gone });
                        }
                        if should_retry(0, attempt, max_retries) {
                            let wait = pending_wait
                                .take()
                                .unwrap_or_else(|| retry_wait_now(None, attempt));
                            drop(permit.take());
                            if !sleep_wait_or_abort(client_disconnected, wait).await {
                                delivery.mark_client_gone();
                                record_pre_stream_abort(
                                    deps,
                                    &snapshot,
                                    request_id,
                                    protocol,
                                    &model,
                                    identity,
                                    started,
                                    &attempts,
                                    payload,
                                    Some(provider.id.as_str()),
                                    account_id.as_ref(),
                                    None,
                                    headers,
                                );
                                return Err(StreamPipelineError { client_gone: true });
                            }
                            permit = match deps.concurrency.acquire(provider, aborted).await {
                                AcquireOutcome::Acquired(permit) => Some(permit),
                                AcquireOutcome::Rejected {
                                    status,
                                    message,
                                    aborted: was_aborted,
                                } => {
                                    push_record(
                                        status,
                                        Some(message.clone()),
                                        Instant::now(),
                                        &iso_now(),
                                        None,
                                    );
                                    handle_stream_queue_rejected(
                                        deps,
                                        &snapshot,
                                        request_id,
                                        protocol,
                                        &model,
                                        identity,
                                        started,
                                        &attempts,
                                        payload,
                                        provider,
                                        &account_id,
                                        headers,
                                        delivery,
                                        status,
                                        &message,
                                        was_aborted,
                                        first_byte_ms,
                                    )
                                    .await;
                                    return Ok(());
                                }
                            };
                            attempt += 1;
                            continue 'attempts;
                        }
                        break 'attempts;
                    }
                }
            }
            drop(permit.take());
        }
    }

    if !success {
        // P1-3:凭据类失败(401/403)推进候选后全链耗尽 —— 以暂存的真实
        // 握手错误回信封(真实状态码/错误体/响应头),401 不吞成 502。
        if let Some((status, upstream_error, body_text, upstream_headers)) =
            last_handshake_error.take()
        {
            let attempts_now = trail_provider_ids(&attempts);
            let (_, envelope) =
                format_error_for_protocol(protocol, &upstream_error, Some(&attempts_now));
            deps.record_log_extra(
                &snapshot,
                request_id,
                &RequestLogEvent {
                    protocol: protocol.to_owned(),
                    model: model.clone(),
                    status,
                    stream: true,
                    duration_ms: elapsed_ms(started),
                    provider_attempts: attempts_now,
                    retry_attempts: trail_values(&attempts),
                    end_reason: "upstream_error",
                    provider: None,
                    account: None,
                    api_key_id: Some(identity.key_id.clone()),
                    first_byte_ms: None,
                    error: Some(upstream_error.message.clone()),
                },
                failure_log_extra(
                    deps,
                    &snapshot,
                    identity,
                    headers,
                    payload,
                    Some(&body_text),
                    Some(upstream_headers),
                ),
            );
            let _ = delivery.error(protocol, status, &envelope).await;
            return Ok(());
        }
        // A4:全部计划/候选/重试耗尽且未发出字节 → 真实 HTTP 状态 + 协议错误。
        let status = last_status.unwrap_or(502);
        let attempts_now = trail_provider_ids(&attempts);
        let message = last_error
            .clone()
            .unwrap_or_else(|| "No streaming provider available".to_owned());
        let upstream_error = classify_upstream_error(
            status,
            &json!({"error": {"message": message, "type": "upstream_error"}}),
        );
        let (_, envelope) =
            format_error_for_protocol(protocol, &upstream_error, Some(&attempts_now));
        deps.record_log_extra(
            &snapshot,
            request_id,
            &RequestLogEvent {
                protocol: protocol.to_owned(),
                model: model.clone(),
                status,
                stream: true,
                duration_ms: elapsed_ms(started),
                provider_attempts: attempts_now,
                retry_attempts: trail_values(&attempts),
                end_reason: if status == 400 {
                    "protocol_not_supported"
                } else {
                    "no_provider"
                },
                provider: None,
                account: None,
                api_key_id: Some(identity.key_id.clone()),
                first_byte_ms: None,
                error: last_error,
            },
            failure_log_extra(deps, &snapshot, identity, headers, payload, None, None),
        );
        let _ = delivery.error(protocol, status, &envelope).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn anthropic_provider() -> Provider {
        let mut raw = serde_json::Map::new();
        raw.insert("id".to_owned(), json!("p"));
        raw.insert("api_base_url".to_owned(), json!("https://upstream.example/v1"));
        raw.insert("protocol".to_owned(), json!("anthropic"));
        Provider::from_raw(&raw).unwrap()
    }

    #[test]
    fn continuation_gate_rejects_stateful_and_thinking_requests() {
        let provider = anthropic_provider();
        let plain = json!({"model": "m", "messages": []});
        assert!(continuation_gate("anthropic", &provider, &plain, false).is_ok());
        assert_eq!(
            continuation_gate("anthropic", &provider, &plain, true).unwrap_err(),
            "continuation_already_attempted"
        );
        assert_eq!(
            continuation_gate(
                "anthropic",
                &provider,
                &json!({"previous_response_id": "resp_1"}),
                false
            )
            .unwrap_err(),
            "continuation_with_stateful_reference"
        );
        assert_eq!(
            continuation_gate(
                "anthropic",
                &provider,
                &json!({"thinking": {"type": "enabled"}}),
                false
            )
            .unwrap_err(),
            "continuation_with_thinking"
        );
        assert_eq!(
            continuation_gate("openai", &provider, &plain, false).unwrap_err(),
            "continuation_protocol_mismatch"
        );
    }
}
