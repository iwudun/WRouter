//! Tauri 桥接层的**纯逻辑**部分(20261102_01 P3-4)。
//!
//! 这里刻意**不依赖 tauri**:桌面方法清单、注入脚本模板、URL 白名单校验
//! 都是纯字符串/纯函数逻辑,放在独立模块 →
//!
//! - 可以被 `tests/pure_logic.rs` 以 `#[path]` 引入,在**不链接 WebView2**
//!   的测试目标里直接跑(Windows GNU 工具链下,链接了 webview2 的
//!   test harness 会因装载器缺失而起不来 —— 见 §9.4 踩坑清单);
//! - 与「壳」解耦:契约校验脚本按源码解析 `DESKTOP_METHODS`。

/// 桌面方法清单(Tauri 侧)。
///
/// 与 Electron preload 的 `IPC_CHANNEL_MAP` 同名集合 —— 同一份前端 bundle
/// 要能同时跑在 Electron / Tauri / 浏览器三种运行时上。
pub const DESKTOP_METHODS: &[&str] = &[
    "exportData",
    "getUpdateStatus",
    "openExternal",
    "setAppLanguage",
    "toggleDevTools",
    "updateCheck",
    "updateDownload",
    "updateInstall",
];

/// 注入 webview 的初始化脚本。
///
/// Tauri 自身的脚本(含 `window.__TAURI__` 全局 API)先于本脚本执行,
/// 但这里仍在**调用时**才解析 `window.__TAURI__`,避免依赖注入顺序。
pub fn init_script(is_dev: bool) -> String {
    const TEMPLATE: &str = r#"(function () {
  if (window.wr) {
    return;
  }
  var isDev = __IS_DEV__;
  function invoke(cmd, args) {
    var tauri = window.__TAURI__;
    if (!tauri || !tauri.core || typeof tauri.core.invoke !== "function") {
      return Promise.reject(new Error("Tauri IPC unavailable"));
    }
    return tauri.core.invoke(cmd, args);
  }
  function noopSubscription() {
    return function () {};
  }
  window.wr = {
    runtime: "desktop",
    isDev: isDev,
    openExternal: function (url) {
      return invoke("wr_open_external", { url: url });
    },
    toggleDevTools: function () {
      return invoke("wr_toggle_devtools", {});
    },
    setAppLanguage: function (language) {
      return invoke("wr_set_app_language", { language: language });
    },
    exportData: function () {
      return invoke("wr_export_data", {});
    },
    getUpdateStatus: function () {
      return invoke("wr_get_update_status", {});
    },
    updateCheck: function () {
      return invoke("wr_update_check", {});
    },
    updateDownload: function () {
      return invoke("wr_update_download", {});
    },
    updateInstall: function () {
      return invoke("wr_update_install", {});
    },
    onOpenSettingsRequest: noopSubscription,
    onOpenUpdateRequest: noopSubscription,
    onUpdateStatusChanged: noopSubscription
  };
})();
"#;
    TEMPLATE.replace("__IS_DEV__", if is_dev { "true" } else { "false" })
}

/// 只允许 http / https 链接(与前端 `web-client-bridge.ts` 的校验一致)。
///
/// 刻意不依赖 `tauri::Url`(那会把纯逻辑模块拖回 tauri 依赖)——
/// 前端已用 `new URL()` 严格校验过一次,这里是第二道防线:
/// 协议必须是 http/https,且必须有非空 host。
pub fn normalize_external_http_url(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    let lower = trimmed.to_ascii_lowercase();
    if !lower.starts_with("http://") && !lower.starts_with("https://") {
        return Err("只允许打开 http / https 链接".to_string());
    }
    let rest = trimmed.split_once("://").map(|(_, tail)| tail).unwrap_or("");
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    if host.is_empty() {
        return Err(format!("URL 格式非法: {trimmed}"));
    }
    Ok(trimmed.to_string())
}
