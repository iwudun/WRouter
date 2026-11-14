//! axum 服务测试夹具:真实监听 socket 上启动 Router 并提供受控关闭。
//!
//! 黄金样本 parity(R2 起)与 e2e 测试通过它拿到一个真实的
//! `http://127.0.0.1:<port>` 基址,避免 TestClient 式内存传输掩盖
//! 流式/SSE 行为差异(R0 的教训:内存传输无法测流式边界)。

use std::time::Duration;

use tokio::task::JoinHandle;

/// 在真实 TCP 监听上启动 router,返回 `(基址, 关闭句柄)`。
///
/// `shutdown` 被触发后 axum 停止接受新连接、等待在途请求完成
/// (graceful),与生产 `wr-server::run` 同一语义。
pub async fn serve(
    router: axum::Router,
    host: &str,
    port: u16,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> (String, JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind((host, port))
        .await
        .expect("bind test listener");
    let base = format!("http://{host}:{port}");
    let handle = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let mut rx = shutdown;
                let _ = rx.wait_for(|stop| *stop).await;
            })
            .await
            .expect("axum serve");
    });
    // 给内核一点时间完成 bind/accept 闭环,避免测试立刻连接时偶发拒绝。
    tokio::time::sleep(Duration::from_millis(20)).await;
    (base, handle)
}

/// 创建关闭信号通道:`(sender, receiver)`;调用 `tx.send(true)` 触发优雅停机。
pub fn shutdown_channel() -> (
    tokio::sync::watch::Sender<bool>,
    tokio::sync::watch::Receiver<bool>,
) {
    tokio::sync::watch::channel(false)
}
