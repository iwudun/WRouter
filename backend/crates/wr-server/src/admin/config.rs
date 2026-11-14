//! Config 组:RPC 分发器(POST /api/wr/rpc) + config REST。
//!
//! 镜像 Python `handlers/config.py`、`rpc_dispatch.py` 与
//! `config_service.py` 的复合逻辑。api_key 三态(C2)由底层
//! `SqliteStore::save_payload`(镜像 `save_config`)处理——本层**必须**
//! 用 `to_payload_text(true)` 序列化(明文进入三态判定;落库侧会把密钥
//! 剥进 secrets 并把 payload 置空串)。用 `false` 会让 "***" 永远
//! 不更新密钥、APIKEYS 被清空(R5 自查修正)。

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use super::{AdminState, status, usage};
use crate::gateway::events;
use wr_infra::config::AppConfig;
use wr_infra::sqlite_config::SqliteConfigSource;

/// 显式 501 文案(rpc_dispatch.py:176-184)。
const NOT_IMPL: &[(&str, &str)] = &[
    (
        "openExternal",
        "Opening external URLs is a desktop concern (desktop shell). 501 in web mode.",
    ),
    (
        "updateCheck",
        "Self-update checks are not available in this build (no update channel). 501 in web mode.",
    ),
    (
        "updateDownload",
        "Self-update downloads are not available in this build (no update channel). 501 in web mode.",
    ),
    (
        "updateInstall",
        "Self-update installation is not available in this build (no update channel). 501 in web mode.",
    ),
];

fn is_known(m: &str) -> bool {
    matches!(
        m,
        "getConfig"
            | "saveConfig"
            | "saveApiKeys"
            | "getGatewayStatus"
            | "getAppInfo"
            | "checkProviderConnectivity"
            | "getProviderAccountSnapshots"
            | "probeProviderCandidates"
            | "fetchProviderModels"
            | "testApiEndpoints"
            | "getUsageStats"
            | "getApiKeyLastUsed"
            | "getRequestLogDetail"
            | "getRequestLogTrace"
            | "getCallLogs"
            | "exportData"
            | "getUpdateStatus"
            | "getProviderCredentialStats"
            | "clearCredentialCooldown"
            | "getSavingsStats"
            | "testProxyPreset"
            // 方案 20261030_01:模型指纹检测与指纹库管理。
            | "detectModelFingerprint"
            | "listModelFingerprints"
            | "enrollModelFingerprint"
            | "deleteModelFingerprint"
            // 方案 §3.7「导入/导出」区块:§3.8 的 RPC 表未分配这两个方法,
            // 补齐后前端才能真正落地导入导出。
            | "exportModelFingerprints"
            | "importModelFingerprint"
            // 20261102_01 P6:服务控制(暂停 / 恢复 / 退出服务)——
            // 通用能力,所有运行时都走同一条 RPC 面。
            | "getRuntimeControl"
            | "setGatewayEnabled"
            | "stopService"
    )
}

fn not_impl_resp(method: &str, message: &str) -> Response {
    let detail = serde_json::json!({
        "ok": false,
        "error": {"code": "NOT_IMPLEMENTED", "message": message, "method": method},
    });
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({"detail": detail})),
    )
        .into_response()
}

/// FastAPI 未捕获异常的默认 500 形态(纯文本)。
pub fn plain_500() -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
}

/// POST /api/wr/rpc(镜像 app.py rpc 端点 + rpc_dispatch.dispatch)。
pub async fn rpc_dispatch(State(state): State<AdminState>, Json(body): Json<Value>) -> Response {
    let method = body
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    // Python: args = payload.get("args") or [](非数组同样落到 [])。
    let args: Vec<Value> = body
        .get("args")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    if let Some((m, msg)) = NOT_IMPL.iter().find(|(m, _)| **m == method) {
        return not_impl_resp(m, msg);
    }
    if !is_known(&method) {
        return not_impl_resp(
            &method,
            &format!("Method '{method}' is not implemented in WRouter. Tracked for P13/P14."),
        );
    }

    match dispatch(&state, &method, &args).await {
        Ok(value) => Json(serde_json::json!({"ok": true, "value": value})).into_response(),
        // Python:handler 内未捕获异常 → FastAPI 500 纯文本。
        Err(message) => {
            tracing::error!(target: "wrouter.rpc", %method, %message, "rpc_handler_failed");
            plain_500()
        }
    }
}

async fn dispatch(state: &AdminState, method: &str, args: &[Value]) -> Result<Value, String> {
    match method {
        "getConfig" => Ok(get_config(state).await),
        "saveConfig" => save_config(state, args).await,
        "saveApiKeys" => save_api_keys(state, args).await,
        "getGatewayStatus" => Ok(status::svc_gateway_status(state).await),
        "getAppInfo" => Ok(status::svc_app_info(state)),
        "getUpdateStatus" => Ok(status::svc_update_status()),
        "exportData" => Ok(export_value(state)),
        // 20261102_01 P6:服务控制三件套(暂停 / 恢复 / 退出服务)。
        "getRuntimeControl" => Ok(super::service_control::svc_runtime_control(state)),
        "setGatewayEnabled" => {
            super::service_control::svc_set_gateway_enabled(state, args)
        }
        "stopService" => super::service_control::svc_stop_service(state),
        "getUsageStats" => usage::svc_usage_stats(&state.store, args).await,
        "checkProviderConnectivity" => {
            let payload = args
                .first()
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let proxy_config = admin_proxy_config(state).await;
            super::connectivity_probe::check_connectivity(&payload, proxy_config.as_ref())
                .await
                .map_err(|e| format!("checkProviderConnectivity: {e}"))
        }
        "probeProviderCandidates" => {
            let payload = args
                .first()
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let proxy_config = admin_proxy_config(state).await;
            super::connectivity_probe::probe_candidates(&payload, proxy_config.as_ref())
                .await
                .map_err(|e| format!("probeProviderCandidates: {e}"))
        }
        // 方案 20261017_01:添加模型弹窗按协议拉上游模型列表。
        "fetchProviderModels" => {
            let payload = args
                .first()
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let proxy_config = admin_proxy_config(state).await;
            super::connectivity_probe::fetch_provider_models(&payload, proxy_config.as_ref())
                .await
                .map_err(|e| format!("fetchProviderModels: {e}"))
        }
        "testApiEndpoints" => {
            let payload = args
                .first()
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let proxy_config = admin_proxy_config(state).await;
            super::speedtest::test_endpoints(&payload, proxy_config.as_ref())
                .await
                .map_err(|e| format!("testApiEndpoints: {e}"))
        }
        // 20260906_01 B5:用**给定预设**(含未保存的编辑值)探测固定目标,
        // 代理就是被测对象本身,故不取全局配置。
        "testProxyPreset" => {
            let payload = args
                .first()
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            super::connectivity_probe::test_proxy_preset(&payload)
                .await
                .map_err(|e| format!("testProxyPreset: {e}"))
        }
        "getProviderAccountSnapshots" => {
            super::account_snapshot::svc_account_snapshots(state, args).await
        }
        "getApiKeyLastUsed" => usage::svc_last_used(&state.store).await,
        "getRequestLogDetail" => usage::svc_request_log_detail(&state.store, args).await,
        "getRequestLogTrace" => usage::svc_request_log_trace(&state.store, args).await,
        "getCallLogs" => usage::svc_call_logs(state, args).await,
        // 凭据池状态(方案 20260905_02 K5;Rust 新增,无 Python 镜像)。
        "getProviderCredentialStats" => {
            super::credential_stats::svc_credential_stats(state, args).await
        }
        "clearCredentialCooldown" => {
            super::credential_stats::svc_clear_credential_cooldown(state, args).await
        }
        // 节省度量(方案 20260905_03 S8;Rust 新增,无 Python 镜像)。
        "getSavingsStats" => super::savings::svc_savings_stats(&state.store, args).await,
        // 方案 20261030_01:模型指纹检测与指纹库管理(4 个管理 RPC + 导入导出 2 个;
        // 导入导出实现在 model_fingerprint_transfer,探针执行在 model_fingerprint_probe)。
        "detectModelFingerprint" => {
            super::model_fingerprint::detect_model_fingerprint(state, args).await
        }
        "listModelFingerprints" => {
            super::model_fingerprint::list_model_fingerprints(state, args).await
        }
        "enrollModelFingerprint" => {
            super::model_fingerprint::enroll_model_fingerprint(state, args).await
        }
        "deleteModelFingerprint" => {
            super::model_fingerprint::delete_model_fingerprint(state, args).await
        }
        "exportModelFingerprints" => {
            super::model_fingerprint_transfer::export_model_fingerprints(state, args).await
        }
        "importModelFingerprint" => {
            super::model_fingerprint_transfer::import_model_fingerprints(state, args).await
        }
        other => Err(format!(
            "method '{other}' reached dispatch but is not wired"
        )),
    }
}

// ---------------------------------------------------------------------------
// config 服务(RPC 与 REST 共用)
// ---------------------------------------------------------------------------

/// 探针/账号快照共用的出站代理决策入参(镜像 `container._proxy_config`
/// = dict(app_config.proxy);快照失败回落 None → 与空 dict 的 Direct 同值)。
pub async fn admin_proxy_config(state: &AdminState) -> Option<serde_json::Map<String, Value>> {
    match state.deps.snapshot().await {
        Ok(snapshot) => Some(snapshot.config.proxy.clone()),
        Err(e) => {
            tracing::warn!(target: "wrouter.admin", %e, "admin_proxy_config_snapshot_failed");
            None
        }
    }
}

/// account 级密钥注水(Python load_with_secrets 的 accounts 循环):
/// provider 级由 `load_app_config` 注水,account 级在此补。
pub async fn hydrate_account_secrets(state: &AdminState, config: &mut AppConfig) {
    let store = &state.store;
    for provider in &mut config.providers {
        let Some(pid) = provider
            .raw
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            continue;
        };
        let Some(accounts) = provider
            .raw
            .get_mut("accounts")
            .and_then(Value::as_array_mut)
        else {
            continue;
        };
        for account in accounts.iter_mut() {
            let Some(map) = account.as_object_mut() else {
                continue;
            };
            let Some(aid) = map
                .get("id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
            else {
                continue;
            };
            if let Ok(secret) =
                wr_app::ports::SecretStore::account_secret(store.as_ref(), &pid, &aid).await
                && !secret.is_empty()
            {
                map.insert("api_key".to_owned(), Value::String(secret));
            }
        }
    }
}

/// `load_with_secrets` 等价(镜像 config_service.py:38-66):
/// provider 级密钥由 `load_app_config` 注水;account 级在此补注水;
/// 最终 `to_dict(true)` 输出明文形态。
pub async fn get_config(state: &AdminState) -> Value {
    let source = SqliteConfigSource::new(&state.deps.settings.data_dir);
    let mut config = match source.load_app_config() {
        Ok(config) => config,
        Err(e) => {
            tracing::warn!(target: "wrouter.admin", %e, "get_config_load_failed");
            AppConfig::default()
        }
    };
    hydrate_account_secrets(state, &mut config).await;
    config.to_dict(true)
}

/// 保存路径共用:AppConfig → 明文 payload(三态在 store 侧判定)→ save
/// → 快照失效 → config-changed 事件(`scope` 镜像 Python 补丁:
/// saveConfig→"config"、saveApiKeys→"api-keys")。
async fn persist_and_reload(
    state: &AdminState,
    config: &AppConfig,
    scope: &str,
) -> Result<(), String> {
    persist_snapshot(state, config).await?;
    state
        .deps
        .events
        .publish(events::config_changed_event(scope));
    Ok(())
}

/// 只保存 + 失效快照,**不发事件**(provider CRUD 路径,镜像 Python
/// set_provider/delete_provider 不发 config-changed)。
pub async fn persist_snapshot(state: &AdminState, config: &AppConfig) -> Result<(), String> {
    let text = config.to_payload_text(true);
    let payload: Value =
        serde_json::from_str(&text).map_err(|e| format!("payload reserialize failed: {e}"))?;
    state
        .deps
        .store
        .save_payload(&payload)
        .await
        .map_err(|e| e.to_string())?;
    state.deps.invalidate_snapshot().await;
    Ok(())
}

/// `saveConfig`:args[0] 非 dict → {};校验失败 → 500(镜像未捕获 ValueError)。
async fn save_config(state: &AdminState, args: &[Value]) -> Result<Value, String> {
    let payload = args
        .first()
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let config = AppConfig::from_json(&Value::Object(payload))
        .map_err(|e| format!("saveConfig invalid payload: {e}"))?;
    validate_persist_config(state, &config)?;
    persist_and_reload(state, &config, "config").await?;
    Ok(get_config(state).await)
}

/// 写路径共用的保存期校验(方案 20260906_01 §4.8)。
///
/// 只在**保存**时生效:加载侧([`AppConfig::from_json`])保持宽松,避免
/// 手改/跨版本配置让应用起不来(与决策 #7 同源)。当前只有代理预设规则
/// (id/name 唯一、端口与 scheme 合法、拒绝指向网关自身端口的自指代理)
/// 与客户端指纹协议配对门禁(20261016_03 §3.5)。
fn validate_persist_config(state: &AdminState, config: &AppConfig) -> Result<(), String> {
    wr_infra::config::validate_proxy_presets(&config.proxy, state.settings.gateway_port)
        .map_err(|e| e.to_string())?;
    for (index, record) in config.providers.iter().enumerate() {
        validate_client_fingerprint_raw(&record.raw, &format!("providers[{index}]"))?;
    }
    Ok(())
}

/// 客户端指纹协议配对门禁(20261016_03 §3.5,单条;saveConfig 与
/// provider upsert 两个写入口共用)。
///
/// `preset` 模式要求:①presetId 必须命中内置指纹表(未知 id 拒绝 ——
/// 防手改/版本更迭后静默归 off);②预设声明的 compatibleProtocols 必须覆盖
/// 该 provider 的有效上游线格式(防「Claude Code 预设打到 openai 上游」
/// 凭空制造 400)。前端下拉禁选是主防线,这里是后端闸门。
pub fn validate_client_fingerprint_raw(
    raw: &serde_json::Map<String, Value>,
    label: &str,
) -> Result<(), String> {
    use wr_domain::provider::{ClientFingerprintMode, ClientFingerprintOverride};
    let Some(fingerprint) = ClientFingerprintOverride::from_value(
        raw.get("clientFingerprint").or_else(|| raw.get("client_fingerprint")),
    ) else {
        return Ok(());
    };
    let ClientFingerprintMode::Preset = fingerprint.mode else {
        return Ok(());
    };
    let preset_id = fingerprint.preset_id.as_deref().unwrap_or_default();
    let Some(preset) = wr_domain::protocol::client_fingerprint::find_preset(preset_id) else {
        return Err(format!(
            "{label}: unknown client fingerprint preset '{preset_id}'"
        ));
    };
    let wire = wr_domain::provider::Provider::from_raw(raw)
        .map(|provider| provider.effective_wire_protocol().to_owned())
        .unwrap_or_default();
    if !preset.compatible_with(&wire) {
        return Err(format!(
            "{label}: client fingerprint preset '{preset_id}' does not support upstream wire format '{wire}'"
        ));
    }
    Ok(())
}

/// `saveApiKeys`:归一化(key trim 非空、按 id 去重末位胜)+ 剪路由绑定
/// (镜像 config_service.save_api_keys + _strip_key_bindings)。
async fn save_api_keys(state: &AdminState, args: &[Value]) -> Result<Value, String> {
    let keys = args
        .first()
        .and_then(Value::as_array)
        .ok_or("saveApiKeys expects a list of ApiKeyConfig")?;

    let mut normalized: Vec<Value> = Vec::new();
    let mut seen_ids: std::collections::HashMap<String, usize> = Default::default();
    for entry in keys {
        let Some(map) = entry.as_object() else {
            continue; // Python: 非 dict 项跳过
        };
        let key = map
            .get("key")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        if key.is_empty() {
            continue; // 空 key 条目跳过
        }
        let mut item = map.clone();
        item.insert("key".to_owned(), Value::String(key.to_owned()));
        let id = item
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("")
            .to_owned();
        if !id.is_empty() {
            if let Some(prev) = seen_ids.get(&id) {
                normalized[*prev] = Value::Null; // 末位胜:占位后重写
            }
            seen_ids.insert(id, normalized.len());
        }
        normalized.push(Value::Object(item));
    }
    normalized.retain(|v| !v.is_null());

    let source = SqliteConfigSource::new(&state.deps.settings.data_dir);
    let mut config = source.load_app_config().map_err(|e| e.to_string())?;
    // removed = 旧列表有 id 且不在新列表。
    let removed: std::collections::HashSet<String> = config
        .api_keys
        .iter()
        .filter_map(Value::as_object)
        .filter_map(|m| m.get("id").and_then(Value::as_str))
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty() && !seen_ids.contains_key(s))
        .collect();
    config.api_keys = normalized;
    // 20261101_02 T3:`_strip_key_bindings` 随路由规则域删除(规则级
    // apiKeyIds 绑定不复存在);removed 集合保留给后续清理用途前先弃用。
    let _ = removed;
    persist_and_reload(state, &config, "api-keys").await?;
    Ok(get_config(state).await)
}

/// 导出信封(`config_service.export`:注意用**未注水**的 load,to_dict(false))。
pub fn export_value(state: &AdminState) -> Value {
    let source = SqliteConfigSource::new(&state.deps.settings.data_dir);
    let payload = source.load_payload().unwrap_or_else(|e| {
        tracing::warn!(target: "wrouter.admin", %e, "export_load_failed");
        Value::Object(Default::default())
    });
    let config = AppConfig::from_json(&payload).unwrap_or_default();
    serde_json::json!({
        "schema": "wrouter.config",
        "schemaVersion": 2,
        "config": config.to_dict(false),
    })
}

// ---------------------------------------------------------------------------
// REST 门面(config)
// ---------------------------------------------------------------------------

pub async fn rest_get_config(State(state): State<AdminState>) -> Json<Value> {
    Json(get_config(&state).await)
}

pub async fn rest_put_config(State(state): State<AdminState>, Json(body): Json<Value>) -> Response {
    match AppConfig::from_json(&body) {
        Ok(config) => {
            if let Err(e) = validate_persist_config(&state, &config) {
                tracing::warn!(target: "wrouter.admin", %e, "rest_put_config_validation_failed");
                return plain_500();
            }
            match persist_and_reload(&state, &config, "config").await {
                Ok(()) => Json(get_config(&state).await).into_response(),
                Err(_) => plain_500(),
            }
        }
        Err(_) => plain_500(),
    }
}

pub async fn rest_export_config(State(state): State<AdminState>) -> Json<Value> {
    Json(export_value(&state))
}

pub async fn rest_import_config(
    State(state): State<AdminState>,
    Json(body): Json<Value>,
) -> Response {
    // unwrap:{"config": {...}} 或裸 dict。
    let payload = match body.get("config") {
        Some(Value::Object(map)) => map.clone(),
        _ => body.as_object().cloned().unwrap_or_default(),
    };
    let config = match AppConfig::from_json(&Value::Object(payload)) {
        Ok(config) => config,
        Err(_) => return plain_500(),
    };
    if let Err(e) = validate_persist_config(&state, &config) {
        tracing::warn!(target: "wrouter.admin", %e, "rest_import_config_validation_failed");
        return plain_500();
    }
    if persist_and_reload(&state, &config, "config").await.is_err() {
        return plain_500();
    }
    Json(serde_json::json!({"ok": true, "config": config.to_dict(false)})).into_response()
}
