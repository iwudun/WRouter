//! Status / Proxy / Providers / Metrics / Events 组。
//!
//! REST 与 RPC 共用 `svc_*` 服务函数;形状逐字段镜像 Python:
//! - `svc_gateway_status` ← handlers/status.py `build_status`
//!   (endpoint 用 **live settings** 端口,勿读持久化 config);
//! - REST `/api/v1/status` ← app.py `status()`(compatGateway 块已随
//!   迁移期兼容网关退役移除,2026-09-01);
//! - proxy/captures 状态服务已随前端 networking 页移除(2026-09-05,
//!   Rust 本就无 MITM 引擎,相应 RPC 从注册表删除)。

use std::net::{IpAddr, SocketAddr};

use axum::Json;
use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use super::AdminState;
use crate::VERSION;
use wr_app::ports::{HealthStore as _, SecretStore as _, UsageStore as _};
use wr_domain::provider::Provider;
use wr_domain::python_semantics::py_bool;
use wr_infra::sqlite_config::SqliteConfigSource;

// ---------------------------------------------------------------------------
// 服务函数(RPC dispatch 与 REST 共用)
// ---------------------------------------------------------------------------

/// RPC `getGatewayStatus`(compatGateway = usage.runtime_status)。
pub async fn svc_gateway_status(state: &AdminState) -> Value {
    let s = &state.settings;
    let compat = state.store.runtime_status().await.unwrap_or_else(|e| {
        tracing::warn!(target: "wrouter.admin", %e, "runtime_status_failed");
        Value::Object(Default::default())
    });
    json!({
        "ok": true,
        "name": s.app_name,
        "version": VERSION,
        "endpoint": format!("http://{}:{}", s.host, s.gateway_port),
        // 20261102_01 P6:state 不再硬编码 "running" —— 真实反映网关面
        // 启停状态(running / stopped / starting / error)。
        "state": state.control.gateway_state(),
        "gatewayEnabled": state.control.gateway_enabled(),
        "lastError": state.control.gateway_error(),
        // 20261102_01 P6:后端是否由外部托管(桌面壳内嵌 = false)。
        // 前端据此决定「退出」按钮是「退出应用」还是「停止服务」。
        "coreManagedExternally": s.managed_by.can_stop_from_page(),
        "generatedConfigFile": "",
        "networkEndpoints": [],
        "ports": {
            "gateway": s.gateway_port,
            "web": s.web_port,
            "proxy": s.proxy_port,
        },
        "dataDir": s.data_dir.to_string_lossy(),
        "compatGateway": compat,
    })
}

/// RPC `getAppInfo`(platform/python 字段为登记偏差:Rust 无法复刻
/// `platform.platform()`)。
pub fn svc_app_info(state: &AdminState) -> Value {
    let s = &state.settings;
    json!({
        "name": s.app_name,
        "version": VERSION,
        "platform": format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        "python": format!("rust ({VERSION})"),
        "ports": {
            "gateway": s.gateway_port,
            "web": s.web_port,
            "proxy": s.proxy_port,
        },
        "dataDir": s.data_dir.to_string_lossy(),
        "configDir": s.config_dir.to_string_lossy(),
        "frontendDir": s.frontend_dir.to_string_lossy(),
    })
}

/// RPC `getUpdateStatus`(静态 idle,镜像 handlers/misc.py)。
pub fn svc_update_status() -> Value {
    json!({
        "state": "idle",
        "supported": false,
        "canCheck": false,
        "canDownload": false,
        "canInstall": false,
        "currentVersion": VERSION,
    })
}

/// 免认证 health(镜像 app.py health)。
pub async fn management_health(State(state): State<AdminState>) -> Json<Value> {
    Json(json!({
        "ok": true,
        "name": "WRouter",
        "version": VERSION,
        "apiVersion": "v1",
        "gatewayPort": state.settings.gateway_port,
        "webPort": state.settings.web_port,
    }))
}

/// 免认证 capabilities(ports 用 live settings 覆盖)。
pub async fn get_capabilities(State(state): State<AdminState>) -> Json<Value> {
    let mut caps = state.capabilities.clone();
    if let Some(obj) = caps.as_object_mut() {
        let ports = obj
            .entry("ports")
            .or_insert_with(|| Value::Object(Default::default()));
        if let Some(p) = ports.as_object_mut() {
            p.insert("gateway".into(), json!(state.settings.gateway_port));
            p.insert("management".into(), json!(state.settings.web_port));
            p.insert("proxy".into(), json!(state.settings.proxy_port));
        }
    }
    Json(caps)
}

/// GET /api/wr/session(方案 20261102_01 P1-3):浏览器 / Tauri 首屏取
/// 管理令牌的入口。
///
/// 设计要点:
/// - **仅 loopback 来源放行**(D4:本版本不放开远程管理);非 loopback
///   一律 403,避免管理面被绑到 `0.0.0.0` 时成为令牌泄露面(R5);
/// - **令牌绝不放进 URL**(URL 会进浏览器历史 / 日志 / Referer)——前端
///   只经同源 `fetch` 获取,拿到后存 sessionStorage;
/// - 该路由挂在免认证组(不进 protected),否则前端拿不到令牌。
pub async fn session_endpoint(
    State(state): State<AdminState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> Response {
    if !is_loopback_peer(peer.ip()) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"detail": "Session endpoint is loopback-only"})),
        )
            .into_response();
    }
    Json(json!({"token": state.settings.auth_token})).into_response()
}

/// loopback 判定:IPv4 回环 + IPv6 `::1` + IPv4-mapped 回环
/// (`::ffff:127.0.0.1`,双栈监听下的常见 peer 形态)。
fn is_loopback_peer(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6
                    .to_ipv4_mapped()
                    .map(|v4| v4.is_loopback())
                    .unwrap_or(false)
        }
    }
}

// ---------------------------------------------------------------------------
// providers 服务
// ---------------------------------------------------------------------------

/// REST GET /api/v1/client-fingerprints(20261016_03):内置客户端指纹
/// 预设清单 —— 前端「客户端指纹」下拉的数据源;`compatibleProtocols`
/// 供按 API format 门禁禁选并给原因。静态数据,无需 state。
pub async fn rest_client_fingerprints() -> Response {
    let presets: Vec<Value> = wr_domain::protocol::client_fingerprint::builtin_presets()
        .iter()
        .map(|preset| {
            json!({
                "id": preset.id,
                "label": preset.label,
                "compatibleProtocols": preset.compatible_protocols,
            })
        })
        .collect();
    Json(json!({ "presets": presets })).into_response()
}

/// REST GET /api/v1/providers(实体 to_dict(false),密钥已注水故掩码为 ***)。
pub async fn rest_providers_list(State(state): State<AdminState>) -> Response {
    match state.deps.snapshot().await {
        Ok(snapshot) => {
            let items: Vec<Value> = snapshot
                .providers
                .iter()
                .map(|p| p.to_dict(false))
                .collect();
            Json(Value::Array(items)).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(Value::String(e))).into_response(),
    }
}

/// REST PUT /api/v1/providers/{id}(镜像 config_service.set_provider)。
pub async fn rest_upsert_provider(
    State(state): State<AdminState>,
    Path(provider_id): Path<String>,
    Json(payload): Json<Value>,
) -> Response {
    let source = SqliteConfigSource::new(&state.settings.data_dir);
    let mut config = match source.load_app_config() {
        Ok(config) => config,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(Value::String(e))).into_response();
        }
    };
    let existed = config
        .providers
        .iter()
        .any(|r| r.raw.get("id").and_then(Value::as_str) == Some(provider_id.as_str()));
    // data = payload 原样(不归一化,镜像 set_provider 的 dict(payload));
    // 已存在且 payload 缺/空 api_key → 从 secrets 回填。
    let mut data = payload.as_object().cloned().unwrap_or_default();
    data.insert("id".to_owned(), Value::String(provider_id.clone()));
    if existed {
        let missing_or_empty = match data.get("api_key") {
            None => true,
            Some(Value::String(s)) => s.is_empty(),
            Some(_) => false,
        };
        if missing_or_empty {
            let secret = state
                .store
                .provider_secret(&provider_id)
                .await
                .unwrap_or_default();
            if !secret.is_empty() {
                data.insert("api_key".to_owned(), Value::String(secret));
            }
        }
    }
    // 形状校验(ValueError → 400,镜像 HTTPException(400, str(exc)))。
    if let Err(message) = Provider::from_raw(&data) {
        return (StatusCode::BAD_REQUEST, Json(json!({"detail": message}))).into_response();
    }
    // 客户端指纹协议配对门禁(20261016_03 §3.5;与 saveConfig 同闸)。
    if let Err(message) = super::config::validate_client_fingerprint_raw(&data, "provider") {
        return (StatusCode::BAD_REQUEST, Json(json!({"detail": message}))).into_response();
    }
    config
        .providers
        .retain(|r| r.raw.get("id").and_then(Value::as_str) != Some(provider_id.as_str()));
    config
        .providers
        .push(wr_infra::config::ProviderRecord { raw: data });
    if let Err(e) = super::config::persist_snapshot(&state, &config).await {
        tracing::error!(target: "wrouter.admin", %e, "upsert_provider_save_failed");
        return super::config::plain_500();
    }
    // 响应 = 注水后的实体 to_dict(false)(镜像 gateway.providers() 的
    // load_with_secrets 形态:provider 级密钥回填后再掩码)。
    let mut reloaded = SqliteConfigSource::new(&state.settings.data_dir)
        .load_app_config()
        .unwrap_or(config);
    super::config::hydrate_account_secrets(&state, &mut reloaded).await;
    let raw = reloaded
        .providers
        .iter()
        .find(|r| r.raw.get("id").and_then(Value::as_str) == Some(provider_id.as_str()))
        .map(|r| r.raw.clone())
        .unwrap_or_default();
    match Provider::from_raw(&raw) {
        Ok(provider) => Json(provider.to_dict(false)).into_response(),
        Err(_) => super::config::plain_500(),
    }
}

/// REST DELETE /api/v1/providers/{id}(镜像 config_service.delete_provider)。
pub async fn rest_delete_provider(
    State(state): State<AdminState>,
    Path(provider_id): Path<String>,
) -> Response {
    let source = SqliteConfigSource::new(&state.settings.data_dir);
    let mut config = match source.load_app_config() {
        Ok(config) => config,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(Value::String(e))).into_response();
        }
    };
    let before = config.providers.len();
    config
        .providers
        .retain(|r| r.raw.get("id").and_then(Value::as_str) != Some(provider_id.as_str()));
    if config.providers.len() == before {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"detail": "Provider not found"})),
        )
            .into_response();
    }
    if let Err(e) = super::config::persist_snapshot(&state, &config).await {
        tracing::error!(target: "wrouter.admin", %e, "delete_provider_save_failed");
        return super::config::plain_500();
    }
    let _ = state.store.delete_provider_secret(&provider_id).await;
    let _ = state.store.delete_provider_accounts(&provider_id).await;
    Json(json!({"ok": true})).into_response()
}

/// REST POST /api/v1/providers/probe(镜像 app.py:208-215 +
/// provider_selection.probe_provider:GET /v1/models 单点探活并落健康快照)。
pub async fn rest_probe_provider(
    State(state): State<AdminState>,
    Json(payload): Json<Value>,
) -> Response {
    // str(payload.get("provider_id") or payload.get("providerId") or "")。
    let provider_id = ["provider_id", "providerId"]
        .iter()
        .find_map(|key| {
            payload
                .get(*key)
                .filter(|v| py_bool(v))
                .and_then(wr_domain::python_semantics::py_str)
        })
        .unwrap_or_default();
    if provider_id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"detail": "provider_id is required"})),
        )
            .into_response();
    }
    match state.deps.snapshot().await {
        Ok(snapshot) => match svc_probe_provider(&state, &snapshot, &provider_id).await {
            Ok(result) => Json(result).into_response(),
            Err(message) => {
                (StatusCode::NOT_FOUND, Json(json!({"detail": message}))).into_response()
            }
        },
        Err(_) => super::config::plain_500(),
    }
}

/// `ProviderSelector.probe_provider`:GET probe_endpoint,超时
/// min(timeout_seconds, 15);HTTPError → ok:false + error 字段;
/// 结果经 `_persist_probe_result` 落 sqlite 健康表。
/// Err = provider 不存在(调用方 404,文案 "Provider not found: <id>")。
async fn svc_probe_provider(
    state: &AdminState,
    snapshot: &crate::gateway::ConfigSnapshot,
    provider_id: &str,
) -> Result<Value, String> {
    let Some(provider) = snapshot.providers.iter().find(|p| p.id == provider_id) else {
        return Err(format!("Provider not found: {provider_id}"));
    };
    let started = std::time::Instant::now();
    let url = super::connectivity_probe::probe_endpoint(provider);
    let proxy_config = super::config::admin_proxy_config(state).await;
    // 20260906_01:健康探针走该供应商自己的代理覆盖 —— 与实际转发同一
    // 出口,否则「探针通、转发不通」会误导排障。
    let decision = wr_infra::proxy::resolve_proxy_decision_with_override(
        proxy_config.as_ref(),
        provider.proxy.as_ref(),
        &provider.base_url,
    );
    let mut builder = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs_f64(
            provider.timeout_seconds.max(0.0),
        ))
        .redirect(reqwest::redirect::Policy::limited(20));
    builder = match decision {
        wr_infra::proxy::ProxyDecision::Direct => builder.no_proxy(),
        wr_infra::proxy::ProxyDecision::Url(proxy) => match reqwest::Proxy::all(&proxy) {
            Ok(proxy) => builder.proxy(proxy),
            Err(_) => builder.no_proxy(),
        },
    };
    let client = builder.build().unwrap_or_else(|_| reqwest::Client::new());
    let mut request = client.get(&url).timeout(std::time::Duration::from_secs_f64(
        provider.timeout_seconds.clamp(0.0, 15.0),
    ));
    // 健康探针与转发同一出站头构造(20261016_03 批3):preset 生效;
    // passthrough 无入站上下文(inbound = None)自然降级为仅注入认证。
    for (key, value) in &wr_domain::protocol::upstream_headers_with_fingerprint(provider, None) {
        if let Some(text) = value.as_str() {
            request = request.header(key.as_str(), text);
        }
    }
    let duration = |started: std::time::Instant| -> f64 {
        (started.elapsed().as_secs_f64() * 1000.0 * 100.0).round() / 100.0
    };
    let result = match request.send().await {
        Ok(response) => {
            let status = i64::from(response.status().as_u16());
            json!({
                "ok": status < 400,
                "provider": provider.id,
                "status": status,
                "url": url,
                "duration_ms": duration(started),
            })
        }
        Err(exc) => json!({
            "ok": false,
            "provider": provider.id,
            "status": Value::Null,
            "url": url,
            "duration_ms": duration(started),
            "error": exc.to_string(),
        }),
    };
    persist_probe_result(state, &result).await;
    Ok(result)
}

/// `_persist_probe_result`(镜像 provider_selection.py:162-188):ok →
/// classify_status_to_health(status),否则 "down";checked_at 用 Python
/// `datetime.now(timezone.utc).isoformat()` 同形(微秒 + +00:00)。
async fn persist_probe_result(state: &AdminState, probe_result: &Value) {
    let ok = probe_result
        .get("ok")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let status_code = probe_result.get("status").and_then(Value::as_i64);
    let latency_ms = probe_result
        .get("duration_ms")
        .and_then(Value::as_f64)
        .unwrap_or(0.0) as i64;
    let status = if !ok {
        "down".to_owned()
    } else {
        wr_domain::gateway::classify_status_to_health(status_code).to_owned()
    };
    let row = wr_app::ports::HealthRow {
        provider_id: probe_result
            .get("provider")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        status,
        available: ok,
        latency_ms,
        checked_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, false),
        error: probe_result
            .get("error")
            .filter(|v| py_bool(v))
            .and_then(Value::as_str)
            .map(str::to_owned),
    };
    if let Err(e) = state.store.save_health(&row).await {
        tracing::warn!(target: "wrouter.admin", %e, "probe_health_persist_failed");
    }
}

/// REST GET /api/v1/providers/health(镜像 provider_selection.provider_health:
/// sqlite 健康行 × 内存熔断快照合并;无记录 → status="unknown",
/// available = 熔断未 open,且无 latency_ms/checked_at 键)。
pub async fn rest_provider_health(State(state): State<AdminState>) -> Response {
    let Ok(snapshot) = state.deps.snapshot().await else {
        return super::config::plain_500();
    };
    let breaker_config = snapshot.config.reliability.circuit_breaker.clone();
    let stored = state.store.load_all_health().await.unwrap_or_default();
    let items: Vec<Value> = snapshot
        .providers
        .iter()
        .map(|p| {
            let breaker_snapshot = state.deps.breaker.snapshot(&p.id, Some(p), &breaker_config);
            match stored.iter().find(|(pid, _)| *pid == p.id) {
                Some((_, row)) => {
                    let mut entry = json!({
                        "provider": p.id,
                        "status": row.status,
                        "available": row.available,
                        "latency_ms": row.latency_ms,
                        "checked_at": row.checked_at,
                        "failures": breaker_snapshot.failures,
                        "open_until": breaker_snapshot.opened_until_ms,
                        "breaker_state": breaker_snapshot.state,
                    });
                    if let Some(error) = &row.error {
                        entry["error"] = json!(error);
                    }
                    entry
                }
                None => json!({
                    "provider": p.id,
                    "status": "unknown",
                    "available": breaker_snapshot.state != "open",
                    "failures": breaker_snapshot.failures,
                    "open_until": breaker_snapshot.opened_until_ms,
                    "breaker_state": breaker_snapshot.state,
                }),
            }
        })
        .collect();
    Json(Value::Array(items)).into_response()
}

/// REST POST /api/v1/providers/health/check(镜像 probe_all_providers:
/// 逐个顺序探活并返回新快照,写 sqlite)。
pub async fn rest_probe_all_providers(State(state): State<AdminState>) -> Response {
    let Ok(snapshot) = state.deps.snapshot().await else {
        return super::config::plain_500();
    };
    let mut results: Vec<Value> = Vec::new();
    for provider in &snapshot.providers {
        if let Ok(result) = svc_probe_provider(&state, &snapshot, &provider.id).await {
            results.push(result);
        }
    }
    Json(Value::Array(results)).into_response()
}

// ---------------------------------------------------------------------------
// 其余 REST 门面
// ---------------------------------------------------------------------------

pub async fn rest_status(State(state): State<AdminState>) -> Json<Value> {
    let s = &state.settings;
    Json(json!({
        "ok": true,
        "name": s.app_name,
        "version": VERSION,
        "ports": {"gateway": s.gateway_port, "web": s.web_port},
        "dataDir": s.data_dir.to_string_lossy(),
    }))
}

pub async fn rest_metrics(
    State(state): State<AdminState>,
    Query(query): Query<std::collections::HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let fmt_prom = query
        .get("format")
        .map(|f| f.eq_ignore_ascii_case("prometheus") || f.eq_ignore_ascii_case("prom"))
        .unwrap_or(false);
    let accept_prom = headers
        .get(axum::http::header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(|a| a.to_ascii_lowercase().contains("text/plain"))
        .unwrap_or(false);
    if fmt_prom || accept_prom {
        let body = state.deps.metrics.render_prometheus(false, &[]);
        Response::builder()
            .status(StatusCode::OK)
            .header(
                axum::http::header::CONTENT_TYPE,
                "text/plain; version=0.0.4; charset=utf-8",
            )
            .body(axum::body::Body::from(body))
            .unwrap()
    } else {
        Json(state.deps.metrics.render_json()).into_response()
    }
}

// ---------------------------------------------------------------------------
// /events SSE
// ---------------------------------------------------------------------------

/// GET /api/wr/events(镜像 app.py events + R5 事件总线):
/// 首帧心跳立即发,之后 select! 总线事件即时转发 / 1s 心跳。
///
/// **停机时主动收流**(2026-11-03):整进程停机开始时先发一帧
/// `service-stopping`,随即结束响应体。少了这一段,浏览器的长连接会让
/// axum 的优雅停机一直等不到连接结束(`close_tx.closed().await`),
/// 表现为「端口已关但进程残留、systemd 仍显示 running」。
pub async fn events_endpoint(State(state): State<AdminState>) -> Response {
    use tokio::time::{Duration, Instant, interval_at};
    let mut subscriber = state.deps.subscribe_event();
    let version = VERSION.to_owned();
    let port = i64::from(state.settings.gateway_port);
    // 停机信号:页面「停止服务」或外部停机信号都会置位。
    let mut stop_rx = state.control.stop_requested_rx();
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Vec<u8>, std::convert::Infallible>>(16);
    tokio::spawn(async move {
        let heartbeat = || {
            crate::gateway::events::gateway_status_event(true, &version, port, epoch_ms())
                .to_sse_frame()
                .into_bytes()
        };
        if tx.send(Ok(heartbeat())).await.is_err() {
            return;
        }
        // interval 首次 tick 立即触发,从 +1s 起算避免首秒双心跳。
        let mut tick = interval_at(
            Instant::now() + Duration::from_secs(1),
            Duration::from_secs(1),
        );
        loop {
            tokio::select! {
                maybe = subscriber.next() => match maybe {
                    Some(event) => {
                        if tx.send(Ok(event.to_sse_frame().into_bytes())).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                },
                _ = tick.tick() => {
                    if tx.send(Ok(heartbeat())).await.is_err() {
                        break;
                    }
                }
                _ = wait_stop_requested(&mut stop_rx) => {
                    // 停机通告:最后一帧告诉客户端「正在停止」,随后收流
                    // (响应体结束 → 连接关闭 → 优雅停机得以继续)。
                    let notice = crate::gateway::events::service_stopping_event(
                        "shutdown",
                        epoch_ms(),
                    )
                    .to_sse_frame()
                    .into_bytes();
                    let _ = tx.send(Ok(notice)).await;
                    break;
                }
            }
        }
    });
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });
    let mut resp = Response::new(axum::body::Body::from_stream(stream));
    *resp.status_mut() = StatusCode::OK;
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/event-stream"),
    );
    resp.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-cache"),
    );
    resp.headers_mut().insert(
        axum::http::header::HeaderName::from_static("x-accel-buffering"),
        axum::http::HeaderValue::from_static("no"),
    );
    resp
}

fn epoch_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 等待停机请求置位(与 `lib.rs::wait_for_value` 同构)。
///
/// `watch::Receiver::wait_for` 在 await 期间持读锁 → 返回的 future
/// **不是 Send**,不能进 `tokio::select!` 里的 `tokio::spawn` 任务。
/// 这里改成「`borrow_and_update()` 先判值,不满足再 `changed().await`」。
///
/// 控制通道被丢弃(理论上不会:控制通道由 `AdminState` 持有)也按停机处理
/// ——宁可提前收流,也不能让长连接把停机拖成无限等待。
async fn wait_stop_requested(receiver: &mut tokio::sync::watch::Receiver<bool>) {
    loop {
        if *receiver.borrow_and_update() {
            return;
        }
        if receiver.changed().await.is_err() {
            return;
        }
    }
}
