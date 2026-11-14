//! 投递协调与输出账本(P2-4)。
//!
//! 三个事实分开记录:
//! - **produced**:编码器已生成帧;
//! - **queued**:本地 mpsc 已接受(只证明队列接受,不是远端确认);
//! - **committed**:HTTP 响应头已确定且至少一帧进入下游通道,之后不得
//!   再更换上游响应(C4/C7)。
//!
//! 提交前失败通过握手返回真实 HTTP 状态;提交后错误只能作为协议内的
//! 合法错误/不完整终态表达。`Drop` 兜底保证管线异常结束时不会留下
//! 未决握手。

use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use wr_domain::protocol::sse_error_event_name;
use wr_domain::sse::{SsePayload, encode_sse};

pub(super) type Tx = mpsc::Sender<Result<Vec<u8>, std::convert::Infallible>>;

/// 管线在首个下游字节前必须给出的 HTTP 决策。
#[derive(Debug)]
pub enum StreamHandshake {
    /// 提交为 200 + `text/event-stream`,附带提交时已知的必要头
    /// (缓存命中/路由/账号归因;空表示只有基础头)。
    Committed { headers: Vec<(String, String)> },
    /// 尚未提交:用真实 HTTP 状态和协议错误体拒绝。
    Rejected { status: u16, body: Value },
}

/// 一批待提交帧及其来源范围。
#[derive(Debug, Default)]
pub(super) struct EmissionBatch {
    pub frames: Vec<Vec<u8>>,
    /// 该批是否包含模型内容(首批内容决定首字打点)。
    pub has_content: bool,
    /// 对应的上游记录范围(合成帧为 None)。
    pub source: Option<SourceSpan>,
}

impl EmissionBatch {
    pub fn from_frames(frames: Vec<String>, has_content: bool) -> Self {
        Self {
            frames: frames.into_iter().map(String::into_bytes).collect(),
            has_content,
            source: None,
        }
    }

    pub fn with_source(mut self, span: SourceSpan) -> Self {
        self.source = Some(span);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SourceSpan {
    pub start: u64,
    pub end: u64,
}

/// 输出账本快照(测试/诊断)。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct DeliveryLedger {
    pub produced_frames: u64,
    pub queued_frames: u64,
    pub queued_bytes: u64,
    /// headers 已提交后成功入队的帧/字节。
    pub committed_frames: u64,
    pub committed_bytes: u64,
    pub committed_source_end: u64,
    pub headers_committed: bool,
    pub content_committed: bool,
    pub client_gone: bool,
    pub rejected: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DeliveryError {
    ClientGone,
    AlreadyRejected,
    HandshakeClosed,
}

/// 串行化提交决策与帧入队;管线内所有下游写入都经过这里。
pub(super) struct Delivery<'a> {
    tx: &'a Tx,
    handshake: Option<oneshot::Sender<StreamHandshake>>,
    ledger: DeliveryLedger,
    commit_headers: Vec<(String, String)>,
}

impl<'a> Delivery<'a> {
    pub fn new(tx: &'a Tx, handshake: oneshot::Sender<StreamHandshake>) -> Self {
        Self {
            tx,
            handshake: Some(handshake),
            ledger: DeliveryLedger::default(),
            commit_headers: Vec::new(),
        }
    }

    /// 在提交前声明必要的响应头(提交后不再生效)。
    pub fn set_headers(&mut self, headers: Vec<(String, String)>) {
        if !self.committed() {
            self.commit_headers = headers;
        }
    }

    pub fn ledger(&self) -> DeliveryLedger {
        self.ledger
    }

    /// HTTP 响应头是否已经不可撤回地提交。
    pub fn committed(&self) -> bool {
        self.ledger.headers_committed
    }

    pub fn content_committed(&self) -> bool {
        self.ledger.content_committed
    }

    pub fn client_gone(&self) -> bool {
        self.ledger.client_gone
    }

    /// 外部取消(客户端断开/选择分支)标记;提交前 Drop 将据此返回 499。
    pub fn mark_client_gone(&mut self) {
        self.ledger.client_gone = true;
    }

    /// 提交前拒绝:返回真实 HTTP 状态与协议错误体。
    pub fn reject(&mut self, status: u16, body: Value) -> bool {
        self.ledger.rejected = true;
        match self.handshake.take() {
            Some(handshake) => handshake
                .send(StreamHandshake::Rejected { status, body })
                .is_ok(),
            None => false,
        }
    }

    /// 无论是否已提交,都按协议错误表达;提交前会升级为 HTTP 错误。
    pub async fn error(
        &mut self,
        protocol: &str,
        status: u16,
        envelope: &Value,
    ) -> Result<(), DeliveryError> {
        if self.ledger.rejected {
            return Err(DeliveryError::AlreadyRejected);
        }
        if !self.committed() {
            self.reject(status, envelope.clone());
            return Ok(());
        }
        let frame = encode_sse(sse_error_event_name(protocol), SsePayload::Json(envelope));
        self.emit(EmissionBatch::from_frames(vec![frame], false))
            .await
    }

    /// 投递一批帧。首帧发送前先提交 HTTP 头。
    pub async fn emit(&mut self, batch: EmissionBatch) -> Result<(), DeliveryError> {
        if self.ledger.rejected {
            return Err(DeliveryError::AlreadyRejected);
        }
        self.ledger.produced_frames = self
            .ledger
            .produced_frames
            .saturating_add(batch.frames.len() as u64);
        if batch.frames.is_empty() {
            return Ok(());
        }
        if !self.committed() {
            let Some(handshake) = self.handshake.take() else {
                return Err(DeliveryError::HandshakeClosed);
            };
            handshake
                .send(StreamHandshake::Committed {
                    headers: std::mem::take(&mut self.commit_headers),
                })
                .map_err(|_| DeliveryError::HandshakeClosed)?;
            self.ledger.headers_committed = true;
        }
        for frame in batch.frames {
            let len = frame.len() as u64;
            if self.tx.send(Ok(frame)).await.is_err() {
                self.ledger.client_gone = true;
                return Err(DeliveryError::ClientGone);
            }
            self.ledger.queued_frames = self.ledger.queued_frames.saturating_add(1);
            self.ledger.queued_bytes = self.ledger.queued_bytes.saturating_add(len);
            self.ledger.committed_frames = self.ledger.committed_frames.saturating_add(1);
            self.ledger.committed_bytes = self.ledger.committed_bytes.saturating_add(len);
        }
        if batch.has_content {
            self.ledger.content_committed = true;
        }
        if let Some(source) = batch.source {
            self.ledger.committed_source_end = self.ledger.committed_source_end.max(source.end);
        }
        Ok(())
    }
}

impl Drop for Delivery<'_> {
    fn drop(&mut self) {
        // 管线异常返回且未做任何决策:给 HTTP 层一个确定失败,而不是
        // 让 handler 永久等待/静默 200。
        if let Some(handshake) = self.handshake.take() {
            let status = if self.ledger.client_gone { 499 } else { 502 };
            let _ = handshake.send(StreamHandshake::Rejected {
                status,
                body: json!({"error": {
                    "type": "wrouter_error",
                    "message": if self.ledger.client_gone {
                        "client disconnected before stream commit"
                    } else {
                        "stream pipeline ended before commit decision"
                    }
                }}),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope() -> Value {
        json!({"error": {"type": "test_error", "message": "boom"}})
    }

    #[tokio::test]
    async fn first_emit_commits_headers_before_queueing() {
        let (tx, mut rx) = mpsc::channel(4);
        let (handshake_tx, handshake_rx) = oneshot::channel();
        let mut delivery = Delivery::new(&tx, handshake_tx);
        delivery
            .emit(EmissionBatch::from_frames(
                vec!["data: x\n\n".to_owned()],
                true,
            ))
            .await
            .unwrap();
        assert!(matches!(
            handshake_rx.await.unwrap(),
            StreamHandshake::Committed { .. }
        ));
        assert!(delivery.committed());
        assert!(delivery.content_committed());
        assert_eq!(delivery.ledger().queued_frames, 1);
        assert!(rx.recv().await.is_some());
    }

    #[tokio::test]
    async fn pre_commit_error_rejects_with_real_status() {
        let (tx, _rx) = mpsc::channel(4);
        let (handshake_tx, handshake_rx) = oneshot::channel();
        let mut delivery = Delivery::new(&tx, handshake_tx);
        delivery.error("openai", 429, &envelope()).await.unwrap();
        assert!(!delivery.committed());
        assert!(delivery.ledger().rejected);
        match handshake_rx.await.unwrap() {
            StreamHandshake::Rejected { status, body } => {
                assert_eq!(status, 429);
                assert_eq!(body["error"]["type"], "test_error");
            }
            other => panic!("unexpected handshake: {other:?}"),
        }
    }

    #[tokio::test]
    async fn post_commit_error_is_encoded_as_sse() {
        let (tx, mut rx) = mpsc::channel(4);
        let (handshake_tx, handshake_rx) = oneshot::channel();
        let mut delivery = Delivery::new(&tx, handshake_tx);
        delivery
            .emit(EmissionBatch::from_frames(
                vec!["data: first\n\n".to_owned()],
                false,
            ))
            .await
            .unwrap();
        let _ = handshake_rx.await.unwrap();
        delivery.error("openai", 500, &envelope()).await.unwrap();
        let first = rx.recv().await.unwrap().unwrap();
        let second = rx.recv().await.unwrap().unwrap();
        assert_eq!(first, b"data: first\n\n");
        let second = String::from_utf8(second).unwrap();
        assert!(second.contains("test_error"), "{second}");
    }

    #[tokio::test]
    async fn receiver_drop_marks_client_gone_without_pretending_success() {
        let (tx, rx) = mpsc::channel(1);
        let (handshake_tx, handshake_rx) = oneshot::channel();
        let mut delivery = Delivery::new(&tx, handshake_tx);
        drop(rx);
        let error = delivery
            .emit(EmissionBatch::from_frames(
                vec!["data: x\n\n".to_owned()],
                true,
            ))
            .await
            .unwrap_err();
        assert_eq!(error, DeliveryError::ClientGone);
        assert!(delivery.client_gone());
        assert!(!delivery.content_committed());
        let _ = handshake_rx.await; // 已提交或关闭,不得挂起。
    }

    #[tokio::test]
    async fn dropping_without_decision_rejects_instead_of_hanging() {
        let (tx, _rx) = mpsc::channel(1);
        let (handshake_tx, handshake_rx) = oneshot::channel();
        {
            let _delivery = Delivery::new(&tx, handshake_tx);
        }
        match handshake_rx.await.unwrap() {
            StreamHandshake::Rejected { status, .. } => assert_eq!(status, 502),
            other => panic!("unexpected handshake: {other:?}"),
        }
    }
}
