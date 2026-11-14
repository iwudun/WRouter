//! `request_logs.payload` 拼装器。
//!
//! 字段顺序严格镜像 Python `application/gateway/request_log.py::_record_request`
//! 的写入次序(R4.12 db 文本 diff 与 R5.9 管理面 parity 都依赖键序):
//!
//! ```text
//! protocol, model, status, stream, duration_ms, provider_attempts,
//! stream_end_reason,
//! [api_key_id, api_key_name]  [requested_model]  [provider]  [account_id*]
//! input_tokens, output_tokens, total_tokens, cache_read_tokens, cache_write_tokens
//! [reasoning_tokens*]  [cost_usd]  [first_byte_ms]
//! [sent_ms*] [queued_ms*] [retried_ms*]
//! [transformed_payload_preview]
//! **extra
//! [request_body_size_bytes, request_body_truncated,
//! response_body_size_bytes, response_body_truncated]
//! [client] [method] [path] [completed_at]
//! ```
//!
//! `*` `account_id` 是 Rust 新增键(方案 20260905_02 K5 凭据池维度,
//! 无 Python 镜像),插在 `provider` 之后,其余键序仍严格镜像。
//! `reasoning_tokens` 同为 Rust 新增键(推理 token 计数,usage 块尾部),
//! 有值才写。
//! `sent_ms` / `queued_ms` / `retried_ms` 是阶段四 C-4 新增键(耗时分解,
//! Rust 新增,无 Python 镜像),紧随 `first_byte_ms`;三者**有值才写**
//! (无尝试明细的行保持历史形态零变化)。

use serde_json::{Map, Value, json};

#[derive(Debug, Clone)]
pub struct RequestLogPayload {
    map: Map<String, Value>,
}

impl RequestLogPayload {
    /// 七个恒定字段(Python 无条件写入的那批)。`end_reason` 对应
    /// `stream_end_reason`。
    pub fn new(
        protocol: &str,
        model: &str,
        status: i64,
        stream: bool,
        duration_ms: f64,
        provider_attempts: Vec<String>,
        end_reason: &str,
    ) -> Self {
        let mut map = Map::new();
        map.insert("protocol".to_owned(), Value::String(protocol.to_owned()));
        map.insert("model".to_owned(), Value::String(model.to_owned()));
        map.insert("status".to_owned(), Value::from(status));
        map.insert("stream".to_owned(), Value::Bool(stream));
        map.insert("duration_ms".to_owned(), json!(duration_ms));
        map.insert(
            "provider_attempts".to_owned(),
            Value::Array(provider_attempts.into_iter().map(Value::String).collect()),
        );
        map.insert(
            "stream_end_reason".to_owned(),
            Value::String(end_reason.to_owned()),
        );
        Self { map }
    }

    /// R3.3 归因:有 `api_key_id` 才写,`api_key_name` 再嵌套一层条件。
    pub fn api_key(mut self, id: Option<&str>, name: Option<&str>) -> Self {
        let Some(id) = id.filter(|value| !value.is_empty()) else {
            return self;
        };
        self.map
            .insert("api_key_id".to_owned(), Value::String(id.to_owned()));
        if let Some(name) = name.filter(|value| !value.is_empty()) {
            self.map
                .insert("api_key_name".to_owned(), Value::String(name.to_owned()));
        }
        self
    }

    /// 仅在「客户端请求的模型 ≠ 实际模型」时写(Python 同款条件)。
    pub fn requested_model(mut self, requested: &str, model: &str) -> Self {
        if !requested.is_empty() && requested != model {
            self.map.insert(
                "requested_model".to_owned(),
                Value::String(requested.to_owned()),
            );
        }
        self
    }

    pub fn provider(mut self, provider_id: &str) -> Self {
        self.map
            .insert("provider".to_owned(), Value::String(provider_id.to_owned()));
        self
    }

    /// 命中的凭据池账号(方案 20260905_02 K5 调用日志维度;Rust 新增键,
    /// 无 Python 镜像 —— 键序紧随 `provider` 之后,登记为偏差)。
    pub fn account(mut self, account_id: &str) -> Self {
        self.map.insert(
            "account_id".to_owned(),
            Value::String(account_id.to_owned()),
        );
        self
    }

    /// usage 五键(`crate::usage::normalize_usage` 的输出形态)+ Rust 新增
    /// 的 `reasoning_tokens`(有值才写,键序登记为偏差,紧随 usage 五键)。
    pub fn usage(mut self, usage: &crate::usage::Usage) -> Self {
        self.map
            .insert("input_tokens".to_owned(), Value::from(usage.input_tokens));
        self.map
            .insert("output_tokens".to_owned(), Value::from(usage.output_tokens));
        self.map
            .insert("total_tokens".to_owned(), Value::from(usage.total_tokens));
        self.map.insert(
            "cache_read_tokens".to_owned(),
            Value::from(usage.cache_read_tokens),
        );
        self.map.insert(
            "cache_write_tokens".to_owned(),
            Value::from(usage.cache_write_tokens),
        );
        if usage.reasoning_tokens != 0 {
            self.map.insert(
                "reasoning_tokens".to_owned(),
                Value::from(usage.reasoning_tokens),
            );
        }
        self
    }

    pub fn cost_usd(mut self, value: f64) -> Self {
        self.map.insert("cost_usd".to_owned(), json!(value));
        self
    }

    pub fn first_byte_ms(mut self, value: f64) -> Self {
        self.map.insert("first_byte_ms".to_owned(), json!(value));
        self
    }

    /// 阶段四 C-4:耗时分解三键(`sent_ms` / `queued_ms` / `retried_ms`)。
    /// 有值才写 —— 无上游尝试的行不落这三个键(历史形态零变化)。
    pub fn timing(mut self, timing: &RequestTiming) -> Self {
        if let Some(sent_ms) = timing.sent_ms {
            self.map.insert("sent_ms".to_owned(), json!(sent_ms));
        }
        if timing.queued_ms > 0.0 {
            self.map
                .insert("queued_ms".to_owned(), json!(timing.queued_ms));
        }
        if timing.retried_ms > 0.0 {
            self.map
                .insert("retried_ms".to_owned(), json!(timing.retried_ms));
        }
        self
    }

    pub fn error(mut self, message: &str) -> Self {
        self.map
            .insert("error".to_owned(), Value::String(message.to_owned()));
        self
    }

    pub fn preview(mut self, preview: Value) -> Self {
        self.map
            .insert("transformed_payload_preview".to_owned(), preview);
        self
    }

    /// `log_payload.update(extra)`:同名键被覆盖,新键追加在后。
    pub fn extra(mut self, extra: &Map<String, Value>) -> Self {
        for (key, value) in extra {
            self.map.insert(key.clone(), value.clone());
        }
        self
    }

    /// 正文捕获只把**小字段**带进主行(尺寸与是否截断),大文本走侧表。
    /// `None` 表示该侧没捕获(Python 的 `if req_text:` 短路)。
    pub fn capture_sizes(
        mut self,
        request: Option<(i64, bool)>,
        response: Option<(i64, bool)>,
    ) -> Self {
        if let Some((size, truncated)) = request {
            self.map
                .insert("request_body_size_bytes".to_owned(), Value::from(size));
            if truncated {
                self.map
                    .insert("request_body_truncated".to_owned(), Value::Bool(true));
            }
        }
        if let Some((size, truncated)) = response {
            self.map
                .insert("response_body_size_bytes".to_owned(), Value::from(size));
            if truncated {
                self.map
                    .insert("response_body_truncated".to_owned(), Value::Bool(true));
            }
        }
        self
    }

    /// `client` / `method` / `path` / `completed_at`:空值不写(Python 的
    /// `if client:` 短路)。
    pub fn request_meta(
        mut self,
        client: Option<&str>,
        method: Option<&str>,
        path: Option<&str>,
        completed_at: Option<&str>,
    ) -> Self {
        for (key, value) in [
            ("client", client),
            ("method", method),
            ("path", path),
            ("completed_at", completed_at),
        ] {
            if let Some(value) = value.filter(|value| !value.is_empty()) {
                self.map
                    .insert(key.to_owned(), Value::String(value.to_owned()));
            }
        }
        self
    }

    pub fn build(self) -> Value {
        Value::Object(self.map)
    }

    /// 每次上游尝试的明细(Rust 新增键,2026-10-08 重试记录丰富化,
    /// 无 Python 镜像)。须紧跟 `new` 之后调用,使键序落在
    /// `stream_end_reason` 之后;空切片不写 —— 无尝试明细的行保持
    /// 历史形态零变化。
    pub fn retry_attempts(mut self, attempts: &[Value]) -> Self {
        if attempts.is_empty() {
            return self;
        }
        self.map
            .insert("retry_attempts".to_owned(), Value::Array(attempts.to_vec()));
        self
    }
}

/// 阶段四 C-4:请求级耗时分解(sent / queued / retried 三量)。
///
/// 口径:`Sent` / `Queued` / `Retried` 三列;本实现不拆 `Own` / `Masking` 粒度:
///
///
/// - `sent_ms` = **出站时刻 − 请求起点**(毫秒)。`TTFT − sent_ms` 才是
///   「供应商到首内容用了多久」—— 记录这三列的动机
///   (一个厂商压了某次 try 约 100 秒后回 503,只记
///   总耗时会被读成「慢了几十秒」)。
/// - `queued_ms` = 并发槽位(限流/许可)等待的累计。
/// - `retried_ms` = 失败尝试的自身耗时 + 其后退避的累计。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RequestTiming {
    /// 出站时刻 − 请求起点;从未出站(熔断跳过 / 排队拒绝)为 `None`。
    pub sent_ms: Option<f64>,
    /// 并发槽位等待累计(毫秒)。
    pub queued_ms: f64,
    /// 失败尝试与退避的累计(毫秒)。
    pub retried_ms: f64,
}

/// 从 `retry_attempts` 明细聚合请求级耗时分解(阶段四 C-4)。
///
/// 明细里的每个尝试带两个新键:`queued_ms`(该次尝试的并发槽位等待)与
/// `sent_offset_ms`(该次尝试出站时刻相对**请求起点**的偏移;未出站为
/// `null`)。聚合规则:
///
/// - `sent_ms` = **应答尝试**的 `sent_offset_ms`;应答尝试 = 最后一条
///   「已出站(`sent_offset_ms` 非空)且成功(`error` 为空且 2xx)」的尝试。
///   没有应答尝试时取**最后一次出站**的偏移(请求整体失败但确实发出去过);
///   从未出站则 `None`。
/// - `queued_ms` = 全部尝试的 `queued_ms` 之和(含未出站的排队拒绝 ——
///   排队等待本身就是我方开销)。
/// - `retried_ms` = 除应答尝试外,所有**已出站**尝试的 `duration_ms` 与
///   `wait_ms` 之和(退避时长已含在 `wait_ms` 里,不再重复计)。
///
/// 旧行没有这两个键 → 聚合出全零/`None` → payload 不写这三个键
/// (历史行形态零变化)。
pub fn timing_from_attempts(attempts: &[Value]) -> RequestTiming {
    let mut timing = RequestTiming::default();
    // 应答尝试:最后一条「已出站且成功」的尝试。
    let mut answering: Option<usize> = None;
    let mut last_sent: Option<(usize, f64)> = None;
    for (index, attempt) in attempts.iter().enumerate() {
        let Some(map) = attempt.as_object() else {
            continue;
        };
        let Some(sent_offset_ms) = map.get("sent_offset_ms").and_then(Value::as_f64) else {
            continue;
        };
        last_sent = Some((index, sent_offset_ms));
        let status = map.get("status").and_then(Value::as_i64).unwrap_or(0);
        let failed = map.get("error").is_some_and(|value| !value.is_null());
        if !failed && (200..300).contains(&status) {
            answering = Some(index);
        }
    }
    for (index, attempt) in attempts.iter().enumerate() {
        let Some(map) = attempt.as_object() else {
            continue;
        };
        timing.queued_ms += map.get("queued_ms").and_then(Value::as_f64).unwrap_or(0.0);
        let Some(sent_offset_ms) = map.get("sent_offset_ms").and_then(Value::as_f64) else {
            continue;
        };
        if Some(index) == answering {
            timing.sent_ms = Some(sent_offset_ms);
            continue;
        }
        let duration_ms = map
            .get("duration_ms")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let wait_ms = map.get("wait_ms").and_then(Value::as_f64).unwrap_or(0.0);
        timing.retried_ms += duration_ms + wait_ms;
    }
    if timing.sent_ms.is_none()
        && let Some((_, sent_offset_ms)) = last_sent
    {
        timing.sent_ms = Some(sent_offset_ms);
    }
    timing
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::Usage;

    #[test]
    fn payload_key_order_mirrors_python() {
        let payload =
            RequestLogPayload::new("openai", "m1", 200, true, 12.5, vec!["p1".into()], "done")
                .api_key(Some("k1"), Some("golden"))
                .requested_model("prov/m1", "m1")
                .provider("p1")
                .account("acct-1")
                .usage(&Usage {
                    input_tokens: 3,
                    output_tokens: 4,
                    total_tokens: 7,
                    cache_read_tokens: 1,
                    cache_write_tokens: 0,
                    reasoning_tokens: 0,
                })
                .cost_usd(0.5)
                .build();
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
                "cost_usd",
            ]
        );
        // reasoning_tokens == 0 时不写(历史行形态零变化)。
        assert!(
            !payload
                .as_object()
                .unwrap()
                .contains_key("reasoning_tokens")
        );
    }

    /// Rust 新增键:reasoning_tokens 有值才写,插在 usage 五键之后。
    #[test]
    fn reasoning_tokens_key_appended_when_present() {
        let payload = RequestLogPayload::new("openai", "m1", 200, false, 1.0, vec![], "ok")
            .retry_attempts(&[json!({"provider": "p1", "status": 429})])
            .usage(&Usage {
                input_tokens: 3,
                output_tokens: 4,
                total_tokens: 7,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                reasoning_tokens: 5,
            })
            .build();
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
                "input_tokens",
                "output_tokens",
                "total_tokens",
                "cache_read_tokens",
                "cache_write_tokens",
                "reasoning_tokens",
            ]
        );
        // 空明细不写键(无重试的行保持历史形态)。
        let bare = RequestLogPayload::new("openai", "m1", 200, false, 1.0, vec![], "ok")
            .retry_attempts(&[])
            .build();
        assert!(!bare.as_object().unwrap().contains_key("retry_attempts"));
    }

    /// 阶段四 C-4:耗时分解三键的键序与聚合口径。
    #[test]
    fn timing_keys_follow_first_byte_and_aggregate_attempts() {
        let attempts = vec![
            json!({
                "provider": "p1",
                "status": 503,
                "error": "boom",
                "duration_ms": 120.0,
                "wait_ms": 250.0,
                "queued_ms": 40.0,
                "sent_offset_ms": 15.0,
            }),
            json!({
                "provider": "p2",
                "status": 200,
                "error": null,
                "duration_ms": 900.0,
                "wait_ms": 0.0,
                "queued_ms": 10.0,
                "sent_offset_ms": 320.0,
            }),
        ];
        let timing = timing_from_attempts(&attempts);
        // 应答尝试 = 最后一条「已出站且成功」的尝试。
        assert_eq!(timing.sent_ms, Some(320.0));
        // 排队等待累计(含未出站的排队拒绝)。
        assert_eq!(timing.queued_ms, 50.0);
        // 失败尝试:自身 120ms + 其后退避 250ms。
        assert_eq!(timing.retried_ms, 370.0);

        let payload =
            RequestLogPayload::new("openai", "m1", 200, true, 1000.0, vec!["p2".into()], "done")
                .first_byte_ms(400.0)
                .timing(&timing)
                .build();
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
                "first_byte_ms",
                "sent_ms",
                "queued_ms",
                "retried_ms",
            ]
        );
        // 无尝试明细 → 三键都不写(历史行形态零变化)。
        let bare = RequestLogPayload::new("openai", "m1", 200, false, 1.0, vec![], "ok")
            .timing(&timing_from_attempts(&[]))
            .build();
        let map = bare.as_object().unwrap();
        assert!(!map.contains_key("sent_ms"));
        assert!(!map.contains_key("queued_ms"));
        assert!(!map.contains_key("retried_ms"));
    }

    #[test]
    fn omitted_fields_stay_absent() {
        let payload = RequestLogPayload::new("openai", "m1", 200, false, 1.0, vec![], "ok")
            .api_key(None, Some("name-only"))
            .requested_model("m1", "m1")
            .request_meta(None, Some("POST"), None, None)
            .build();
        let map = payload.as_object().unwrap();
        assert!(!map.contains_key("api_key_id"));
        assert!(
            !map.contains_key("api_key_name"),
            "name 嵌在 id 的条件里:id 缺失则 name 也不写"
        );
        assert!(
            !map.contains_key("requested_model"),
            "同模型不写 requested_model"
        );
        assert!(!map.contains_key("provider"));
        assert_eq!(map["method"], json!("POST"));
        assert!(!map.contains_key("path"));
        assert!(!map.contains_key("client"));
    }
}
