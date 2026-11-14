//! 服务控制通道(20261102_01 P6)。
//!
//! 「暂停 / 恢复 / 退出服务」的**进程内**控制面:
//! - [`ServiceControl`]:四路 `tokio::sync::watch` 广播 ——
//!   网关面启停意图、网关面实际监听态、网关面最近错误、整进程停机请求;
//! - [`ManagedBy`]:后端**被谁托管**(桌面壳内嵌 / 独立进程 / systemd),
//!   决定页面「退出」按钮的语义与文案。
//!
//! 约束:后端 `Cargo.toml` / `Cargo.lock` **零变更**(方案硬约束)——
//! 这里只用已在依赖里的 `tokio::sync::watch`,不引入任何新 crate。

use std::sync::Arc;

use tokio::sync::watch;

/// 后端的托管方式(20261102_01 P6)。
///
/// 决定「退出服务」在不同运行时下的**实际语义**:
///
/// | 变体 | 场景 | 「退出」含义 | 能否从页面触发 |
/// |---|---|---|---|
/// | [`ManagedBy::Embedded`] | Tauri 桌面壳内同进程(P3-2) | 退出应用(后端随之退出) | **否** —— 页面停后端会让窗口变废,应由用户关窗口 |
/// | [`ManagedBy::Process`] | 独立后端进程(手跑 `wr serve`) | 停止后端进程 | 是 |
/// | [`ManagedBy::Systemd`] | systemd 单元托管(RPM 交付形态) | 停止 systemd 用户服务 | 是 |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedBy {
    /// 后端跑在桌面壳进程内(同进程)。
    Embedded,
    /// 独立后端进程(手动启动 / 其它 supervisor)。
    Process,
    /// systemd 托管(用户级或系统级单元)。
    Systemd,
}

impl ManagedBy {
    /// 契约里的字符串形态(`embedded` / `process` / `systemd`)。
    pub fn as_str(self) -> &'static str {
        match self {
            ManagedBy::Embedded => "embedded",
            ManagedBy::Process => "process",
            ManagedBy::Systemd => "systemd",
        }
    }

    /// 解析 `WROUTER_MANAGED_BY`;非法值返回 `None`(调用方回落探测)。
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "embedded" | "tauri" | "desktop" => Some(ManagedBy::Embedded),
            "process" | "standalone" => Some(ManagedBy::Process),
            "systemd" | "service" => Some(ManagedBy::Systemd),
            _ => None,
        }
    }

    /// 是否允许**从页面**触发「停止服务」。
    ///
    /// 桌面壳内嵌时为否:后端与壳同进程,停掉后端只会让窗口变空白,
    /// 正确做法是让用户关窗口(壳退出时后端随之优雅停机)。
    pub fn can_stop_from_page(self) -> bool {
        !matches!(self, ManagedBy::Embedded)
    }

    /// 重新拉起服务的提示命令(空串 = 无需提示)。
    pub fn restart_hint(self) -> &'static str {
        match self {
            ManagedBy::Embedded => "",
            ManagedBy::Systemd => "systemctl --user start wrouter.service",
            ManagedBy::Process => "wr serve",
        }
    }

    /// 从环境变量装配:
    /// 1. `WROUTER_MANAGED_BY` 显式指定(桌面壳置 `embedded`);
    /// 2. systemd 会给服务进程注入 `INVOCATION_ID` → 视为 systemd 托管;
    /// 3. 其余 → 独立进程。
    pub fn from_env() -> Self {
        if let Ok(raw) = std::env::var("WROUTER_MANAGED_BY")
            && let Some(parsed) = ManagedBy::parse(&raw)
        {
            return parsed;
        }
        if std::env::var_os("INVOCATION_ID").is_some() {
            return ManagedBy::Systemd;
        }
        ManagedBy::Process
    }
}

/// 服务控制通道(可克隆;克隆体共享同一组广播)。
#[derive(Clone)]
pub struct ServiceControl {
    /// 网关面**启停意图**:true = 应在运行,false = 应暂停。
    gateway_enabled: watch::Sender<bool>,
    /// 网关面**实际监听态**:true = 3460 正在监听。
    gateway_running: watch::Sender<bool>,
    /// 网关面最近一次错误(bind 失败等);`None` = 无错误。
    gateway_error: watch::Sender<Option<String>>,
    /// 整进程停机请求(页面「停止服务」)。
    stop_requested: watch::Sender<bool>,
    /// 停机**已受理**(页面已点「停止服务」,但进程尚未开始退出)。
    ///
    /// 与 [`ServiceControl::stop_requested`] 的差别是**时间差**:
    /// - `stop_accepted` = 立刻置位 → 页面立刻能报「正在停止」;
    /// - `stop_requested` = 延迟置位(给 HTTP 响应留落地时间)→ 真正开始收尾。
    stop_accepted: watch::Sender<bool>,
}

impl Default for ServiceControl {
    fn default() -> Self {
        Self::new()
    }
}

impl ServiceControl {
    /// 新建控制通道:网关面默认启用、未运行、无错误、未请求停机。
    pub fn new() -> Self {
        let (gateway_enabled, _) = watch::channel(true);
        let (gateway_running, _) = watch::channel(false);
        let (gateway_error, _) = watch::channel(None::<String>);
        let (stop_requested, _) = watch::channel(false);
        let (stop_accepted, _) = watch::channel(false);
        Self {
            gateway_enabled,
            gateway_running,
            gateway_error,
            stop_requested,
            stop_accepted,
        }
    }

    /// 网关面启停意图。
    pub fn gateway_enabled(&self) -> bool {
        *self.gateway_enabled.borrow()
    }

    /// 设置网关面启停意图(广播给网关面任务)。
    pub fn set_gateway_enabled(&self, enabled: bool) {
        self.gateway_enabled.send_replace(enabled);
    }

    /// 订阅网关面启停意图。
    pub fn gateway_enabled_rx(&self) -> watch::Receiver<bool> {
        self.gateway_enabled.subscribe()
    }

    /// 网关面当前是否真的在监听 3460。
    pub fn gateway_running(&self) -> bool {
        *self.gateway_running.borrow()
    }

    /// 设置网关面监听态。
    pub fn set_gateway_running(&self, running: bool) {
        self.gateway_running.send_replace(running);
    }

    /// 网关面最近一次错误。
    pub fn gateway_error(&self) -> Option<String> {
        self.gateway_error.borrow().clone()
    }

    /// 设置网关面最近一次错误。
    pub fn set_gateway_error(&self, error: Option<String>) {
        self.gateway_error.send_replace(error);
    }

    /// 请求整进程停机(页面「停止服务」)。
    pub fn request_stop(&self) {
        self.stop_requested.send_replace(true);
    }

    /// 是否已收到停机请求。
    pub fn stop_requested(&self) -> bool {
        *self.stop_requested.borrow()
    }

    /// 订阅停机请求。
    pub fn stop_requested_rx(&self) -> watch::Receiver<bool> {
        self.stop_requested.subscribe()
    }

    /// 标记「停机已受理」(页面点下「停止服务」的瞬间)。
    ///
    /// 与 [`ServiceControl::request_stop`] 分开:受理要**立刻**可见
    /// (页面据此显示「正在停止」),真正停机要等 HTTP 响应落地后再开始。
    pub fn accept_stop(&self) {
        self.stop_accepted.send_replace(true);
    }

    /// 是否已受理停机(进程尚未退出)。
    pub fn stop_accepted(&self) -> bool {
        *self.stop_accepted.borrow()
    }

    /// 订阅「停机已受理」。
    pub fn stop_accepted_rx(&self) -> watch::Receiver<bool> {
        self.stop_accepted.subscribe()
    }

    /// 综合停机态:受理或已请求都算「正在停止」。
    pub fn stopping(&self) -> bool {
        self.stop_accepted() || self.stop_requested()
    }

    /// 网关面状态机(供 `getGatewayStatus` / `getRuntimeControl` 上报):
    /// `error` > `stopped`(已暂停)> `starting`(已启用但尚未监听)> `running`。
    ///
    /// 按**启停意图**推导(而不是只看瞬时监听态)—— 启停是异步的:
    /// 用户点了「暂停」而监听还没释放的那一瞬间,也应报 `stopped`,
    /// 否则界面会闪回「运行中」。
    pub fn gateway_state(&self) -> &'static str {
        if self.gateway_error().is_some() {
            return "error";
        }
        if !self.gateway_enabled() {
            return "stopped";
        }
        if self.gateway_running() {
            "running"
        } else {
            "starting"
        }
    }

    /// 同 [`ServiceControl::gateway_state`]:启停指令的**即时回执**也用同一套推导。
    pub fn gateway_state_intent(&self) -> &'static str {
        self.gateway_state()
    }
}

/// 便捷别名:控制通道的共享句柄。
pub type SharedServiceControl = Arc<ServiceControl>;

#[cfg(test)]
mod tests {
    use super::{ManagedBy, ServiceControl};

    #[test]
    fn managed_by_parse_is_case_insensitive_and_rejects_garbage() {
        assert_eq!(ManagedBy::parse("embedded"), Some(ManagedBy::Embedded));
        assert_eq!(ManagedBy::parse("  Systemd "), Some(ManagedBy::Systemd));
        assert_eq!(ManagedBy::parse("process"), Some(ManagedBy::Process));
        assert_eq!(ManagedBy::parse("nope"), None);
    }

    #[test]
    fn embedded_cannot_be_stopped_from_page() {
        assert!(!ManagedBy::Embedded.can_stop_from_page());
        assert!(ManagedBy::Process.can_stop_from_page());
        assert!(ManagedBy::Systemd.can_stop_from_page());
        assert_eq!(ManagedBy::Embedded.restart_hint(), "");
        assert!(ManagedBy::Systemd.restart_hint().contains("systemctl"));
    }

    #[test]
    fn gateway_state_transitions() {
        let control = ServiceControl::new();
        assert_eq!(control.gateway_state_intent(), "starting");
        control.set_gateway_running(true);
        assert_eq!(control.gateway_state(), "running");
        control.set_gateway_enabled(false);
        assert_eq!(control.gateway_state_intent(), "stopped");
        control.set_gateway_error(Some("bind failed".to_owned()));
        assert_eq!(control.gateway_state_intent(), "error");
    }

    #[test]
    fn stop_request_is_visible_to_subscribers() {
        let control = ServiceControl::new();
        let rx = control.stop_requested_rx();
        assert!(!control.stop_requested());
        control.request_stop();
        assert!(control.stop_requested());
        assert!(*rx.borrow());
    }

    #[test]
    fn stop_accepted_is_visible_before_the_process_actually_stops() {
        let control = ServiceControl::new();
        assert!(!control.stopping());
        // 受理 → 立刻可见(页面据此显示「正在停止」)。
        control.accept_stop();
        assert!(control.stop_accepted());
        assert!(control.stopping());
        // 但进程此刻还没开始停机。
        assert!(!control.stop_requested());
        control.request_stop();
        assert!(control.stopping());
    }
}
