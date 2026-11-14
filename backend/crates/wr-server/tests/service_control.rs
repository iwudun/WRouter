//! P6 验收测试(方案 20261102_01 §P6):服务控制(暂停 / 恢复 / 退出服务)。
//!
//! 覆盖三条契约:
//! - **暂停网关面**:`setGatewayEnabled(false)` → 3460 **释放监听**
//!   (客户端 connection refused)→ 管理面 3462 **继续在线**(页面不废);
//! - **恢复网关面**:`setGatewayEnabled(true)` → 3460 重新监听并恢复 200;
//! - **停止服务**:`stopService` → 后端**自行**优雅停机并返回 Ok
//!   (不依赖外部注入的停机信号)——这是页面「停止服务」的唯一入口。
//!
//! 起停方式与桌面壳同构(独立线程 + 独立 tokio runtime 调用
//! [`wr_server::run_serve_with_shutdown`]),因此本测试同时覆盖了
//! 桌面壳依赖的那条启动路径。
//!
//! HTTP 探测走裸 TCP(零第三方依赖,与 `serve_shutdown.rs` 同风格)。

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use wr_server::settings::Settings;

/// 借一个空闲端口(绑定 0 → 读回端口 → 释放)。
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

fn test_settings(tag: &str) -> (Settings, u16, u16) {
    let data_dir =
        std::env::temp_dir().join(format!("wr-service-control-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);
    let mut settings = Settings::from_env();
    settings.host = "127.0.0.1".to_owned();
    settings.gateway_port = free_port();
    settings.web_port = free_port();
    settings.data_dir = data_dir.clone();
    settings.config_dir = data_dir.join("config");
    settings.auth_token = "test-service-control-token".to_owned();
    settings.ready_file = None;
    settings.parent_pid = None;
    settings.serve_ui = false;
    // 20261102_01 P6:测试环境按「独立进程」语义(页面可停服务)。
    settings.managed_by = wr_server::service_control::ManagedBy::Process;
    let web_port = settings.web_port;
    let gateway_port = settings.gateway_port;
    (settings, web_port, gateway_port)
}

/// 裸 TCP 发一次 GET,返回完整响应文本(连不上返回 None)。
fn http_get(port: u16, path: &str) -> Option<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .ok()?;
    let mut buf = String::new();
    stream.read_to_string(&mut buf).ok()?;
    Some(buf)
}

/// 裸 TCP 发一次带管理令牌的 RPC POST,返回完整响应文本。
fn rpc_call(port: u16, token: &str, method: &str, args: &str) -> Option<String> {
    let body = format!("{{\"method\":\"{method}\",\"args\":{args}}}");
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok()?;
    stream
        .write_all(
            format!(
                "POST /api/wr/rpc HTTP/1.1\r\nHost: 127.0.0.1\r\nx-wrouter-web-auth: {token}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .ok()?;
    let mut buf = String::new();
    stream.read_to_string(&mut buf).ok()?;
    Some(buf)
}

/// 轮询直到管理面健康检查返回 200。
fn wait_for_health(port: u16, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(response) = http_get(port, "/api/v1/health")
            && response.starts_with("HTTP/1.1 200")
        {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

/// 轮询直到网关面健康检查返回 200。
fn wait_for_gateway(port: u16, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(response) = http_get(port, "/health")
            && response.starts_with("HTTP/1.1 200")
        {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

/// 轮询直到网关端口**连不上**(监听已释放)。
fn wait_for_gateway_closed(port: u16, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_err() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

/// 起服务(独立线程 + 独立 runtime),返回 (停机发送端, 线程句柄)。
fn spawn_server(
    settings: Settings,
) -> (
    tokio::sync::oneshot::Sender<()>,
    std::thread::JoinHandle<anyhow::Result<()>>,
) {
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let handle = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("构建 tokio runtime 失败");
        runtime.block_on(async move {
            wr_server::run_serve_with_shutdown(settings, async move {
                let _ = stop_rx.await;
            })
            .await
        })
    });
    (stop_tx, handle)
}

#[test]
fn pause_and_resume_gateway_face() {
    let (settings, web_port, gateway_port) = test_settings("pause");
    let (stop_tx, handle) = spawn_server(settings);

    assert!(
        wait_for_health(web_port, Duration::from_secs(60)),
        "管理面未在 60s 内就绪(port={web_port})"
    );
    assert!(
        wait_for_gateway(gateway_port, Duration::from_secs(30)),
        "网关面未监听(port={gateway_port})"
    );

    // 1) 暂停 → 3460 释放监听。
    let response = rpc_call(web_port, "test-service-control-token", "setGatewayEnabled", "[false]")
        .expect("setGatewayEnabled 请求失败");
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "setGatewayEnabled(false) 未返回 200: {response}"
    );
    assert!(
        response.contains("\"state\":\"stopped\""),
        "回执状态应为 stopped: {response}"
    );
    assert!(
        wait_for_gateway_closed(gateway_port, Duration::from_secs(15)),
        "暂停后网关端口仍可连接(port={gateway_port})"
    );

    // 2) 管理面必须仍然在线(页面不废)。
    assert!(
        wait_for_health(web_port, Duration::from_secs(10)),
        "暂停网关后管理面不可用(port={web_port})"
    );
    let status = rpc_call(web_port, "test-service-control-token", "getGatewayStatus", "[]")
        .expect("getGatewayStatus 请求失败");
    assert!(
        status.contains("\"state\":\"stopped\""),
        "getGatewayStatus 应报告 stopped: {status}"
    );

    // 3) 恢复 → 3460 重新监听并回到 200。
    let response = rpc_call(web_port, "test-service-control-token", "setGatewayEnabled", "[true]")
        .expect("setGatewayEnabled 请求失败");
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "setGatewayEnabled(true) 未返回 200: {response}"
    );
    assert!(
        wait_for_gateway(gateway_port, Duration::from_secs(30)),
        "恢复后网关面未重新监听(port={gateway_port})"
    );

    // 4) 收尾:注入停机信号 → 服务优雅退出。
    let _ = stop_tx.send(());
    let result = handle.join().expect("后端线程 panic");
    assert!(result.is_ok(), "run_serve_with_shutdown 返回错误: {result:?}");
}

#[test]
fn stop_service_stops_the_process_by_itself() {
    let (settings, web_port, _gateway_port) = test_settings("stop");
    // **不注入任何停机信号** —— 服务必须靠页面「停止服务」自己停下来。
    let (_stop_tx, handle) = spawn_server(settings);

    assert!(
        wait_for_health(web_port, Duration::from_secs(60)),
        "管理面未在 60s 内就绪(port={web_port})"
    );

    let response = rpc_call(web_port, "test-service-control-token", "stopService", "[]")
        .expect("stopService 请求失败");
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "stopService 未返回 200: {response}"
    );
    assert!(
        response.contains("\"stopping\":true"),
        "stopService 回执应含 stopping=true: {response}"
    );

    // 停机延迟约 1.2s → 之后服务应自行结束(函数返回 Ok)。
    let result = handle.join().expect("后端线程 panic");
    assert!(result.is_ok(), "run_serve_with_shutdown 返回错误: {result:?}");
    assert!(
        TcpStream::connect(("127.0.0.1", web_port)).is_err(),
        "停止服务后管理面端口仍被占用(port={web_port})"
    );
}

#[test]
fn runtime_control_reports_capabilities() {
    let (settings, web_port, _gateway_port) = test_settings("control");
    let (stop_tx, handle) = spawn_server(settings);

    assert!(
        wait_for_health(web_port, Duration::from_secs(60)),
        "管理面未在 60s 内就绪(port={web_port})"
    );

    let response = rpc_call(web_port, "test-service-control-token", "getRuntimeControl", "[]")
        .expect("getRuntimeControl 请求失败");
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "getRuntimeControl 未返回 200: {response}"
    );
    assert!(
        response.contains("\"canPause\":true"),
        "canPause 应为 true: {response}"
    );
    // 独立进程语义 → 页面可停服务,动作是 stopService。
    assert!(
        response.contains("\"canExit\":true"),
        "独立进程下 canExit 应为 true: {response}"
    );
    assert!(
        response.contains("\"exitAction\":\"stopService\""),
        "独立进程下 exitAction 应为 stopService: {response}"
    );

    let _ = stop_tx.send(());
    let _ = handle.join();
}
