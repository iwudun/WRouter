//! 读后端配置的桌面壳侧小工具(20261102_01 P4-3 / P4-5)。
//!
//! 桌面壳同进程跑后端,管理面就在回环端口上 → 直接打 RPC 即可,
//! 不需要另起一套配置存储。

use serde_json::Value;
use wr_server::settings::Settings;

/// 取回管理面 `getConfig` 的配置对象。
pub async fn read_config(settings: &Settings) -> Result<Value, String> {
    crate::rpc::rpc_value(settings, "getConfig", serde_json::json!([]), "getConfig").await
}

/// 取配置里的 `launchAtLogin`(缺失/非布尔 → false)。
pub fn launch_at_login(config: &Value) -> bool {
    config
        .get("launchAtLogin")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// 取配置里的 `theme`(缺失/非字符串 → `"system"`)。
pub fn theme(config: &Value) -> String {
    config
        .get("theme")
        .and_then(Value::as_str)
        .unwrap_or("system")
        .to_owned()
}
