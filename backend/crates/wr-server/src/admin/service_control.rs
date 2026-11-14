//! 服务控制组(20261102_01 P6):暂停 / 恢复 / 退出服务。
//!
//! 三个管理 RPC(全部**通用能力**,不限运行时):
//! - `getRuntimeControl`:自报后端的托管方式与可执行的控制动作;
//! - `setGatewayEnabled`:暂停 / 恢复**网关面**(3460);管理面(3462)不受影响;
//! - `stopService`:优雅停机整进程(仅独立进程 / systemd 托管时可用)。
//!
//! 语义要点(与前端契约 `RuntimeControl` 对齐):
//! - **暂停网关 = 关闭 3460 监听**(不占端口、不转发流量)→ 客户端得到
//!   connection refused;管理面保持在线,可随时恢复;
//! - **停止服务 = 进程退出**(退出码 0)→ systemd `Restart=on-failure`
//!   不会把它拉起来,即服务真的停了;页面给出重新拉起的提示命令;
//! - **桌面壳内嵌模式**(`managedBy=embedded`)下 `stopService` 一律拒绝 ——
//!   后端与壳同进程,停后端只会让窗口变废,应由用户关窗口(壳退出时后端随之停机)。
//!
//! 「受理」与「退出」是两件事(2026-11-03):
//! - `stopService` 的回执只代表**请求已受理**(`stopping: true`);
//! - 进程真正退出要等收尾完成(SSE 收流 → 两面排空 → 落日志 → 关库),
//!   期间页面应显示「正在停止」,**不得**把「已受理」当成「已退出」。

use std::time::Duration;

use serde_json::{Value, json};

use super::AdminState;
use crate::service_control::ManagedBy;

/// 停机前的延迟(毫秒):先让 HTTP 响应落地,再优雅停机。
///
/// 前端收到响应后立刻切到「服务已停止」落地页;这 1.2s 留给响应写回 +
/// 浏览器渲染,避免页面先断、提示后到。
const STOP_DELAY_MS: u64 = 1200;

/// RPC `getRuntimeControl`:后端自报运行时控制能力。
pub fn svc_runtime_control(state: &AdminState) -> Value {
    let managed: ManagedBy = state.settings.managed_by;
    json!({
        "ok": true,
        "managedBy": managed.as_str(),
        // 暂停网关:所有运行时都支持(桌面壳内嵌时管理面与网关面同进程,
        // 暂停只关 3460 → 窗口与 API 不受影响)。
        "canPause": true,
        // 停止服务:仅独立进程 / systemd 托管可用(桌面壳内嵌请关窗口)。
        "canExit": managed.can_stop_from_page(),
        "exitAction": if managed.can_stop_from_page() { "stopService" } else { "exitApp" },
        "restartHint": managed.restart_hint(),
        // 停机**已受理但进程尚未退出** —— 页面据此显示「正在停止」,
        // 而不是把「请求已受理」当成「进程已退出」。
        "stopping": state.control.stopping(),
        "gatewayState": state.control.gateway_state(),
        "gatewayEnabled": state.control.gateway_enabled(),
        "gatewayError": state.control.gateway_error(),
        "gatewayEndpoint": format!("http://{}:{}", state.settings.host, state.settings.gateway_port),
        "managementEndpoint": format!("http://{}:{}", state.settings.host, state.settings.web_port),
    })
}

/// RPC `setGatewayEnabled`:暂停 / 恢复网关面。
pub fn svc_set_gateway_enabled(state: &AdminState, args: &[Value]) -> Result<Value, String> {
    let enabled = args
        .first()
        .and_then(Value::as_bool)
        .ok_or_else(|| "setGatewayEnabled 需要布尔参数 enabled".to_string())?;
    state.control.set_gateway_enabled(enabled);
    tracing::info!(
        target: "wrouter.admin",
        enabled,
        "网关面启停请求已受理"
    );
    Ok(json!({
        "ok": true,
        "enabled": enabled,
        // 真正的停/启是异步的 → 回执按**意图**给状态,不按瞬时监听态。
        "state": state.control.gateway_state_intent(),
    }))
}

/// RPC `stopService`:优雅停机整进程(页面「停止服务」)。
///
/// 桌面壳内嵌模式直接拒绝(见模块文档)。
///
/// 回执语义:「**停机请求已受理**」,**不是**「进程已退出」。
/// 真正的退出发生在 [`STOP_DELAY_MS`] 之后,由 [`crate::service_control::ServiceControl`]
/// 的停机信号驱动整进程收尾(管理面 SSE 主动收流 → 两面排空 → 落日志 → 关库)。
pub fn svc_stop_service(state: &AdminState) -> Result<Value, String> {
    let managed: ManagedBy = state.settings.managed_by;
    if !managed.can_stop_from_page() {
        return Err(
            "桌面端请直接关闭应用窗口(后端与应用同进程,随应用退出)".to_string(),
        );
    }
    let control = state.control.clone();
    // 受理即刻可见 → 页面能立刻显示「正在停止」,而不是等进程真的退出。
    control.accept_stop();
    tokio::spawn(async move {
        // 给 HTTP 响应留落地时间,再真正开始停机收尾。
        tokio::time::sleep(Duration::from_millis(STOP_DELAY_MS)).await;
        control.request_stop();
    });
    Ok(json!({
        "ok": true,
        "stopping": true,
        "restartHint": managed.restart_hint(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_service_is_refused_for_embedded_runtime() {
        // 桌面壳内嵌:后端与壳同进程 → 页面不得停后端。
        assert!(!ManagedBy::Embedded.can_stop_from_page());
        assert!(ManagedBy::Process.can_stop_from_page());
    }
}
