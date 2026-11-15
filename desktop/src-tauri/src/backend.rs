//! 后端**同进程**托管(20261102_01 P3-2)。
//!
//! Tauri 桌面壳不再 spawn `wr-backend` 子进程:后端跑在**同进程**的独立
//! 线程 + 独立 tokio runtime 上,复用 CLI(`wr serve`)同一套
//! [`wr_server::run_serve_with_shutdown`]。
//!
//! 为什么用独立 runtime 而不是 Tauri 的 `async_runtime`:
//! - 后端有 SQLite 阻塞读(必须 multi_thread),独立 runtime 可控且不与
//!   窗口事件循环抢线程;
//! - 与桌面壳生命周期解耦:窗口关闭 → 停机信号 → 双面优雅停机 →
//!   WAL checkpoint 落库,再退出进程。

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wr_server::settings::Settings;

/// 后端就绪等待上限(秒)。
pub const READY_TIMEOUT: Duration = Duration::from_secs(60);

/// 后端线程句柄:负责「等就绪」与「停机」。
pub struct BackendHandle {
    ready: Receiver<Result<(), String>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl BackendHandle {
    /// 阻塞等待后端就绪(管理面 + 网关面双面监听成功)。
    pub fn wait_ready(&self, timeout: Duration) -> Result<(), String> {
        match self.ready.recv_timeout(timeout) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(message)) => Err(message),
            Err(RecvTimeoutError::Timeout) => {
                Err(format!("后端启动超时({}s)", timeout.as_secs()))
            }
            Err(RecvTimeoutError::Disconnected) => Err("后端线程意外退出".to_string()),
        }
    }

    /// 停机:发停机信号 → 等线程收尾(双面优雅停机 + WAL checkpoint)。
    pub fn shutdown(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for BackendHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// 后端线程结束时的结果通道。
///
/// `Err` = 异常结束(管理面等核心能力已不可用);`Ok(())` = 正常停机。
/// 壳据此判断「后端已死」并退出窗口 —— 而不是留一个所有请求都 502 的空壳
/// (2026-11-04 现场:窗口还在,但页面全是「WRouter web API failed with HTTP 502」)。
pub type BackendExit = mpsc::Receiver<Result<(), String>>;

/// 启动后端线程(同进程)。返回句柄;调用方负责 `wait_ready` 与 `shutdown`。
pub fn spawn(settings: Settings) -> Result<(BackendHandle, BackendExit), String> {
    let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
    // 后端线程的最终结果:壳需要知道「后端什么时候没了、为什么没了」。
    let (exit_tx, exit_rx) = mpsc::channel::<Result<(), String>>();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let web_port = settings.web_port;

    let thread = std::thread::Builder::new()
        .name("wrouter-backend".to_owned())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
                Ok(runtime) => runtime,
                Err(error) => {
                    let message = format!("构建 tokio runtime 失败: {error}");
                    let _ = ready_tx.send(Err(message.clone()));
                    let _ = exit_tx.send(Err(message));
                    return;
                }
            };
            let final_result = runtime.block_on(async move {
                // 注意:后端 future **不是 Send**(wr-server 的
                // `shutdown_request_logs` 在 await 之间持有 MutexGuard),
                // 因此不能 `tokio::spawn` —— 直接在本线程的 runtime 上
                // `block_on`;就绪探测用 `select!` 与服务器**并行**推进
                // (不能把探测写进 select! 分支体里 await:那样服务器
                //  future 会暂停轮询,健康检查永远等不到响应 → 死锁)。
                let mut server = Box::pin(wr_server::run_serve_with_shutdown(
                    settings,
                    async move {
                        let _ = stop_rx.await;
                    },
                ));
                let readiness = tokio::select! {
                    biased;
                    result = &mut server => Err(match result {
                        Ok(()) => "后端已提前退出(未报错)".to_string(),
                        Err(error) => format!("后端启动失败: {error}"),
                    }),
                    result = health_probe(web_port) => result,
                };
                match readiness {
                    Ok(()) => {
                        let _ = ready_tx.send(Ok(()));
                        // 服务器继续跑,直到停机信号到达。
                        match server.await {
                            Ok(()) => Ok(()),
                            Err(error) => Err(format!("后端异常结束: {error}")),
                        }
                    }
                    Err(message) => {
                        let _ = ready_tx.send(Err(message.clone()));
                        // 启动失败 → 丢弃服务器 future(取消本次启动)。
                        drop(server);
                        Err(message)
                    }
                }
            });
            let _ = exit_tx.send(final_result);
        })
        .map_err(|error| format!("启动后端线程失败: {error}"))?;

    Ok((
        BackendHandle {
            ready: ready_rx,
            stop: Some(stop_tx),
            thread: Some(thread),
        },
        exit_rx,
    ))
}

/// 轮询管理面健康端点,直到服务就绪或超时。
async fn health_probe(web_port: u16) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + READY_TIMEOUT;
    loop {
        if health_ok(web_port).await {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "后端启动超时({}s,管理面端口 {web_port})",
                READY_TIMEOUT.as_secs()
            ));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// 裸 TCP 打一次 `GET /api/v1/health`(免认证),判断管理面是否已服务。
async fn health_ok(web_port: u16) -> bool {
    let Ok(mut stream) = tokio::net::TcpStream::connect(("127.0.0.1", web_port)).await else {
        return false;
    };
    let request = "GET /api/v1/health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n";
    if stream.write_all(request.as_bytes()).await.is_err() {
        return false;
    }
    let mut buffer = [0u8; 128];
    let Ok(Ok(read)) = tokio::time::timeout(
        Duration::from_secs(5),
        stream.read(&mut buffer),
    )
    .await
    else {
        return false;
    };
    String::from_utf8_lossy(&buffer[..read]).starts_with("HTTP/1.1 200")
}

// ---------------------------------------------------------------------------
// 设置装配
// ---------------------------------------------------------------------------

/// 装配桌面壳的后端设置。
///
/// - **D4**:只监听回环 `127.0.0.1`(与 systemd 服务一致)。
/// - **P1**:开启静态托管 —— 窗口加载 `http://127.0.0.1:3462/` 时需要
///   SPA 资源与 `/api/wr/session` 令牌。
/// - **数据目录**:不再区分开发态与运行态,统一使用运行态目录 ——
///   与 Electron 壳保持同一约定(Windows `%APPDATA%\WRouter\data`,
///   Linux `~/.wrouter`),否则双轨并存期用户会看到「配置消失」。
///   需要隔离时用 `WROUTER_DATA_DIR` 显式覆盖。
/// - **同进程**:没有父进程探活、没有 ready 文件(Electron 宿主链路专用)。
pub fn build_settings() -> Settings {
    // 令牌按数据目录持久化(Settings::from_env 内部解析),因此数据目录必须
    // 在装配前就定下来 —— 先落进环境变量,再读 Settings。
    if std::env::var_os("WROUTER_DATA_DIR").is_none() {
        let data_dir = default_data_dir();
        // SAFETY:桌面壳启动早期(单线程、任何子线程创建之前)设置进程级环境
        // 变量,此后不再修改 → 不存在与其他线程的并发读写。
        unsafe {
            std::env::set_var("WROUTER_DATA_DIR", &data_dir);
        }
    }
    // 20261102_01 P6:后端**嵌在壳进程内** → 告知后端「托管方式 = embedded」。
    // 后端据此拒绝页面「停止服务」(停后端只会让窗口变废;正确做法是关窗口,
    // 壳退出时后端随之优雅停机)。「暂停网关」不受影响(所有运行时都支持)。
    unsafe {
        std::env::set_var("WROUTER_MANAGED_BY", "embedded");
    }
    let mut settings = Settings::from_env();
    settings.host = "127.0.0.1".to_owned();
    settings.serve_ui = true;
    settings.parent_pid = None;
    settings.ready_file = None;
    settings
}

/// 桌面壳的默认数据目录。
///
/// 不区分构建类型,统一使用运行态路径(Windows `%APPDATA%\WRouter\data`,
/// macOS `~/Library/Application Support/WRouter/data`,Linux `~/.wrouter`)。
fn default_data_dir() -> PathBuf {
    production_data_dir()
}

/// 桌面壳运行日志目录(2026-11-07 恢复 Tauri 迁移时丢失的日志落盘)。
///
/// 约定与旧 Electron 壳一致:
/// - `WROUTER_LOG_DIR` 显式覆盖;
/// - Windows / macOS 发布态 → 数据目录的同级 `logs/`
///   (`%APPDATA%\WRouter\logs` / `~/Library/Application Support/WRouter/logs`);
/// - Linux 发布态 → 数据目录内 `logs/`(`~/.wrouter/logs`,与 systemd 数据同根)。
pub fn log_dir(settings: &Settings) -> PathBuf {
    if let Some(dir) = std::env::var_os("WROUTER_LOG_DIR") {
        return PathBuf::from(dir);
    }
    resolve_log_dir(&settings.data_dir)
}

fn resolve_log_dir(data_dir: &Path) -> PathBuf {
    if cfg!(any(target_os = "windows", target_os = "macos")) {
        data_dir
            .parent()
            .map(|parent| parent.join("logs"))
            .unwrap_or_else(|| data_dir.join("logs"))
    } else {
        data_dir.join("logs")
    }
}

/// 发布态数据目录(与 Electron 壳等价的路径约定)。
///
/// 独立成函数便于单测直接断言路径算法。
fn production_data_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        let appdata = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .or_else(|| home_dir().map(|home| home.join("AppData").join("Roaming")))
            .unwrap_or_else(|| PathBuf::from("."));
        appdata.join("WRouter").join("data")
    }
    #[cfg(target_os = "macos")]
    {
        home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Library")
            .join("Application Support")
            .join("WRouter")
            .join("data")
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        // Linux:与 systemd 服务 / 手工起后端同一目录(TD-ARCH-002)。
        home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".wrouter")
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    /// 发布态(Windows):数据目录必须与 Electron 壳完全一致(`%APPDATA%\WRouter\data`)
    /// —— 双轨并存期两边指向同一个目录,否则用户会看到「配置 / API key 消失」。
    ///
    /// 数据目录已不再区分构建类型,这条直接断言运行态路径算法。
    #[test]
    #[cfg(target_os = "windows")]
    fn windows_production_data_dir_matches_electron_convention() {
        let expected = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .expect("Windows 宿主应有 APPDATA")
            .join("WRouter")
            .join("data");
        assert_eq!(super::production_data_dir(), expected);
    }

    /// 发布态(Linux):统一 `~/.wrouter`(与 systemd 服务 / 手工起后端同目录)。
    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn linux_production_data_dir_matches_service_convention() {
        let expected = std::env::var_os("HOME")
            .map(PathBuf::from)
            .expect("Linux 宿主应有 HOME")
            .join(".wrouter");
        assert_eq!(super::production_data_dir(), expected);
    }

    /// 日志目录:Windows/macOS 与数据目录同级,Linux 在数据目录内。
    #[test]
    fn production_log_dir_matches_platform_convention() {
        let data = super::production_data_dir();
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        let expected = data.parent().expect("生产数据目录应有父目录").join("logs");
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        let expected = data.join("logs");
        assert_eq!(super::resolve_log_dir(&data), expected);
    }
}
