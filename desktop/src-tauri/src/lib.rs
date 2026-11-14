//! WRouter Tauri 桌面壳(方案 20261102_01 · P3)。
//!
//! 与 Electron 壳的关系:**同进程**跑后端(不再 spawn `wr-backend`
//! 子进程),窗口加载同一份管理面 bundle(前端一份代码服务
//! Electron / Tauri / 浏览器三种运行时)。
//!
//! 关键设计(方案 §0 D5 + §6):
//! - **D5**:窗口加载 `http://127.0.0.1:3462/`(复用 P1 的静态托管),
//!   不用 `tauri://` 资产协议;
//! - **D4**:后端只监听回环;
//! - 后端生命周期由窗口/应用生命周期驱动(见 [`backend`]);
//! - 桥接层注入 `window.wr`(`runtime: "desktop"`),见 [`bridge`]。

mod autostart;
mod backend;
// 桥接层的纯逻辑部分(桌面方法清单 / 注入脚本 / URL 白名单)——不依赖 tauri,
// 可被 `tests/pure_logic.rs` 以 `#[path]` 引入独立跑测试。
pub mod bridge_core;
// 桥接层模块对外可见:`DESKTOP_METHODS` 是 Tauri 侧桌面方法的公开声明点
// (契约校验脚本按源码解析它)。
pub mod bridge;
mod config;
mod http;
mod rpc;
// 窗口层的纯逻辑(主题归一 / 颜色解析)——同样不依赖 tauri。
pub mod shell_core;
// 窗口层命令(自定义标题栏的 min/max/close、配色同步、开机自启)。
mod shell_window;

use std::sync::{Arc, Mutex};

use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_dialog::DialogExt;
use wr_server::settings::Settings;

use backend::BackendHandle;

/// 主窗口尺寸(沿用 `electron/src/windows.ts:47-52`)。
const WINDOW_WIDTH: f64 = 1280.0;
const WINDOW_HEIGHT: f64 = 820.0;
const WINDOW_MIN_WIDTH: f64 = 960.0;
const WINDOW_MIN_HEIGHT: f64 = 640.0;

/// 桌面壳共享状态(命令与生命周期共用)。
pub struct DesktopState {
    /// 后端设置(端口、令牌、数据目录……)。
    pub settings: Arc<Settings>,
    /// 界面语言(Tauri 侧暂无原生菜单,P4 接线;先落状态不丢信息)。
    pub language: Mutex<Option<String>>,
    /// 后端句柄(停机时优雅收尾)。
    backend: Mutex<Option<BackendHandle>>,
}

impl DesktopState {
    /// 停后端(应用退出时调用)。
    pub fn shutdown(&self) {
        if let Ok(mut slot) = self.backend.lock()
            && let Some(handle) = slot.take()
        {
            // Drop 即停机(发停机信号 + 等线程收尾)。
            drop(handle);
        }
    }
}

/// 桌面壳入口。
pub fn run() {
    let app = tauri::Builder::default()
        // 单实例必须第一个注册(插件文档要求)。
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .invoke_handler(tauri::generate_handler![
            bridge::wr_open_external,
            bridge::wr_toggle_devtools,
            bridge::wr_set_app_language,
            bridge::wr_export_data,
            bridge::wr_get_update_status,
            bridge::wr_update_check,
            bridge::wr_update_download,
            bridge::wr_update_install,
            // 窗口层(壳 chrome,不属于 window.wr 的 8 个桌面方法面)
            shell_window::wr_window_minimize,
            shell_window::wr_window_toggle_maximize,
            shell_window::wr_window_close,
            shell_window::wr_window_is_maximized,
            shell_window::wr_window_apply_theme,
            shell_window::wr_sync_launch_at_login,
        ])
        .setup(|app| {
            if let Err(message) = bootstrap(app.handle()) {
                report_fatal(app.handle(), &message);
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("WRouter Tauri 壳构建失败");

    app.run(|app_handle, event| {
        if let tauri::RunEvent::Exit = event
            && let Some(state) = app_handle.try_state::<DesktopState>()
        {
            state.shutdown();
        }
    });
}

/// 把**随包资源**的路径写进环境变量,供后端解析前端 SPA 与能力契约。
///
/// 20261102_01 P5-1(打包实测发现):后端 `Settings::from_env` 的
/// `frontend_dir` / `capabilities_file` 默认值都指向 `<repo_root>/...`
/// —— 那是**编译期常量** `CARGO_MANIFEST_DIR` 推出来的**源码树**路径。
/// 安装态的机器上没有源码树 → 管理面托管不到 SPA,窗口加载
/// `http://127.0.0.1:3462/` 直接 404(后端与 RPC 仍正常,很容易误判成"装好了")。
///
/// 处置:`tauri.conf.json` 的 `bundle.resources` 把
/// `frontend/dist` 与 `contracts/wrouter-capabilities.json` 装到
/// `<安装目录>\resources\` 下,这里在启动时把环境变量指过去:
/// - `WROUTER_FRONTEND_DIR` → `<resources>\frontend`
/// - `WROUTER_CAPABILITIES_FILE` → `<resources>\wrouter-capabilities.json`
///
/// 开发态(`tauri dev`)资源目录里没有这两个东西 → 不设置环境变量,
/// 后端继续用仓库里的 `frontend/dist` 与 `contracts/`。
fn resolve_packaged_resources(app: &tauri::AppHandle) {
    if std::env::var_os("WROUTER_FRONTEND_DIR").is_some()
        && std::env::var_os("WROUTER_CAPABILITIES_FILE").is_some()
    {
        return;
    }
    let Ok(resource_dir) = app.path().resource_dir() else {
        return;
    };

    let frontend = resource_dir.join("frontend");
    if std::env::var_os("WROUTER_FRONTEND_DIR").is_none() && frontend.is_dir() {
        // SAFETY:启动早期(单线程、子线程创建之前)设置进程级环境变量,
        // 此后不再修改 → 不存在与其他线程的并发读写。与
        // `backend::build_settings` 里设置 WROUTER_DATA_DIR 同理。
        unsafe {
            std::env::set_var("WROUTER_FRONTEND_DIR", &frontend);
        }
    }

    let capabilities = resource_dir.join("wrouter-capabilities.json");
    if std::env::var_os("WROUTER_CAPABILITIES_FILE").is_none() && capabilities.is_file() {
        unsafe {
            std::env::set_var("WROUTER_CAPABILITIES_FILE", &capabilities);
        }
    }
}

/// 起后端 + 建窗口。
fn bootstrap(app: &tauri::AppHandle) -> Result<(), String> {
    resolve_packaged_resources(app);
    let settings = backend::build_settings();
    // 2026-11-07:Electron→Tauri 同进程化后遗漏了日志初始化/落盘。
    // 这里恢复旧行为:stdout + `<log_dir>/backend.log`(5MB 轮转 ×9) +
    // 统一 panic 钩子。必须在后端线程与任何 tracing 调用之前完成。
    let log_dir = backend::log_dir(&settings);
    if !wr_server::logging::init_with_file(&log_dir) {
        eprintln!(
            "[wrouter-desktop] 文件日志未安装(可能已初始化): {}",
            log_dir.display()
        );
    }
    wr_server::install_panic_hook();
    tracing::info!(
        target: "wrouter.desktop",
        version = wr_server::VERSION,
        data_dir = %settings.data_dir.display(),
        log_dir = %log_dir.display(),
        "WRouter 桌面壳启动"
    );
    let web_port = settings.web_port;
    let (handle, exit_rx) = backend::spawn(settings.clone())?;
    handle.wait_ready(backend::READY_TIMEOUT)?;
    app.manage(DesktopState {
        settings: Arc::new(settings.clone()),
        language: Mutex::new(None),
        backend: Mutex::new(Some(handle)),
    });
    // 后端线程监视:后端一死,壳必须退出。
    //
    // 留着窗口但后端已死,页面只会一直报「WRouter web API failed with HTTP
    // 502」,用户既看不到原因也退不掉(2026-11-04 现场:管理面在启动后固定
    // 时长消失,窗口照旧显示)。
    let watch_app = app.clone();
    std::thread::Builder::new()
        .name("wrouter-backend-watch".to_owned())
        .spawn(move || match exit_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(message)) => {
                tracing::error!(target: "wrouter.desktop", %message, "backend_thread_exited");
                eprintln!("[wrouter-desktop] 后端线程异常结束: {message}");
                report_failure(&watch_app, "WRouter 后端已停止", &message);
            }
            Err(_) => {}
        })
        .map_err(|error| format!("启动后端监视线程失败: {error}"))?;
    create_main_window(app, web_port)?;
    // P4-3 / P4-5:启动对账(开机自启 + 标题栏配色)放后台线程跑 ——
    // 这里不能阻塞建窗。两者共用同一次 `getConfig` 读取。
    let app_handle = app.clone();
    let sync_settings = settings.clone();
    std::thread::Builder::new()
        .name("wrouter-startup-sync".to_owned())
        .spawn(move || {
            tauri::async_runtime::block_on(sync_startup_state(&app_handle, &sync_settings));
        })
        .map_err(|error| format!("启动对账线程失败: {error}"))?;
    Ok(())
}

/// 启动对账:读一次后端配置 → 同步开机自启 + 标题栏配色。
///
/// 与 Electron 壳 `electron/src/main-app.ts:98-112` 同口径(配置为准)。
async fn sync_startup_state(app: &tauri::AppHandle, settings: &Settings) {
    let config = match config::read_config(settings).await {
        Ok(value) => value,
        Err(error) => {
            eprintln!("[wrouter-desktop] 读后端配置失败(跳过启动对账): {error}");
            return;
        }
    };
    // 开机自启(P4-3)
    // 20261102_01 P4:冒烟测试用临时数据目录 → 配置里 launchAtLogin 恒为
    // false,若照常对账会把用户机器上真实存在的「登录时启动」条目删掉。
    // 一次性校验脚本用这个开关跳过开机自启对账(开机自启本身另有专项
    // 校验:备份注册表原值 → 改 → 验 → 原样还原)。
    if std::env::var("WROUTER_DESKTOP_SKIP_AUTOSTART_SYNC").as_deref() != Ok("1") {
        let launch_at_login = config::launch_at_login(&config);
        if let Err(error) = autostart::apply(app.clone(), launch_at_login) {
            eprintln!("[wrouter-desktop] 开机自启对账失败: {error}");
        }
    }
    // 标题栏配色(P4-5):窗口底色按配置主题定,避免深色用户看到白闪。
    let theme = config::theme(&config);
    if let Some(window) = app.get_webview_window("main") {
        let system_is_dark = window.theme().map(|current| current == tauri::Theme::Dark).unwrap_or(false);
        let (resolved, color) = shell_core::resolve_window_theme(&theme, None, system_is_dark);
        if resolved.is_forced() {
            let _ = window.set_theme(Some(match resolved {
                shell_core::WindowTheme::Light => tauri::Theme::Light,
                _ => tauri::Theme::Dark,
            }));
        }
        let _ = window.set_background_color(Some(tauri::window::Color(color.0, color.1, color.2, 0xff)));
    }
}

/// 创建主窗口。
///
/// - **dev**(注入 `WROUTER_VITE_DEV_URL`):加载 vite dev server(HMR);
/// - **生产**:加载 `http://127.0.0.1:3462/`(D5:复用 P1 静态托管)。
fn create_main_window(app: &tauri::AppHandle, web_port: u16) -> Result<(), String> {
    let url_text = match std::env::var("WROUTER_VITE_DEV_URL") {
        Ok(value) if !value.trim().is_empty() => value.trim().to_string(),
        _ => format!("http://127.0.0.1:{web_port}/"),
    };
    let url: tauri::Url = url_text
        .parse()
        .map_err(|error| format!("窗口 URL 非法({url_text}): {error}"))?;
    let builder = WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url))
        .title("WRouter")
        .inner_size(WINDOW_WIDTH, WINDOW_HEIGHT)
        .min_inner_size(WINDOW_MIN_WIDTH, WINDOW_MIN_HEIGHT)
        // P4-5:窗口底色先取浅色默认,启动对账 / 前端主题同步会按实际主题改写。
        .background_color(tauri::window::Color(
            shell_core::BACKGROUND_LIGHT.0,
            shell_core::BACKGROUND_LIGHT.1,
            shell_core::BACKGROUND_LIGHT.2,
            0xff,
        ))
        .initialization_script(bridge_core::init_script(cfg!(debug_assertions)));
    // P4-4:自定义标题栏 —— Windows 去掉系统标题栏,改由前端自绘
    // min/max/close 三键(Tauri v2 没有 Electron `titleBarOverlay` 的等价 API)。
    //
    // 仅 Windows:macOS 保留系统标题栏(红绿灯),Linux 无桌面壳。
    let builder = if cfg!(target_os = "windows") {
        builder.decorations(false)
    } else {
        builder
    };
    // dev 自检:`WROUTER_DESKTOP_SELFTEST=1` 时在页面加载完成后把
    // `window.wr` 的形状探回来打到 stderr(P3 实机验收用),
    // 并**真调一遍**窗口层命令验证权限与通道(P4)。
    let builder = if std::env::var("WROUTER_DESKTOP_SELFTEST").as_deref() == Ok("1") {
        builder.on_page_load(|window, _payload| {
            tauri::async_runtime::spawn(async move {
                // 前端 bundle 是 module script(延后执行)→ 等一会儿再探。
                tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
                let _ = window.eval_with_callback(SELFTEST_SCRIPT, |report| {
                    eprintln!("[wrouter-desktop][selftest] {report}");
                });
                // 窗口层命令探针:发起异步调用 → 结果挂到 window.__shellProbe
                // → 再读回来(不依赖 eval_with_callback 是否等待 Promise)。
                tokio::time::sleep(std::time::Duration::from_millis(800)).await;
                let _ = window.eval(SHELL_PROBE_SCRIPT);
                tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
                let _ = window.eval_with_callback(SHELL_PROBE_READ_SCRIPT, |report| {
                    eprintln!("[wrouter-desktop][shellprobe] {report}");
                });
            });
        })
    } else {
        builder
    };
    builder
        .build()
        .map_err(|error| format!("窗口创建失败: {error}"))?;
    Ok(())
}

/// dev 自检脚本:报告 `window.wr` 的形状(供 P3 验收判定 runtime 与方法面)。
///
/// 20261102_01 P4-4 追加:自定义标题栏的 DOM 证据 —— 拖拽区
/// (`data-tauri-drag-region`)与自绘窗口控制键(`.app-window-controls`)。
const SELFTEST_SCRIPT: &str = r#"(function () {
  var wr = window.wr || {};
  var dragRegions = document.querySelectorAll("[data-tauri-drag-region]").length;
  var controls = document.querySelector(".app-window-controls");
  return {
    hasWr: Boolean(window.wr),
    runtime: wr.runtime || null,
    isDev: wr.isDev === true,
    businessOk: typeof wr.getConfig === "function" && typeof wr.getUsageStats === "function",
    desktopOk: typeof wr.openExternal === "function" && typeof wr.toggleDevTools === "function" && typeof wr.exportData === "function",
    tauriInternals: "__TAURI_INTERNALS__" in window,
    dragRegions: dragRegions,
    windowControls: controls ? controls.querySelectorAll("button").length : 0,
    windowControlsNoDrag: controls ? controls.getAttribute("data-tauri-drag-region") : null,
    pageBackground: getComputedStyle(document.documentElement).getPropertyValue("--background").trim()
  };
})()"#;

/// 窗口层命令探针(20261102_01 P4-4 / P4-5)。
///
/// 真调一遍壳命令 → 验证 **capability 授权 + IPC 通道**都通。
///
/// 副作用控制:
/// - `wr_window_is_maximized` —— 只读;
/// - `wr_window_apply_theme` —— 传 `"system"` + 当前 `--background`:
///   壳侧对 `system` **不调用** `set_theme`(不钉死 `prefers-color-scheme`),
///   只把底色设成当前值 → 幂等,不改任何状态;
/// - `wr_window_toggle_maximize` —— 调两次(最大化再还原)→ 证明通道可用,
///   结束时回到原状态。
const SHELL_PROBE_SCRIPT: &str = r#"(function () {
  var internals = window.__TAURI_INTERNALS__;
  if (!internals || typeof internals.invoke !== "function") {
    window.__shellProbe = { error: "no __TAURI_INTERNALS__" };
    return;
  }
  var probe = {};
  window.__shellProbe = probe;
  var background = getComputedStyle(document.documentElement)
    .getPropertyValue("--background")
    .trim();
  internals.invoke("wr_window_is_maximized", {}).then(
    function (value) { probe.isMaximized = value; },
    function (error) { probe.isMaximizedError = String(error); }
  );
  internals.invoke("wr_window_apply_theme", { theme: "system", background: background }).then(
    function () { probe.applyTheme = "ok"; },
    function (error) { probe.applyThemeError = String(error); }
  );
  internals.invoke("wr_window_toggle_maximize", {}).then(
    function (value) {
      probe.toggleMaximize = value;
      // 还原(证明第二次调用也通),状态回到探针开始前。
      return internals.invoke("wr_window_toggle_maximize", {});
    },
    function (error) { probe.toggleMaximizeError = String(error); }
  ).then(
    function () { probe.toggleMaximizeRestored = "ok"; },
    function (error) { probe.toggleMaximizeRestoreError = String(error); }
  );
})();"#;

/// 读回窗口层探针结果(同步脚本,返回对象本身 —— 不要再 stringify:
/// `eval_with_callback` 会把返回值再序列化一层,stringify 一次就变成
/// "{\"...\"}" 的嵌套字符串,消费方得二次解析)。
const SHELL_PROBE_READ_SCRIPT: &str = r#"(function () {
  return window.__shellProbe || {};
})()"#;

/// 启动失败:stderr 留痕 + 弹窗告知 + 退出。
fn report_fatal(app: &tauri::AppHandle, message: &str) {
    report_failure(app, "WRouter 启动失败", message);
}

/// 故障告知:stderr 留痕 + 弹窗告知 + 退出。
///
/// 弹窗标题与正文分开,便于「启动失败」与「运行期后端异常」复用同一套出口。
fn report_failure(app: &tauri::AppHandle, title: &str, message: &str) {
    tracing::error!(target: "wrouter.desktop", title, message, "desktop_failure");
    eprintln!("[wrouter-desktop] {title}: {message}");
    let handle = app.clone();
    app.dialog()
        .message(message)
        .title(title.to_owned())
        .show(move |_| {
            handle.exit(1);
        });
}
