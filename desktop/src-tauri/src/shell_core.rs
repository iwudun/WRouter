//! 桌面壳**窗口层**的纯逻辑(20261102_01 P4-4 / P4-5)。
//!
//! 与 [`crate::bridge_core`] 同理:刻意**不依赖 tauri**,好让
//! `tests/pure_logic.rs` 用 `#[path]` 引入、在不链接 WebView2 的测试目标里
//! 直接跑(Windows GNU 工具链下链接了 webview2 的 harness 起不来 ——
//! 见方案文档 §9.4 踩坑清单)。
//!
//! 职责:
//! - **标题栏配色**的十六进制解析与主题解析(前端把 CSS 变量
//!   `--background` 的值传进来,这里解析成 RGBA);
//! - **窗口主题偏好**的归一(`light` / `dark` / `system`)。

/// 浅色页面背景(`#f4f4f6`)。
///
/// 与前端 `styles/globals.css` 的 `--background`(浅色)同值,也与
/// Electron 壳 `electron/src/windows.ts:20` 的 `titlebarOverlayColors()`
/// 同口径 —— 窗口底色与页面底色一致,最大化/缩放时不会闪白。
pub const BACKGROUND_LIGHT: (u8, u8, u8) = (0xf4, 0xf4, 0xf6);

/// 深色页面背景(`#1a1a1e`)。
///
/// 与前端 `--background`(深色)与 Electron 壳 `titlebarOverlayColors()`
/// 同值。
pub const BACKGROUND_DARK: (u8, u8, u8) = (0x1a, 0x1a, 0x1e);

/// 窗口主题偏好。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowTheme {
    /// 强制浅色。
    Light,
    /// 强制深色。
    Dark,
    /// 跟随系统(**不强制** webview 的 `prefers-color-scheme`)。
    System,
}

impl WindowTheme {
    /// 归一前端传来的主题偏好字符串。
    ///
    /// 未知值 → `System`(跟随系统),与前端 `normalizeThemePreference()`
    /// 的兜底一致。
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "light" => Self::Light,
            "dark" => Self::Dark,
            _ => Self::System,
        }
    }

    /// 是否强制窗口主题(而非跟随系统)。
    pub fn is_forced(self) -> bool {
        !matches!(self, Self::System)
    }
}

/// 解析 `#rgb` / `#rrggbb` 形式的颜色。
///
/// 前端从 CSS 变量 `--background` 读出来的值就是 `#rrggbb` 形式;这里
/// 只认这一种(带不带 `#` 都收),其余一律报错 → 调用方退回默认色。
pub fn parse_hex_color(value: &str) -> Option<(u8, u8, u8)> {
    let trimmed = value.trim();
    let hex = trimmed.strip_prefix('#').unwrap_or(trimmed);
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    match hex.len() {
        6 => {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            Some((r, g, b))
        }
        // 三位形式(#abc)按 CSS 规则展开(#abc → #aabbcc)。
        3 => {
            let expand = |index: usize| {
                let digit = u8::from_str_radix(&hex[index..index + 1], 16).ok()?;
                Some(digit * 17)
            };
            Some((expand(0)?, expand(1)?, expand(2)?))
        }
        _ => None,
    }
}

/// 解析主题偏好 + 背景色 → 实际要用的窗口主题与底色。
///
/// - `theme`:`"light"` / `"dark"` / `"system"`(其余按 `system` 处理);
/// - `background`:前端解析出的 `#rrggbb` 底色(可空 → 按主题取默认色);
/// - `system_is_dark`:系统当前是否为深色(仅 `system` 时用来选默认底色)。
///
/// 返回 `(主题, 底色)`。`system` 时主题恒为 `System` —— **不强制**
/// webview 的 `prefers-color-scheme`,否则系统主题在运行期切换就传不进来。
pub fn resolve_window_theme(
    theme: &str,
    background: Option<&str>,
    system_is_dark: bool,
) -> (WindowTheme, (u8, u8, u8)) {
    let parsed = WindowTheme::parse(theme);
    let fallback = match parsed {
        WindowTheme::Light => BACKGROUND_LIGHT,
        WindowTheme::Dark => BACKGROUND_DARK,
        WindowTheme::System => {
            if system_is_dark {
                BACKGROUND_DARK
            } else {
                BACKGROUND_LIGHT
            }
        }
    };
    let color = background
        .and_then(parse_hex_color)
        .unwrap_or(fallback);
    (parsed, color)
}
