//! 请求日志落库(方案 #14 R4.5/R4.6;镜像 Python
//! `application/gateway/request_log.py::_record_request`)。
//!
//! 一次请求结束的三件事,顺序与 Python 逐字对齐:
//! 1. **指标恒记**:`metrics.record_http_request(...)` 在 `callLogs` 门**之前**
//!    调用 —— 观测计数与「是否落审计行」是两条独立链路;
//! 2. **C3 门控**:`observability.callLogs` 关掉 → 整行不落(元数据与正文都不
//!    落);用量统计因为从 `request_logs` 派生而自然变空(设计如此);
//! 3. **落库 + 事件**:主行元数据 + 侧表正文一次写入,随后发 `log-appended`
//!    与 `usage-updated`(R5 的 `/events` 端点消费)。
//!
//! ## 与 Python 的一处架构差异(R4.13)
//! Python 每条日志 `await asyncio.to_thread(append_request_log)` —— 比早期
//! 「在事件循环里直接写 sqlite」快,但请求仍要等一次落库。Rust 侧把落库
//! 任务投进 unbounded 队列,由单个 drain 任务交给写线程:请求路径不等磁盘,
//! 写序仍是入队序(单写者),[`RequestLogRecorder::flush`] 用栅栏任务等队列
//! 排空。停机时先 `flush()` 再关 store,故「停机不丢」(R4.11)。

use std::sync::{Arc, Mutex};

use serde_json::{Map, Value};
use tokio::sync::{mpsc, oneshot};
use wr_app::ports::{LogBodies, PendingRequestLog, RequestLogStore};
use wr_domain::observability::{
    RequestLogPayload, build_preview, call_logs_enabled, capture_bodies_enabled, capture_text,
    timing_from_attempts,
};
use wr_domain::usage::Usage;

use super::ConfigSnapshot;
use super::events::{EventPublisher, log_appended_event, usage_updated_event};
use super::metrics::MetricsRegistry;

/// 一次请求的日志事实(字段集与 R2/R3 的 tracing 输出一致)。
#[derive(Debug, Clone, Default)]
pub struct RequestLogEvent {
    pub protocol: String,
    pub model: String,
    pub status: u16,
    pub stream: bool,
    pub duration_ms: f64,
    pub provider_attempts: Vec<String>,
    /// 每次上游尝试的结构化明细(2026-10-08 重试记录丰富化;payload 键
    /// `retry_attempts`,详情页「重试尝试」表的数据源)。空 = 无明细
    /// (未触达上游/旧行为路径),不写键。
    pub retry_attempts: Vec<Value>,
    /// 对应 `stream_end_reason`:completed / client_disconnected /
    /// upstream_error / precheck_failed / policy_denied /
    /// idempotency_conflict / no_provider …
    pub end_reason: &'static str,
    pub provider: Option<String>,
    /// 命中的凭据池账号(方案 20260905_02 K5;仅账号注入成功时非空,
    /// legacy 单 key 为 None)。payload 键 `account_id`,键序紧随 provider。
    pub account: Option<String>,
    pub api_key_id: Option<String>,
    pub first_byte_ms: Option<f64>,
    pub error: Option<String>,
}

/// 20261108_01 §5.2:答案段计时事实(日志 payload 键;有值才写)。
#[derive(Debug, Clone, Copy, Default)]
pub struct AnswerTiming {
    /// 答案完成时刻相对请求起点(毫秒;观察到语义 `Finish` 事实)。
    pub answer_end_ms: Option<f64>,
    /// 首个**答案**内容 token 相对请求起点(毫秒;答案 TTFT,不含思考)。
    pub first_answer_byte_ms: Option<f64>,
    /// 答案内容 10%→90% 跨度(毫秒,×10/8 折算;吞吐分母)。
    pub answer_flow_ms: Option<f64>,
    /// 思考内容 10%→90% 跨度(毫秒;诊断,不进吞吐)。
    pub reasoning_decode_ms: Option<f64>,
    /// 思考增量以文本流可见的字节总量(20261108_01 §13:Chat 可见性判定;
    /// 有可见文本才写键)。
    pub reasoning_content_bytes: Option<i64>,
    /// 协议终态帧/EOF 相对答案完成的拖尾(毫秒;诊断,不进主耗时)。
    pub stream_tail_ms: Option<f64>,
    /// 本流是否观察到思考增量(20261108_01:OpenAI 系上游若未报
    /// `reasoning_tokens`,答案吞吐的分子无法剥离思考 → 样本作废)。
    pub reasoning_observed: bool,
}

/// R4 才补齐的扩展事实(用量入账、正文捕获、请求元信息)。
///
/// 单独拆出来是为了让 R2/R3 既有的 13 个调用点字面量保持不变,只有真的
/// 拿到这些事实的分支才传 `extra`。
#[derive(Debug, Clone, Default)]
pub struct RequestLogExtra {
    pub api_key_name: Option<String>,
    pub requested_model: Option<String>,
    /// 有值即写五个 usage 键(R4.6 的「入账」)。
    pub usage: Option<Usage>,
    pub cost_usd: Option<f64>,
    /// `transformed_payload_preview`。
    pub preview: Option<Value>,
    /// 分支专属字段(如 `precheck` 的 details)。
    pub extra: Map<String, Value>,
    /// 侧表正文(仅 `captureBodies` 打开时非空)。
    pub bodies: Option<LogBodies>,
    pub client: Option<String>,
    pub method: Option<String>,
    pub path: Option<String>,
    /// 请求完成时刻(ISO-Z);Python 的 `completed_at` 字段。
    pub completed_at: Option<String>,
}

impl RequestLogExtra {
    /// 密钥显示名(`api_key_name`,仅当 `api_key_id` 存在时才会落盘)。
    pub fn with_identity(mut self, key_name: &str) -> Self {
        self.api_key_name = Some(key_name.to_owned());
        self
    }

    /// 由转换后的 payload 生成 `transformed_payload_preview`。
    ///
    /// 20261016_01 B-1:带 `enabled` 开关 —— callLogs 关闭时调用方传
    /// `false`,跳过脱敏克隆与截断(行不会落库,构造纯属浪费)。
    pub fn with_preview(mut self, enabled: bool, payload: &Value) -> Self {
        self.preview = if enabled { preview_for(payload) } else { None };
        self
    }

    /// 分支专属字段(Python 的 `extra=` 关键字)。
    pub fn with_extra(mut self, extra: serde_json::Map<String, Value>) -> Self {
        self.extra = extra;
        self
    }

    /// 阶段四 C-7:内容 10%→90% 跨度(payload 键 `flow_ms`;不可测时
    /// 不写键)。
    ///
    /// ⚠️ 须在 [`Self::with_extra`] **之后**调用 —— `with_extra` 整体
    /// 替换 `extra` 映射,先调用会被覆盖掉。
    pub fn with_flow_ms(mut self, flow_ms: Option<f64>) -> Self {
        if let Some(flow_ms) = flow_ms {
            self.extra
                .insert("flow_ms".to_owned(), serde_json::json!(flow_ms));
        }
        self
    }

    /// 20261108_01 §5.2:答案段计时键(`answer_end_ms` /
    /// `first_answer_byte_ms` / `answer_flow_ms` / `reasoning_decode_ms` /
    /// `stream_tail_ms`;有值才写)。
    ///
    /// ⚠️ 同 [`Self::with_flow_ms`]:须在 [`Self::with_extra`] **之后**调用。
    pub fn with_answer_timing(mut self, timing: AnswerTiming) -> Self {
        for (key, value) in [
            ("answer_end_ms", timing.answer_end_ms),
            ("first_answer_byte_ms", timing.first_answer_byte_ms),
            ("answer_flow_ms", timing.answer_flow_ms),
            ("reasoning_decode_ms", timing.reasoning_decode_ms),
            (
                "reasoning_content_bytes",
                timing.reasoning_content_bytes.map(|value| value as f64),
            ),
            ("stream_tail_ms", timing.stream_tail_ms),
        ] {
            if let Some(value) = value {
                self.extra.insert(key.to_owned(), serde_json::json!(value));
            }
        }
        if timing.reasoning_observed {
            self.extra
                .insert("reasoning_observed".to_owned(), serde_json::json!(true));
        }
        self
    }

    /// 用量与成本一次性挂上(R4.6 的入账口径)。
    pub fn with_usage(mut self, usage: Usage, cost_usd: f64) -> Self {
        self.usage = Some(usage);
        self.cost_usd = Some(cost_usd);
        self
    }

    pub fn with_requested_model(mut self, requested: &str, model: &str) -> Self {
        self.requested_model = Some(requested.to_owned());
        let _ = model;
        self
    }

    pub fn with_request_headers(mut self, headers: &[(String, String)]) -> Self {
        if let Some(bodies) = self.bodies.as_mut() {
            let map = headers.iter().map(|(name, value)| (name.clone(), Value::String(value.clone()))).collect();
            bodies.request_headers = Some(redact_headers(map));
        }
        self
    }

    pub fn with_stream_capture_metadata(mut self, original_size: usize, truncated: bool) -> Self {
        if let Some(bodies) = self.bodies.as_mut() {
            bodies.response_body_size = Some(original_size as i64);
            bodies.response_body_truncated = truncated;
        }
        self
    }

    pub fn with_trace_items(mut self, items: Vec<Value>) -> Self {
        if let Some(bodies) = self.bodies.as_mut() {
            let mut used = 0usize;
            bodies.trace_items = items.into_iter().take(200).filter_map(|item| {
                let item = redact_value(item);
                let size = wr_domain::json_py::py_json_dumps(&item).len();
                if used.saturating_add(size) > wr_domain::observability::RAW_BODY_MAX_BYTES { return None; }
                used += size;
                Some(item)
            }).collect();
        }
        self
    }

    pub fn with_request_meta(
        mut self,
        client: Option<&str>,
        method: Option<&str>,
        path: Option<&str>,
        completed_at: Option<&str>,
    ) -> Self {
        self.client = client.map(str::to_owned);
        self.method = method.map(str::to_owned);
        self.path = path.map(str::to_owned);
        self.completed_at = completed_at.map(str::to_owned);
        self
    }

    /// 捕获开关打开时,把请求/响应正文写进侧表行(C3:默认不捕获)。
    ///
    /// 主行只带尺寸与截断标记;三样都为空时不建侧表行(Python 的
    /// `if req_text or resp_text or response_headers:` 条件)。
    pub fn with_capture(
        mut self,
        enabled: bool,
        max_bytes: usize,
        request_body: Option<&Value>,
        response_body: Option<&str>,
        response_headers: Option<Map<String, Value>>,
    ) -> Self {
        if !enabled {
            return self;
        }
        let request = request_body.map(|value| {
            let safe = redact_value(value.clone());
            capture_text(&wr_domain::json_py::py_json_dumps(&safe), max_bytes)
        });
        let response = response_body.map(|text| {
            let safe = redact_text(text);
            capture_text(&safe, max_bytes)
        });
        let response_headers = response_headers.map(redact_headers);
        let request_text = request
            .as_ref()
            .map(|captured| captured.text.clone())
            .filter(|text| !text.is_empty());
        let response_text = response
            .as_ref()
            .map(|captured| captured.text.clone())
            .filter(|text| !text.is_empty());
        if request_text.is_none() && response_text.is_none() && response_headers.is_none() {
            return self;
        }
        self.bodies = Some(LogBodies {
            request_body: request_text,
            request_body_size: request.as_ref().map(|captured| captured.size_bytes as i64),
            request_body_truncated: request.as_ref().is_some_and(|captured| captured.truncated),
            response_body: response_text,
            response_body_size: response.as_ref().map(|captured| captured.size_bytes as i64),
            response_body_truncated: response.as_ref().is_some_and(|captured| captured.truncated),
            response_headers,
            request_headers: None,
            trace_items: Vec::new(),
        });
        self
    }
}

fn sensitive_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase().replace('_', "-");
    wr_domain::observability::SENSITIVE_FIELDS.iter().any(|field| normalized == field.replace('_', "-"))
        || normalized.contains("authorization")
        || normalized.contains("api-key")
        // 单数 token 结尾(access_token/session_token 等)是凭据;复数
        // *_tokens(prompt_tokens/cached_tokens 等)是 usage 计数,是排障
        // 要看的数值不是机密 —— 旧 contains("token") 把非流式 JSON 正文
        // 里的用量全打成 "***",无法区分「上游没报缓存」和「被打了码」
        // (流式 SSE 文本不走递归脱敏,两侧显示不一致,曾误导 agnes 缓存
        // 为 0 的排障,2026-10-17)。
        || normalized.ends_with("token")
}

/// 非 JSON 文本的凭据名扫描名单(`redact_text` 按 find 子串匹配)。
/// 与 `SENSITIVE_FIELDS` 对齐但**不含裸 "token"/"auth"**:文本扫描是
/// 子串式的,裸 "token" 会命中 `cached_tokens` 之类的 usage 计数
/// (重蹈 2026-10-17 JSON 侧修复的覆辙),裸 "auth" 会误伤
/// author/oauth 之类普通词。
const REDACT_TEXT_NAMES: &[&str] = &[
    "authorization",
    "x-api-key",
    "api_key",
    "apikey",
    "api-key",
    "access_token",
    "refresh_token",
    "session_token",
    "id_token",
    "x-goog-api-key",
    "cookie",
    "set-cookie",
];

/// 递归脱敏(阶段 D3:每次尝试的采集复用同一套规则)。原地改写,
/// 不再逐节点 clone(正文捕获在网关线程同步执行,512KB 预算下克隆
/// 是纯浪费)。
pub fn redact_value(value: Value) -> Value {
    let mut value = value;
    redact_value_in_place(&mut value);
    value
}

fn redact_value_in_place(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if sensitive_key(key) { *child = Value::String("***".to_owned()); }
                else { redact_value_in_place(child); }
            }
        }
        Value::Array(items) => for child in items { redact_value_in_place(child); },
        Value::String(text) => *text = redact_text(text),
        _ => {}
    }
}

/// 文本脱敏(JSON 则递归;否则按 [`REDACT_TEXT_NAMES`] 扫凭据名 + bearer)。
pub fn redact_text(text: &str) -> String {
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        return wr_domain::json_py::py_json_dumps(&redact_value(value));
    }
    let mut output = text.to_owned();
    for name in REDACT_TEXT_NAMES {
        let mut cursor = 0usize;
        loop {
            let lower = output.to_ascii_lowercase();
            let Some(relative_start) = lower[cursor..].find(name) else { break };
            let start = cursor + relative_start;
            let value_start = start + name.len();
            let Some(delimiter) = output[value_start..].find([':', '=', ' ']).map(|i| value_start + i) else { break };
            let end = output[delimiter + 1..].find([',', '}', '\n', '\r', '&']).map(|i| delimiter + 1 + i).unwrap_or(output.len());
            output.replace_range(delimiter..end, ":\"***\"");
            cursor = (delimiter + 6).min(output.len());
        }
    }
    let mut cursor = 0usize;
    loop {
        let lower = output.to_ascii_lowercase();
        let Some(relative_start) = lower[cursor..].find("bearer ") else { break };
        let start = cursor + relative_start;
        let value_start = start + "bearer ".len();
        let end = output[value_start..].find([' ', ',', '\n', '\r', '"']).map(|i| value_start + i).unwrap_or(output.len());
        output.replace_range(value_start..end, "***");
        cursor = (value_start + 3).min(output.len());
    }
    output
}

/// 敏感 header 过滤。
pub fn redact_headers(headers: Map<String, Value>) -> Map<String, Value> {
    headers.into_iter().map(|(key, value)| {
        let value = if sensitive_key(&key) { Value::String("***".to_owned()) } else { value };
        (key, value)
    }).collect()
}

/// 转换后的 payload → `transformed_payload_preview`(脱敏 + 指纹 + 截断)。
pub fn preview_for(payload: &Value) -> Option<Value> {
    payload.as_object().map(|_| {
        let redacted = redact_value(payload.clone());
        build_preview(redacted.as_object().expect("object shape is preserved"))
    })
}

/// 队列元素:一次落库,或一个「排空到此为止」的栅栏。
#[allow(clippy::large_enum_variant)]
enum LogJob {
    Write {
        snapshot: Arc<ConfigSnapshot>,
        request_id: String,
        event: RequestLogEvent,
        extra: RequestLogExtra,
    },
    Barrier(oneshot::Sender<()>),
}

/// 日志队列容量(20261016_01 B-5):有界 —— 生成速率持续高于落库速率时
/// 丢弃新行并计数告警,而不是无界堆积把内存与尾延迟拖穿。1024 行
/// 约为正常负载下数秒的落库量,瞬时突刺(批量 drain 之前)完全够用。
const LOG_QUEUE_DEPTH: usize = 1024;
/// 单批落库行数上限(B-6):多行一个 SQLite 事务,摊薄 fsync/事务开销。
const LOG_BATCH_MAX: usize = 64;

struct RecorderCore {
    store: Arc<dyn RequestLogStore>,
    metrics: Arc<MetricsRegistry>,
    events: Option<EventPublisher>,
    sender: Mutex<Option<mpsc::Sender<LogJob>>>,
}

/// 入队侧(克隆廉价,网关持有)。
#[derive(Clone)]
pub struct RequestLogRecorder {
    core: Arc<RecorderCore>,
}

/// drain 侧(交给一个后台任务 `run`)。
pub struct RequestLogDrainer {
    core: Arc<RecorderCore>,
    receiver: mpsc::Receiver<LogJob>,
}

impl RequestLogRecorder {
    /// 构造入队侧与 drain 侧(后者交给 [`RequestLogDrainer::run`])。
    pub fn new(
        store: Arc<dyn RequestLogStore>,
        metrics: Arc<MetricsRegistry>,
        events: Option<EventPublisher>,
    ) -> (Self, RequestLogDrainer) {
        let (sender, receiver) = mpsc::channel(LOG_QUEUE_DEPTH);
        let core = Arc::new(RecorderCore {
            store,
            metrics,
            events,
            sender: Mutex::new(Some(sender)),
        });
        (
            Self { core: core.clone() },
            RequestLogDrainer { core, receiver },
        )
    }

    /// 正文捕获是否打开(C3 默认关)。
    pub fn capture_enabled(snapshot: &ConfigSnapshot) -> bool {
        capture_bodies_enabled(Some(&snapshot.config.observability))
    }

    /// 调用日志是否落行(20261016_01 B-1):请求路径用它在**构造前**跳过
    /// preview/capture/trace 等重型序列化 —— 此前门控只在 drain 侧判,
    /// 构造成本已经发生,关闭调用日志省不掉请求路径的 CPU。
    pub fn call_logs_enabled(snapshot: &ConfigSnapshot) -> bool {
        call_logs_enabled(Some(&snapshot.config.observability))
    }

    /// 请求路径入口:即时记指标 + 落库入队(不等磁盘)。
    pub fn enqueue(
        &self,
        snapshot: &Arc<ConfigSnapshot>,
        request_id: &str,
        event: &RequestLogEvent,
    ) {
        self.enqueue_extra(snapshot, request_id, event, RequestLogExtra::default());
    }

    /// 带扩展事实的入口。
    pub fn enqueue_extra(
        &self,
        snapshot: &Arc<ConfigSnapshot>,
        request_id: &str,
        event: &RequestLogEvent,
        extra: RequestLogExtra,
    ) {
        // 指标必须在请求线程上即时累计(callLogs 门管不到它)。
        self.core.metrics.record_http_request(
            "POST",
            &format!("/{}", event.protocol),
            i64::from(event.status),
            event.duration_ms,
        );
        // C3 门控前移(20261016_01 B-1):关掉时不再入队(drain 侧的判定
        // 保留作兜底)。event.clone 与后续落库/事件发布全部省去。
        if !call_logs_enabled(Some(&snapshot.config.observability)) {
            return;
        }
        let guard = self.core.sender.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(sender) = guard.as_ref() {
            let job = LogJob::Write {
                snapshot: Arc::clone(snapshot),
                request_id: request_id.to_owned(),
                event: event.clone(),
                extra,
            };
            // B-5:满即丢 + 计数告警(首条与每 100 条各告警一次,不刷屏)。
            // 20261016_01 收官项:入队成功/丢弃连同瞬时水位一并进指标面。
            if sender.try_send(job).is_err() {
                self.core.metrics.log_pipeline.record_dropped(LOG_QUEUE_DEPTH);
                let dropped = self
                    .core
                    .metrics
                    .log_pipeline
                    .dropped_total
                    .load(std::sync::atomic::Ordering::Relaxed);
                if dropped == 1 || dropped % 100 == 0 {
                    tracing::warn!(target: "wrouter.gateway", dropped, %request_id,
                                   "request_log_queue_full_dropped");
                }
            } else {
                let available = sender.capacity();
                let depth = LOG_QUEUE_DEPTH.saturating_sub(available);
                self.core.metrics.log_pipeline.record_enqueued(depth);
            }
        }
    }

    /// 队列满被丢弃的日志行数(20261016_01 B-5;可观测降级)。
    pub fn dropped_jobs(&self) -> u64 {
        self.core
            .metrics
            .log_pipeline
            .dropped_total
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 等队列排空(测试与停机用)。
    pub async fn flush(&self) {
        let (reply, waiter) = oneshot::channel();
        {
            let guard = self.core.sender.lock().unwrap_or_else(|e| e.into_inner());
            let Some(sender) = guard.as_ref() else {
                return;
            };
            if sender.clone().send(LogJob::Barrier(reply)).await.is_err() {
                return;
            }
        }
        let _ = waiter.await;
    }

    /// 关闭队列(停机时先 [`flush`](Self::flush) 再 `shutdown`)。
    pub fn shutdown(&self) {
        let mut guard = self.core.sender.lock().unwrap_or_else(|e| e.into_inner());
        guard.take();
    }
}

impl RequestLogDrainer {
    /// 批量落库(20261016_01 B-6):`recv_many` 一次取走至多
    /// [`LOG_BATCH_MAX`] 行,单个 SQLite 事务写入;写序仍是入队序。
    /// 栅栏前的写入先落库再应答,`flush()` 的「排空」语义不变。
    pub async fn run(mut self) {
        let mut batch: Vec<LogJob> = Vec::with_capacity(LOG_BATCH_MAX);
        let mut store_rows: Vec<PendingRequestLog> = Vec::with_capacity(LOG_BATCH_MAX);
        let mut notify: Vec<(String, i64, bool, Option<String>)> = Vec::new();
        loop {
            batch.clear();
            let received = self.receiver.recv_many(&mut batch, LOG_BATCH_MAX).await;
            if received == 0 {
                return; // 发送端全部关闭且队列已排空。
            }
            for job in batch.drain(..) {
                match job {
                    LogJob::Barrier(reply) => {
                        self.flush_pending(&mut store_rows, &mut notify).await;
                        drop(reply);
                    }
                    LogJob::Write {
                        snapshot,
                        request_id,
                        event,
                        extra,
                    } => {
                        // C3:callLogs 关掉则整行不落(指标已记过;
                        // enqueue 侧已提前门控,这里是兜底)。
                        if !call_logs_enabled(Some(&snapshot.config.observability)) {
                            continue;
                        }
                        let payload = build_payload(&event, &extra);
                        notify.push((
                            request_id.clone(),
                            i64::from(event.status),
                            event.provider.is_some(),
                            event.api_key_id.clone(),
                        ));
                        store_rows.push(PendingRequestLog {
                            request_id,
                            payload,
                            bodies: extra.bodies.clone(),
                        });
                    }
                }
            }
            self.flush_pending(&mut store_rows, &mut notify).await;
        }
    }

    /// 把积压行交给存储(单事务),成功后发 `log-appended` /
    /// `usage-updated`(与逐条版同序)。
    async fn flush_pending(
        &self,
        store_rows: &mut Vec<PendingRequestLog>,
        notify: &mut Vec<(String, i64, bool, Option<String>)>,
    ) {
        if store_rows.is_empty() {
            notify.clear();
            return;
        }
        // 20261016_01 收官项:批量落库耗时进指标面 —— `flush_ms_max` 飙升
        // 或 `flush_failures_total` 增长是「写线程吃紧」的直接信号。
        let flush_started = std::time::Instant::now();
        let flush_result = self.core.store.append_request_log_batch(store_rows).await;
        let elapsed_us = u64::try_from(flush_started.elapsed().as_micros()).unwrap_or(u64::MAX);
        self.core.metrics.log_pipeline.record_flush(
            store_rows.len(),
            elapsed_us,
            flush_result.is_ok(),
        );
        if let Err(error) = flush_result {
            tracing::warn!(target: "wrouter.gateway", count = store_rows.len(), %error,
                           "request_log_persist_failed");
        } else if let Some(events) = &self.core.events {
            for (request_id, status, has_provider, api_key_id) in notify.drain(..) {
                events.publish(log_appended_event(&request_id, status));
                // 只有真正调用过 provider 的行才动用量(与聚合的「有效请求」
                // 同口径)。
                if has_provider {
                    events.publish(usage_updated_event(api_key_id.as_deref()));
                }
            }
        }
        store_rows.clear();
        notify.clear();
    }
}

/// 拼主行 payload(键序镜像 `_record_request`)。
fn build_payload(event: &RequestLogEvent, extra: &RequestLogExtra) -> Value {
    let mut builder = RequestLogPayload::new(
        &event.protocol,
        &event.model,
        i64::from(event.status),
        event.stream,
        event.duration_ms,
        event.provider_attempts.clone(),
        event.end_reason,
    )
    // 尝试明细紧跟 new 落在 stream_end_reason 之后;空切片不写键。
    .retry_attempts(&event.retry_attempts)
    .api_key(event.api_key_id.as_deref(), extra.api_key_name.as_deref());
    if let Some(requested) = extra.requested_model.as_deref() {
        builder = builder.requested_model(requested, &event.model);
    }
    if let Some(provider) = event.provider.as_deref() {
        builder = builder.provider(provider);
    }
    if let Some(account) = event.account.as_deref() {
        builder = builder.account(account);
    }
    if let Some(usage) = &extra.usage {
        builder = builder.usage(usage);
    }
    if let Some(cost_usd) = extra.cost_usd {
        builder = builder.cost_usd(cost_usd);
    }
    if let Some(first_byte_ms) = event.first_byte_ms {
        builder = builder.first_byte_ms(first_byte_ms);
    }
    // 阶段四 C-4:耗时分解三键(sent/queued/retried),由尝试明细聚合。
    builder = builder.timing(&timing_from_attempts(&event.retry_attempts));
    if let Some(error) = event.error.as_deref() {
        builder = builder.error(error);
    }
    if let Some(preview) = &extra.preview {
        builder = builder.preview(preview.clone());
    }
    if !extra.extra.is_empty() {
        builder = builder.extra(&extra.extra);
    }
    builder = builder.capture_sizes(
        extra
            .bodies
            .as_ref()
            .filter(|bodies| bodies.request_body.is_some())
            .map(|bodies| {
                (
                    bodies.request_body_size.unwrap_or(0),
                    bodies.request_body_truncated,
                )
            }),
        extra
            .bodies
            .as_ref()
            .filter(|bodies| bodies.response_body.is_some())
            .map(|bodies| {
                (
                    bodies.response_body_size.unwrap_or(0),
                    bodies.response_body_truncated,
                )
            }),
    );
    builder = builder.request_meta(
        extra.client.as_deref(),
        extra.method.as_deref(),
        extra.path.as_deref(),
        extra.completed_at.as_deref(),
    );
    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};
    use wr_infra::config::AppConfig;
    use wr_infra::sqlite::SqliteStore;

    static SEQ: AtomicU32 = AtomicU32::new(0);

    fn temp_dir(name: &str) -> PathBuf {
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("wr-r4-logrec-{}-{name}-{seq}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn snapshot_with(observability: Value) -> Arc<ConfigSnapshot> {
        let config = AppConfig {
            observability: observability.as_object().cloned().unwrap_or_default(),
            ..AppConfig::default()
        };
        Arc::new(ConfigSnapshot::from_parts(config, Vec::new(), None))
    }

    async fn harness(name: &str) -> (Arc<SqliteStore>, RequestLogRecorder, PathBuf) {
        let dir = temp_dir(name);
        let store = Arc::new(SqliteStore::open(&dir).await.unwrap());
        let (recorder, drainer) =
            RequestLogRecorder::new(store.clone(), Arc::new(MetricsRegistry::new()), None);
        tokio::spawn(drainer.run());
        (store, recorder, dir)
    }

    /// C3:`callLogs=false` 时整行不落,但指标仍记(两条链路独立)。
    #[tokio::test]
    async fn calllogs_gating_skips_row_but_keeps_metrics() {
        let (store, recorder, dir) = harness("gate").await;
        let snapshot = snapshot_with(json!({"callLogs": false}));
        recorder.enqueue(
            &snapshot,
            "req-off",
            &RequestLogEvent {
                protocol: "openai".to_owned(),
                model: "m".to_owned(),
                status: 200,
                ..Default::default()
            },
        );
        recorder.flush().await;
        assert!(
            store.list_request_logs(10).await.unwrap().is_empty(),
            "callLogs 关掉不得落 request_logs"
        );
        assert_eq!(
            recorder.core.metrics.render_json()["http"][0]["count"],
            json!(1),
            "指标链路不受 callLogs 影响"
        );
        recorder.shutdown();
        store.close().await;
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 落库行含 usage 入账与请求元信息。
    #[tokio::test]
    async fn persists_row_with_usage() {
        let (store, recorder, dir) = harness("usage").await;
        let snapshot = snapshot_with(json!({}));
        recorder.enqueue_extra(
            &snapshot,
            "req-1",
            &RequestLogEvent {
                protocol: "openai".to_owned(),
                model: "m1".to_owned(),
                status: 200,
                stream: true,
                duration_ms: 12.0,
                provider_attempts: vec!["p1".to_owned()],
                retry_attempts: Vec::new(),
                end_reason: "completed",
                provider: Some("p1".to_owned()),
                account: Some("acct-1".to_owned()),
                api_key_id: Some("k1".to_owned()),
                first_byte_ms: Some(3.0),
                error: None,
            },
            extra_with_usage(),
        );
        recorder.flush().await;
        let rows = store.list_request_logs(10).await.unwrap();
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row["request_id"], json!("req-1"));
        assert_eq!(row["account_id"], json!("acct-1"), "命中账号必须补记");
        assert_eq!(row["input_tokens"], json!(7), "usage 必须入账");
        assert_eq!(row["cache_write_tokens"], json!(1));
        assert_eq!(row["reasoning_tokens"], json!(3), "Rust 新增推理计数");
        assert_eq!(row["total_tokens"], json!(12));
        assert_eq!(row["cost_usd"], json!(0.25));
        assert_eq!(row["api_key_name"], json!("golden"));
        assert_eq!(row["requested_model"], json!("prov/m1"));
        assert_eq!(row["stream_end_reason"], json!("completed"));
        assert_eq!(row["method"], json!("POST"));
        recorder.shutdown();
        store.close().await;
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 20261016_01 收官项:入队/落库行为全部反映到 metrics 的 log_pipeline
    /// 块(入队计数、水位、批量行数、落库耗时)。
    #[tokio::test]
    async fn log_pipeline_stats_flow_into_metrics() {
        let (store, recorder, dir) = harness("pipeline").await;
        let snapshot = snapshot_with(json!({}));
        recorder.enqueue(
            &snapshot,
            "req-p1",
            &RequestLogEvent {
                protocol: "openai".to_owned(),
                model: "m".to_owned(),
                status: 200,
                ..Default::default()
            },
        );
        assert_eq!(recorder.dropped_jobs(), 0);
        let pipeline = &recorder.core.metrics.render_json()["log_pipeline"];
        assert_eq!(pipeline["enqueued_total"], json!(1));
        assert!(pipeline["queue_depth"].as_u64().unwrap() >= 1, "入队后水位至少为 1");
        recorder.flush().await;
        let pipeline = &recorder.core.metrics.render_json()["log_pipeline"];
        assert_eq!(pipeline["flushed_batches_total"], json!(1));
        assert_eq!(pipeline["flushed_rows_total"], json!(1));
        assert_eq!(pipeline["flush_failures_total"], json!(0));
        assert_eq!(pipeline["last_batch_rows"], json!(1));
        assert!(
            pipeline["flush_ms_max"].as_f64().unwrap() >= 0.0,
            "落库耗时应已记录"
        );
        recorder.shutdown();
        store.close().await;
        std::fs::remove_dir_all(&dir).ok();
    }

    fn extra_with_usage() -> RequestLogExtra {
        RequestLogExtra {
            api_key_name: Some("golden".to_owned()),
            requested_model: Some("prov/m1".to_owned()),
            usage: Some(Usage {
                input_tokens: 7,
                output_tokens: 5,
                total_tokens: 12,
                cache_read_tokens: 2,
                cache_write_tokens: 1,
                reasoning_tokens: 3,
            }),
            cost_usd: Some(0.25),
            method: Some("POST".to_owned()),
            path: Some("/v1/chat/completions".to_owned()),
            ..Default::default()
        }
    }

    #[test]
    fn preview_helper_redacts() {
        let preview = preview_for(&json!({"model": "m", "api_key": "sk-x"})).unwrap();
        assert_eq!(preview["sha256"].as_str().unwrap().len(), 64);
        assert!(!preview["preview"].as_str().unwrap().contains("sk-x"));
        assert!(preview_for(&json!("not an object")).is_none());
    }

    /// 20261016_01 B-1:callLogs 关闭时 `with_preview(false, …)` 不构造
    /// 预览(脱敏克隆与截断全跳过);开启时行为不变。
    #[test]
    fn preview_gate_skips_construction_when_disabled() {
        let payload = json!({"model": "m", "api_key": "sk-x", "messages": ["x"]});
        let off = RequestLogExtra::default().with_preview(false, &payload);
        assert!(off.preview.is_none(), "门关时不构造预览");
        let on = RequestLogExtra::default().with_preview(true, &payload);
        let preview = on.preview.expect("门开时预览照常");
        assert!(!preview["preview"].as_str().unwrap().contains("sk-x"));
    }

    #[test]
    fn capture_gate_builds_side_row() {
        let off = RequestLogExtra::default().with_capture(
            false,
            1024,
            Some(&json!({"a": 1})),
            Some("b"),
            None,
        );
        assert!(off.bodies.is_none(), "门控关闭时不建侧表行");
        let on = RequestLogExtra::default().with_capture(
            true,
            1024,
            Some(&json!({"a": 1})),
            Some("body"),
            None,
        );
        let bodies = on.bodies.expect("开启后应有侧表行");
        assert_eq!(bodies.request_body.as_deref(), Some(r#"{"a": 1}"#));
        assert_eq!(bodies.response_body.as_deref(), Some("body"));
        assert_eq!(bodies.request_body_size, Some(8));
        let tight = RequestLogExtra::default().with_capture(true, 4, None, Some("中文中文"), None);
        let bodies = tight.bodies.expect("截断仍要落库");
        assert!(bodies.response_body_truncated);
        assert_eq!(bodies.response_body_size, Some(12));
    }

    #[test]
    fn capture_redacts_nested_body_headers_and_stream_text() {
        let captured = RequestLogExtra::default().with_capture(
            true,
            4096,
            Some(&json!({"messages":[{"content":"ok","api_key":"sk-nested"}]})),
            Some(r#"data: {"authorization":"Bearer secret"}\n\n"#),
            Some(json!({"Authorization":"Bearer secret","content-type":"text/event-stream"}).as_object().unwrap().clone()),
        ).with_request_headers(&[("x-api-key".to_owned(), "key-secret".to_owned()), ("accept".to_owned(), "application/json".to_owned())]);
        let bodies = captured.bodies.unwrap();
        let request = bodies.request_body.unwrap();
        assert!(!request.contains("sk-nested"));
        let response = bodies.response_body.unwrap();
        assert!(!response.contains("Bearer secret"));
        assert_eq!(bodies.response_headers.unwrap()["Authorization"], json!("***"));
        assert_eq!(bodies.request_headers.unwrap()["x-api-key"], json!("***"));
    }

    /// D9:逐 attempt 采集复用同一套脱敏(敏感键/敏感 header 不得外泄),
    /// 且正文受字节预算截断。
    #[test]
    fn attempt_capture_redacts_and_bounds() {
        let mut capture = super::super::forward::AttemptCapture::default();
        capture.set_request(
            true,
            4096,
            &json!({"model": "m", "api_key": "sk-secret", "nested": {"access_token": "tok"}}),
        );
        capture.set_headers(
            true,
            &[
                ("Authorization".to_owned(), "Bearer secret".to_owned()),
                ("content-type".to_owned(), "application/json".to_owned()),
            ],
        );
        capture.set_response(true, 4096, r#"{"error":"bad","api_key":"sk-secret"}"#);
        let request = capture.upstream_request.clone().unwrap();
        let request_text = request.as_str().unwrap();
        assert!(!request_text.contains("sk-secret"), "{request_text}");
        // 键名本身含 token 是允许的;被脱敏的是它的值。
        assert!(!request_text.contains("\"access_token\": \"tok\""), "{request_text}");
        assert!(request_text.contains("\"access_token\": \"***\""), "{request_text}");
        assert_eq!(capture.upstream_headers.as_ref().unwrap()["Authorization"], json!("***"));
        assert_eq!(
            capture.upstream_headers.as_ref().unwrap()["content-type"],
            json!("application/json")
        );
        let response = capture.upstream_response.clone().unwrap();
        assert!(!response.contains("sk-secret"), "{response}");

        // 预算:小预算下必须截断并如实标记 size/truncated。
        let mut tight = super::super::forward::AttemptCapture::default();
        tight.set_response(true, 4, "中文中文");
        assert!(tight.response_truncated);
        assert_eq!(tight.response_size, Some(12));
    }

    /// 2026-10-17:usage 计数键(*_tokens 复数结尾)是数值不是凭据,捕获
    /// 脱敏不得打码;凭据键(单数 token 结尾)照旧打码。
    #[test]
    fn capture_keeps_usage_numbers_but_masks_credentials() {
        let mut capture = super::super::forward::AttemptCapture::default();
        capture.set_response(
            true,
            4096,
            r#"{"usage":{"prompt_tokens":8400,"total_tokens":9242,"prompt_tokens_details":{"cached_tokens":4864}},"access_token":"tok-secret","session_token":"tok-s"}"#,
        );
        let response = capture.upstream_response.clone().unwrap();
        assert!(response.contains(r#""prompt_tokens": 8400"#), "{response}");
        assert!(response.contains(r#""cached_tokens": 4864"#), "{response}");
        assert!(response.contains(r#""access_token": "***""#), "{response}");
        assert!(response.contains(r#""session_token": "***""#), "{response}");
        assert!(!response.contains("tok-secret"), "{response}");
    }

    /// 2026-10-17 脱敏审查:文本扫描名单与 SENSITIVE_FIELDS 对齐
    /// (apikey/session_token/id_token/x-goog-api-key 补入),且裸
    /// "token" 不入名单 —— 子串式扫描会误伤 cached_tokens 等 usage 计数。
    #[test]
    fn text_redaction_covers_credential_variants_but_keeps_usage_numbers() {
        let text = "connect to https://x.example/rest?apikey=SK1&session_token=TK2&id_token=TK3&x-goog-api-key=GK4&page=2";
        let redacted = super::redact_text(text);
        assert!(!redacted.contains("SK1"), "{redacted}");
        assert!(!redacted.contains("TK2"), "{redacted}");
        assert!(!redacted.contains("TK3"), "{redacted}");
        assert!(!redacted.contains("GK4"), "{redacted}");
        // 非凭据查询参数保留。
        assert!(redacted.contains("page=2"), "{redacted}");
        // usage 计数不受影响(裸 token 不进文本扫描名单;合法 JSON 走
        // 递归分支,复数 *_tokens 键已按 sensitive_key 例外保留)。
        let usage = r#"{"cached_tokens":4864,"total_tokens":9242}"#;
        assert_eq!(
            super::redact_text(usage),
            r#"{"cached_tokens": 4864, "total_tokens": 9242}"#
        );
    }

    /// D9:captureBodies 关闭时逐 attempt 采集不得写入任何内容。
    #[test]
    fn attempt_capture_disabled_writes_nothing() {
        let mut capture = super::super::forward::AttemptCapture::default();
        capture.set_request(false, 4096, &json!({"model": "m"}));
        capture.set_headers(false, &[("authorization".to_owned(), "Bearer x".to_owned())]);
        capture.set_response(false, 4096, "secret body");
        assert!(capture.upstream_request.is_none());
        assert!(capture.upstream_response.is_none());
        assert!(capture.upstream_headers.is_none());
    }

    /// D3 回归:`take_into` 是消费语义 —— 响应侧字段取走即清空,
    /// 同候选多次重试共享一个槽,后一轮尝试(如连接失败,无新响应
    /// 写入)不得拿到上一轮的响应数据;请求体与转换报告是候选级
    /// 常量,保留给后续重试轮复用。
    #[test]
    fn attempt_capture_take_into_consumes_response_fields() {
        let mut capture = super::super::forward::AttemptCapture::default();
        capture.set_request(true, 4096, &json!({"model": "m"}));
        capture.set_request_headers(
            true,
            &json_map(json!({"content-type": "application/json"})),
        );
        capture.set_conversion(&wr_domain::semantic::ConversionReport::passthrough());
        capture.set_headers(true, &[("content-type".to_owned(), "application/json".to_owned())]);
        capture.set_response(true, 4096, r#"{"error":"first-failure"}"#);

        let mut first = super::super::forward::AttemptRecord {
            attempt_id: "prov#1".to_owned(),
            provider: "prov".to_owned(),
            model: "m".to_owned(),
            account: None,
            status: 500,
            error: None,
            duration_ms: 1.0,
            wait_ms: 0.0,
            wait_source: None,
            queued_ms: 0.0,
            sent_offset_ms: None,
            started_at: String::new(),
            upstream_request: None,
            upstream_request_headers: None,
            upstream_response: None,
            upstream_headers: None,
            conversion: None,
            response_truncated: false,
            response_size: None,
        };
        capture.take_into(&mut first);
        let first_response = first.upstream_response.as_deref().unwrap_or_default();
        assert!(
            first_response.contains("first-failure"),
            "第一轮应取到响应正文: {first_response}"
        );
        assert!(first.upstream_headers.is_some());
        assert!(first.conversion.is_some());

        // 第二次尝试(连接失败,无新响应写入):不得串到上一轮的响应。
        let mut second = first.clone();
        second.attempt_id = "prov#2".to_owned();
        second.upstream_response = None;
        second.upstream_headers = None;
        second.conversion = None;
        second.response_size = None;
        second.response_truncated = false;
        capture.take_into(&mut second);
        assert!(second.upstream_response.is_none());
        assert!(second.upstream_headers.is_none());
        assert_eq!(second.response_size, None);
        assert!(!second.response_truncated);
        // 候选级字段(出站请求体/请求头/转换报告)仍在,后续重试轮继续复用。
        assert!(second.upstream_request.is_some());
        assert!(second.upstream_request_headers.is_some());
        assert!(second.conversion.is_some());
    }

    #[test]
    fn attempt_capture_request_headers_redact_credentials() {
        let mut capture = super::super::forward::AttemptCapture::default();
        // 采集关闭:不写键(未捕获 ≠ 空值)。
        capture.set_request_headers(false, &json_map(json!({"authorization": "Bearer sk-1"})));
        assert!(capture.upstream_request_headers.is_none());
        // 开启:认证键打码,普通键保真。
        capture.set_request_headers(
            true,
            &json_map(json!({
                "content-type": "application/json",
                "authorization": "Bearer sk-secret",
                "x-api-key": "sk-anthropic",
                "x-stainless-lang": "js",
            })),
        );
        let headers = capture.upstream_request_headers.clone().unwrap();
        assert_eq!(headers.get("content-type").unwrap(), "application/json");
        assert_eq!(headers.get("authorization").unwrap(), "***");
        assert_eq!(headers.get("x-api-key").unwrap(), "***");
        assert_eq!(headers.get("x-stainless-lang").unwrap(), "js");
        // to_value 写键(take_into 消费)。
        let mut record = super::super::forward::AttemptRecord {
            attempt_id: "prov#1".to_owned(),
            provider: "prov".to_owned(),
            model: "m".to_owned(),
            account: None,
            status: 200,
            error: None,
            duration_ms: 1.0,
            wait_ms: 0.0,
            wait_source: None,
            queued_ms: 0.0,
            sent_offset_ms: None,
            started_at: String::new(),
            upstream_request: None,
            upstream_request_headers: None,
            upstream_response: None,
            upstream_headers: None,
            conversion: None,
            response_truncated: false,
            response_size: None,
        };
        capture.take_into(&mut record);
        assert!(record.upstream_request_headers.is_some());
    }

    fn json_map(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        value.as_object().cloned().unwrap()
    }

    #[test]
    fn payload_key_order_mirrors_record_request() {
        let event = RequestLogEvent {
            protocol: "openai".to_owned(),
            model: "m1".to_owned(),
            status: 200,
            stream: true,
            duration_ms: 12.0,
            provider_attempts: vec!["p1".to_owned()],
            retry_attempts: vec![json!({"provider": "p1", "status": 0})],
            end_reason: "completed",
            provider: Some("p1".to_owned()),
            account: Some("acct-1".to_owned()),
            api_key_id: Some("k1".to_owned()),
            first_byte_ms: Some(3.0),
            error: None,
        };
        let payload = build_payload(&event, &extra_with_usage());
        let keys: Vec<&str> = payload
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            vec![
                "protocol",
                "model",
                "status",
                "stream",
                "duration_ms",
                "provider_attempts",
                "stream_end_reason",
                "retry_attempts",
                "api_key_id",
                "api_key_name",
                "requested_model",
                "provider",
                "account_id",
                "input_tokens",
                "output_tokens",
                "total_tokens",
                "cache_read_tokens",
                "cache_write_tokens",
                "reasoning_tokens",
                "cost_usd",
                "first_byte_ms",
                "method",
                "path",
            ]
        );
    }
}
