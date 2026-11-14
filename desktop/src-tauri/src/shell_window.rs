//! 桌面壳的**窗口层命令**(20261102_01 P4-3 / P4-4 / P4-5)。
//!
//! 这些命令**不属于** `window.wr` 的桌面方法面(`contracts/wrouter-capabilities.json`
//! 的 `desktopMethods`,8 项)—— 那 8 项是**跨运行时公共面**(Electron / Tauri /
//! 浏览器同一份前端 bundle 都要认)。窗口控制是**壳层 chrome**:Electron 用
//! 系统绘制的 `titleBarOverlay`,Tauri 没有等价 API,只能自建。
//!
//! 因此这里走**独立命令通道**:前端用 `window.__TAURI_INTERNALS__.invoke()`
//! 直接调,不走 `window.wr` —— 契约(8 个桌面方法)保持不变,
//! `scripts/validate-capabilities.mjs` 的四方一致校验也不受影响。
//!
//! 命令清单:
//! - `wr_window_minimize` / `wr_window_toggle_maximize` / `wr_window_close`
//!   / `wr_window_is_maximized` —— 自定义标题栏的 min/max/close 键;
//! - `wr_window_apply_theme` —— 标题栏配色随应用主题同步(P4-5);
//! - `wr_sync_launch_at_login` —— 开机自启开关立即生效(P4-3)。

use tauri::window::Color;
use tauri::{AppHandle, Theme, WebviewWindow};

use crate::shell_core::{WindowTheme, resolve_window_theme};

/// 把 `(r, g, b)` 变成 Tauri 的窗口底色(不透明)。
fn opaque(rgb: (u8, u8, u8)) -> Color {
    Color(rgb.0, rgb.1, rgb.2, 0xff)
}

/// 最小化窗口。
#[tauri::command]
pub fn wr_window_minimize(window: WebviewWindow) -> Result<(), String> {
    window.minimize().map_err(|error| format!("最小化窗口失败: {error}"))
}

/// 最大化 / 还原切换。返回切换**之后**的最大化状态(前端据此换图标)。
#[tauri::command]
pub fn wr_window_toggle_maximize(window: WebviewWindow) -> Result<bool, String> {
    let maximized = window
        .is_maximized()
        .map_err(|error| format!("读取最大化状态失败: {error}"))?;
    if maximized {
        window.unmaximize().map_err(|error| format!("还原窗口失败: {error}"))?;
    } else {
        window.maximize().map_err(|error| format!("最大化窗口失败: {error}"))?;
    }
    Ok(!maximized)
}

/// 关闭窗口(= 应用退出;后端随之优雅停机)。
#[tauri::command]
pub fn wr_window_close(window: WebviewWindow) -> Result<(), String> {
    window.close().map_err(|error| format!("关闭窗口失败: {error}"))
}

/// 当前是否已最大化。
#[tauri::command]
pub fn wr_window_is_maximized(window: WebviewWindow) -> Result<bool, String> {
    window
        .is_maximized()
        .map_err(|error| format!("读取最大化状态失败: {error}"))
}

/// 标题栏配色随应用主题同步(P4-5)。
///
/// - `theme`:`"light"` / `"dark"` / `"system"`。
///   **只有显式 light/dark 才强制**窗口主题 —— 强制会让 webview 的
///   `prefers-color-scheme` 被钉死,系统主题在运行期切换就传不到前端;
///   `"system"` 必须放行(不调用 `set_theme`)。
/// - `background`:`#rrggbb` 形式的页面底色(前端从 CSS 变量 `--background`
///   读出)→ 设为 webview 默认底色,最大化/缩放时不闪白。
#[tauri::command]
pub fn wr_window_apply_theme(
    window: WebviewWindow,
    theme: String,
    background: Option<String>,
) -> Result<(), String> {
    let system_is_dark = window
        .theme()
        .map(|current| current == Theme::Dark)
        .unwrap_or(false);
    let (resolved, color) =
        resolve_window_theme(&theme, background.as_deref(), system_is_dark);
    if resolved.is_forced() {
        window
            .set_theme(Some(match resolved {
                WindowTheme::Light => Theme::Light,
                _ => Theme::Dark,
            }))
            .map_err(|error| format!("设置窗口主题失败: {error}"))?;
    }
    window
        .set_background_color(Some(opaque(color)))
        .map_err(|error| format!("设置窗口底色失败: {error}"))
}

/// 开机自启开关立即生效(P4-3)。
///
/// 前端在设置页改 `config.launchAtLogin` 后调这个命令:不必等下次启动
/// 才生效(启动时另有一次对账,见 [`crate::lib`] 的 `sync_startup_state`)。
#[tauri::command]
pub fn wr_sync_launch_at_login(app: AppHandle, enabled: bool) -> Result<(), String> {
    crate::autostart::apply(app, enabled)
}
