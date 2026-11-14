//! 本机管理面 RPC 调用骨架(2026-11-07 W4):`config.rs` / `bridge.rs` 共用
//! 同一份信封解析,避免三处 `post + ok/value` 分支漂移。

use serde_json::Value;
use wr_server::settings::Settings;

/// 调用 `POST /api/wr/rpc` 并解出信封里的 `value` 字段。
///
/// `label` 只用于错误文案,保持各调用点原有的可观测措辞。
pub async fn rpc_value(
    settings: &Settings,
    method: &str,
    args: Value,
    label: &str,
) -> Result<Value, String> {
    let body = serde_json::json!({ "method": method, "args": args }).to_string();
    let response = crate::http::post(
        settings.web_port,
        "/api/wr/rpc",
        &[
            ("content-type", "application/json"),
            ("x-wrouter-web-auth", settings.auth_token.as_str()),
        ],
        &body,
    )
    .await?;
    let payload: Value = serde_json::from_str(&response)
        .map_err(|error| format!("{label} 响应不是合法 JSON: {error}"))?;
    if payload.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(format!("{label} RPC 失败: {payload}"));
    }
    payload
        .get("value")
        .cloned()
        .ok_or_else(|| format!("{label} 响应缺少 value 字段"))
}
