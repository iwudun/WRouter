//! P3-2 验收测试(方案 20261102_01 §6):后端同进程化。
//!
//! 覆盖 [`wr_server::run_serve_with_shutdown`] 的核心契约:
//! - 管理面 + 网关面**双面同时**起来(注入式停机不能只停一面);
//! - 调用方注入的停机信号触发后,两个面都优雅退出、函数返回 Ok;
//! - 退出后端口被释放(桌面壳可以在同进程里反复起停)。
//!
//! 这里刻意用「独立线程 + 独立 tokio runtime」起服务 —— 与 Tauri 桌面壳
//! 的同进程方案(`desktop/src-tauri/src/backend.rs`)完全同构,因此本测试
//! 同时验证了桌面壳依赖的那条启动路径。
//!
//! HTTP 探测走裸 TCP(零第三方依赖,与 `static_ui.rs` 同风格)。

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
        std::env::temp_dir().join(format!("wr-serve-shutdown-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);
    let mut settings = Settings::from_env();
    settings.host = "127.0.0.1".to_owned();
    settings.gateway_port = free_port();
    settings.web_port = free_port();
    settings.data_dir = data_dir.clone();
    settings.config_dir = data_dir.join("config");
    settings.auth_token = "test-shutdown-token".to_owned();
    // ready 文件与父进程探活是 Electron 宿主链路专用的,同进程模式不用。
    settings.ready_file = None;
    settings.parent_pid = None;
    settings.serve_ui = false;
    let web_port = settings.web_port;
    let gateway_port = settings.gateway_port;
    (settings, web_port, gateway_port)
}

/// 裸 TCP 发一次 GET,返回完整响应文本(连不上返回 None)。
fn http_get(port: u16, path: &str) -> Option<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .ok()?;
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

#[test]
fn injected_shutdown_stops_both_faces() {
    let (settings, web_port, gateway_port) = test_settings("inject");

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

    // 1) 双面同时就绪。
    assert!(
        wait_for_health(web_port, Duration::from_secs(60)),
        "管理面未在 60s 内就绪(port={web_port})"
    );
    let gateway_response = http_get(gateway_port, "/health")
        .expect("网关面未监听(port={gateway_port})");
    assert!(
        gateway_response.starts_with("HTTP/1.1 200"),
        "网关面健康检查未返回 200: {gateway_response}"
    );

    // 2) 注入停机信号 → 函数返回 Ok。
    stop_tx.send(()).expect("发送停机信号失败");
    let result = handle.join().expect("后端线程 panic");
    assert!(result.is_ok(), "run_serve_with_shutdown 返回错误: {result:?}");

    // 3) 端口已释放。
    assert!(
        TcpStream::connect(("127.0.0.1", web_port)).is_err(),
        "停机后管理面端口仍被占用(port={web_port})"
    );
    assert!(
        TcpStream::connect(("127.0.0.1", gateway_port)).is_err(),
        "停机后网关面端口仍被占用(port={gateway_port})"
    );
}

#[test]
fn port_conflict_is_reported_as_error() {
    // 占住管理面端口 → 同进程启动必须返回错误(而不是静默挂起)。
    let (settings, web_port, _gateway_port) = test_settings("conflict");
    let occupied = std::net::TcpListener::bind(("127.0.0.1", web_port)).unwrap();

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

    let result = handle.join().expect("后端线程 panic");
    assert!(result.is_err(), "端口冲突时必须返回错误,实际: {result:?}");
    drop(occupied);
    let _ = stop_tx.send(());
}
