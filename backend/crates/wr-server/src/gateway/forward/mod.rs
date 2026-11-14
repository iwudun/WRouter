//! 转发管线(镜像 Python `forwarding.py` / `retry_fallback.py`,R3 全量)。
//!
//! 非流式 `forward_json`:
//!   认证(HTTP 层)→ Claude 模型名解码 → provider/model 前缀解析 →
//!   路由策略链(引擎改写折叠回 payload/headers)→ 执行计划构建
//!   (规则级 fallback > 全局 Router.fallback)→ API key 模型白名单 →
//!   访问控制策略(deny → 403)→ `_request_with_fallback` 三层回退 →
//!   跨协议响应解码(非流面只接受 JSON 表示,不做流聚合)→
//!   tracing 请求日志(R4 落库)。
//!
//! 流式 `spawn_forward_stream`:
//!   同前半;precheck/policy 拒绝经 SSE 错误信封下发 → 计划 attempts →
//!   候选链 → 同 provider 重试(仅在**尚未向客户端发出任何字节**时
//!   允许切换/重试,C4 流式无原地重试)→ relay 逐事件转换 →
//!   A6 流合成(流式请求收到单 JSON 时合成帧序列)→ 经有界 mpsc
//!   通道回写 axum Body(背压由通道容量提供;客户端断开时 body 被
//!   drop → 通道发送失败 → 终止并记 499)。
//!
//! R3 完成记录里的旧待办清单已不具当前性;当前缺口以
//! `docs/reference/03-网关协议与路由.md` §15 为准。
//!
//! ## 20260905_02 凭据池接线(K3/K4/K8,行为变更无 Python 镜像)
//! - 非流式:上游响应返回处以最终状态码调 `record_outcome`(分档冷却
//!   401/403→300s、429/5xx→60s;`Retry-After` 头优先,429 无头时从错误
//!   正文做 reset 文本解析兜底;上限 24h);fallback 候选(未注入账号)
//!   不记录;
//! - 流式:仅在**握手期**(响应头已到且状态非 2xx)记录;SSE 开始后的
//!   中途断连不驱动 key 冷却;
//! - 会话键(K4):请求头 `x-claude-code-session-id` → `x-session-id` →
//!   `x-request-id`,兜底首条 user 消息 SHA-256 前 16 hex,只在原始请求
//!   上提取一次并传递到选号点(`sticky` 策略),不落库;
//! - 可观测:命中账号时非流式响应带 `x-wr-account` 头(流式响应头在
//!   管线 spawn 前已定,故不加);调用日志 payload 补 `account_id` 键。

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use tokio::sync::mpsc;

use wr_domain::gateway::{
    CREDENTIAL_FAILURE_STATUS_CODES, NO_RETRY_STATUS_CODES, RoutingError, should_fallback_mode,
    should_retry,
};
use wr_domain::policy::{self as access_policy, PolicyResource};
use wr_domain::protocol::{
    self as domain_protocol, classify_upstream_error, client_fingerprint, decode_claude_app_model,
    endpoint_with_query, format_error_for_protocol, upstream_headers_with_fingerprint,
    upstream_payload_with_report,
};

use wr_domain::provider::Provider;
use wr_domain::routing::contracts::{
    ModelGroup, ModelGroupStrategy, ModelGroupTarget, RouteExecutionPlan, RouteModelRef,
};
use wr_domain::routing::execution_plan::resolve_execution_plan;
use wr_infra::config::CircuitBreakerConfig;
use wr_infra::sqlite::engine::iso_now;
use wr_infra::upstream::{UpstreamError, UpstreamResponse};

use super::auth::GatewayIdentity;
use super::concurrency::AcquireOutcome;
use super::events;
use super::idempotency::{EntryState, idempotency_header, idempotency_store_key, request_hash};
use super::limits;
use super::model_groups::{RouteCandidate, expand_group_candidates, group_sticky_key};
use super::request_log::{
    AnswerTiming, RequestLogEvent, RequestLogExtra, RequestLogRecorder, preview_for,
};
use super::response_cache;
use super::savings::{ResolvedSavings, resolve_savings};
use super::session_affinity::{self, AffinityEntry, PrefixPinEntry};
use super::{ConfigSnapshot, GatewayDeps};
use wr_domain::cache_prefix::prefix_pin_key;

/// 指纹层 query(20261016_03 §3.4;实现下沉在 domain
/// `client_fingerprint::resolve_fingerprint_query`,转发与实探共用)。
/// 一次上游尝试的明细(2026-10-08 重试记录丰富化):落库 payload 键
/// `retry_attempts`,调用日志详情「重试尝试」表的数据源。此前的
/// `provider_attempts` 只有 provider id 串,等待时间/失败状态/错误/耗时
/// 全部丢失(只在 tracing 里)。
#[derive(Debug, Clone)]
pub struct AttemptRecord {
    /// 稳定 attempt id(阶段 D3):`{provider}#{attempt ordinal}`,同一请求
    /// 内唯一,使 trace 子记录与 retry 摘要可以按 id 关联。
    pub attempt_id: String,
    pub provider: String,
    /// 实际请求的上游模型名(聚合场景与客户端请求名不同)。
    pub model: String,
    /// 命中的凭据池账号(legacy 单 key 为 None)。
    pub account: Option<String>,
    /// 上游 HTTP 状态;0 = 未拿到响应(传输错误/熔断跳过/排队拒绝)。
    pub status: u16,
    /// 错误摘要(传输错误消息、上游错误正文摘要、跳过/拒绝原因)。
    pub error: Option<String>,
    /// 本次尝试耗时(发起 → 判定,毫秒;排队拒绝 = 排队等待时长)。
    pub duration_ms: f64,
    /// 判定失败后为下次重试等待的时长(最终/成功次为 0)。
    pub wait_ms: f64,
    /// 等待来源:上游 `Retry-After` 头(retry-after)或指数退避(backoff)。
    pub wait_source: Option<&'static str>,
    /// 阶段四 C-4:本次尝试在并发槽位上的等待时长(毫秒;未排队为 0)。
    /// 与请求级 `queued_ms` 同源 —— 多次尝试的排队等待在这里逐次留痕。
    pub queued_ms: f64,
    /// 阶段四 C-4:本次尝试**出站时刻**相对请求起点的偏移(毫秒);
    /// 未出站(熔断跳过 / 排队拒绝 / 预算耗尽)为 `None`。
    /// 请求级 `sent_ms` 由它聚合而来(`TTFT − sent_ms` 即供应商侧耗时)。
    pub sent_offset_ms: Option<f64>,
    /// 尝试发起时刻(ISO-Z,与日志行 created_at 同形)。
    pub started_at: String,
    /// 本次尝试的上游请求体(仅 captureBodies 开启;已脱敏并受字节预算)。
    pub upstream_request: Option<Value>,
    /// 本次尝试实际发出的上游请求头(同上;已过滤敏感头 —— 认证头打码)。
    pub upstream_request_headers: Option<Map<String, Value>>,
    /// 本次尝试的上游响应正文(同上;流式为已捕获的原始 SSE 文本)。
    pub upstream_response: Option<String>,
    /// 上游响应头(同上;已过滤敏感头)。
    pub upstream_headers: Option<Map<String, Value>>,
    /// 本次尝试的出站转换报告(策略 + 诊断;阶段 D3/D4)。
    pub conversion: Option<Value>,
    /// 上游响应正文是否被截断。
    pub response_truncated: bool,
    /// 上游响应正文原始字节数(截断前)。
    pub response_size: Option<i64>,
}

impl AttemptRecord {
    fn to_value(&self) -> Value {
        let mut map = Map::new();
        map.insert("attempt_id".to_owned(), json!(self.attempt_id));
        map.insert("provider".to_owned(), json!(self.provider));
        map.insert("model".to_owned(), json!(self.model));
        map.insert(
            "account".to_owned(),
            self.account
                .as_deref()
                .map(|v| json!(v))
                .unwrap_or(Value::Null),
        );
        map.insert("status".to_owned(), Value::from(self.status));
        map.insert(
            "error".to_owned(),
            self.error
                .as_deref()
                .map(|v| json!(v))
                .unwrap_or(Value::Null),
        );
        map.insert("duration_ms".to_owned(), json!(self.duration_ms));
        map.insert("wait_ms".to_owned(), json!(self.wait_ms));
        // 2026-10-20:等待时长单位标记。历史行没有本键且 wait_ms 实为秒
        // (旧写入 bug),读取侧据此换算旧值;新行恒为毫秒。
        map.insert("wait_unit".to_owned(), json!("ms"));
        map.insert(
            "wait_source".to_owned(),
            self.wait_source.map(|v| json!(v)).unwrap_or(Value::Null),
        );
        // 阶段四 C-4:排队等待与出站偏移。未出站的尝试 `sent_offset_ms`
        // 为 null(读取侧据此判定「没发出去」,不把它计入 sent_ms)。
        map.insert("queued_ms".to_owned(), json!(self.queued_ms));
        map.insert(
            "sent_offset_ms".to_owned(),
            self.sent_offset_ms.map(|v| json!(v)).unwrap_or(Value::Null),
        );
        map.insert("started_at".to_owned(), json!(self.started_at));
        // 采集字段只在真有数据时写键(未捕获 ≠ 空值,前端据此区分)。
        if let Some(request) = &self.upstream_request {
            map.insert("upstream_request".to_owned(), request.clone());
        }
        if let Some(request_headers) = &self.upstream_request_headers {
            map.insert(
                "upstream_request_headers".to_owned(),
                Value::Object(request_headers.clone()),
            );
        }
        if let Some(response) = &self.upstream_response {
            map.insert("upstream_response".to_owned(), json!(response));
        }
        if let Some(headers) = &self.upstream_headers {
            map.insert("upstream_headers".to_owned(), Value::Object(headers.clone()));
        }
        if let Some(conversion) = &self.conversion {
            map.insert("conversion".to_owned(), conversion.clone());
        }
        if let Some(size) = self.response_size {
            map.insert("upstream_response_size".to_owned(), json!(size));
        }
        if self.response_truncated {
            map.insert("upstream_response_truncated".to_owned(), json!(true));
        }
        Value::Object(map)
    }
}

/// 转发管线共享的尝试明细收集器(原 `Vec<String>` provider 串的升级;
/// `providerAttempts` 仍由 [`trail_provider_ids`] 派生,历史消费方零变化)。
pub type AttemptTrail = Arc<std::sync::Mutex<Vec<AttemptRecord>>>;

/// 单次尝试的采集槽(阶段 D3):在尝试过程中逐步填充,结束时由
/// `push_record` 一次性读走。用槽位而不是给每个 `push_record` 调用点
/// 加参数,是因为尝试分支很多而采集只在少数分支发生。
#[derive(Debug, Clone, Default)]
pub struct AttemptCapture {
    /// 本次尝试实际发出的上游请求体(captureBodies 开启时;已脱敏)。
    pub upstream_request: Option<Value>,
    /// 上游请求头(captureBodies 开启时;已过滤敏感头 —— 认证头打码)。
    pub upstream_request_headers: Option<Map<String, Value>>,
    /// 上游响应正文(非流为 JSON 文本,流式为已捕获的原始 SSE)。
    pub upstream_response: Option<String>,
    /// 上游响应头(已过滤敏感头)。
    pub upstream_headers: Option<Map<String, Value>>,
    /// 出站转换报告(策略 + 诊断)。
    pub conversion: Option<Value>,
    pub response_truncated: bool,
    pub response_size: Option<i64>,
}

impl AttemptCapture {
    /// 用采集开关与预算写入上游请求体(脱敏 + 截断)。
    pub fn set_request(&mut self, enabled: bool, max_bytes: usize, payload: &Value) {
        if !enabled {
            return;
        }
        let safe = crate::gateway::request_log::redact_value(payload.clone());
        let captured = wr_domain::observability::capture_text(
            &wr_domain::json_py::py_json_dumps(&safe),
            max_bytes,
        );
        self.upstream_request = Some(Value::String(captured.text));
    }

    /// 写入上游请求头(过滤敏感头 —— authorization/x-api-key/x-goog-api-key
    /// 等认证键打码;与响应头同一套 [`redact_headers`] 过滤)。
    pub fn set_request_headers(&mut self, enabled: bool, headers: &Map<String, Value>) {
        if !enabled {
            return;
        }
        self.upstream_request_headers = Some(crate::gateway::request_log::redact_headers(
            headers.clone(),
        ));
    }

    /// 写入上游响应体(脱敏 + 截断 + size/truncated 标记)。
    pub fn set_response(&mut self, enabled: bool, max_bytes: usize, body: &str) {
        if !enabled {
            return;
        }
        let safe = crate::gateway::request_log::redact_text(body);
        let captured = wr_domain::observability::capture_text(&safe, max_bytes);
        self.response_size = Some(captured.size_bytes as i64);
        self.response_truncated = captured.truncated;
        self.upstream_response = Some(captured.text);
    }

    /// 写入上游响应头(过滤敏感头)。
    pub fn set_headers(&mut self, enabled: bool, headers: &[(String, String)]) {
        if !enabled {
            return;
        }
        let map = headers
            .iter()
            .map(|(name, value)| (name.clone(), Value::String(value.clone())))
            .collect();
        self.upstream_headers = Some(crate::gateway::request_log::redact_headers(map));
    }

    /// 写入出站转换报告。
    pub fn set_conversion(&mut self, report: &wr_domain::semantic::ConversionReport) {
        self.conversion = serde_json::to_value(report).ok();
    }

    /// 取走本次尝试的采集内容:响应侧字段(体/头/size/truncated)进
    /// record 后即清空 —— 同候选多次重试共享一个槽,后一轮尝试没有
    /// 新响应写入时不得拿到上一轮的响应数据(串 attempt);请求体、
    /// 请求头与转换报告是候选级常量,保留给后续重试轮复用。
    pub(crate) fn take_into(&mut self, record: &mut AttemptRecord) {
        record.upstream_request = self.upstream_request.clone();
        record.upstream_request_headers = self.upstream_request_headers.clone();
        record.upstream_response = self.upstream_response.take();
        record.upstream_headers = self.upstream_headers.take();
        record.conversion = self.conversion.clone();
        record.response_truncated = std::mem::take(&mut self.response_truncated);
        record.response_size = self.response_size.take();
    }
}

/// 一次尝试的采集槽(与 [`AttemptTrail`] 同生命周期;响应侧字段随
/// [`AttemptCapture::take_into`] 消费)。
pub type AttemptCaptureSlot = Arc<std::sync::Mutex<AttemptCapture>>;

fn new_capture_slot() -> AttemptCaptureSlot {
    Arc::new(std::sync::Mutex::new(AttemptCapture::default()))
}

fn capture_with<R>(slot: &AttemptCaptureSlot, f: impl FnOnce(&mut AttemptCapture) -> R) -> R {
    let mut guard = slot.lock().unwrap();
    f(&mut guard)
}

/// 稳定 attempt id(阶段 D3):`{provider}#{ordinal}`,同一请求内唯一。
/// 用 ordinal 而不是随机 id,是为了让「第几次尝试」在 trace 与 retry
/// 摘要里可直接对照,也便于日志人工排查。
fn attempt_id_for(provider_id: &str, ordinal: u32) -> String {
    format!("{provider_id}#{}", ordinal + 1)
}

fn new_trail() -> AttemptTrail {
    Arc::new(std::sync::Mutex::new(Vec::new()))
}

fn trail_push(trail: &AttemptTrail, record: AttemptRecord) {
    trail.lock().unwrap().push(record);
}

/// 历史口径:provider id 串列表(`providerAttempts` / x-wr-fallback-* 头)。
fn trail_provider_ids(trail: &AttemptTrail) -> Vec<String> {
    trail
        .lock()
        .unwrap()
        .iter()
        .map(|record| record.provider.clone())
        .collect()
}

/// payload 键 `retry_attempts` 的值(空 Vec = 不写键)。
fn trail_values(trail: &AttemptTrail) -> Vec<Value> {
    trail
        .lock()
        .unwrap()
        .iter()
        .map(AttemptRecord::to_value)
        .collect()
}

/// trace 子表里的 attempt 条目(阶段 D3/D4):打上 `kind: "attempt"`,
/// 与 `kind: "sse_event"` 的原始流事件分开,便于各自分页。
fn trace_attempt_items(trail: &AttemptTrail) -> Vec<Value> {
    trail_values(trail)
        .into_iter()
        .map(|mut value| {
            if let Some(map) = value.as_object_mut() {
                map.insert("kind".to_owned(), json!("attempt"));
            }
            value
        })
        .collect()
}

/// 失败终态行的扩展事实(2026-10-14):身份 + 预览 + 正文捕获。
///
/// 此前失败路径只落 preview —— captureBodies 开启时详情页的请求正文
/// 依旧是空的,排障只能翻 attempt 明细。现在失败行与成功行同规:
/// 请求体必带;拿到过上游错误响应时(`response_body`/`response_headers`
/// 非 None)一并落侧表。捕获关闭时自然退化为身份 + 预览,与旧行为一致。
pub(super) fn failure_log_extra(
    deps: &GatewayDeps,
    snapshot: &ConfigSnapshot,
    identity: &GatewayIdentity,
    client_headers: &[(String, String)],
    payload: &Value,
    response_body: Option<&str>,
    response_headers: Option<Map<String, Value>>,
) -> RequestLogExtra {
    // 20261016_01 B-1:callLogs 关闭时整行不落,preview/capture 的脱敏
    // 与序列化全部跳过(此前门控只在 drain 侧,构造已经白做)。
    if !RequestLogRecorder::call_logs_enabled(snapshot) {
        return RequestLogExtra::default();
    }
    RequestLogExtra::default()
        .with_identity(&identity.key_name)
        .with_preview(true, payload)
        .with_capture(
            RequestLogRecorder::capture_enabled(snapshot),
            deps.settings.body_capture_max_bytes,
            Some(payload),
            response_body,
            response_headers,
        )
        .with_request_headers(client_headers)
}

/// 一次非流式转发的结果(镜像 forward_json 的三元组)。
#[derive(Default)]
pub struct ForwardJsonResult {
    pub status: u16,
    pub body: Value,
    /// 路由可观测头(x-wr-route-* / x-wr-fallback-*)。
    pub headers: Vec<(String, String)>,
    /// P3-1:原生 Raw 成功响应的出站原字节(Some 时 HTTP 层直接透传,
    /// 不再经 `Json(body)` 重序列化);`body` 仍保留解析视图供计量/缓存。
    pub raw_body: Option<Vec<u8>>,
}

/// 非流式三层回退的成功产物。
///
/// 方案 20260906_01 B4:附 `proxy_source` 供请求日志记录本次实际走的出口
/// (`direct | system | preset:<name>@<host:port>`;凭据已在
/// `describe_proxy_source` 里掩掉)。原先的四元组返回改为具名结构体 ——
/// 三个 String 位置的元组已不可读。
struct ForwardSuccess {
    response: UpstreamResponse,
    provider: Provider,
    /// 命中账号 id —— 仅主候选注入成功时非 None。
    hit_account: Option<String>,
    /// 实际发给上游的模型名(聚合请求时与客户端请求名不同)。
    actual_model: String,
    /// 本次生效的出站代理来源标签。
    proxy_source: String,
    /// P3-1:本次交换规划为原生 ByteExact(请求/响应均 Raw)。
    byte_exact: bool,
}

/// 转发管线错误(升格为 502 wrouter_error,镜像 app.py 的 catch)。
#[derive(Debug)]
pub struct ForwardError {
    pub message: String,
    pub attempts: Vec<String>,
}

/// 非流式三层回退的失败出口(20261016_01 A-3):除普通错误外,还承载
/// 「并发排队被拒」—— 与流式管线同语义落 429 `concurrency_limited`,
/// 不再静默超发上游(旧实现对 Rejected 置 None 后照常转发)。
#[derive(Debug)]
pub(super) enum ForwardFailure {
    Error(ForwardError),
    QueueRejected {
        status: u16,
        message: String,
        provider: Option<String>,
        account: Option<String>,
    },
}

impl ForwardError {
    pub fn into_body(self, request_id: &str) -> Value {
        let mut error = Map::new();
        error.insert("message".to_owned(), Value::String(self.message));
        error.insert("type".to_owned(), Value::String("wrouter_error".to_owned()));
        error.insert(
            "request_id".to_owned(),
            Value::String(request_id.to_owned()),
        );
        if !self.attempts.is_empty() {
            error.insert(
                "wrouter_attempts".to_owned(),
                Value::Array(
                    self.attempts
                        .iter()
                        .map(|a| Value::String(a.clone()))
                        .collect(),
                ),
            );
        }
        json!({ "error": Value::Object(error) })
    }
}

// ---------------------------------------------------------------------------
// 子模块(20261013_01 C1 目录化:纯搬移,`gateway::forward::X` 路径不变)
// ---------------------------------------------------------------------------

mod cache_io;
mod cache_replay;
mod credential;
mod delivery;
mod record_encoder;
mod stream_control;
mod json;
mod native;
mod prepared;
mod routing;
mod metrics;
mod support;
mod stream;

pub use delivery::StreamHandshake;
pub use json::{forward_json, spawn_forward_json};
pub use routing::{build_fallback_plan, resolve_client_prefix, select_provider};
pub use support::{cost_of, estimate_usage};
pub use stream::{spawn_forward_stream, StreamPipelineError};

// 跨子文件共享的私有件在 forward 命名空间内再绑定(子模块经 `use super::*` 复用)。
use cache_io::*;
use credential::*;
use native::*;
use routing::*;
use metrics::*;
use support::*;
