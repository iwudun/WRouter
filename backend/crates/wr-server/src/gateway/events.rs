//! 管理面事件总线(方案 #14 R4.9;端点在 R5 落地)。
//!
//! 契约来自 `contracts/events-schema.json`(D7 / R0.6):单一
//! `GET /api/wr/events` SSE 端点,四类事件 `config-changed` /
//! `log-appended` / `usage-updated` / `gateway-status`,framing 为
//! `event: <type>` + `data: <json>` + 空行。
//!
//! R4 只负责**产生**事件(网关侧的重试/回退/熔断/落库节点)+ tracing;
//! R5 订阅本总线把它变成 SSE 响应。总线用 `tokio::sync::broadcast`:
//! - 多读者(未来多个 SSE 连接)各自独立游标;
//! - 无订阅者时发布直接丢弃(不为没人听的推送留内存);
//! - `Last-Event-ID` 在 R0 契约里明确「不支持回放」,所以这里不做重放缓冲。

use serde_json::{Map, Value, json};
use tokio::sync::broadcast;

/// 事件类型(与 schema 的 `type` 字段逐字一致)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    ConfigChanged,
    LogAppended,
    UsageUpdated,
    GatewayStatus,
    /// 在途请求登记/进入处理中(调用日志 等待中/处理中;Rust 新增)。
    InflightChanged,
    /// 整进程停机开始(Rust 新增):管理面 SSE 在此事件后**主动收流**,
    /// 否则浏览器长连接会让 axum 的优雅停机永远等不到连接结束 →
    /// 进程退不出(systemd 里一直显示 running)。
    ServiceStopping,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ConfigChanged => "config-changed",
            Self::LogAppended => "log-appended",
            Self::UsageUpdated => "usage-updated",
            Self::GatewayStatus => "gateway-status",
            Self::InflightChanged => "inflight-changed",
            Self::ServiceStopping => "service-stopping",
        }
    }
}

/// 一条事件(`data` 必须与 schema 的 required 字段一致)。
#[derive(Debug, Clone)]
pub struct Event {
    pub kind: EventKind,
    pub data: Map<String, Value>,
}

impl Event {
    /// SSE 帧文本(framing 与 Python `StreamingResponse` 一致:`\n\n` 收尾)。
    pub fn to_sse_frame(&self) -> String {
        format!(
            "event: {}\ndata: {}\n\n",
            self.kind.as_str(),
            wr_domain::json_py::py_json_dumps_compact(&Value::Object(self.data.clone()))
        )
    }
}

/// 事件发布端(网关持有)。
#[derive(Clone)]
pub struct EventPublisher {
    sender: broadcast::Sender<Event>,
}

/// 事件订阅端(R5 的 SSE handler 持有)。
pub struct EventSubscriber {
    receiver: broadcast::Receiver<Event>,
}

impl EventPublisher {
    pub fn new() -> (Self, EventSubscriber) {
        // 256 深度:管理面事件是「提示重取」而非事实源,溢出按丢弃处理。
        let (sender, receiver) = broadcast::channel(256);
        (Self { sender }, EventSubscriber { receiver })
    }

    /// 发布一条事件。当前没有订阅者时返回 `false`(不是错误)。
    pub fn publish(&self, event: Event) -> bool {
        self.sender.send(event).is_ok()
    }

    pub fn subscriber_count(&self) -> usize {
        self.sender.receiver_count()
    }

    /// 创建独立订阅者，支持多个管理面 SSE 客户端并行连接。
    pub fn subscribe(&self) -> EventSubscriber {
        EventSubscriber {
            receiver: self.sender.subscribe(),
        }
    }
}

impl Default for EventPublisher {
    fn default() -> Self {
        let (publisher, _subscriber) = Self::new();
        publisher
    }
}

impl EventSubscriber {
    /// 取下一条事件;通道关闭返回 `None`。
    pub async fn next(&mut self) -> Option<Event> {
        loop {
            match self.receiver.recv().await {
                Ok(event) => return Some(event),
                // 落后太多:管理面事件允许丢,直接取下一条。
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 事件构造器(schema required 字段集中在这里,避免各调用点漂移)
// ---------------------------------------------------------------------------

/// `gateway-status` 心跳(R0.6 已落地的唯一活事件)。
pub fn gateway_status_event(ok: bool, version: &str, port: i64, ts_ms: i64) -> Event {
    Event {
        kind: EventKind::GatewayStatus,
        data: map_of(vec![
            ("ok", json!(ok)),
            ("name", json!("WRouter")),
            ("version", json!(version)),
            ("port", json!(port)),
            ("ts", json!(ts_ms)),
        ]),
    }
}

/// `config-changed`:带 scope 提示,客户端可只失效对应查询。
pub fn config_changed_event(scope: &str) -> Event {
    Event {
        kind: EventKind::ConfigChanged,
        data: map_of(vec![("scope", json!(scope))]),
    }
}

/// `log-appended`:至少带 row id,供定点重取(C3 关闭时不发)。
pub fn log_appended_event(request_id: &str, status: i64) -> Event {
    Event {
        kind: EventKind::LogAppended,
        data: map_of(vec![
            ("requestId", json!(request_id)),
            ("status", json!(status)),
        ]),
    }
}

/// `usage-updated`:允许合批,且**不得携带密钥**。
pub fn usage_updated_event(api_key_id: Option<&str>) -> Event {
    let mut data = Map::new();
    if let Some(id) = api_key_id.filter(|value| !value.is_empty()) {
        data.insert("apiKeyId".to_owned(), json!(id));
    }
    Event {
        kind: EventKind::UsageUpdated,
        data,
    }
}

/// `inflight-changed`:在途请求登记(`pending`)或进入处理中(`processing`);
/// 完成事件仍走 `log-appended`。data 只带 request_id 与阶段(不携密钥)。
pub fn inflight_changed_event(request_id: &str, phase: &str) -> Event {
    Event {
        kind: EventKind::InflightChanged,
        data: map_of(vec![
            ("requestId", json!(request_id)),
            ("phase", json!(phase)),
        ]),
    }
}

/// `service-stopping`:整进程停机开始(Rust 新增,无 Python 镜像)。
///
/// 语义:这是**停机已受理**的通告,不是「进程已退出」的确认。客户端收到后
/// 应切到「正在停止」态、取消 SSE 订阅与自动重连,再去确认进程是否退出。
pub fn service_stopping_event(reason: &str, ts_ms: i64) -> Event {
    Event {
        kind: EventKind::ServiceStopping,
        data: map_of(vec![("reason", json!(reason)), ("ts", json!(ts_ms))]),
    }
}

fn map_of(entries: Vec<(&str, Value)>) -> Map<String, Value> {
    let mut map = Map::new();
    for (key, value) in entries {
        map.insert(key.to_owned(), value);
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_names_match_the_contract() {
        assert_eq!(EventKind::ConfigChanged.as_str(), "config-changed");
        assert_eq!(EventKind::LogAppended.as_str(), "log-appended");
        assert_eq!(EventKind::UsageUpdated.as_str(), "usage-updated");
        assert_eq!(EventKind::GatewayStatus.as_str(), "gateway-status");
        assert_eq!(EventKind::InflightChanged.as_str(), "inflight-changed");
        assert_eq!(EventKind::ServiceStopping.as_str(), "service-stopping");
    }

    #[test]
    fn sse_frame_framing_matches_python() {
        let event = gateway_status_event(true, "0.1.2", 3460, 1_700_000_000_000);
        let frame = event.to_sse_frame();
        assert!(frame.starts_with("event: gateway-status\ndata: "));
        assert!(frame.ends_with("\n\n"));
        // Python events endpoint uses separators=(",", ":").
        assert!(frame.contains(r#""name":"WRouter""#));
    }

    /// 停机通告帧:类型必须是 `service-stopping`,data 带 reason + ts。
    #[test]
    fn service_stopping_event_frame_is_well_formed() {
        let event = service_stopping_event("page-stop", 1_700_000_000_000);
        assert_eq!(event.kind, EventKind::ServiceStopping);
        let frame = event.to_sse_frame();
        assert!(frame.starts_with("event: service-stopping\ndata: "));
        assert!(frame.ends_with("\n\n"));
        assert!(frame.contains(r#""reason":"page-stop""#));
        assert!(frame.contains(r#""ts":1700000000000"#));
    }

    #[tokio::test]
    async fn publish_reaches_subscribers_and_drops_without_one() {
        let (publisher, mut subscriber) = EventPublisher::new();
        assert!(
            publisher.publish(config_changed_event("providers")),
            "有订阅者时发布应成功"
        );
        let delivered = subscriber.next().await.expect("订阅者收到事件");
        assert_eq!(delivered.kind, EventKind::ConfigChanged);
        assert_eq!(delivered.data["scope"], json!("providers"));
    }

    /// 唯一订阅者被 drop 后,发布是无副作用的丢弃(不为没人听的事件留内存);多订阅者各自独立。
    #[test]
    fn publish_is_dropped_without_subscribers() {
        let (publisher, subscriber) = EventPublisher::new();
        assert_eq!(publisher.subscriber_count(), 1);
        drop(subscriber);
        assert!(!publisher.publish(config_changed_event("api-keys")));
        assert_eq!(publisher.subscriber_count(), 0);
    }

    #[tokio::test]
    async fn multiple_subscribers_receive_independently() {
        let (publisher, mut sub_a) = EventPublisher::new();
        let mut sub_b = publisher.subscribe();
        assert_eq!(publisher.subscriber_count(), 2);
        assert!(publisher.publish(usage_updated_event(Some("k"))));
        let event_a = sub_a.next().await.expect("sub_a should receive");
        let event_b = sub_b.next().await.expect("sub_b should receive");
        assert_eq!(event_a.kind, EventKind::UsageUpdated);
        assert_eq!(event_b.kind, EventKind::UsageUpdated);
        assert_eq!(event_a.data["apiKeyId"], json!("k"));
        assert_eq!(event_b.data["apiKeyId"], json!("k"));
    }

    #[tokio::test]
    async fn lagging_subscriber_skips_instead_of_failing() {
        let (publisher, mut subscriber) = EventPublisher::new();
        for _ in 0..(256 + 10) {
            publisher.publish(usage_updated_event(Some("k")));
        }
        let event = subscriber.next().await.expect("跳过后仍有事件");
        assert_eq!(event.kind, EventKind::UsageUpdated);
    }

    #[test]
    fn usage_event_carries_no_secret() {
        let event = usage_updated_event(Some("k1"));
        assert_eq!(event.data["apiKeyId"], json!("k1"));
        assert!(usage_updated_event(None).data.is_empty());
        assert!(usage_updated_event(Some("")).data.is_empty());
    }
}
