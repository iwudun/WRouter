//! 管理面生命周期回归(2026-11-04)。
//!
//! 回归现象:页面(调用日志 / 模型指纹 / 用量统计……)全部报
//! 「WRouter web API failed with HTTP 502」,且没有任何数据。
//!
//! 根因(20261103 的 395280b 引入):「管理面排空上限」的 `timeout` 被**套在
//! 整个服务生命周期外面** ——
//!
//! ```text
//! timeout(5s, mgmt_server)   // ← 错:这是「服务存活 5 秒」的截止时间
//! ```
//!
//! 于是管理面在**启动 5 秒后**就被强制掐断:监听套接字被释放 → 前端所有
//! 管理面请求变成「连不上」→ 反代(vite dev proxy / nginx)回 502 → 前端
//! 拿到空响应体 → 落到 `rpcErrorMessage` 的兜底文案。
//!
//! 之所以现场看起来「只有页面坏、进程还活着」:网关面没有这个超时(它的
//! 排空上限只在停机时生效),`tokio::join!` 又会一直等网关面 → 进程不退。
//!
//! 断言口径:管理面必须在**活过排空上限之后**仍然在线并能应答 RPC。
//! 排空上限的用途是「停机时不被长连接钉死」,与「服务该活多久」无关。

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use wr_server::settings::Settings;

/// 管理令牌(与 `test_settings` 一致)。
const TOKEN: &str = "test-mgmt-lifetime-token";

/// 借一个空闲端口(绑定 0 → 读回端口 → 释放)。
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

fn test_settings(tag: &str) -> (Settings, u16, u16) {
    let data_dir =
        std::env::temp_dir().join(format!("wr-mgmt-lifetime-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);
    let mut settings = Settings::from_env();
    settings.host = "127.0.0.1".to_owned();
    settings.gateway_port = free_port();
    settings.web_port = free_port();
    settings.data_dir = data_dir.clone();
    settings.config_dir = data_dir.join("config");
    settings.auth_token = TOKEN.to_owned();
    settings.ready_file = None;
    settings.parent_pid = None;
    settings.serve_ui = false;
    // 页面可停服务的语义(独立进程 / systemd 托管)—— 用于测试收尾。
    settings.managed_by = wr_server::service_control::ManagedBy::Process;
    let web_port = settings.web_port;
    let gateway_port = settings.gateway_port;
    (settings, web_port, gateway_port)
}

/// 起服务(独立线程 + 独立 runtime),返回 (结果接收端, 线程句柄)。
///
/// 停机信号用 `pending()`(永不触发)—— 本测试要断言的是「**不**停机时
/// 管理面长期在线」,停机由页面 RPC 触发。
fn spawn_server(
    settings: Settings,
) -> (
    mpsc::Receiver<anyhow::Result<()>>,
    std::thread::JoinHandle<()>,
) {
    let (result_tx, result_rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("构建 tokio runtime 失败");
        let result = runtime.block_on(async move {
            wr_server::run_serve_with_shutdown(settings, std::future::pending::<()>()).await
        });
        let _ = result_tx.send(result);
    });
    (result_rx, handle)
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
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .ok()?;
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

/// 端口是否已释放(连不上 = 已释放)。
fn port_closed(port: u16) -> bool {
    TcpStream::connect(("127.0.0.1", port)).is_err()
}

/// 服务完全退出允许的最长时间(停机收尾:排空 + 落日志 + 关库)。
const EXIT_BUDGET: Duration = Duration::from_secs(45);

/// 核心回归:管理面必须在**活过排空上限**之后仍然在线并能应答。
///
/// 预算取 `MGMT_DRAIN_TIMEOUT_SECS + 4` 秒 —— 回归实现下管理面恰好在
/// 排空上限那一刻消失,留 4 秒余量确保一定踩到。
#[test]
fn management_face_stays_online_past_the_drain_timeout() {
    let (settings, web_port, gateway_port) = test_settings("lifetime");
    let (result_rx, _handle) = spawn_server(settings);

    assert!(
        wait_for_health(web_port, Duration::from_secs(60)),
        "管理面未在 60s 内就绪(port={web_port})"
    );

    // 活过排空上限:管理面必须一直在线。
    let uptime = Duration::from_secs(wr_server::MGMT_DRAIN_TIMEOUT_SECS + 4);
    let deadline = Instant::now() + uptime;
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(500));
        assert!(
            wait_for_health(web_port, Duration::from_secs(2)),
            "管理面在启动后 {uptime:?} 内消失 —— \
             排空上限被套在整个服务生命周期外面(管理面一启动就被掐断 → 前端全部 502)"
        );
    }

    // 不只是健康端点:业务 RPC 也必须能通(调用日志 / 模型指纹走的就是它)。
    for method in ["getRuntimeControl", "getCallLogs", "listModelFingerprints"] {
        let response = rpc_call(web_port, TOKEN, method, "[]")
            .unwrap_or_else(|| panic!("管理面 RPC {method} 不可达(port={web_port})"));
        assert!(
            response.starts_with("HTTP/1.1 200"),
            "管理面 RPC {method} 未返回 200: {response}"
        );
    }

    // 网关面同样必须还在(进程不该只死一半)。
    assert!(
        !port_closed(gateway_port),
        "网关面端口不应消失(port={gateway_port})"
    );

    // 收尾:停掉服务,避免测试进程被挂住。
    let _ = rpc_call(web_port, TOKEN, "stopService", "[]");
    match result_rx.recv_timeout(EXIT_BUDGET) {
        Ok(Ok(())) => {}
        Ok(Err(error)) => panic!("服务返回错误: {error:?}"),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("服务在 {EXIT_BUDGET:?} 内未退出")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("服务线程意外结束(未回传结果)")
        }
    }
}
