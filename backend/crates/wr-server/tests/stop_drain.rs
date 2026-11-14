//! 停机排空回归测试(2026-11-03)。
//!
//! 覆盖「页面点停止服务 → 进程真正退出」这条链路上真实踩过的三个坑:
//!
//! 1. **管理面 SSE 长连接钉死进程**:浏览器的事件流 `/api/wr/events` 是
//!    长连接,axum 的优雅停机会等所有连接结束(`close_tx.closed().await`)
//!    → 流不结束,进程就永远退不出。现场表现:端口已关、但后端进程仍在、
//!    systemd 仍显示 running,只能手动关掉所有管理页面标签页。
//! 2. **网关面处于暂停态时收不到停机信号**:暂停分支只盯着「恢复」意图,
//!    「先暂停网关、再点停止服务」→ 网关面任务永不返回 → 进程同样退不出。
//! 3. **排空无上限**:长流式响应(客户端不主动断开)会让优雅停机无限等待。
//!
//! 断言口径:服务端函数必须在**有界时间**内返回(而不是无限挂起)。
//! 因此这里用「结果通道 + `recv_timeout`」而不是直接 `join()` ——
//! 回归时测试会**失败并给出超时信息**,而不是把整个测试进程挂死。

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use wr_server::settings::Settings;

/// 服务端完全退出(含日志排空 + 关库)允许的最长时间。
/// 取值要大于后端的排空上限(管理面 8s + 网关面 20s),但远小于测试超时。
const EXIT_BUDGET: Duration = Duration::from_secs(45);

/// 借一个空闲端口(绑定 0 → 读回端口 → 释放)。
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

fn test_settings(tag: &str) -> (Settings, u16, u16) {
    let data_dir = std::env::temp_dir().join(format!("wr-stop-drain-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);
    let mut settings = Settings::from_env();
    settings.host = "127.0.0.1".to_owned();
    settings.gateway_port = free_port();
    settings.web_port = free_port();
    settings.data_dir = data_dir.clone();
    settings.config_dir = data_dir.join("config");
    settings.auth_token = "test-stop-drain-token".to_owned();
    settings.ready_file = None;
    settings.parent_pid = None;
    settings.serve_ui = false;
    // 页面可停服务的语义(独立进程 / systemd 托管)。
    settings.managed_by = wr_server::service_control::ManagedBy::Process;
    let web_port = settings.web_port;
    let gateway_port = settings.gateway_port;
    (settings, web_port, gateway_port)
}

/// 起服务(独立线程 + 独立 runtime),返回 (结果接收端, 线程句柄)。
///
/// 结果经通道回传:调用方用 `recv_timeout` 有界等待,回归时不会挂死测试。
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

/// 打开一条**保持存活**的 SSE 长连接(模拟浏览器管理页面)。
///
/// 返回连接句柄:调用方必须持有它,直到断言结束 —— 一旦 drop,
/// 服务端就看不到长连接,测试就测不到「长连接钉死进程」这个坑。
fn open_sse(port: u16, token: &str) -> TcpStream {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("连接管理面失败");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("设置读超时失败");
    stream
        .write_all(
            format!(
                "GET /api/wr/events HTTP/1.1\r\nHost: 127.0.0.1\r\nx-wrouter-web-auth: {token}\r\nAccept: text/event-stream\r\n\r\n"
            )
            .as_bytes(),
        )
        .expect("发送 SSE 请求失败");
    stream
}

/// 在网关面挂一条**永不读完**的请求(制造一个在途请求)。
///
/// 手法:声明 `content-length: 64` 但只发 5 字节 → axum 的 `Bytes` 提取器
/// 一直等body 收完 → 该请求永远处于 in-flight → 网关面的优雅停机会等它。
///
/// 返回的连接句柄必须持有到断言结束(否则请求会立刻结束,测不到排空)。
fn open_hanging_gateway_request(port: u16) -> TcpStream {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("连接网关面失败");
    stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .expect("设置读超时失败");
    stream
        .write_all(
            b"POST /v1/chat/completions HTTP/1.1\r\nHost: 127.0.0.1\r\ncontent-type: application/json\r\ncontent-length: 64\r\n\r\n{\"a\":",
        )
        .expect("发送半截请求失败");
    stream
}

/// 从长连接上读一段数据(带上限,避免无限阻塞)。
fn read_some(stream: &mut TcpStream, max_bytes: usize) -> String {
    let mut buf = vec![0u8; max_bytes];
    match stream.read(&mut buf) {
        Ok(count) => String::from_utf8_lossy(&buf[..count]).to_string(),
        Err(_) => String::new(),
    }
}

/// 端口是否已释放(连不上 = 已释放)。
fn port_closed(port: u16) -> bool {
    TcpStream::connect(("127.0.0.1", port)).is_err()
}

/// 核心断言:服务必须在有界时间内自行结束。
fn assert_exits(result_rx: mpsc::Receiver<anyhow::Result<()>>, tag: &str) {
    match result_rx.recv_timeout(EXIT_BUDGET) {
        Ok(Ok(())) => {}
        Ok(Err(error)) => panic!("[{tag}] 服务返回错误: {error:?}"),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!(
                "[{tag}] 服务在 {EXIT_BUDGET:?} 内未退出 —— \
                 排空被长连接 / 未收尾的请求钉死(端口已关但进程残留)"
            )
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("[{tag}] 服务线程意外结束(未回传结果)")
        }
    }
}

#[test]
fn stop_service_exits_even_with_an_open_sse_connection() {
    let (settings, web_port, _gateway_port) = test_settings("sse");
    let (result_rx, _handle) = spawn_server(settings);

    assert!(
        wait_for_health(web_port, Duration::from_secs(60)),
        "管理面未在 60s 内就绪(port={web_port})"
    );

    // 模拟浏览器管理页面:建立并**保持**一条事件流长连接。
    let mut sse = open_sse(web_port, "test-stop-drain-token");
    let greeting = read_some(&mut sse, 4096);
    assert!(
        greeting.starts_with("HTTP/1.1 200"),
        "SSE 未建立成功: {greeting}"
    );
    assert!(
        greeting.contains("text/event-stream"),
        "SSE 响应类型不对: {greeting}"
    );

    // 页面点「停止服务」→ 后端自行停机。
    let response = rpc_call(web_port, "test-stop-drain-token", "stopService", "[]")
        .expect("stopService 请求失败");
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "stopService 未返回 200: {response}"
    );

    // 关键断言:长连接存在时,进程仍必须自行退出。
    assert_exits(result_rx, "sse-hang");

    assert!(
        port_closed(web_port),
        "退出后管理面端口仍被占用(port={web_port})"
    );
}

#[test]
fn sse_stream_ends_with_a_service_stopping_notice() {
    let (settings, web_port, _gateway_port) = test_settings("notice");
    let (result_rx, _handle) = spawn_server(settings);

    assert!(
        wait_for_health(web_port, Duration::from_secs(60)),
        "管理面未在 60s 内就绪(port={web_port})"
    );

    let mut sse = open_sse(web_port, "test-stop-drain-token");
    let greeting = read_some(&mut sse, 4096);
    assert!(greeting.starts_with("HTTP/1.1 200"), "SSE 未建立成功");

    let _ = rpc_call(web_port, "test-stop-drain-token", "stopService", "[]")
        .expect("stopService 请求失败");

    // 停机通告帧:客户端据此切「正在停止」态并取消重连。
    let mut notice = String::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && !notice.contains("event: service-stopping") {
        notice.push_str(&read_some(&mut sse, 8192));
    }
    assert!(
        notice.contains("event: service-stopping"),
        "未收到 service-stopping 停机通告帧: {notice}"
    );

    assert_exits(result_rx, "notice");
}

#[test]
fn stop_service_exits_while_the_gateway_face_is_paused() {
    let (settings, web_port, gateway_port) = test_settings("paused");
    let (result_rx, _handle) = spawn_server(settings);

    assert!(
        wait_for_health(web_port, Duration::from_secs(60)),
        "管理面未在 60s 内就绪(port={web_port})"
    );

    // 先暂停网关面(3460 释放监听)→ 再点停止服务。
    let response = rpc_call(
        web_port,
        "test-stop-drain-token",
        "setGatewayEnabled",
        "[false]",
    )
    .expect("setGatewayEnabled 请求失败");
    assert!(
        response.contains("\"state\":\"stopped\""),
        "暂停回执应为 stopped: {response}"
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline && !port_closed(gateway_port) {
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(
        port_closed(gateway_port),
        "暂停后网关端口仍被占用(port={gateway_port})"
    );

    let _ = rpc_call(web_port, "test-stop-drain-token", "stopService", "[]")
        .expect("stopService 请求失败");

    // 关键断言:网关面处于暂停态时,停机信号也必须能让整个进程退出。
    assert_exits(result_rx, "paused-gateway");
}

#[test]
fn management_face_stays_online_while_the_gateway_face_drains() {
    let (settings, web_port, gateway_port) = test_settings("ordered");
    let (result_rx, _handle) = spawn_server(settings);

    assert!(
        wait_for_health(web_port, Duration::from_secs(60)),
        "管理面未在 60s 内就绪(port={web_port})"
    );

    // 网关面挂一条永远收不完的请求 → 网关面排空会被拖住。
    let _hanging = open_hanging_gateway_request(gateway_port);

    let mut sse = open_sse(web_port, "test-stop-drain-token");
    let greeting = read_some(&mut sse, 4096);
    assert!(greeting.starts_with("HTTP/1.1 200"), "SSE 未建立成功");

    let _ = rpc_call(web_port, "test-stop-drain-token", "stopService", "[]")
        .expect("stopService 请求失败");

    // 等到停机真正开始(SSE 收到 service-stopping = 停机请求已生效)。
    let mut notice = String::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && !notice.contains("event: service-stopping") {
        notice.push_str(&read_some(&mut sse, 8192));
    }
    assert!(
        notice.contains("event: service-stopping"),
        "未收到 service-stopping 停机通告帧: {notice}"
    );

    // 顺序停机:网关面还在排空,管理面必须**仍然在线**且如实报「正在停止」——
    // 浏览器据此区分「正在停止」与「已停止」。
    let body = rpc_call(web_port, "test-stop-drain-token", "getRuntimeControl", "[]")
        .expect("网关面排空期间管理面不应关闭");
    assert!(
        body.contains("\"stopping\":true"),
        "排空期间 stopping 应为 true: {body}"
    );

    // 最终仍必须退出(网关面排空超时兜底)。
    assert_exits(result_rx, "ordered-stop");
}

#[test]
fn runtime_control_reports_stopping_after_stop_request() {
    let (settings, web_port, _gateway_port) = test_settings("stopping-flag");
    let (result_rx, _handle) = spawn_server(settings);

    assert!(
        wait_for_health(web_port, Duration::from_secs(60)),
        "管理面未在 60s 内就绪(port={web_port})"
    );

    let response = rpc_call(web_port, "test-stop-drain-token", "getRuntimeControl", "[]")
        .expect("getRuntimeControl 请求失败");
    assert!(
        response.contains("\"stopping\":false"),
        "停机前 stopping 应为 false: {response}"
    );

    let _ = rpc_call(web_port, "test-stop-drain-token", "stopService", "[]")
        .expect("stopService 请求失败");

    // 受理后 `stopping` 应立刻变 true;但管理面监听也会随之关闭,
    // 因此「连不上」同样说明已进入停机态 —— 两种结果都算通过。
    let deadline = Instant::now() + Duration::from_millis(900);
    let mut observed = false;
    while Instant::now() < deadline && !observed {
        match rpc_call(web_port, "test-stop-drain-token", "getRuntimeControl", "[]") {
            Some(body) => {
                assert!(
                    body.contains("\"stopping\":true"),
                    "停机受理后 stopping 应为 true: {body}"
                );
                observed = true;
            }
            None => {
                // 管理面监听已关闭 → 已进入停机收尾。
                observed = true;
            }
        }
    }
    assert!(observed, "未观察到停机受理状态");

    assert_exits(result_rx, "stopping-flag");
}
