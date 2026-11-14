//! 开机自启(20261102_01 P4-3)。
//!
//! 与 Electron 壳的 `electron/src/launch-at-login.ts` 同口径:
//! **配置为准,OS 登录项跟着配置走** —— 启动时对账一次,设置页改动后
//! 前端再显式调一次(立即生效,不必等重启)。
//!
//! 落地依赖 `tauri-plugin-autostart`(Windows 写
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`)。

use tauri::AppHandle;
use tauri_plugin_autostart::ManagerExt;

/// 把 OS 登录项同步到目标状态(幂等:状态一致则不动)。
pub fn apply(app: AppHandle, enabled: bool) -> Result<(), String> {
    let manager = app.autolaunch();
    let current = manager
        .is_enabled()
        .map_err(|error| format!("读取开机自启状态失败: {error}"))?;
    if current == enabled {
        return Ok(());
    }
    let outcome = if enabled {
        manager.enable()
    } else {
        manager.disable()
    };
    outcome.map_err(|error| format!("设置开机自启失败: {error}"))
}
