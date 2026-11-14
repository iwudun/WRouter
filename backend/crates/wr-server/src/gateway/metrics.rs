//! 网关指标注册表(方案 #14 R4.9 的记录面;镜像 Python
//! `application/observability/metrics.py`)。
//!
//! **纯内存**:计数器与 HTTP 时长直方图不落库(`metadata` 表里没有指标行),
//! 进程重启即归零。Python 的 `MetricsRegistry` 依赖单事件循环串行化,Rust
//! 侧改为 `Mutex`,在 tokio 多线程下同样安全。
//!
//! 直方图桶边界:`0.05/0.1/0.25/0.5/1/3/10/30` 秒,
//! 每个请求给所有 `le >= duration` 的桶 +1(累积桶),另有 `+Inf` = count。
//!
//! 唯一的活调用点是请求日志链路里的 `record_http_request`(与 Python 相同:
//! `record_*` 家族里其余几个方法目前只有测试调用,不擅自接线)。

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Map, Value, json};

/// 时长桶边界(秒)。
pub const HTTP_DURATION_BUCKETS_SECONDS: &[f64] = &[0.05, 0.1, 0.25, 0.5, 1.0, 3.0, 10.0, 30.0];

/// 请求日志管线水位(20261016_01 收官项:队列健康度进指标面)。
///
/// 全原子更新(入队侧与 drain 侧各写各的字段,无锁竞争);`queue_depth`
/// 是「最近一次入队尝试时的瞬时水位」近似值 —— 由入队侧用剩余 permit
/// 推算,drain 后不回写,只用于趋势观察而非精确计量。持续增长的
/// `dropped_total` 或飙升的 `flush_ms_max` 是「落库跟不上生成」的信号,
/// 也是方案 §10 微优化立项与否的判据。
#[derive(Debug, Default)]
pub struct LogPipelineStats {
    pub enqueued_total: AtomicU64,
    pub dropped_total: AtomicU64,
    pub queue_depth: AtomicU64,
    pub flushed_batches_total: AtomicU64,
    pub flushed_rows_total: AtomicU64,
    pub flush_failures_total: AtomicU64,
    /// 落库耗时累计(微秒)。
    pub flush_us_sum: AtomicU64,
    pub flush_us_max: AtomicU64,
    pub last_batch_rows: AtomicU64,
}

impl LogPipelineStats {
    pub(crate) fn record_enqueued(&self, queue_depth: usize) {
        self.enqueued_total.fetch_add(1, Ordering::Relaxed);
        self.queue_depth
            .store(queue_depth as u64, Ordering::Relaxed);
    }

    pub(crate) fn record_dropped(&self, queue_depth: usize) {
        self.dropped_total.fetch_add(1, Ordering::Relaxed);
        self.queue_depth
            .store(queue_depth as u64, Ordering::Relaxed);
    }

    pub(crate) fn record_flush(&self, rows: usize, elapsed_us: u64, ok: bool) {
        self.flushed_batches_total.fetch_add(1, Ordering::Relaxed);
        self.flushed_rows_total
            .fetch_add(rows as u64, Ordering::Relaxed);
        if !ok {
            self.flush_failures_total.fetch_add(1, Ordering::Relaxed);
        }
        self.flush_us_sum.fetch_add(elapsed_us, Ordering::Relaxed);
        self.flush_us_max.fetch_max(elapsed_us, Ordering::Relaxed);
        self.last_batch_rows.store(rows as u64, Ordering::Relaxed);
    }
}

/// 1xx–5xx 归类,其余 `unknown`。
pub fn status_class(status_code: i64) -> &'static str {
    match status_code {
        100..=199 => "1xx",
        200..=299 => "2xx",
        300..=399 => "3xx",
        400..=499 => "4xx",
        500..=599 => "5xx",
        _ => "unknown",
    }
}

/// Python `_normalize`:strip 后为空则用 fallback。
fn normalize(value: &str, fallback: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        fallback.to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Python `_normalize_status`:非 100..=999 一律归 0。
fn normalize_status(status_code: i64) -> i64 {
    if (100..=999).contains(&status_code) {
        status_code
    } else {
        0
    }
}

#[derive(Debug, Clone)]
struct HttpMetric {
    method: String,
    route: String,
    status_code: i64,
    status_class: String,
    count: i64,
    duration_ms_sum: f64,
    duration_ms_max: f64,
    duration_buckets: Vec<i64>,
}

#[derive(Debug, Default)]
struct CounterMetric {
    labels: Vec<(String, String)>,
    count: i64,
}

/// 转换耗时按策略聚合(count/sum/max,微秒)。
#[derive(Debug, Clone, Default)]
struct ConversionDuration {
    count: i64,
    elapsed_us_sum: u64,
    elapsed_us_max: u64,
}

/// 内存指标注册表。
#[derive(Default)]
pub struct MetricsRegistry {
    http: Mutex<HashMap<String, HttpMetric>>,
    stream_conversions: Mutex<Vec<CounterMetric>>,
    tool_executions: Mutex<Vec<CounterMetric>>,
    billing_deliveries: Mutex<Vec<CounterMetric>>,
    /// E1:请求面转换按 (strategy, source, target) 计数。
    conversions: Mutex<Vec<CounterMetric>>,
    /// E1:转换诊断按 DiagnosticCode 计数(只有类别,无内容)。
    conversion_diagnostics: Mutex<Vec<CounterMetric>>,
    /// P5-3:响应面转换按 (strategy, source, target) 计数。
    response_conversions: Mutex<Vec<CounterMetric>>,
    /// E1:转换耗时按策略聚合(微秒)。
    conversion_duration: Mutex<HashMap<String, ConversionDuration>>,
    /// P5-3:每次 attempt 的保真声明(等级/请求模式/响应模式/协议方向)。
    exchange_plans: Mutex<Vec<CounterMetric>>,
    /// 请求日志管线水位(20261016_01 收官项)。
    pub log_pipeline: LogPipelineStats,
}

impl MetricsRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记一次 HTTP 请求(请求日志链路唯一活调用点)。
    pub fn record_http_request(
        &self,
        method: &str,
        route: &str,
        status_code: i64,
        duration_ms: f64,
    ) {
        let code = normalize_status(status_code);
        let key = format!("{}\n{route}\n{code}", method.to_uppercase());
        let duration = if duration_ms.is_finite() {
            duration_ms.max(0.0)
        } else {
            0.0
        };
        let mut guard = self.http.lock().unwrap_or_else(|e| e.into_inner());
        let metric = guard.entry(key).or_insert_with(|| HttpMetric {
            method: normalize(method, "unknown").to_uppercase(),
            route: normalize(route, "unknown"),
            status_code: code,
            status_class: status_class(code).to_owned(),
            count: 0,
            duration_ms_sum: 0.0,
            duration_ms_max: 0.0,
            duration_buckets: vec![0; HTTP_DURATION_BUCKETS_SECONDS.len()],
        });
        metric.count += 1;
        metric.duration_ms_sum += duration;
        if duration > metric.duration_ms_max {
            metric.duration_ms_max = duration;
        }
        let seconds = duration / 1000.0;
        for (index, le) in HTTP_DURATION_BUCKETS_SECONDS.iter().enumerate() {
            if seconds <= *le {
                metric.duration_buckets[index] += 1;
            }
        }
    }

    pub fn record_stream_conversion(
        &self,
        source_adapter: &str,
        target_provider: &str,
        mode: &str,
        target_provider_name: &str,
    ) {
        let labels = vec![
            (
                "source_adapter".to_owned(),
                normalize(source_adapter, "unknown"),
            ),
            (
                "target_provider".to_owned(),
                normalize(target_provider, "unknown"),
            ),
            (
                "target_provider_name".to_owned(),
                normalize(target_provider_name, "default"),
            ),
            ("mode".to_owned(), normalize(mode, "unknown")),
        ];
        bump_counter(&self.stream_conversions, labels);
    }

    pub fn record_tool_execution(
        &self,
        provider: &str,
        source_adapter: &str,
        outcome: &str,
        provider_name: &str,
    ) {
        let labels = vec![
            ("provider".to_owned(), normalize(provider, "unknown")),
            (
                "provider_name".to_owned(),
                normalize(provider_name, "default"),
            ),
            (
                "source_adapter".to_owned(),
                normalize(source_adapter, "unknown"),
            ),
            ("outcome".to_owned(), normalize(outcome, "unknown")),
        ];
        bump_counter(&self.tool_executions, labels);
    }

    /// E1:记一次请求面转换(策略 × 客户端协议 × 上游协议)与耗时。
    /// 只记类别与微秒数,不含任何请求 payload。
    pub fn record_conversion(&self, strategy: &str, source: &str, target: &str, elapsed_us: u64) {
        bump_counter(
            &self.conversions,
            vec![
                ("strategy".to_owned(), normalize(strategy, "unknown")),
                ("source".to_owned(), normalize(source, "unknown")),
                ("target".to_owned(), normalize(target, "unknown")),
            ],
        );
        let mut guard = self
            .conversion_duration
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let entry = guard
            .entry(normalize(strategy, "unknown").to_owned())
            .or_default();
        entry.count += 1;
        entry.elapsed_us_sum = entry.elapsed_us_sum.saturating_add(elapsed_us);
        entry.elapsed_us_max = entry.elapsed_us_max.max(elapsed_us);
    }

    /// P5-3:记一次响应面转换(策略 × 客户端协议 × 上游协议)。
    pub fn record_response_conversion(&self, strategy: &str, source: &str, target: &str) {
        bump_counter(
            &self.response_conversions,
            vec![
                ("strategy".to_owned(), normalize(strategy, "unknown")),
                ("source".to_owned(), normalize(source, "unknown")),
                ("target".to_owned(), normalize(target, "unknown")),
            ],
        );
    }

    /// E1:记一次转换诊断(仅 DiagnosticCode 类别)。
    pub fn record_conversion_diagnostic(&self, code: &str) {
        bump_counter(
            &self.conversion_diagnostics,
            vec![("code".to_owned(), normalize(code, "unknown"))],
        );
    }

    /// P5-3(G-10):记录一次交换计划的保真承诺与正文模式。
    /// 标签全部来自固定枚举,不包含模型名/请求正文,基数有界。
    pub fn record_exchange_plan(
        &self,
        fidelity: &str,
        request_mode: &str,
        response_mode: &str,
        source: &str,
        target: &str,
    ) {
        bump_counter(
            &self.exchange_plans,
            vec![
                ("fidelity".to_owned(), normalize(fidelity, "unknown")),
                (
                    "request_mode".to_owned(),
                    normalize(request_mode, "unknown"),
                ),
                (
                    "response_mode".to_owned(),
                    normalize(response_mode, "unknown"),
                ),
                ("source".to_owned(), normalize(source, "unknown")),
                ("target".to_owned(), normalize(target, "unknown")),
            ],
        );
    }

    pub fn record_billing_delivery(&self, outcome: &str, transport: &str) {
        let labels = vec![
            ("outcome".to_owned(), normalize(outcome, "unknown")),
            ("transport".to_owned(), normalize(transport, "unknown")),
        ];
        bump_counter(&self.billing_deliveries, labels);
    }

    /// `GET /api/v1/metrics` 的 JSON 形态。
    pub fn render_json(&self) -> Value {
        let http = sorted_http(&self.http);
        let http_json: Vec<Value> = http
            .into_iter()
            .map(|metric| {
                json!({
                    "method": metric.method,
                    "route": metric.route,
                    "status_code": metric.status_code,
                    "status_class": metric.status_class,
                    "count": metric.count,
                    "duration_ms_sum": round3(metric.duration_ms_sum),
                    "duration_ms_max": round3(metric.duration_ms_max),
                    "duration_buckets_seconds": HTTP_DURATION_BUCKETS_SECONDS,
                    "duration_bucket_counts": metric.duration_buckets,
                })
            })
            .collect();
        let duration = self
            .conversion_duration
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let duration_json: Vec<Value> = duration
            .iter()
            .map(|(strategy, stat)| {
                json!({
                    "strategy": strategy,
                    "count": stat.count,
                    "elapsed_us_sum": stat.elapsed_us_sum,
                    "elapsed_us_max": stat.elapsed_us_max,
                })
            })
            .collect();
        let pipeline = &self.log_pipeline;
        json!({
            "http": http_json,
            "stream_conversions": counters_json(&self.stream_conversions),
            "tool_executions": counters_json(&self.tool_executions),
            "billing_deliveries": counters_json(&self.billing_deliveries),
            "conversions": counters_json(&self.conversions),
            "response_conversions": counters_json(&self.response_conversions),
            "exchange_plans": counters_json(&self.exchange_plans),
            "conversion_diagnostics": counters_json(&self.conversion_diagnostics),
            "conversion_duration": duration_json,
            "log_pipeline": {
                "enqueued_total": pipeline.enqueued_total.load(Ordering::Relaxed),
                "dropped_total": pipeline.dropped_total.load(Ordering::Relaxed),
                "queue_depth": pipeline.queue_depth.load(Ordering::Relaxed),
                "flushed_batches_total": pipeline.flushed_batches_total.load(Ordering::Relaxed),
                "flushed_rows_total": pipeline.flushed_rows_total.load(Ordering::Relaxed),
                "flush_failures_total": pipeline.flush_failures_total.load(Ordering::Relaxed),
                "flush_ms_sum": round3(
                    pipeline.flush_us_sum.load(Ordering::Relaxed) as f64 / 1000.0
                ),
                "flush_ms_max": round3(
                    pipeline.flush_us_max.load(Ordering::Relaxed) as f64 / 1000.0
                ),
                "last_batch_rows": pipeline.last_batch_rows.load(Ordering::Relaxed),
            },
        })
    }

    /// Prometheus 文本暴露格式(`?format=prometheus`)。
    pub fn render_prometheus(
        &self,
        include_provider_health: bool,
        provider_snapshots: &[Value],
    ) -> String {
        let mut lines: Vec<String> = Vec::new();
        let http = sorted_http(&self.http);
        let push_group = |lines: &mut Vec<String>,
                          help: &str,
                          kind: &str,
                          render: &dyn Fn(&HttpMetric, String) -> String| {
            lines.push(format!("# HELP {help}"));
            lines.push(format!(
                "# TYPE {kind} {metric_kind}",
                kind = kind,
                metric_kind = if kind.ends_with("_max") {
                    "gauge"
                } else {
                    "counter"
                }
            ));
            for metric in &http {
                lines.push(render(metric, http_labels(metric)));
            }
        };
        push_group(
            &mut lines,
            "gateway_http_requests_total Total HTTP requests handled by the gateway.",
            "gateway_http_requests_total",
            &|metric, labels| format!("gateway_http_requests_total{labels} {}", metric.count),
        );
        push_group(
            &mut lines,
            "gateway_http_request_duration_ms_sum Sum of gateway HTTP request durations in milliseconds.",
            "gateway_http_request_duration_ms_sum",
            &|metric, labels| {
                format!(
                    "gateway_http_request_duration_ms_sum{labels} {}",
                    fmt_float(metric.duration_ms_sum)
                )
            },
        );
        push_group(
            &mut lines,
            "gateway_http_request_duration_ms_max Max gateway HTTP request duration in milliseconds.",
            "gateway_http_request_duration_ms_max",
            &|metric, labels| {
                format!(
                    "gateway_http_request_duration_ms_max{labels} {}",
                    fmt_float(metric.duration_ms_max)
                )
            },
        );
        lines.push("# HELP gateway_http_request_duration_seconds Gateway HTTP request duration in seconds.".to_owned());
        lines.push("# TYPE gateway_http_request_duration_seconds histogram".to_owned());
        for metric in &http {
            let base = http_labels(metric);
            for (index, le) in HTTP_DURATION_BUCKETS_SECONDS.iter().enumerate() {
                let labels = merge_label(&base, "le", &fmt_float(*le));
                lines.push(format!(
                    "gateway_http_request_duration_seconds_bucket{labels} {}",
                    metric.duration_buckets[index]
                ));
            }
            let inf = merge_label(&base, "le", "+Inf");
            lines.push(format!(
                "gateway_http_request_duration_seconds_bucket{inf} {}",
                metric.count
            ));
            lines.push(format!(
                "gateway_http_request_duration_seconds_sum{base} {}",
                fmt_float(metric.duration_ms_sum / 1000.0)
            ));
            lines.push(format!(
                "gateway_http_request_duration_seconds_count{base} {}",
                metric.count
            ));
        }
        render_counter_group(
            &mut lines,
            "gateway_stream_conversions_total",
            "Total streaming requests by gateway conversion mode.",
            "counter",
            &self.stream_conversions,
        );
        render_counter_group(
            &mut lines,
            "gateway_tool_executions_total",
            "Total transparent gateway tool executions.",
            "counter",
            &self.tool_executions,
        );
        render_counter_group(
            &mut lines,
            "gateway_billing_events_total",
            "Gateway billing event delivery attempts by outcome.",
            "counter",
            &self.billing_deliveries,
        );
        render_counter_group(
            &mut lines,
            "gateway_conversions_total",
            "Total request-face conversions by strategy, source and target protocol.",
            "counter",
            &self.conversions,
        );
        render_counter_group(
            &mut lines,
            "gateway_response_conversions_total",
            "Total response-face conversions by strategy, source and target protocol.",
            "counter",
            &self.response_conversions,
        );
        render_counter_group(
            &mut lines,
            "gateway_conversion_diagnostics_total",
            "Total conversion diagnostics by code.",
            "counter",
            &self.conversion_diagnostics,
        );
        render_counter_group(
            &mut lines,
            "gateway_exchange_plans_total",
            "Total planned exchanges by fidelity and body modes.",
            "counter",
            &self.exchange_plans,
        );
        {
            let duration = self
                .conversion_duration
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let mut rows: Vec<(&String, &ConversionDuration)> = duration.iter().collect();
            rows.sort_by(|left, right| left.0.cmp(right.0));
            lines.push("# HELP gateway_conversion_duration_us_sum Sum of request conversion durations in microseconds.".to_owned());
            lines.push("# TYPE gateway_conversion_duration_us_sum counter".to_owned());
            for (strategy, stat) in &rows {
                lines.push(format!(
                    "gateway_conversion_duration_us_sum{{strategy=\"{strategy}\"}} {}",
                    stat.elapsed_us_sum
                ));
            }
            lines.push("# HELP gateway_conversion_duration_us_max Max request conversion duration in microseconds.".to_owned());
            lines.push("# TYPE gateway_conversion_duration_us_max gauge".to_owned());
            for (strategy, stat) in &rows {
                lines.push(format!(
                    "gateway_conversion_duration_us_max{{strategy=\"{strategy}\"}} {}",
                    stat.elapsed_us_max
                ));
            }
        }
        {
            let pipeline = &self.log_pipeline;
            let enqueued = pipeline.enqueued_total.load(Ordering::Relaxed);
            let dropped = pipeline.dropped_total.load(Ordering::Relaxed);
            let flushed_rows = pipeline.flushed_rows_total.load(Ordering::Relaxed);
            let flush_failures = pipeline.flush_failures_total.load(Ordering::Relaxed);
            lines.push("# HELP gateway_log_pipeline_rows_total Request log pipeline rows by outcome (enqueued/dropped/flushed/flush_failed).".to_owned());
            lines.push("# TYPE gateway_log_pipeline_rows_total counter".to_owned());
            lines.push(format!(
                "gateway_log_pipeline_rows_total{{outcome=\"enqueued\"}} {enqueued}"
            ));
            lines.push(format!(
                "gateway_log_pipeline_rows_total{{outcome=\"dropped\"}} {dropped}"
            ));
            lines.push(format!(
                "gateway_log_pipeline_rows_total{{outcome=\"flushed\"}} {flushed_rows}"
            ));
            lines.push(format!(
                "gateway_log_pipeline_rows_total{{outcome=\"flush_failed\"}} {flush_failures}"
            ));
            lines.push("# HELP gateway_log_pipeline_flush_duration_ms_sum Sum of request log batch flush durations in milliseconds.".to_owned());
            lines.push("# TYPE gateway_log_pipeline_flush_duration_ms_sum counter".to_owned());
            lines.push(format!(
                "gateway_log_pipeline_flush_duration_ms_sum {}",
                fmt_float(pipeline.flush_us_sum.load(Ordering::Relaxed) as f64 / 1000.0)
            ));
            lines.push("# HELP gateway_log_pipeline_flush_duration_ms_max Max request log batch flush duration in milliseconds.".to_owned());
            lines.push("# TYPE gateway_log_pipeline_flush_duration_ms_max gauge".to_owned());
            lines.push(format!(
                "gateway_log_pipeline_flush_duration_ms_max {}",
                fmt_float(pipeline.flush_us_max.load(Ordering::Relaxed) as f64 / 1000.0)
            ));
            lines.push(
                "# HELP gateway_log_pipeline_queue_depth Last observed request log queue depth."
                    .to_owned(),
            );
            lines.push("# TYPE gateway_log_pipeline_queue_depth gauge".to_owned());
            lines.push(format!(
                "gateway_log_pipeline_queue_depth {}",
                pipeline.queue_depth.load(Ordering::Relaxed)
            ));
        }
        if include_provider_health && !provider_snapshots.is_empty() {
            lines.push("# HELP gateway_provider_health_status Current provider health status, one-hot encoded.".to_owned());
            lines.push("# TYPE gateway_provider_health_status gauge".to_owned());
            for snapshot in provider_snapshots {
                let provider_id = snapshot
                    .get("provider_id")
                    .or_else(|| snapshot.get("provider"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let current = snapshot
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                for status in ["healthy", "degraded", "down", "unknown"] {
                    let labels = format_labels(&[("provider", provider_id), ("status", status)]);
                    lines.push(format!(
                        "gateway_provider_health_status{labels} {}",
                        i64::from(status == current)
                    ));
                }
                let latency = snapshot
                    .get("latency_ms")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                let labels = format_labels(&[("provider", provider_id)]);
                lines.push(format!("gateway_provider_latency_ms{labels} {latency}"));
            }
        }
        lines.join("\n") + "\n"
    }
}

fn bump_counter(store: &Mutex<Vec<CounterMetric>>, labels: Vec<(String, String)>) {
    let key = labels
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut guard = store.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(existing) = guard.iter_mut().find(|metric| {
        metric
            .labels
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("\n")
            == key
    }) {
        existing.count += 1;
        return;
    }
    guard.push(CounterMetric { labels, count: 1 });
}

type CounterRow = (String, Vec<(String, String)>, i64);

fn counters_json(store: &Mutex<Vec<CounterMetric>>) -> Vec<Value> {
    let guard = store.lock().unwrap_or_else(|e| e.into_inner());
    let mut rows: Vec<CounterRow> = guard
        .iter()
        .map(|metric| {
            (
                metric
                    .labels
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                metric.labels.clone(),
                metric.count,
            )
        })
        .collect();
    rows.sort_by(|left, right| left.0.cmp(&right.0));
    rows.into_iter()
        .map(|(_, labels, count)| {
            let mut map = Map::new();
            for (key, value) in labels {
                map.insert(key, Value::String(value));
            }
            map.insert("count".to_owned(), Value::from(count));
            Value::Object(map)
        })
        .collect()
}

fn sorted_http(store: &Mutex<HashMap<String, HttpMetric>>) -> Vec<HttpMetric> {
    let guard = store.lock().unwrap_or_else(|e| e.into_inner());
    let mut rows: Vec<HttpMetric> = guard.values().cloned().collect();
    rows.sort_by(|a, b| {
        (&a.method, &a.route, a.status_code).cmp(&(&b.method, &b.route, b.status_code))
    });
    rows
}

fn http_labels(metric: &HttpMetric) -> String {
    let status_code = metric.status_code.to_string();
    format_labels(&[
        ("method", metric.method.as_str()),
        ("route", metric.route.as_str()),
        ("status_code", status_code.as_str()),
        ("status_class", metric.status_class.as_str()),
    ])
}

fn format_labels(labels: &[(&str, &str)]) -> String {
    if labels.is_empty() {
        return String::new();
    }
    let inner = labels
        .iter()
        .map(|(key, value)| format!("{key}=\"{}\"", escape(value)))
        .collect::<Vec<_>>()
        .join(",");
    format!("{{{inner}}}")
}

fn merge_label(formatted: &str, key: &str, value: &str) -> String {
    // Python 先 _parse_labels 再加 le;这里等价地在尾部追加。
    let extra = format!("{key}=\"{}\"", escape(value));
    if formatted.is_empty() || !formatted.starts_with('{') {
        return format!("{{{extra}}}");
    }
    let body = &formatted[1..formatted.len() - 1];
    if body.is_empty() {
        return format!("{{{extra}}}");
    }
    format!("{{{body},{extra}}}")
}

fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

/// Python `_fmt`:0 → "0",其余 `repr(float)`。
fn fmt_float(value: f64) -> String {
    if value == 0.0 {
        return "0".to_owned();
    }
    let text = format!("{value}");
    if text.contains('.') || text.contains('e') {
        text
    } else {
        format!("{text}.0")
    }
}

fn render_counter_group(
    lines: &mut Vec<String>,
    kind: &str,
    help: &str,
    metric_kind: &str,
    store: &Mutex<Vec<CounterMetric>>,
) {
    lines.push(format!("# HELP {kind} {help}"));
    lines.push(format!("# TYPE {kind} {metric_kind}"));
    let guard = store.lock().unwrap_or_else(|e| e.into_inner());
    let mut rows: Vec<(&Vec<(String, String)>, i64)> = guard
        .iter()
        .map(|metric| (&metric.labels, metric.count))
        .collect();
    rows.sort_by(|left, right| {
        let key = |labels: &Vec<(String, String)>| {
            labels
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        key(left.0).cmp(&key(right.0))
    });
    for (labels, count) in rows {
        let owned: Vec<(&str, &str)> = labels
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        lines.push(format!("{kind}{} {count}", format_labels(&owned)));
    }
}

fn round3(value: f64) -> f64 {
    (value * 1000.0).round_ties_even() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_class_buckets_and_normalization() {
        assert_eq!(status_class(100), "1xx");
        assert_eq!(status_class(204), "2xx");
        assert_eq!(status_class(302), "3xx");
        assert_eq!(status_class(429), "4xx");
        assert_eq!(status_class(503), "5xx");
        assert_eq!(status_class(99), "unknown");
        assert_eq!(normalize_status(0), 0);
        assert_eq!(normalize_status(1000), 0);
        assert_eq!(normalize_status(404), 404);
    }

    #[test]
    fn histogram_buckets_are_cumulative() {
        let registry = MetricsRegistry::new();
        // 120ms → 落在 0.25/0.5/1/3/10/30 六个桶里(累积桶)。
        registry.record_http_request("post", "/v1/chat/completions", 200, 120.0);
        let rendered = registry.render_json();
        let row = &rendered["http"][0];
        assert_eq!(row["method"], json!("POST"), "method 大写归一");
        assert_eq!(row["status_class"], json!("2xx"));
        assert_eq!(row["count"], json!(1));
        assert_eq!(row["duration_ms_sum"], json!(120.0));
        assert_eq!(
            row["duration_bucket_counts"],
            json!([0, 0, 1, 1, 1, 1, 1, 1])
        );
    }

    #[test]
    fn counters_group_by_labels() {
        let registry = MetricsRegistry::new();
        registry.record_http_request("POST", "/v1/chat/completions", 200, 10.0);
        registry.record_http_request("POST", "/v1/chat/completions", 200, 30.0);
        registry.record_http_request("POST", "/v1/messages", 500, 5.0);
        let rendered = registry.render_json();
        assert_eq!(rendered["http"].as_array().unwrap().len(), 2);
        assert_eq!(rendered["http"][0]["count"], json!(2));
        assert_eq!(rendered["http"][0]["duration_ms_max"], json!(30.0));
        registry.record_stream_conversion("openai", "anthropic", "sse", "");
        registry.record_stream_conversion("openai", "anthropic", "sse", "");
        let rendered = registry.render_json();
        let conversions = rendered["stream_conversions"].as_array().unwrap();
        assert_eq!(conversions.len(), 1);
        assert_eq!(conversions[0]["count"], json!(2));
        assert_eq!(conversions[0]["target_provider_name"], json!("default"));
    }

    #[test]
    fn exchange_and_response_conversion_metrics_are_bounded_labels() {
        let registry = MetricsRegistry::new();
        registry.record_exchange_plan(
            "semantic_equivalent",
            "translate",
            "translate",
            "openai",
            "anthropic",
        );
        registry.record_exchange_plan("byte_exact", "raw", "raw", "anthropic", "anthropic");
        registry.record_response_conversion("semantic", "openai", "anthropic");
        registry.record_response_conversion("semantic", "openai", "anthropic");
        let rendered = registry.render_json();
        let plans = rendered["exchange_plans"].as_array().unwrap();
        assert_eq!(plans.len(), 2);
        let fidelity: Vec<&str> = plans
            .iter()
            .map(|row| row["fidelity"].as_str().unwrap())
            .collect();
        assert!(fidelity.contains(&"semantic_equivalent"));
        assert!(fidelity.contains(&"byte_exact"));
        let responses = rendered["response_conversions"].as_array().unwrap();
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0]["count"], json!(2));
        let text = registry.render_prometheus(false, &[]);
        assert!(text.contains("gateway_exchange_plans_total"));
        assert!(text.contains("gateway_response_conversions_total"));
    }

    #[test]
    fn conversion_metrics_group_by_strategy_and_code() {
        let registry = MetricsRegistry::new();
        registry.record_conversion("semantic", "openai", "anthropic", 120);
        registry.record_conversion("semantic", "openai", "anthropic", 80);
        registry.record_conversion("native_passthrough", "gemini", "gemini", 5);
        registry.record_conversion_diagnostic("dropped_field");
        registry.record_conversion_diagnostic("dropped_field");
        registry.record_conversion_diagnostic("opaque_preserved");
        let rendered = registry.render_json();
        let conversions = rendered["conversions"].as_array().unwrap();
        assert_eq!(conversions.len(), 2);
        assert_eq!(conversions[0]["strategy"], json!("native_passthrough"));
        assert_eq!(conversions[1]["count"], json!(2));
        let diagnostics = rendered["conversion_diagnostics"].as_array().unwrap();
        assert_eq!(diagnostics[0]["code"], json!("dropped_field"));
        assert_eq!(diagnostics[0]["count"], json!(2));
        let duration = rendered["conversion_duration"].as_array().unwrap();
        let semantic = duration
            .iter()
            .find(|row| row["strategy"] == json!("semantic"))
            .expect("semantic 行应存在");
        assert_eq!(semantic["elapsed_us_sum"], json!(200));
        assert_eq!(semantic["elapsed_us_max"], json!(120));
        let text = registry.render_prometheus(false, &[]);
        assert!(text.contains("gateway_conversions_total{strategy=\"semantic\",source=\"openai\",target=\"anthropic\"} 2"));
        assert!(text.contains("gateway_conversion_diagnostics_total{code=\"dropped_field\"} 2"));
        assert!(text.contains("gateway_conversion_duration_us_max{strategy=\"semantic\"} 120"));
    }

    #[test]
    fn prometheus_exposition_shape() {
        let registry = MetricsRegistry::new();
        registry.record_http_request("POST", "/v1/chat/completions", 200, 120.0);
        let text = registry.render_prometheus(false, &[]);
        assert!(text.contains("# TYPE gateway_http_requests_total counter"));
        assert!(text.contains(
            "gateway_http_requests_total{method=\"POST\",route=\"/v1/chat/completions\",status_code=\"200\",status_class=\"2xx\"} 1"
        ));
        assert!(text.contains("gateway_http_request_duration_ms_sum"));
        assert!(text.contains("le=\"0.25\""));
        assert!(text.contains("le=\"+Inf\"} 1"));
        assert!(text.ends_with('\n'));
        assert!(!text.contains("gateway_provider_health_status"));
    }

    #[test]
    fn provider_health_block_is_opt_in() {
        let registry = MetricsRegistry::new();
        let snapshots = vec![json!({"provider_id": "p1", "status": "degraded", "latency_ms": 7})];
        let text = registry.render_prometheus(true, &snapshots);
        assert!(
            text.contains("gateway_provider_health_status{provider=\"p1\",status=\"healthy\"} 0")
        );
        assert!(
            text.contains("gateway_provider_health_status{provider=\"p1\",status=\"degraded\"} 1")
        );
        assert!(text.contains("gateway_provider_latency_ms{provider=\"p1\"} 7"));
    }

    /// 20261016_01 收官项:日志管线水位进指标面(JSON + Prometheus)。
    #[test]
    fn log_pipeline_stats_render_in_both_formats() {
        let registry = MetricsRegistry::new();
        registry.log_pipeline.record_enqueued(3);
        registry.log_pipeline.record_dropped(1024);
        registry.log_pipeline.record_flush(5, 1500, true);
        registry.log_pipeline.record_flush(2, 800, false);

        let rendered = registry.render_json();
        let pipeline = &rendered["log_pipeline"];
        assert_eq!(pipeline["enqueued_total"], json!(1));
        assert_eq!(pipeline["dropped_total"], json!(1));
        assert_eq!(pipeline["queue_depth"], json!(1024));
        assert_eq!(pipeline["flushed_batches_total"], json!(2));
        assert_eq!(pipeline["flushed_rows_total"], json!(7));
        assert_eq!(pipeline["flush_failures_total"], json!(1));
        assert_eq!(pipeline["flush_ms_sum"], json!(2.3));
        assert_eq!(pipeline["flush_ms_max"], json!(1.5));
        assert_eq!(pipeline["last_batch_rows"], json!(2));

        let text = registry.render_prometheus(false, &[]);
        assert!(text.contains("gateway_log_pipeline_rows_total{outcome=\"enqueued\"} 1"));
        assert!(text.contains("gateway_log_pipeline_rows_total{outcome=\"dropped\"} 1"));
        assert!(text.contains("gateway_log_pipeline_rows_total{outcome=\"flushed\"} 7"));
        assert!(text.contains("gateway_log_pipeline_rows_total{outcome=\"flush_failed\"} 1"));
        assert!(text.contains("gateway_log_pipeline_flush_duration_ms_sum 2.3"));
        assert!(text.contains("gateway_log_pipeline_flush_duration_ms_max 1.5"));
        assert!(text.contains("gateway_log_pipeline_queue_depth 1024"));
    }
}
