//! 网关数据面 HTTP 分派(20261013_01 C2 自 lib.rs 独立)。
//!
//! 路由处理器 → 认证 → 在途登记 → 流式/非流式分流 → 转发管线的
//! HTTP 接线层(镜像 Python `app.py::gateway_request` 面);转发本体在
//! [`crate::gateway::forward`]。路由表组装仍在 [`crate::lib`] 的
//! `gateway_router`。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use axum::Json;
use axum::body::Body;
use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures_util::Stream;
use serde_json::{Value, json};

use crate::gateway::{self, GatewayDeps, SharedGatewayDeps, auth, forward};
use crate::{VERSION, settings};

/// axum 共享状态。
#[derive(Clone)]
pub struct AppState {
    pub settings: Arc<settings::Settings>,
    /// 网关数据面依赖;管理面处理器不用(为 None)。
    pub gateway: Option<SharedGatewayDeps>,
}

/// 镜像 Python `app.py::gateway_health`(网关面)。
pub(crate) async fn gateway_health(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "ok": true,
        "name": "WRouter",
        "version": VERSION,
        "port": state.settings.gateway_port,
    }))
}

pub(crate) async fn gateway_openai_chat(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: axum::body::Bytes,
) -> Response {
    handle_gateway(state, headers, query, body, "openai").await
}

pub(crate) async fn gateway_anthropic_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: axum::body::Bytes,
) -> Response {
    handle_gateway(state, headers, query, body, "anthropic").await
}

pub(crate) async fn gateway_openai_responses(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: axum::body::Bytes,
) -> Response {
    handle_gateway(state, headers, query, body, "openai_responses").await
}

pub(crate) async fn gateway_openai_embeddings(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: axum::body::Bytes,
) -> Response {
    handle_gateway(state, headers, query, body, "openai_embeddings").await
}

pub(crate) async fn gateway_gemini_interactions(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: axum::body::Bytes,
) -> Response {
    handle_gateway(state, headers, query, body, "gemini_interactions").await
}

/// gemini 路由:整段 `{model_action}` = `<model>:<action>`。
/// 镜像 Python:`payload["model"] = model`;`:generateContent` 依
/// payload.stream 决定 gemini / gemini_stream;`:streamGenerateContent`
/// 强制 `stream=true` + gemini_stream。
pub(crate) async fn gateway_gemini_model(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    axum::extract::Path(model_action): axum::extract::Path<String>,
    body: axum::body::Bytes,
) -> Response {
    let Some((model, action)) = model_action.rsplit_once(':') else {
        return not_found();
    };
    let payload = match parse_body(&body) {
        Ok(payload) => payload,
        Err(response) => return *response,
    };
    let mut payload = payload;
    payload["model"] = Value::String(model.to_owned());
    let protocol = match action.to_ascii_lowercase().as_str() {
        "generatecontent" => {
            if payload
                .get("stream")
                .map(wr_domain::python_semantics::py_bool)
                .unwrap_or(false)
            {
                "gemini_stream"
            } else {
                "gemini"
            }
        }
        "streamgeneratecontent" => {
            payload["stream"] = Value::Bool(true);
            "gemini_stream"
        }
        _ => return not_found(),
    };
    dispatch_gateway(state, headers, query, payload, protocol, body.to_vec()).await
}

/// `/v1/models`(镜像 app.py:397-414):同认证;openai/anthropic/gemini
/// 三种列表形态。
pub(crate) async fn gateway_models(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(deps) = state.gateway.clone() else {
        return internal_no_gateway();
    };
    let snapshot = match deps.snapshot().await {
        Ok(snapshot) => snapshot,
        Err(message) => return internal_error(&message),
    };
    if let Err(failure) = auth::authenticate(&snapshot, &state.settings.auth_token, &headers) {
        return auth_failure_response(&failure, None);
    }
    let mut values = Vec::new();
    // 2026-09-26 调用契约:列表 id 即可调用选择器 —— 供应商模型以
    // `供应商名/模型名`(名称空白回落 id)入列,顺带消除多供应商同名
    // 模型的重复 id;聚合以 `wrouter/<聚合名>` 入列。
    for provider in &snapshot.providers {
        let provider_label = provider.name.trim();
        let provider_label = if provider_label.is_empty() {
            provider.id.as_str()
        } else {
            provider_label
        };
        for model in &provider.models {
            values.push(json!({
                "id": format!("{provider_label}/{model}"),
                "object": "model",
                "owned_by": provider.id,
            }));
        }
    }
    // 20260905_04 §3.4:聚合与 provider 模型并列输出;可用性是**列表期
    // 静态检查**(存在 enabled 且服务该模型的 provider 即可用),忽略熔断等
    // 请求级条件,保证列表稳定。disabled 的组不列出。id 带 `wrouter/`
    // 前缀(2026-09-26 调用契约:显式命名空间)。
    let mut group_names: Vec<&String> = snapshot
        .compiled
        .model_registry
        .model_groups()
        .iter()
        .filter(|(_, group)| group.enabled)
        .map(|(name, _)| name)
        .collect();
    group_names.sort();
    for name in group_names {
        let group = &snapshot.compiled.model_registry.model_groups()[name];
        let available = group.targets.iter().any(|target| {
            target.enabled
                && snapshot.providers.iter().any(|provider| {
                    provider.id == target.provider_id
                        && provider.enabled
                        && (provider.models.is_empty()
                            || provider
                                .models
                                .iter()
                                .any(|m| m == &target.model || m == "*"))
                })
        });
        values.push(json!({
            "id": format!("wrouter/{name}"),
            "object": "model",
            "owned_by": "wrouter",
            "aggregate": true,
            "available": available,
        }));
    }
    // format/protocol query 参数形态分支在 replay 集之外;R2 支持三种:
    // 默认 openai 列表 / anthropic / gemini。
    let _ = &values;
    Json(json!({ "object": "list", "data": values })).into_response()
}

/// 共享入口:解析请求体 → 认证 → 分发(镜像 `gateway_request`)。
async fn handle_gateway(
    state: AppState,
    headers: HeaderMap,
    query: Option<String>,
    body: axum::body::Bytes,
    protocol: &str,
) -> Response {
    let payload = match parse_body(&body) {
        Ok(payload) => payload,
        Err(response) => return *response,
    };
    dispatch_gateway(state, headers, query, payload, protocol, body.to_vec()).await
}

async fn dispatch_gateway(
    state: AppState,
    headers: HeaderMap,
    // 20261016_03:入站原始 query(此前全仓无捕获;客户端指纹的
    // passthrough 源,经 allowlist 过滤后才可能出站)。
    inbound_query: Option<String>,
    payload: Value,
    protocol: &str,
    // P3-1:入站原始正文字节(原生请求面 ByteExact 透传/定点修改的权威来源)。
    inbound_raw: Vec<u8>,
) -> Response {
    // 20261016_01 §2.6.3:全请求唯一计时起点。此前 in_flight 登记、
    // spawn 任务、流式/非流式管线各自 Instant::now(),在途行「已耗时」
    // 与终态 durationMs 的口径存在 spawn/调度级分裂;统一后首字与时长
    // 从同一锚点计量。
    let request_started = Instant::now();
    let Some(deps) = state.gateway.clone() else {
        return internal_no_gateway();
    };
    let request_id = headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let snapshot = match deps.snapshot().await {
        Ok(snapshot) => snapshot,
        Err(message) => return internal_error(&message),
    };
    // 认证(镜像 gateway_request:324-344;失败附审计日志,R4 落库)。
    let identity = match auth::authenticate(&snapshot, &state.settings.auth_token, &headers) {
        Ok(identity) => identity,
        Err(failure) => {
            tracing::info!(
                target: "wrouter.gateway",
                request_id = %request_id,
                status = failure.status,
                end_reason = "auth_failed",
                "gateway_request rejected",
            );
            // 审计行(R4.5):认证失败的请求从未触达 provider,`provider` 为空
            // ⇒ 只进 request_logs,不进用量统计。记录失败不影响拒绝响应。
            let model = payload
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or_default();
            deps.record_rejected(
                &snapshot,
                protocol,
                model,
                &request_id,
                failure.status,
                "auth_failed",
                Some(&failure.message),
            );
            return auth_failure_response(&failure, Some(&request_id));
        }
    };

    // 请求头快照:`(小写名, 值)`,同名取首见(镜像 starlette Headers →
    // dict 的按键读取语义);路由引擎与访问控制策略消费。
    let mut request_headers: Vec<(String, String)> = Vec::new();
    for (name, value) in headers.iter() {
        let name = name.as_str().to_ascii_lowercase();
        if !request_headers.iter().any(|(k, _)| *k == name) {
            request_headers.push((name, value.to_str().unwrap_or_default().to_owned()));
        }
    }

    // 流式/非流式分流(镜像 payload.get("stream") truthy 语义)。
    let wants_stream = payload
        .get("stream")
        .map(wr_domain::python_semantics::py_bool)
        .unwrap_or(false);

    // 在途登记(等待中):已认证请求进入注册表,调用日志页实时可见;
    // 落库汇点(record_log*)取走条目,完成「内存行 → 落库行」交接。
    deps.in_flight.register(
        &request_id,
        gateway::in_flight::InFlightRequest {
            protocol: protocol.to_owned(),
            model: payload
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned(),
            stream: wants_stream,
            api_key_id: Some(identity.key_id.clone()),
            api_key_name: Some(identity.key_name.clone()),
            reasoning_effort: wr_domain::gateway::extract_reasoning_effort(protocol, &payload),
            created_at: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            started_at: request_started,
            first_byte_ms: None,
            answer_end_ms: None,
            row_id: 0,
            phase: gateway::in_flight::InFlightPhase::Waiting,
        },
    );
    deps.events.publish(gateway::events::inflight_changed_event(
        &request_id,
        "pending",
    ));

    if wants_stream {
        return stream_response(
            deps,
            snapshot,
            payload,
            protocol,
            &request_id,
            identity,
            request_headers,
            request_started,
            inbound_query,
            inbound_raw,
        )
        .await;
    }

    // 非流式:工作 task 化,handler 只等结果(2026-10-08)。客户端断开会
    // 丢弃 handler future(接收端随之消失),工作任务随即取消并落 499
    // 终态行 —— 在途条目不再悬挂「处理中」,请求也不再静默无日志。
    let result_rx = forward::spawn_forward_json(
        deps.clone(),
        snapshot,
        protocol.to_owned(),
        payload,
        request_id.clone(),
        identity,
        request_headers,
        request_started,
        inbound_query,
        inbound_raw,
    );
    match result_rx.await {
        Ok(Ok(result)) => {
            let status = StatusCode::from_u16(result.status).unwrap_or(StatusCode::OK);
            // P3-1:原生 Raw 成功响应直接透传原字节,不经 Value 重序列化。
            let mut response = match result.raw_body {
                Some(raw) => {
                    let mut response = Response::new(Body::from(raw));
                    *response.status_mut() = status;
                    response.headers_mut().insert(
                        header::CONTENT_TYPE,
                        header::HeaderValue::from_static("application/json"),
                    );
                    response
                }
                None => (status, Json(result.body)).into_response(),
            };
            if let Ok(value) = header::HeaderValue::from_str(&request_id) {
                response.headers_mut().insert("x-wrouter-request-id", value);
            }
            for (name, value) in result.headers {
                if let (Ok(name), Ok(value)) = (
                    header::HeaderName::from_bytes(name.as_bytes()),
                    header::HeaderValue::from_str(&value),
                ) {
                    response.headers_mut().insert(name, value);
                }
            }
            response
        }
        Ok(Err(error)) => {
            let mut response =
                (StatusCode::BAD_GATEWAY, Json(error.into_body(&request_id))).into_response();
            if let Ok(value) = header::HeaderValue::from_str(&request_id) {
                response.headers_mut().insert("x-wrouter-request-id", value);
            }
            response
        }
        // 实际不可达(工作任务总以 send 或 closed 分支结束);防御性兜底。
        Err(_) => internal_error("forward task ended without a result"),
    }
}

/// 流式响应:spawn 转发管线,把通道接到 axum Body。
/// 断开感知:body 被 drop(客户端断开)时置位标志,管线内
/// 通道发送失败随之终止并记 499。
async fn stream_response(
    deps: Arc<GatewayDeps>,
    snapshot: Arc<gateway::ConfigSnapshot>,
    payload: Value,
    protocol: &str,
    request_id: &str,
    identity: auth::GatewayIdentity,
    headers: Vec<(String, String)>,
    request_started: Instant,
    inbound_query: Option<String>,
    inbound_raw: Vec<u8>,
) -> Response {
    let client_disconnected = Arc::new(AtomicBool::new(false));
    let session = forward::spawn_forward_stream(
        deps,
        snapshot,
        protocol.to_owned(),
        payload,
        request_id.to_owned(),
        identity,
        headers,
        client_disconnected.clone(),
        request_started,
        inbound_query,
        inbound_raw,
    );
    // P2-4:提交前等待管线给出真实 HTTP 决策。首个下游字节入队前失败
    // 返回真实状态码;只有管线确认提交后才构造 200 SSE。
    let decision = session.handshake.await;
    match decision {
        Ok(forward::StreamHandshake::Committed { headers }) => {
            let stream = DisconnectAwareStream {
                inner: session.body,
                flag: client_disconnected,
            };
            let mut response = Response::new(Body::from_stream(stream));
            *response.status_mut() = StatusCode::OK;
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("text/event-stream"),
            );
            if let Ok(value) = header::HeaderValue::from_str(request_id) {
                response.headers_mut().insert("x-wrouter-request-id", value);
            }
            for (name, value) in headers {
                if let (Ok(name), Ok(value)) = (
                    header::HeaderName::from_bytes(name.as_bytes()),
                    header::HeaderValue::from_str(&value),
                ) {
                    response.headers_mut().insert(name, value);
                }
            }
            response
        }
        Ok(forward::StreamHandshake::Rejected { status, body }) => {
            let mut response = (
                StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(body),
            )
                .into_response();
            if let Ok(value) = header::HeaderValue::from_str(request_id) {
                response.headers_mut().insert("x-wrouter-request-id", value);
            }
            response
        }
        Err(_) => {
            let mut response = internal_error("stream pipeline ended before commit decision");
            if let Ok(value) = header::HeaderValue::from_str(request_id) {
                response.headers_mut().insert("x-wrouter-request-id", value);
            }
            response
        }
    }
}

/// 接收端流:转发 mpsc 项;drop 时置位断开标志。
struct DisconnectAwareStream {
    inner: tokio::sync::mpsc::Receiver<Result<Vec<u8>, std::convert::Infallible>>,
    flag: Arc<AtomicBool>,
}

impl Stream for DisconnectAwareStream {
    type Item = Result<Vec<u8>, std::convert::Infallible>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.inner.poll_recv(cx)
    }
}

impl Drop for DisconnectAwareStream {
    fn drop(&mut self) {
        self.flag.store(true, Ordering::Relaxed);
    }
}

fn parse_body(body: &axum::body::Bytes) -> Result<Value, Box<Response>> {
    serde_json::from_slice::<Value>(body).map_err(|error| {
        // Python 侧 request.json() 抛错表现为 500;此处保留同状态码,
        // 但给出结构化错误体(见 R2 完成记录)。
        Box::new(
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": {
                        "message": format!("invalid JSON body: {error}"),
                        "type": "wrouter_error",
                    }
                })),
            )
                .into_response(),
        ) as Box<Response>
    })
}

fn auth_failure_response(failure: &auth::AuthFailure, request_id: Option<&str>) -> Response {
    let status = StatusCode::from_u16(failure.status).unwrap_or(StatusCode::UNAUTHORIZED);
    let mut response = (
        status,
        Json(json!({
            "error": {"type": "authentication_error", "message": failure.message}
        })),
    )
        .into_response();
    if let Some(request_id) = request_id
        && let Ok(value) = header::HeaderValue::from_str(request_id)
    {
        response.headers_mut().insert("x-wrouter-request-id", value);
    }
    response
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({"detail": "Not Found"}))).into_response()
}

fn internal_error(message: &str) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({
            "error": {"message": message, "type": "wrouter_error"}
        })),
    )
        .into_response()
}

fn internal_no_gateway() -> Response {
    internal_error("gateway data plane is not wired")
}
