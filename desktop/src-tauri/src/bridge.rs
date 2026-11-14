//! Tauri 侧桥接层(20261102_01 P3-4)。
//!
//! 职责:在 webview 页面脚本之前注入 `window.wr`,并把**桌面方法**接到
//! Tauri 命令上。
//!
//! 与 Electron preload 的关系:
//! - Electron:`preload.ts` 在 `contextIsolated` 的独立上下文里注入
//!   `window.wr`(8 个 IPC 通道 + 25 个业务 RPC);
//! - Tauri:本模块的 `initialization_script` 在**页面脚本之前**注入
//!   `window.wr`(8 个桌面方法),前端 `web-client-bridge.ts` 随后**补齐**
//!   25 个业务方法(复用同一套 RPC 逻辑)。
//!
//! 关键约束(方案 §6 P3-4):`web-client-bridge.ts` 的 `if (!window.wr)`
//! 守卫**不得覆盖**本模块注入的值 —— 前端侧改成「就地合并」实现。
//!
//! 桌面方法清单与 `contracts/wrouter-capabilities.json` 的 `desktopMethods`
//! 一一对应;`scripts/validate-capabilities.mjs` 会正则解析本文件的
//! [`DESKTOP_METHODS`] 做三方一致校验。

use serde::Serialize;
use tauri::State;
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

use crate::bridge_core::normalize_external_http_url;
use crate::DesktopState;

/// 更新状态(形状对齐后端 `getUpdateStatus` RPC 与前端 `AppUpdateStatus`)。
///
/// **D2 决策:v2 不接自动更新通道** —— 没有 updater 插件、没有 feed。
/// 后端 `svc_update_status()` 恒定返回「不支持 / idle」的只读状态,
/// 设置页只读展示。这里**直接走后端 RPC**(不复制一份形状),
/// 单一事实源在后端。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    state: String,
    supported: bool,
    can_check: bool,
    can_download: bool,
    can_install: bool,
    current_version: String,
}

/// D2 降级常量:无更新通道时的只读状态(与后端 `svc_update_status()` 同值)。
fn read_only_update_status() -> UpdateStatus {
    UpdateStatus {
        state: "idle".to_owned(),
        supported: false,
        can_check: false,
        can_download: false,
        can_install: false,
        current_version: wr_server::VERSION.to_owned(),
    }
}

/// 导出结果(形状对齐前端 `AppDataExportResult`)。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    canceled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    file: Option<String>,
}

/// 打开外部链接(系统默认浏览器)。
#[tauri::command]
pub fn wr_open_external(app: tauri::AppHandle, url: String) -> Result<(), String> {
    let normalized = normalize_external_http_url(&url)?;
    app.opener()
        .open_url(normalized, None::<&str>)
        .map_err(|error| format!("打开外部链接失败: {error}"))
}

/// 切换开发者工具(侧栏左下角「开发」入口)。
///
/// **仅 dev 构建可用**(方案 §7 P4-1):打包版没有 DevTools,开着只会
/// 让用户看到一个空白调试面板。前端侧本来也只在 dev 壳显示该入口,
/// 这里再加一道壳级门禁。
///
/// 实现注意:`WebviewWindow::open_devtools` 只在 `debug_assertions` 下
/// (或显式开 tauri 的 `devtools` feature)才存在 —— release 编译期就
/// 没有这个方法。因此这里必须用 `#[cfg]` **编译期**分支,不能用
/// `cfg!(debug_assertions)` 运行时判断:后者在 release 构建会直接
/// E0599 编译失败(20261102_01 P5-1 打包时实测)。
#[tauri::command]
pub fn wr_toggle_devtools(window: tauri::WebviewWindow) -> Result<(), String> {
    #[cfg(debug_assertions)]
    {
        window.open_devtools();
        Ok(())
    }
    #[cfg(not(debug_assertions))]
    {
        let _ = window;
        Err("仅开发构建可打开开发者工具".to_string())
    }
}

/// 记录界面语言(Tauri 侧尚无原生菜单,P4 接线;先落状态不丢信息)。
#[tauri::command]
pub fn wr_set_app_language(
    state: State<'_, DesktopState>,
    language: String,
) -> Result<(), String> {
    let mut slot = state
        .language
        .lock()
        .map_err(|error| format!("语言状态锁异常: {error}"))?;
    *slot = Some(language);
    Ok(())
}

/// 导出数据:保存对话框 + 从后端取导出信封 + 落盘。
#[tauri::command]
pub async fn wr_export_data(
    app: tauri::AppHandle,
    state: State<'_, DesktopState>,
) -> Result<ExportResult, String> {
    let payload = fetch_export_payload(&state).await?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_file_name("wrouter-export.json")
        .add_filter("JSON", &["json"])
        .save_file(move |picked| {
            let _ = tx.send(picked);
        });
    let picked = rx
        .await
        .map_err(|error| format!("保存对话框异常: {error}"))?;
    let Some(picked) = picked else {
        return Ok(ExportResult {
            canceled: true,
            file: None,
        });
    };
    let path = picked
        .into_path()
        .map_err(|error| format!("导出路径无效: {error}"))?;
    std::fs::write(&path, payload)
        .map_err(|error| format!("写入导出文件失败: {error}"))?;
    Ok(ExportResult {
        canceled: false,
        file: Some(path.to_string_lossy().to_string()),
    })
}

/// 更新状态(D2:只读,不接自动更新通道)。
///
/// 走后端 RPC `getUpdateStatus` —— 单一事实源在后端
/// (`wr-server/src/admin/status.rs` 的 `svc_update_status()`)。
#[tauri::command]
pub async fn wr_get_update_status(
    state: State<'_, DesktopState>,
) -> Result<UpdateStatus, String> {
    match fetch_update_status(&state).await {
        Some(status) => Ok(status),
        // 后端拿不到(启动早期/异常)→ 退回本地只读状态,不让设置页崩。
        None => Ok(read_only_update_status()),
    }
}

/// 检查更新(D2:恒为「不支持」)。
#[tauri::command]
pub async fn wr_update_check(
    state: State<'_, DesktopState>,
) -> Result<UpdateStatus, String> {
    Ok(fetch_update_status(&state).await.unwrap_or_else(read_only_update_status))
}

/// 下载更新(D2:恒为「不支持」)。
#[tauri::command]
pub async fn wr_update_download(
    state: State<'_, DesktopState>,
) -> Result<UpdateStatus, String> {
    Ok(fetch_update_status(&state).await.unwrap_or_else(read_only_update_status))
}

/// 安装更新(D2:无更新可装,直接成功返回)。
#[tauri::command]
pub fn wr_update_install() -> Result<(), String> {
    Ok(())
}

/// 向本机管理面取 `getUpdateStatus` 的返回值。
async fn fetch_update_status(state: &State<'_, DesktopState>) -> Option<UpdateStatus> {
    let settings = state.settings.clone();
    let value = crate::rpc::rpc_value(
        &settings,
        "getUpdateStatus",
        serde_json::json!([]),
        "getUpdateStatus",
    )
    .await
    .ok()?;
    Some(UpdateStatus {
        state: value
            .get("state")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("idle")
            .to_owned(),
        supported: value
            .get("supported")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        can_check: value
            .get("canCheck")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        can_download: value
            .get("canDownload")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        can_install: value
            .get("canInstall")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        current_version: value
            .get("currentVersion")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(wr_server::VERSION)
            .to_owned(),
    })
}

/// 向本机管理面取导出信封(RPC `exportData`)。
async fn fetch_export_payload(state: &State<'_, DesktopState>) -> Result<String, String> {
    let settings = state.settings.clone();
    let value =
        crate::rpc::rpc_value(&settings, "exportData", serde_json::json!([]), "导出").await?;
    serde_json::to_string_pretty(&value).map_err(|error| format!("导出信封序列化失败: {error}"))
}

