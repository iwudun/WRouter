//! WRouter 测试基建(方案 #14 D14 / R1.9)。
//!
//! 仅允许作为各 crate 的 dev-dependency 使用,不进入生产装配图。
//! 职责(随阶段扩充):
//! - R1:端口分配与 axum Router 生命周期管理(`net` 模块)。
//! - R2+:wiremock 风格 mock 上游、黄金样本回放夹具
//!   (`testdata/golden/`,路径约定对齐 R7 迁移前的 `backend/tests/fixtures`)。

use std::net::TcpListener;

/// 模块化测试工具集。
pub mod net;

/// 绑定一个空闲的 loopback 端口并返回 `(地址, 端口)`。
///
/// 通过绑定 `127.0.0.1:0` 向操作系统要一个临时端口再立即释放,
/// 存在理论上的竞态,但用于本地测试足够可靠。
pub fn free_tcp_addr() -> (std::net::SocketAddr, u16) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind 127.0.0.1:0");
    let addr = listener.local_addr().expect("local_addr");
    (addr, addr.port())
}
