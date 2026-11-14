//! 请求日志存储实现(方案 #14 R4.5;约束 C3)。
//!
//! 镜像 Python `infrastructure/persistence/sqlite/request_log_store.py`。
//! 落库形态是 A9.7 的「主行元数据 + 侧表正文」拆分:
//! - `request_logs(request_id, created_at, payload)` 主行只带元数据(约 2 KB),
//!   列表/用量查询因此永远不解析多 MB 的正文;
//! - `request_log_bodies` 侧表存正文与响应头,`created_at` **与主行取同一条
//!   `SELECT datetime('now')`**,这样正文层的独立保留清理无需 join;
//! - 未开启捕获(`bodies = None`)时会 **删掉** 同 id 的旧侧表行,防止
//!   request_id 复用翻出历史正文。
//!
//! C3 三条策略各自对应一个命名测试:
//! - [`RequestLogStore::prune_logs`] 分档过期(正文默认 7 天 / 元数据永久)
//!   → `body_pruning_keeps_metadata_forever_on_default_window`;
//! - `payload` 文本用默认分隔符 + 插入序(与 Python `json.dumps` 同形)
//!   → `request_log_payload_text_matches_python_encoding`;
//! - 捕获/门控语义在 `wr-domain::observability::log_policy`(纯函数),
//!   `callLogs` 只门控 request_logs → `calllogs_gating_*`。

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value};

use wr_app::ports::{LogBodies, PendingRequestLog, PortError, PruneStats, RequestLogStore};

use crate::python_semantics::{py_bool, py_int, py_str};

use super::SqliteStore;
use super::usage_store::{
    DecodeSource, HistogramSamples, ProviderCatalog, RowDims, UsageSums, catalog_from_providers,
    decode_tps_sample, dims_from_item, float_or, histogram_samples_for_item, is_error_item,
    normalize_model_display, number, output_tps_sample, parse_iso, round_even, str_or_empty,
    usage_sums_from_item,
};
use super::{SqlContext as _, object};

/// `query_request_logs` 检视的最新行数。
const CALL_LOG_WINDOW: i64 = 2000;
const CALL_LOG_MAX_PAGE_SIZE: i64 = 100;

/// 旧行内嵌正文形态的字段名;最新格式只从侧表读取正文/头部。
const RETIRED_INLINE_BODY_KEYS: &[&str] = &[
    "request_body_text",
    "response_body_text",
    "response_headers",
    "request_headers",
    "request_body_capture_status",
    "response_body_capture_status",
    "request_body_size_bytes",
    "request_body_truncated",
    "response_body_size_bytes",
    "response_body_truncated",
];

/// 落一行日志的 SQL 时间戳:`SELECT datetime('now')`(UTC,
/// `YYYY-MM-DD HH:MM:SS`,**不是** ISO-Z)。主行与侧表行共用同一个值。
fn db_now(connection: &Connection) -> Result<String, PortError> {
    connection
        .query_row("SELECT datetime('now')", [], |row| row.get(0))
        .context("SELECT datetime('now')")
}

/// 把侧表正文并回主行 payload。
///
/// 2026-11-07(W3-8):只认最新拆分格式。旧行内嵌在 payload 里的正文/
/// 头部字段不再读取 —— 缺侧表行时按「不可用」降级,不崩溃、不改写历史。
fn with_split_bodies(
    payload_json: Option<&str>,
    request_body: Option<&str>,
    response_body: Option<&str>,
    response_headers_json: Option<&str>,
    request_headers_json: Option<&str>,
    request_body_size: Option<i64>,
    request_body_truncated: Option<i64>,
    response_body_size: Option<i64>,
    response_body_truncated: Option<i64>,
    mut result: Map<String, Value>,
) -> Value {
    if let Some(text) = payload_json
        && let Ok(Value::Object(parsed)) = serde_json::from_str::<Value>(text)
    {
        for (key, value) in parsed {
            if RETIRED_INLINE_BODY_KEYS.contains(&key.as_str()) {
                continue;
            }
            result.insert(key, value);
        }
    }
    if let Some(text) = request_body {
        result.insert(
            "request_body_text".to_owned(),
            Value::String(text.to_owned()),
        );
    }
    if let Some(text) = response_body {
        result.insert(
            "response_body_text".to_owned(),
            Value::String(text.to_owned()),
        );
    }
    if let Some(text) = response_headers_json
        && let Ok(Value::Object(headers)) = serde_json::from_str::<Value>(text)
    {
        result.insert("response_headers".to_owned(), Value::Object(headers));
    }
    if let Some(text) = request_headers_json
        && let Ok(Value::Object(headers)) = serde_json::from_str::<Value>(text)
    {
        result.insert("request_headers".to_owned(), Value::Object(headers));
    }
    let request_status = match (request_body, request_body_truncated) {
        (Some(_), Some(1)) => "truncated",
        (Some(_), _) => "captured",
        _ => "unavailable",
    };
    let response_status = match (response_body, response_body_truncated) {
        (Some(_), Some(1)) => "truncated",
        (Some(_), _) => "captured",
        _ => "unavailable",
    };
    result.insert("request_body_capture_status".to_owned(), Value::String(request_status.to_owned()));
    result.insert("response_body_capture_status".to_owned(), Value::String(response_status.to_owned()));
    for (key, value) in [
        ("request_body_size_bytes", request_body_size),
        ("request_body_truncated", request_body_truncated),
        ("response_body_size_bytes", response_body_size),
        ("response_body_truncated", response_body_truncated),
    ] {
        if let Some(value) = value {
            let value = if key.ends_with("_truncated") { Value::Bool(value != 0) } else { Value::from(value) };
            result.insert(key.to_owned(), value);
        }
    }
    Value::Object(result)
}

/// attempt 项里会进侧表的采集键(20261017_02 批5):上游请求/响应/响应头/
/// 转换报告。主行瘦身 = 移除这四键 + 补 `captured` 标记;其余轻量元数据
/// (attempt_id/provider/status/wait_ms/size/truncated…)原样保留。
const ATTEMPT_CAPTURE_KEYS: &[&str] = &[
    "upstream_request",
    "upstream_request_headers",
    "upstream_response",
    "upstream_headers",
    "conversion",
];

/// 主行 payload 的 retryAttempts 瘦身(20261017_02 批5):把各 attempt 的
/// 采集四件套剥出,落 `request_log_attempts` 侧表(ordinal = 数组下标)。
/// 捕获预算默认 512KB/body,内嵌时单行 payload 可达 MB 级 —— 2000 行
/// 投影窗口的读取/解析/克隆成本由它主导;列表只用 `captured` 标记。
/// 返回 (瘦身后的 payload 文本, [(ordinal, 原始 attempt JSON)])。
fn split_attempt_captures(payload: &Value) -> (String, Vec<(i64, String)>) {
    let dumps = || wr_domain::json_py::py_json_dumps(payload);
    let Some(map) = payload.as_object() else {
        return (dumps(), Vec::new());
    };
    let Some(Value::Array(attempts)) = map.get("retry_attempts") else {
        return (dumps(), Vec::new());
    };
    let mut split_rows: Vec<(i64, String)> = Vec::new();
    let mut slim_attempts: Vec<Value> = Vec::with_capacity(attempts.len());
    for (index, item) in attempts.iter().enumerate() {
        let Some(obj) = item.as_object() else {
            slim_attempts.push(item.clone());
            continue;
        };
        let mut captured = false;
        let mut slim = obj.clone();
        for key in ATTEMPT_CAPTURE_KEYS {
            captured |= slim.remove(*key).is_some();
        }
        if captured {
            slim.insert("captured".to_owned(), Value::Bool(true));
            split_rows.push((index as i64, wr_domain::json_py::py_json_dumps(item)));
            slim_attempts.push(Value::Object(slim));
        } else {
            slim_attempts.push(item.clone());
        }
    }
    if split_rows.is_empty() {
        return (dumps(), split_rows);
    }
    let mut rebuilt = map.clone();
    rebuilt.insert("retry_attempts".to_owned(), Value::Array(slim_attempts));
    (
        wr_domain::json_py::py_json_dumps(&Value::Object(rebuilt)),
        split_rows,
    )
}

/// attempts 侧表并回(20261017_02 批5;详情读取专用):侧表行是剥离前的
/// 原始 attempt 对象。最新格式只认侧表;旧行内嵌采集不参与合成。
fn with_split_attempts(payload: &mut Value, attempt_rows: &[(i64, String)]) {
    let Some(map) = payload.as_object_mut() else {
        return;
    };
    let Some(Value::Array(attempts)) = map.get_mut("retry_attempts") else {
        return;
    };
    for (ordinal, text) in attempt_rows {
        let Some(slot) = attempts.get_mut(*ordinal as usize) else {
            continue;
        };
        if let Ok(captured) = serde_json::from_str::<Value>(text) {
            *slot = captured;
        }
    }
}

/// 读一个请求的全部 attempt 采集行(详情读取专用;ordinal 升序)。
fn read_attempt_captures(
    connection: &Connection,
    request_id: &str,
) -> Result<Vec<(i64, String)>, PortError> {
    let mut statement = connection
        .prepare(
            "SELECT ordinal, payload FROM request_log_attempts \
             WHERE request_id=?1 ORDER BY ordinal",
        )
        .context("prepare request_log_attempts read")?;
    let rows = statement
        .query_map(params![request_id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .context("query request_log_attempts")?;
    let mut items = Vec::new();
    for row in rows {
        items.push(row.context("row request_log_attempts")?);
    }
    Ok(items)
}

fn positive_int(value: Option<&Value>, default: i64, maximum: Option<i64>) -> i64 {    let parsed = match value {
        Some(raw) if py_bool(raw) => py_int(raw),
        _ => None,
    };
    let parsed = match parsed {
        Some(parsed) => parsed,
        None => return default,
    };
    if parsed < 1 {
        return default;
    }
    maximum.map_or(parsed, |cap| parsed.min(cap))
}

/// 调用日志投影缓存的 TTL(20261016_01 C-5):version(MAX rowid)覆盖
/// 增删 REPLACE;TTL 兜底 prune 删掉最大 rowid 行这类 version 不变的
/// 窗口,prune/迁移另有显式失效。
/// 投影缓存的兜底 TTL。version 对上即数据未变,缓存逻辑上恒新鲜;
/// TTL 只为「显式失效链漏网」的场景兜底(如未来新增改写历史行的路径
/// 忘了调 [`SqliteStore::invalidate_call_log_cache`]),因此可以远宽于
/// 旧的 5s(20261017_02 批1)。
const CALL_LOG_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(300);

/// provider 目录缓存槽(C-1):键 = `app_config` 原文。
#[derive(Debug, Clone)]
pub(crate) struct CatalogCacheSlot {
    raw: String,
    entry: std::sync::Arc<(ProviderCatalog, Map<String, Value>)>,
}

/// 最近 2000 行的已投影调用日志(C-5):一次构建,同版本查询复用。
#[derive(Debug, Clone)]
pub(crate) struct CallLogProjection {
    /// 全表行数(`totalRows`;COUNT(*) 只在构建时执行)。
    total_rows: i64,
    /// 已投影条目( camelCase,与 `call_log_entry` 输出同形)。
    entries: std::sync::Arc<Vec<Value>>,
    /// 筛选下拉项(models / credentials,构建时派生)。
    model_labels: std::sync::Arc<Vec<String>>,
    credentials: std::sync::Arc<Vec<(String, String)>>,
}

#[derive(Debug, Clone)]
pub(crate) struct CallLogCacheSlot {
    version: i64,
    built_at: std::time::Instant,
    inner: std::sync::Arc<CallLogProjection>,
}

impl SqliteStore {
    /// provider 目录 + id→protocol 映射(C-1 缓存:配置原文未变即复用)。
    ///
    /// 映射值取**有效上游协议**:配置里 `protocol` 只存协议家族
    /// (`openai`/`anthropic`/`gemini`),真正的线格式在 `type`
    /// (`openai_responses`/`openai_chat_completions`/`anthropic_messages`),
    /// 由前端 `protocolFamily` 拆分写入。只读 `protocol` 会把所有 openai
    /// 家族供应商都显示成 `OpenAI Compatible`,原生 Responses 供应商的转换
    /// 链路因此错乱(2026-10-13)。
    pub(crate) async fn catalog_snapshot(
        &self,
    ) -> std::sync::Arc<(ProviderCatalog, Map<String, Value>)> {
        let raw = self.load_payload_text().await;
        {
            let guard = self
                .catalog_cache
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Some(hit) = guard.as_ref()
                && hit.raw == raw
            {
                return hit.entry.clone();
            }
        }
        let entry = std::sync::Arc::new(catalog_from_raw(&raw));
        if let Ok(mut guard) = self.catalog_cache.lock() {
            *guard = Some(CatalogCacheSlot {
                raw,
                entry: entry.clone(),
            });
        }
        entry
    }

    /// `app_config` 原文(读失败按空配置处理,与 `load_payload` 同口径)。
    async fn load_payload_text(&self) -> String {
        self.engine
            .read(|connection| {
                let text: Option<String> = connection
                    .query_row("SELECT payload FROM app_config WHERE id=1", [], |row| {
                        row.get(0)
                    })
                    .optional()
                    .context("read app_config")?;
                Ok(text.unwrap_or_default())
            })
            .await
            .unwrap_or_default()
    }

    /// 最近 2000 行的投影(C-5;20261017_02 批1 增量化):version 未变
    /// 且未过 TTL 时直接复用;version 单调推进(落库新行的常态)时只对
    /// 新增行(`rowid > 缓存 version`)做投影并合并,不再全量重建。
    /// TTL / 删除(prune、正文迁移)/ 配置变更(save_payload)走显式
    /// 失效或全量兜底。`COUNT(*)` 只发生在全量路径。
    async fn call_log_projection(
        &self,
        version: i64,
    ) -> Result<std::sync::Arc<CallLogProjection>, PortError> {
        let incremental_base = {
            let guard = self
                .call_log_cache
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            match guard.as_ref() {
                // 命中:同 version 且未过 TTL。
                Some(hit) if hit.version == version && hit.built_at.elapsed() < CALL_LOG_CACHE_TTL => {
                    return Ok(hit.inner.clone());
                }
                // 增量基线:缓存落后(新行落库)且未过 TTL。TTL 过期的缓存
                // 不做基线 —— 增量会续命缓存,漏失效的目录标签会因此永生,
                // TTL 是这类场景的唯一兜底。
                Some(hit) if hit.version < version && hit.built_at.elapsed() < CALL_LOG_CACHE_TTL => {
                    Some(hit.clone())
                }
                _ => None,
            }
        };
        if let Some(hit) = incremental_base
            && let Some(merged) = self.merge_call_log_projection(&hit, version).await?
        {
            return Ok(merged);
        }
        self.rebuild_call_log_projection(version).await
    }

    /// 增量合并:只读 `rowid > 基线 version` 的新行投影后 prepend。
    /// 返回 `None` = 增量不可行(新行读不到 / 合并后乱序),调用方回退全量。
    async fn merge_call_log_projection(
        &self,
        hit: &CallLogCacheSlot,
        version: i64,
    ) -> Result<Option<std::sync::Arc<CallLogProjection>>, PortError> {
        let raw_rows = {
            let after = hit.version;
            self.engine
                .read(move |connection| {
                    query_call_log_rows_after(connection, after, version)
                })
                .await?
        };
        if raw_rows.is_empty() {
            // version 推进但窗口内无新行:中间发生过删除或 REPLACE 后又
            // 被清,增量计数不可信,回退全量。
            return Ok(None);
        }
        let catalog_pair = self.catalog_snapshot().await;
        let (catalog, provider_protocols) = (&catalog_pair.0, &catalog_pair.1);
        let (new_entries, new_labels, new_credentials) =
            project_raw_rows(&raw_rows, catalog, provider_protocols);

        let old = &hit.inner;
        // REPLACE/删除防御:增量只加不删。新行若复用了缓存已有的
        // request_id(INSERT OR REPLACE 改写历史行,旧 rowid 删新 rowid
        // 插),缓存里的旧副本必须靠全量重建清除。
        let known_ids: std::collections::HashSet<&str> = old
            .entries
            .iter()
            .filter_map(|entry| entry.get("requestId").and_then(Value::as_str))
            .collect();
        if raw_rows
            .iter()
            .any(|raw| known_ids.contains(raw.request_id.as_str()))
        {
            return Ok(None);
        }
        let mut entries =
            Vec::with_capacity(new_entries.len() + old.entries.len());
        entries.extend(new_entries);
        entries.extend(old.entries.iter().cloned());
        entries.truncate(CALL_LOG_WINDOW as usize);
        // 有序性防御:时钟回拨等会破坏降序 —— 退回全量重建,行为不劣于
        // 增量前的版本。
        if !sorted_by_created_at_desc(&entries) {
            return Ok(None);
        }
        let mut model_labels = old.model_labels.as_ref().clone();
        for label in new_labels {
            if !model_labels.iter().any(|known| known == &label) {
                model_labels.push(label);
            }
        }
        model_labels.sort();
        let mut credentials = old.credentials.as_ref().clone();
        for (id, name) in new_credentials {
            if !credentials.iter().any(|(known, _)| known == &id) {
                credentials.push((id, name));
            }
        }
        credentials.sort_by_key(|left| left.1.to_lowercase());
        let merged = std::sync::Arc::new(CallLogProjection {
            total_rows: old.total_rows + raw_rows.len() as i64,
            entries: std::sync::Arc::new(entries),
            model_labels: std::sync::Arc::new(model_labels),
            credentials: std::sync::Arc::new(credentials),
        });
        if let Ok(mut guard) = self.call_log_cache.lock() {
            *guard = Some(CallLogCacheSlot {
                version,
                built_at: std::time::Instant::now(),
                inner: merged.clone(),
            });
        }
        Ok(Some(merged))
    }

    /// 全量重建(C-5 原路径):读最近 2000 行、全量解析并构建。
    async fn rebuild_call_log_projection(
        &self,
        version: i64,
    ) -> Result<std::sync::Arc<CallLogProjection>, PortError> {
        let (raw_rows, total_rows) = self.engine.read(query_call_log_rows).await?;
        let catalog_pair = self.catalog_snapshot().await;
        let (catalog, provider_protocols) = (&catalog_pair.0, &catalog_pair.1);
        let (entries, mut model_labels, mut credentials) =
            project_raw_rows(&raw_rows, catalog, provider_protocols);
        model_labels.sort();
        credentials.sort_by_key(|left| left.1.to_lowercase());
        let inner = std::sync::Arc::new(CallLogProjection {
            total_rows,
            entries: std::sync::Arc::new(entries),
            model_labels: std::sync::Arc::new(model_labels),
            credentials: std::sync::Arc::new(credentials),
        });
        if let Ok(mut guard) = self.call_log_cache.lock() {
            *guard = Some(CallLogCacheSlot {
                version,
                built_at: std::time::Instant::now(),
                inner: inner.clone(),
            });
        }
        Ok(inner)
    }

    /// 投影缓存显式失效(prune / 正文迁移改写历史行后调用)。
    pub(crate) fn invalidate_call_log_cache(&self) {
        if let Ok(mut guard) = self.call_log_cache.lock() {
            *guard = None;
        }
    }
}

/// `app_config` 原文 → (provider 目录, id→有效上游协议)。
fn catalog_from_raw(raw: &str) -> (ProviderCatalog, Map<String, Value>) {
    let payload: Value = serde_json::from_str(raw).unwrap_or(Value::Null);
    let providers: Vec<Value> = match payload.get("Providers") {
        Some(Value::Array(items)) => items.clone(),
        _ => Vec::new(),
    };
    let mut protocols = Map::new();
    for item in &providers {
        let Some(map) = item.as_object() else {
            continue;
        };
        let pid = str_or_empty(map.get("id")).trim().to_owned();
        let proto = effective_provider_protocol(map);
        if !pid.is_empty() && !proto.is_empty() {
            protocols.insert(pid, Value::String(proto));
        }
    }
    (catalog_from_providers(&providers), protocols)
}

/// 供应商的**有效上游协议**(协议家族 + 线格式合并)。家族缺省 openai;
/// 家族为 openai 时按 `type` 细分 Responses / Chat Completions,其余家族
/// 原样返回(anthropic/gemini 的 `type` 与家族一致,不参与细分)。
fn effective_provider_protocol(map: &Map<String, Value>) -> String {
    let family = str_or_empty(map.get("protocol")).trim().to_lowercase();
    let style = str_or_empty(map.get("type")).trim().to_lowercase();
    match style.as_str() {
        "openai_responses" => "openai_responses".to_owned(),
        "openai_chat_completions" => "openai".to_owned(),
        _ => {
            if family.is_empty() {
                "openai".to_owned()
            } else {
                family
            }
        }
    }
}

/// 单行插入(主行 + 可选侧表);trace 用调用方复用的 prepared statement
/// (20261016_01 B-6:批量路径 200 条 trace 不再逐条重新 prepare)。
/// `attempt_rows`:已剥离的 attempt 采集行(批5),先删后插保证 REPLACE
/// 幂等 —— 新 payload 无采集时顺带清掉旧行残留。
fn insert_request_log_row(
    connection: &Connection,
    trace_stmt: &mut rusqlite::Statement<'_>,
    request_id: &str,
    text: &str,
    bodies: Option<&LogBodies>,
    now: &str,
    attempt_rows: &[(i64, String)],
) -> Result<(), PortError> {
    connection
        .execute(
            "INSERT OR REPLACE INTO request_logs(request_id, created_at, payload) \
             VALUES(?1, ?2, ?3)",
            params![request_id, now, text],
        )
        .context("insert request_logs")?;
    connection
        .execute(
            "DELETE FROM request_log_attempts WHERE request_id=?1",
            params![request_id],
        )
        .context("clear stale request_log_attempts")?;
    for (ordinal, payload_text) in attempt_rows {
        connection
            .execute(
                "INSERT INTO request_log_attempts(request_id, ordinal, created_at, payload) \
                 VALUES(?1, ?2, ?3, ?4)",
                params![request_id, ordinal, now, payload_text],
            )
            .context("insert request_log_attempts")?;
    }
    match bodies {
        Some(bodies) => {
            // 列绑定口径与 Python 一致:`bodies.get(key)` 原样入参,
            // truncated 落 1/0,响应头存在才序列化成文本。
            let request_body = bodies.request_body.as_deref();
            let request_body_size = bodies.request_body_size;
            let request_body_truncated = i64::from(bodies.request_body_truncated);
            let response_body = bodies.response_body.as_deref();
            let response_body_size = bodies.response_body_size;
            let response_body_truncated = i64::from(bodies.response_body_truncated);
            let response_headers = bodies.response_headers.as_ref().map(|map| {
                wr_domain::json_py::py_json_dumps(&Value::Object(map.clone()))
            });
            let request_headers = bodies.request_headers.as_ref().map(|map| {
                wr_domain::json_py::py_json_dumps(&Value::Object(map.clone()))
            });
            connection
                .execute(
                    "INSERT OR REPLACE INTO request_log_bodies(request_id, \
                     created_at, request_body, request_body_size, \
                     request_body_truncated, response_body, response_body_size, \
                     response_body_truncated, response_headers) \
                     VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![request_id, now, request_body, request_body_size,
                        request_body_truncated, response_body, response_body_size,
                        response_body_truncated, response_headers],
                )
                .context("insert request_log_bodies")?;
            if let Some(headers) = request_headers {
                connection.execute(
                    "INSERT OR REPLACE INTO request_log_headers(request_id, created_at, request_headers) VALUES(?1, ?2, ?3)",
                    params![request_id, now, headers],
                ).context("insert request_log_headers")?;
            } else {
                connection.execute("DELETE FROM request_log_headers WHERE request_id=?1", params![request_id]).context("clear request headers")?;
            }
            connection.execute("DELETE FROM request_log_trace WHERE request_id=?1", params![request_id]).context("clear old trace")?;
            for (ordinal, item) in bodies.trace_items.iter().enumerate() {
                let kind = item.get("kind").and_then(Value::as_str).unwrap_or("event");
                trace_stmt
                    .execute(params![
                        request_id,
                        ordinal as i64,
                        now,
                        kind,
                        wr_domain::json_py::py_json_dumps(item)
                    ])
                    .context("insert request_log_trace")?;
            }
        }
        None => {
            connection
                .execute(
                    "DELETE FROM request_log_bodies WHERE request_id=?1",
                    params![request_id],
                )
                .context("clear stale bodies")?;
            connection.execute("DELETE FROM request_log_headers WHERE request_id=?1", params![request_id]).context("clear stale headers")?;
            connection.execute("DELETE FROM request_log_trace WHERE request_id=?1", params![request_id]).context("clear stale trace")?;
        }
    }
    Ok(())
}

const INSERT_TRACE_SQL: &str =
    "INSERT INTO request_log_trace(request_id, ordinal, created_at, kind, payload) \
     VALUES(?1, ?2, ?3, ?4, ?5)";

// ---------------------------------------------------------------------------
// 用量小时卷积(20261016_01 C-6):随日志落库增量 UPSERT,Dashboard 读它
// 替代每次现场全量聚合;口径与读取侧共用(usage_sums_from_item)。
// ---------------------------------------------------------------------------

/// 一行日志对卷积的增量(维度 + 计数;`bucket_epoch` 落库时按行时间算)。
pub(crate) struct RollupDelta {
    dims: RowDims,
    sums: UsageSums,
    /// 阶段四 C-8:该行的直方图样本(TTFT / 吞吐分桶;与 `sums` 同口径)。
    hist: HistogramSamples,
}

/// 从落库 payload 抽卷积增量(无效/空 payload → None,该行不进卷积,
/// 覆盖判定按「无行」处理 —— 正常路径 created_at/payload 恒有效)。
pub(crate) fn rollup_delta_for_payload(item: &Map<String, Value>) -> Option<RollupDelta> {
    if item.is_empty() {
        return None;
    }
    let is_error = is_error_item(item);
    Some(RollupDelta {
        dims: dims_from_item(item),
        sums: usage_sums_from_item(item, is_error),
        hist: histogram_samples_for_item(item, is_error),
    })
}

/// `created_at`(两种历史形态都兼容)→ 小时桶 epoch。
fn bucket_epoch_of(created_text: &str) -> Option<i64> {
    let moment = parse_iso(created_text)?;
    let epoch = moment.timestamp();
    Some(epoch - epoch.rem_euclid(3600))
}

/// 20261108_01 §13:派生表重建 —— 从 `request_logs` 原始行重放。
///
/// 只在 schema 升级(旧列名)时调用一次;调用方负责事务与建表。
/// 无法解析的历史行跳过(与读取侧 raw-fallback 的容错口径一致)。
pub(crate) fn rebuild_usage_derived(connection: &Connection) -> Result<(), PortError> {
    let mut statement = connection
        .prepare(
            "SELECT created_at, payload FROM request_logs \
             ORDER BY created_at ASC, rowid ASC",
        )
        .context("prepare usage rollup rebuild")?;
    let mut rows = statement.query([]).context("query usage rollup rebuild")?;
    while let Some(row) = rows.next().context("row usage rollup rebuild")? {
        let created: String = row.get(0).context("read rebuild created_at")?;
        let text: String = row.get(1).context("read rebuild payload")?;
        let Some(bucket) = bucket_epoch_of(&created) else {
            continue;
        };
        let Ok(Value::Object(item)) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let Some(delta) = rollup_delta_for_payload(&item) else {
            continue;
        };
        upsert_usage_rollup(connection, bucket, &delta.dims, 1, &delta.sums)?;
        upsert_usage_rollup_hist(connection, bucket, &delta.dims, &delta.hist, 1)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn upsert_usage_rollup(
    connection: &Connection,
    bucket_epoch: i64,
    dims: &RowDims,
    request_count_delta: i64,
    sums: &UsageSums,
) -> Result<(), PortError> {
    let decode_source = sums.decode_source.map(DecodeSource::as_str).unwrap_or("");
    connection
        .execute(
            "INSERT INTO usage_rollup(bucket_epoch, model, upstream_model, requested_model, \
             provider, client, api_key_id, api_key_name, request_count, error_count, \
             input_tokens, output_tokens, total_tokens, cache_tokens, cost_usd, \
             duration_ms_sum, first_byte_ms_sum, first_byte_count, decode_ms_sum, \
             decode_tokens, decode_source) \
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, \
             ?17, ?18, ?19, ?20, ?21) \
             ON CONFLICT(bucket_epoch, model, upstream_model, requested_model, provider, \
             client, api_key_id, api_key_name, decode_source) DO UPDATE SET \
             request_count = request_count + excluded.request_count, \
             error_count = error_count + excluded.error_count, \
             input_tokens = input_tokens + excluded.input_tokens, \
             output_tokens = output_tokens + excluded.output_tokens, \
             total_tokens = total_tokens + excluded.total_tokens, \
             cache_tokens = cache_tokens + excluded.cache_tokens, \
             cost_usd = cost_usd + excluded.cost_usd, \
             duration_ms_sum = duration_ms_sum + excluded.duration_ms_sum, \
             first_byte_ms_sum = first_byte_ms_sum + excluded.first_byte_ms_sum, \
             first_byte_count = first_byte_count + excluded.first_byte_count, \
             decode_ms_sum = decode_ms_sum + excluded.decode_ms_sum, \
             decode_tokens = decode_tokens + excluded.decode_tokens",
            params![
                bucket_epoch,
                dims.model,
                if dims.accounting_model == dims.model {
                    ""
                } else {
                    dims.accounting_model.as_str()
                },
                dims.requested_model,
                dims.provider,
                dims.client,
                dims.api_key_id,
                dims.api_key_name,
                request_count_delta,
                sums.error_count,
                sums.input_tokens,
                sums.output_tokens,
                sums.total_tokens,
                sums.cache_tokens,
                sums.cost_usd,
                sums.duration_ms_sum,
                sums.first_byte_ms_sum,
                sums.first_byte_count,
                sums.decode_ms_sum,
                sums.decode_tokens,
                decode_source,
            ],
        )
        .context("upsert usage_rollup")?;
    Ok(())
}

/// 阶段四 C-8:直方图行 UPSERT(写入侧分桶累加)。
///
/// `weight` = +1 增量 / -1 扣回(REPLACE 同 id 时把旧行的贡献扣回来)。
/// 计数下限夹到 0:旧行(本特性之前写的)在表里没有对应计数,扣回时
/// 不能把别的行的计数扣成负数。
fn upsert_usage_rollup_hist(
    connection: &Connection,
    bucket_epoch: i64,
    dims: &RowDims,
    samples: &HistogramSamples,
    weight: i64,
) -> Result<(), PortError> {
    for sample in samples {
        connection
            .execute(
                "INSERT INTO usage_rollup_hist(bucket_epoch, model, upstream_model, \
                 requested_model, provider, client, api_key_id, api_key_name, metric, bucket, \
                 sample_count, decode_source) \
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12) \
                 ON CONFLICT(bucket_epoch, model, upstream_model, requested_model, provider, \
                 client, api_key_id, api_key_name, metric, bucket, decode_source) DO UPDATE SET \
                 sample_count = MAX(0, sample_count + excluded.sample_count)",
                params![
                    bucket_epoch,
                    dims.model,
                    if dims.accounting_model == dims.model {
                        ""
                    } else {
                        dims.accounting_model.as_str()
                    },
                    dims.requested_model,
                    dims.provider,
                    dims.client,
                    dims.api_key_id,
                    dims.api_key_name,
                    sample.metric,
                    sample.bucket,
                    weight,
                    sample.source,
                ],
            )
            .context("upsert usage_rollup_hist")?;
    }
    Ok(())
}

#[async_trait]
impl RequestLogStore for SqliteStore {
    async fn append_request_log(
        &self,
        request_id: &str,
        payload: &Value,
        bodies: Option<&LogBodies>,
    ) -> Result<(), PortError> {
        self.append_request_log_batch(&[PendingRequestLog {
            request_id: request_id.to_owned(),
            payload: payload.clone(),
            bodies: bodies.cloned(),
        }])
        .await
    }

    async fn append_request_log_batch(&self, batch: &[PendingRequestLog]) -> Result<(), PortError> {
        if batch.is_empty() {
            return Ok(());
        }
        // payload 文本在闭包外序列化(纯 CPU,不占写线程);与 Python 同形:
        // 默认分隔符 + 非 ASCII 原样 + 插入序。卷积增量一并在此抽取。
        // 20261017_02 批5:同处把 retryAttempts 的采集四件套剥离到
        // request_log_attempts 侧表 —— 主行只留轻量元数据 + captured 标记。
        let rows: Vec<
            (
                String,
                String,
                Option<LogBodies>,
                Option<RollupDelta>,
                Vec<(i64, String)>,
            ),
        > = batch
            .iter()
            .map(|row| {
                let delta = row
                    .payload
                    .as_object()
                    .and_then(rollup_delta_for_payload);
                let (text, attempt_rows) = split_attempt_captures(&row.payload);
                (
                    row.request_id.clone(),
                    text,
                    row.bodies.clone(),
                    delta,
                    attempt_rows,
                )
            })
            .collect();
        self.engine
            .write(move |connection| {
                // 批内共用一个时间戳:created_at 秒粒度,同批无感知差异;
                // 一次 SELECT 替代每行一次。
                let now = db_now(connection)?;
                let now_bucket = bucket_epoch_of(&now);
                let mut trace_stmt = connection
                    .prepare(INSERT_TRACE_SQL)
                    .context("prepare request_log_trace insert")?;
                for (request_id, text, bodies, delta, attempt_rows) in &rows {
                    // C-6:INSERT OR REPLACE 命中同 id 旧行时,先把旧行的
                    // 卷积增量按其原时间桶扣回,再落新行 —— REPLACE 不重复计。
                    let existing: Option<(String, String)> = connection
                        .query_row(
                            "SELECT created_at, payload FROM request_logs WHERE request_id=?1",
                            params![request_id],
                            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                        )
                        .optional()
                        .context("probe existing request log")?;
                    insert_request_log_row(
                        connection,
                        &mut trace_stmt,
                        request_id,
                        text,
                        bodies.as_ref(),
                        &now,
                        attempt_rows,
                    )?;
                    if let Some((old_created, old_text)) = existing {
                        let old_delta = serde_json::from_str::<Value>(&old_text)
                            .ok()
                            .and_then(|value| value.as_object().cloned())
                            .and_then(|item| rollup_delta_for_payload(&item));
                        if let (Some(bucket), Some(old_delta)) =
                            (bucket_epoch_of(&old_created), old_delta)
                        {
                            upsert_usage_rollup(
                                connection,
                                bucket,
                                &old_delta.dims,
                                -1,
                                &old_delta.sums.negated(),
                            )?;
                            // 阶段四 C-8:直方图同源扣回(否则同一 id 改写
                            // 会把旧样本算两遍)。
                            upsert_usage_rollup_hist(
                                connection,
                                bucket,
                                &old_delta.dims,
                                &old_delta.hist,
                                -1,
                            )?;
                        }
                    }
                    if let (Some(bucket), Some(delta)) = (now_bucket, delta) {
                        upsert_usage_rollup(connection, bucket, &delta.dims, 1, &delta.sums)?;
                        upsert_usage_rollup_hist(connection, bucket, &delta.dims, &delta.hist, 1)?;
                    }
                }
                Ok(())
            })
            .await
    }

    async fn list_request_logs(&self, limit: i64) -> Result<Vec<Value>, PortError> {
        let limit = limit.clamp(1, 1000);
        self.engine
            .read(move |connection| {
                let mut statement = connection
                    .prepare(
                        "SELECT rowid AS id, request_id, created_at, payload FROM request_logs \
                         ORDER BY created_at DESC LIMIT ?1",
                    )
                    .context("prepare list_request_logs")?;
                let rows = statement
                    .query_map(params![limit], |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                        ))
                    })
                    .context("query list_request_logs")?;
                let mut items = Vec::new();
                for row in rows {
                    let (id, request_id, created_at, payload_text) =
                        row.context("row list_request_logs")?;
                    let mut map = Map::new();
                    map.insert("id".to_owned(), Value::from(id));
                    map.insert("request_id".to_owned(), Value::String(request_id));
                    map.insert("created_at".to_owned(), Value::String(created_at));
                    if let Ok(Value::Object(payload)) = serde_json::from_str::<Value>(&payload_text)
                    {
                        for (key, value) in payload {
                            if RETIRED_INLINE_BODY_KEYS.contains(&key.as_str()) {
                                continue;
                            }
                            map.insert(key, value);
                        }
                    }
                    items.push(Value::Object(map));
                }
                Ok(items)
            })
            .await
    }

    async fn get_request_log(&self, request_id: &str) -> Result<Option<Value>, PortError> {
        let request_id = request_id.to_owned();
        self.engine
            .read(move |connection| {
                let row = connection
                    .query_row(
                        "SELECT r.payload, b.request_body, b.response_body, b.response_headers, \
                         h.request_headers, b.request_body_size, b.request_body_truncated, \
                         b.response_body_size, b.response_body_truncated \
                         FROM request_logs r LEFT JOIN request_log_bodies b ON b.request_id = r.request_id \
                         LEFT JOIN request_log_headers h ON h.request_id = r.request_id WHERE r.request_id=?1",
                        params![request_id],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, Option<String>>(1)?,
                                row.get::<_, Option<String>>(2)?,
                                row.get::<_, Option<String>>(3)?,
                                row.get::<_, Option<String>>(4)?,
                                row.get::<_, Option<i64>>(5)?,
                                row.get::<_, Option<i64>>(6)?,
                                row.get::<_, Option<i64>>(7)?,
                                row.get::<_, Option<i64>>(8)?,
                            ))
                        },
                    )
                    .optional()
                    .context("read request log")?;
                let Some((payload, request_body, response_body, headers, request_headers, request_size, request_truncated, response_size, response_truncated)) = row else {
                    return Ok(None);
                };
                let mut map = Map::new();
                map.insert("request_id".to_owned(), Value::String(request_id.clone()));
                // 20261017_02 批5:attempt 采集按需并回(瘦身主行 → 侧表;
                // 旧格式内嵌行原样保留)。详情页尝试表吃列表条目的
                // retryAttempts(已瘦身)—— 这里把并回后的形态投影成同形
                // `retryAttempts` 附在详情响应上,前端按需覆盖。
                let attempt_rows = read_attempt_captures(connection, &request_id)?;
                let mut merged = with_split_bodies(
                    Some(&payload), request_body.as_deref(), response_body.as_deref(),
                    headers.as_deref(), request_headers.as_deref(), request_size, request_truncated,
                    response_size, response_truncated, map,
                );
                with_split_attempts(&mut merged, &attempt_rows);
                if let Some(merged_map) = merged.as_object_mut() {
                    let attempts = project_retry_attempts(merged_map);
                    merged_map.insert("retryAttempts".to_owned(), Value::Array(attempts));
                }
                Ok(Some(merged))
            })
            .await
    }

    async fn get_request_log_by_rowid(&self, rowid: i64) -> Result<Option<Value>, PortError> {
        self.engine
            .read(move |connection| {
                let row = connection
                    .query_row(
                        "SELECT r.rowid AS id, r.request_id, r.created_at, r.payload, \
                         b.request_body, b.response_body, b.response_headers, h.request_headers, \
                         b.request_body_size, b.request_body_truncated, b.response_body_size, b.response_body_truncated \
                         FROM request_logs r LEFT JOIN request_log_bodies b ON b.request_id = r.request_id \
                         LEFT JOIN request_log_headers h ON h.request_id = r.request_id WHERE r.rowid=?1",
                        params![rowid],
                        |row| {
                            Ok((
                                row.get::<_, i64>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, String>(2)?,
                                row.get::<_, String>(3)?,
                                row.get::<_, Option<String>>(4)?,
                                row.get::<_, Option<String>>(5)?,
                                row.get::<_, Option<String>>(6)?,
                                row.get::<_, Option<String>>(7)?,
                                row.get::<_, Option<i64>>(8)?,
                                row.get::<_, Option<i64>>(9)?,
                                row.get::<_, Option<i64>>(10)?,
                                row.get::<_, Option<i64>>(11)?,
                            ))
                        },
                    )
                    .optional()
                    .context("read request log by rowid")?;
                let Some((
                    id,
                    request_id,
                    created_at,
                    payload,
                    request_body,
                    response_body,
                    headers,
                    request_headers,
                    request_size,
                    request_truncated,
                    response_size,
                    response_truncated,
                )) = row
                else {
                    return Ok(None);
                };
                let mut map = Map::new();
                map.insert("id".to_owned(), Value::from(id));
                map.insert("request_id".to_owned(), Value::String(request_id.clone()));
                map.insert("created_at".to_owned(), Value::String(created_at));
                // 20261017_02 批5:attempt 采集按需并回(同 get_request_log,
                // 附投影后的 retryAttempts 供详情页尝试表覆盖列表瘦身影)。
                let attempt_rows = read_attempt_captures(connection, &request_id)?;
                let mut merged = with_split_bodies(
                    Some(&payload),
                    request_body.as_deref(),
                    response_body.as_deref(),
                    headers.as_deref(),
                    request_headers.as_deref(),
                    request_size,
                    request_truncated,
                    response_size,
                    response_truncated,
                    map,
                );
                with_split_attempts(&mut merged, &attempt_rows);
                if let Some(merged_map) = merged.as_object_mut() {
                    let attempts = project_retry_attempts(merged_map);
                    merged_map.insert("retryAttempts".to_owned(), Value::Array(attempts));
                }
                Ok(Some(merged))
            })
            .await
    }

    /// 调用日志分页查询(镜像 `query_request_logs`)。
    ///
    /// 与 `usage_summary` 不同,这里检视**全部**日志行(含被拒请求),
    /// 调用日志页因此同时充当审计面。最新 2000 行先全量投影再在内存里
    /// 过滤(payload 是 JSON 大对象,SQL 侧筛不动它的字段)。
    ///
    /// `in_flight`:调用方(网关在途注册表)传入的「等待中/处理中」内存行,
    /// 已是投影后形态;并到结果最前,与正式行共用过滤/计数/分页。
    async fn get_request_log_trace(
        &self,
        request_id: &str,
        offset: i64,
        limit: i64,
        kind: Option<&str>,
    ) -> Result<wr_app::ports::RequestLogTracePage, PortError> {
        let request_id = request_id.to_owned();
        let kind = kind.map(str::to_owned).filter(|kind| !kind.is_empty());
        let offset = offset.max(0);
        let limit = limit.clamp(1, 200);
        self.engine.read(move |connection| {
            // D4:按 kind 过滤 —— attempt 摘要与 SSE chunk 是两类数据,
            // 分开分页可以各自独立翻页,不必把 200 条 chunk 挤在 attempts 前面。
            let (items, total) = match &kind {
                Some(kind) => {
                    let total: i64 = connection
                        .query_row(
                            "SELECT COUNT(*) FROM request_log_trace WHERE request_id=?1 AND kind=?2",
                            params![request_id, kind],
                            |row| row.get(0),
                        )
                        .context("count request log trace")?;
                    let mut statement = connection
                        .prepare(
                            "SELECT payload FROM request_log_trace WHERE request_id=?1 AND kind=?2 ORDER BY ordinal LIMIT ?3 OFFSET ?4",
                        )
                        .context("prepare request log trace")?;
                    let rows = statement
                        .query_map(params![request_id, kind, limit, offset], |row| {
                            row.get::<_, String>(0)
                        })
                        .context("query request log trace")?;
                    (
                        rows.filter_map(Result::ok)
                            .filter_map(|text| serde_json::from_str(&text).ok())
                            .collect::<Vec<Value>>(),
                        total,
                    )
                }
                None => {
                    let total: i64 = connection
                        .query_row(
                            "SELECT COUNT(*) FROM request_log_trace WHERE request_id=?1",
                            params![request_id],
                            |row| row.get(0),
                        )
                        .context("count request log trace")?;
                    let mut statement = connection
                        .prepare(
                            "SELECT payload FROM request_log_trace WHERE request_id=?1 ORDER BY ordinal LIMIT ?2 OFFSET ?3",
                        )
                        .context("prepare request log trace")?;
                    let rows = statement
                        .query_map(params![request_id, limit, offset], |row| {
                            row.get::<_, String>(0)
                        })
                        .context("query request log trace")?;
                    (
                        rows.filter_map(Result::ok)
                            .filter_map(|text| serde_json::from_str(&text).ok())
                            .collect::<Vec<Value>>(),
                        total,
                    )
                }
            };
            let next_offset =
                (offset + (items.len() as i64) < total).then_some(offset + items.len() as i64);
            Ok(wr_app::ports::RequestLogTracePage { items, next_offset, total })
        }).await
    }

    async fn query_request_logs(
        &self,
        filter: &Value,
        in_flight: &[Value],
    ) -> Result<Value, PortError> {
        let map = filter.as_object().cloned().unwrap_or_default();
        let search = str_or_empty(map.get("query")).trim().to_lowercase();
        let status_filter = {
            let raw = str_or_empty(map.get("status")).trim().to_owned();
            if raw.is_empty() {
                "all".to_owned()
            } else {
                raw
            }
        };
        let model_filter = str_or_empty(map.get("model")).trim().to_owned();
        let credential_filter = str_or_empty(map.get("credential")).trim().to_owned();
        let mut page = positive_int(map.get("page"), 1, None);
        let page_size = positive_int(map.get("pageSize"), 25, Some(CALL_LOG_MAX_PAGE_SIZE));

        // C-5:投影缓存(version = MAX(rowid),插入/REPLACE 都会推进;
        // TTL + prune/迁移显式失效兜底)。命中时不再重读与全量解析
        // 最近 2000 行,provider 目录随构建一并缓存(C-1)。
        let version: i64 = self
            .engine
            .read(|connection| {
                connection
                    .query_row(
                        "SELECT COALESCE(MAX(rowid), 0) FROM request_logs",
                        [],
                        |row| row.get(0),
                    )
                    .context("probe request_logs version")
            })
            .await?;
        let projection = self.call_log_projection(version).await?;

        // 在途行(等待中/处理中)排最前 —— 它们比任何已落库行都新;
        // 已投影行按引用链在后面,不再逐查询克隆。
        let in_flight_rows: Vec<Value> = in_flight.to_vec();

        let passes_base = |entry: &Value| -> bool {
            if !search.is_empty() {
                let haystack: String = [
                    "requestId",
                    "modelLabel",
                    "baseModel",
                    "apiKeyName",
                    // 20261104_01 阶段三 · B-1a:调用方标识入搜索串(能按 UA
                    // 片段检索到「谁在用我的网关」)。
                    "client",
                ]
                .iter()
                .filter_map(|key| entry.get(key).and_then(Value::as_str))
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase();
                if !haystack.contains(&search) {
                    return false;
                }
            }
            // 20260905_04:model 筛选同时匹配 modelLabel(聚合名维度)与
            // baseModel(实际模型维度),聚合行可按任一维度过滤。
            if !model_filter.is_empty()
                && entry.get("modelLabel").and_then(Value::as_str) != Some(model_filter.as_str())
                && entry.get("baseModel").and_then(Value::as_str) != Some(model_filter.as_str())
            {
                return false;
            }
            if !credential_filter.is_empty() {
                let id = entry
                    .get("apiKeyId")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if id != credential_filter {
                    return false;
                }
            }
            true
        };
        let base: Vec<&Value> = in_flight_rows
            .iter()
            .chain(projection.entries.iter())
            .filter(|entry| passes_base(entry))
            .collect();
        let counted = |predicate: fn(&Value) -> bool| -> i64 {
            base.iter().filter(|entry| predicate(entry)).count() as i64
        };
        // statusCounts 在 status 过滤之前统计(chip 要能显示「点它出多少行」)。
        // 五档状态:pending/processing 来自在途注册表,completed/failed/
        // canceled 来自落库行(投影时按 ok + end_reason 推导)。
        let status_counts = object(vec![
            ("all", Value::from(base.len() as i64)),
            (
                "pending",
                Value::from(counted(|entry| entry_status(entry) == "pending")),
            ),
            (
                "processing",
                Value::from(counted(|entry| entry_status(entry) == "processing")),
            ),
            (
                "completed",
                Value::from(counted(|entry| entry_status(entry) == "completed")),
            ),
            (
                "failed",
                Value::from(counted(|entry| entry_status(entry) == "failed")),
            ),
            (
                "canceled",
                Value::from(counted(|entry| entry_status(entry) == "canceled")),
            ),
        ]);
        let selected: Vec<&Value> = match status_filter.as_str() {
            "pending" | "processing" | "completed" | "failed" | "canceled" => base
                .iter()
                .copied()
                .filter(|entry| entry_status(entry) == status_filter.as_str())
                .collect(),
            _ => base.clone(),
        };
        let total = selected.len() as i64;
        let total_pages = total.div_euclid(page_size) + i64::from(total.rem_euclid(page_size) != 0);
        let total_pages = total_pages.max(1);
        page = page.min(total_pages);
        let items: Vec<Value> = selected
            .into_iter()
            .skip(((page - 1) * page_size).max(0) as usize)
            .take(page_size as usize)
            .cloned()
            .collect();

        Ok(object(vec![
            ("generatedAt", Value::String(utc_second_text())),
            ("items", Value::Array(items)),
            (
                "options",
                object(vec![
                    (
                        "models",
                        Value::Array(
                            projection
                                .model_labels
                                .iter()
                                .map(|label| Value::String(label.clone()))
                                .collect(),
                        ),
                    ),
                    (
                        "credentials",
                        Value::Array(
                            projection
                                .credentials
                                .iter()
                                .map(|(id, name)| {
                                    object(vec![
                                        ("id", Value::String(id.clone())),
                                        ("name", Value::String(name.clone())),
                                    ])
                                })
                                .collect(),
                        ),
                    ),
                ]),
            ),
            ("statusCounts", status_counts),
            ("page", Value::from(page)),
            ("pageSize", Value::from(page_size)),
            ("total", Value::from(total)),
            ("totalPages", Value::from(total_pages)),
            ("windowSize", Value::from(CALL_LOG_WINDOW)),
            ("totalRows", Value::from(projection.total_rows)),
        ]))
    }

    /// 分档过期。`*_retention_days = 0` 表示该档永久保留。
    ///
    /// `bytesDeleted` 实际统计的是**字符数**(SQL `length()` 按字符计),
    /// Python 侧字段名沿用 `bytes`,这里保持同名以不破坏维护服务的阈值判断。
    async fn prune_logs(
        &self,
        body_retention_days: i64,
        log_retention_days: i64,
    ) -> Result<PruneStats, PortError> {
        let now = chrono::Utc::now();
        let body_cutoff = (body_retention_days > 0).then(|| {
            (now - chrono::Duration::days(body_retention_days))
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        });
        let log_cutoff = (log_retention_days > 0).then(|| {
            (now - chrono::Duration::days(log_retention_days))
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        });
        // C-6:元数据过期时卷积桶级联清理用(整个桶过期才清,边界桶保留)。
        let log_cutoff_epoch = log_cutoff.as_deref().and_then(parse_iso).map(|m| m.timestamp());
        let stats = self
            .engine
            .write(move |connection| {
                let mut stats = PruneStats::default();
                if let Some(cutoff) = body_cutoff.as_deref() {
                    let (count, bytes): (i64, i64) = connection
                        .query_row(
                            "SELECT COUNT(*), COALESCE(SUM(COALESCE(length(request_body), 0) + \
                             COALESCE(length(response_body), 0) + \
                             COALESCE(length(response_headers), 0)), 0) \
                             FROM request_log_bodies WHERE created_at < ?1",
                            params![cutoff],
                            |row| Ok((row.get(0)?, row.get(1)?)),
                        )
                        .context("count bodies to prune")?;
                    stats.bodies_deleted = count;
                    stats.bytes_deleted = bytes;
                    connection
                        .execute(
                            "DELETE FROM request_log_bodies WHERE created_at < ?1",
                            params![cutoff],
                        )
                        .context("prune bodies")?;
                    connection.execute("DELETE FROM request_log_headers WHERE created_at < ?1", params![cutoff]).context("prune headers")?;
                    connection.execute("DELETE FROM request_log_trace WHERE created_at < ?1", params![cutoff]).context("prune trace")?;
                    connection.execute("DELETE FROM request_log_attempts WHERE created_at < ?1", params![cutoff]).context("prune attempts")?;
                }
                if let Some(cutoff) = log_cutoff.as_deref() {
                    // 元数据过期行的正文一并删除(不分档)。
                    connection
                        .execute(
                            "DELETE FROM request_log_bodies WHERE request_id IN \
                             (SELECT request_id FROM request_logs WHERE created_at < ?1)",
                            params![cutoff],
                        )
                        .context("prune bodies of expired logs")?;
                    connection.execute("DELETE FROM request_log_headers WHERE request_id IN (SELECT request_id FROM request_logs WHERE created_at < ?1)", params![cutoff]).context("prune headers of expired logs")?;
                    connection.execute("DELETE FROM request_log_trace WHERE request_id IN (SELECT request_id FROM request_logs WHERE created_at < ?1)", params![cutoff]).context("prune trace of expired logs")?;
                    connection.execute("DELETE FROM request_log_attempts WHERE request_id IN (SELECT request_id FROM request_logs WHERE created_at < ?1)", params![cutoff]).context("prune attempts of expired logs")?;
                    let deleted = connection
                        .execute(
                            "DELETE FROM request_logs WHERE created_at < ?1",
                            params![cutoff],
                        )
                        .context("prune logs")?;
                    stats.logs_deleted = deleted as i64;
                    // C-6:卷积随元数据保留窗级联(只清整个桶都已过期的;
                    // 边界桶的卷积行无行级时间,保留 —— 与「现场聚合随日志
                    // 删除而缩小」相比宁可略多、不少计)。
                    if let Some(epoch) = log_cutoff_epoch {
                        connection
                            .execute(
                                "DELETE FROM usage_rollup WHERE bucket_epoch + 3600 <= ?1",
                                params![epoch],
                            )
                            .context("prune usage rollup")?;
                        // 阶段四 C-8:直方图随卷积同一保留窗清理(否则
                        // 样本计数会与卷积行脱节,分位数越算越偏)。
                        connection
                            .execute(
                                "DELETE FROM usage_rollup_hist WHERE bucket_epoch + 3600 <= ?1",
                                params![epoch],
                            )
                            .context("prune usage rollup hist")?;
                    }
                }
                Ok(stats)
            })
            .await?;
        // 清理动了历史行 → 投影缓存整体失效(20261016_01 C-5;MAX(rowid)
        // 对删除不敏感,TTL 之外在此显式失效)。
        self.invalidate_call_log_cache();
        Ok(stats)
    }

    /// 归还空闲页。两条语句都不能开在事务里 → 走 autocommit 通道。
    async fn compact(&self) -> Result<(), PortError> {
        self.engine
            .write_autocommit(|connection| {
                connection
                    .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
                    .context("wal_checkpoint")?;
                connection.execute_batch("VACUUM").context("VACUUM")?;
                Ok(())
            })
            .await
    }
}

fn truthy(value: Option<&Value>) -> bool {
    value.is_some_and(py_bool)
}

// ---------------------------------------------------------------------------
// 调用日志投影(镜像 Python 的模块级 _PROTOCOL_LABELS / _PROTOCOL_ENDPOINTS /
// _protocol_label / _endpoint_path / _conversion_chain / _call_log_entry)
// ---------------------------------------------------------------------------

/// 协议在转换链里的展示名。
///
/// 转换链写协议**全称**(`OpenAI Compatible` / `OpenAI Responses` /
/// `OpenAI Chat`),与 `Anthropic`/`Gemini` 同粒度;不再用缩写形式
/// (2026-10-13)。
const PROTOCOL_LABELS: &[(&str, &str)] = &[
    ("openai", "OpenAI Compatible"),
    ("openai.chat.completions", "OpenAI Compatible"),
    // 前端 `type` 细粒度值(有效上游协议合并时可能透传)。
    ("openai_chat_completions", "OpenAI Compatible"),
    ("openai_responses", "OpenAI Responses"),
    ("openai.responses", "OpenAI Responses"),
    ("openai_embeddings", "OpenAI Embeddings"),
    ("openai.embeddings", "OpenAI Embeddings"),
    ("anthropic", "Anthropic"),
    ("anthropic.messages", "Anthropic"),
    ("anthropic_messages", "Anthropic"),
    ("gemini", "Gemini"),
    ("gemini_stream", "Gemini"),
    ("gemini.generateContent", "Gemini"),
    ("gemini.streamGenerateContent", "Gemini"),
];

/// 各协议面向客户端的网关端点(payload 的 `path` 历史上存的是协议名)。
const PROTOCOL_ENDPOINTS: &[(&str, &str)] = &[
    ("openai", "/v1/chat/completions"),
    ("openai.chat.completions", "/v1/chat/completions"),
    ("anthropic", "/v1/messages"),
    ("anthropic.messages", "/v1/messages"),
    ("openai_responses", "/v1/responses"),
    ("openai.responses", "/v1/responses"),
    ("openai_embeddings", "/v1/embeddings"),
    ("openai.embeddings", "/v1/embeddings"),
    ("gemini", "/v1beta/models/{model}:generateContent"),
    (
        "gemini.generateContent",
        "/v1beta/models/{model}:generateContent",
    ),
    (
        "gemini_stream",
        "/v1beta/models/{model}:streamGenerateContent",
    ),
    (
        "gemini.streamGenerateContent",
        "/v1beta/models/{model}:streamGenerateContent",
    ),
    ("gemini_interactions", "/v1/interactions"),
];

fn protocol_label(protocol: &str) -> String {
    let lowered = protocol.trim().to_lowercase();
    if let Some((_, label)) = PROTOCOL_LABELS.iter().find(|(name, _)| *name == lowered) {
        return (*label).to_owned();
    }
    let stripped = protocol.trim().to_owned();
    if stripped.is_empty() {
        protocol.to_owned()
    } else {
        stripped
    }
}

/// 真实 `/` 开头的 path 优先于「按协议名映射」的历史形态。
fn endpoint_path(protocol: Option<&Value>, path: Option<&Value>) -> String {
    if let Some(Value::String(text)) = path
        && text.starts_with('/')
    {
        return text.clone();
    }
    let lowered = str_or_empty(protocol).trim().to_lowercase();
    match PROTOCOL_ENDPOINTS.iter().find(|(name, _)| *name == lowered) {
        Some((_, endpoint)) => (*endpoint).to_owned(),
        None => lowered,
    }
}

/// `OpenAI Compatible → OpenAI Responses` 形态;上游同协议时只给客户端标签。
/// 上游那一腿取**请求时刻落库的线格式**(payload 键 `provider_protocol`,
/// 网关写入侧注入);旧行无此键才回退按当前配置现算 —— 用户改供应商
/// type 不应改写历史行的链路(2026-10-14)。只有从未到达 provider 的行
/// (被拒请求,无存量键)才回落到首个 attempt 的协议。
fn conversion_chain(
    client_protocol: Option<&Value>,
    provider: Option<&Value>,
    attempts: Option<&Value>,
    stored_protocol: Option<&Value>,
    provider_protocols: &Map<String, Value>,
) -> Option<String> {
    let client_label = protocol_label(&str_or_empty(client_protocol));
    if client_label.is_empty() {
        return None;
    }
    let mut upstream_label = protocol_label(str_or_empty(stored_protocol).trim());
    if upstream_label.is_empty() {
        let provider_id = str_or_empty(provider).trim().to_owned();
        if !provider_id.is_empty() {
            upstream_label = protocol_label(
                provider_protocols
                    .get(&provider_id)
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            );
        }
    }
    if upstream_label.is_empty()
        && let Some(Value::Array(items)) = attempts
    {
        for attempt in items {
            let name = py_text(attempt);
            if let Some(protocol) = provider_protocols.get(&name).and_then(Value::as_str) {
                let label = protocol_label(protocol);
                if !label.is_empty() {
                    upstream_label = label;
                    break;
                }
            }
        }
    }
    if !upstream_label.is_empty() && upstream_label != client_label {
        return Some(format!("{client_label} → {upstream_label}"));
    }
    Some(client_label)
}

fn is_true(entry: &Value, key: &str) -> bool {
    entry.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// 行状态:在途行直接携带(pending/processing);落库行在投影时推导
/// (completed/failed/canceled)。缺失时按 ok 兜底(历史防御)。
fn entry_status(entry: &Value) -> &str {
    match entry.get("status").and_then(Value::as_str) {
        Some(status) => status,
        None => {
            if is_true(entry, "ok") {
                "completed"
            } else {
                "failed"
            }
        }
    }
}

fn py_text(value: &Value) -> String {
    if let Value::String(text) = value {
        return text.clone();
    }
    py_str(value).unwrap_or_default()
}

/// Python `_utc_now_iso()`:`strftime("%Y-%m-%dT%H:%M:%SZ", gmtime())`。
fn utc_second_text() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// 一行原始日志(rowid + request_id + 时间 + payload 文本)。
struct RawLogRow {
    id: i64,
    request_id: String,
    created_at: String,
    payload: String,
}

fn raw_log_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawLogRow> {
    Ok(RawLogRow {
        id: row.get(0)?,
        request_id: row.get(1)?,
        created_at: row.get(2)?,
        payload: row.get(3)?,
    })
}

fn query_call_log_rows(connection: &Connection) -> Result<(Vec<RawLogRow>, i64), PortError> {
    let mut statement = connection
        .prepare(
            "SELECT rowid AS id, request_id, created_at, payload FROM request_logs \
             ORDER BY created_at DESC, rowid DESC LIMIT ?1",
        )
        .context("prepare call log rows")?;
    let rows = statement
        .query_map(params![CALL_LOG_WINDOW], raw_log_row)
        .context("query call log rows")?;
    let mut loaded = Vec::new();
    for row in rows {
        loaded.push(row.context("row call log")?);
    }
    let total_rows: i64 = connection
        .query_row("SELECT COUNT(*) FROM request_logs", [], |row| row.get(0))
        .context("count request_logs")?;
    Ok((loaded, total_rows))
}

/// 增量投影的行读取(20261017_02 批1):只取 `(after, upto]` rowid 区间
/// 的新行,按 rowid 升序(插入序)。无 `COUNT` —— 增量路径 total = 旧值 + Δ。
fn query_call_log_rows_after(
    connection: &Connection,
    after_rowid: i64,
    upto_rowid: i64,
) -> Result<Vec<RawLogRow>, PortError> {
    let mut statement = connection
        .prepare(
            "SELECT rowid AS id, request_id, created_at, payload FROM request_logs \
             WHERE rowid > ?1 AND rowid <= ?2 ORDER BY rowid LIMIT ?3",
        )
        .context("prepare incremental call log rows")?;
    let rows = statement
        .query_map(params![after_rowid, upto_rowid, CALL_LOG_WINDOW], raw_log_row)
        .context("query incremental call log rows")?;
    let mut loaded = Vec::new();
    for row in rows {
        loaded.push(row.context("row incremental call log")?);
    }
    Ok(loaded)
}

/// 原始行 → 投影条目(全量与增量共用;下拉项去重后由调用方排序)。
fn project_raw_rows(
    raw_rows: &[RawLogRow],
    catalog: &ProviderCatalog,
    provider_protocols: &Map<String, Value>,
) -> (Vec<Value>, Vec<String>, Vec<(String, String)>) {
    let mut entries = Vec::with_capacity(raw_rows.len());
    let mut model_labels: Vec<String> = Vec::new();
    let mut credentials: Vec<(String, String)> = Vec::new();
    for raw in raw_rows {
        let Ok(Value::Object(payload)) = serde_json::from_str::<Value>(&raw.payload) else {
            continue;
        };
        let mut row = Map::new();
        row.insert("id".to_owned(), Value::from(raw.id));
        row.insert("request_id".to_owned(), Value::String(raw.request_id.clone()));
        row.insert("created_at".to_owned(), Value::String(raw.created_at.clone()));
        for (key, value) in payload {
            if RETIRED_INLINE_BODY_KEYS.contains(&key.as_str()) {
                continue;
            }
            row.insert(key, value);
        }
        let entry = call_log_entry(&row, catalog, provider_protocols);
        if let Some(label) = entry.get("modelLabel").and_then(Value::as_str)
            && !model_labels.iter().any(|known| known == label)
        {
            model_labels.push(label.to_owned());
        }
        if let Some(id) = entry.get("apiKeyId").and_then(Value::as_str)
            && !credentials.iter().any(|(known, _)| known == id)
        {
            let name = entry
                .get("apiKeyName")
                .and_then(Value::as_str)
                .unwrap_or(id)
                .to_owned();
            credentials.push((id.to_owned(), name));
        }
        entries.push(entry);
    }
    (entries, model_labels, credentials)
}

/// 投影条目是否按 `created_at` 非严格降序(同秒相等允许 —— 秒精度时间戳)。
fn sorted_by_created_at_desc(entries: &[Value]) -> bool {
    entries
        .iter()
        .zip(entries.iter().skip(1))
        .all(|(newer, older)| {
            match (newer.get("createdAt"), older.get("createdAt")) {
                (Some(a), Some(b)) => a.as_str() >= b.as_str(),
                _ => true,
            }
        })
}

/// 一行原始日志 → camelCase 调用日志条目(镜像 `_call_log_entry`)。
///
/// 刻意丢掉大字段(正文/预览),列表必须轻;正文走 `get_request_log`。
fn call_log_entry(
    row: &Map<String, Value>,
    catalog: &ProviderCatalog,
    provider_protocols: &Map<String, Value>,
) -> Value {
    let model = str_or_empty(row.get("model"));
    let provider = str_or_empty(row.get("provider"));
    let (label, base_model, _) = normalize_model_display(&model, &provider, catalog);
    // 20260905_04 §3.4 双归属:聚合行 payload 带 `upstream_model`(实际
    // 调用的上游模型)→ baseModel 用它;modelLabel 保持「供应商/聚合名」
    // 维度不变。非聚合行无此键,形态零变化。
    let upstream_model = str_or_empty(row.get("upstream_model"));
    let base_model = if upstream_model.is_empty() {
        base_model
    } else {
        upstream_model.to_owned()
    };
    let status = positive_int(row.get("status"), 0, None);
    let input_tokens = positive_int(row.get("input_tokens"), 0, None);
    let output_tokens = positive_int(row.get("output_tokens"), 0, None);
    let cache_read = positive_int(row.get("cache_read_tokens"), 0, None);
    // 持久化用 cache_write_tokens;历史/异构行可能带 Anthropic 原生名。
    let cache_write = positive_int(row.get("cache_write_tokens"), 0, None).max(positive_int(
        row.get("cache_creation_input_tokens"),
        0,
        None,
    ));
    let stored_total = positive_int(row.get("total_tokens"), 0, None);
    let created_text = match row.get("created_at") {
        Some(Value::String(text)) if !text.is_empty() => parse_iso(text)
            .map(|moment| moment.format("%Y-%m-%dT%H:%M:%SZ").to_string())
            .unwrap_or_else(|| text.clone()),
        _ => String::new(),
    };
    let retry_attempts = project_retry_attempts(row);
    let provider_attempts: Vec<Value> = row
        .get("provider_attempts")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| Value::String(py_text(item)))
                .collect()
        })
        .unwrap_or_default();
    // 空串按 Python 的 `str(...) or None` 落 null。
    let opt_text = |key: &str| -> Value {
        let text = str_or_empty(row.get(key));
        if text.is_empty() {
            Value::Null
        } else {
            Value::String(text)
        }
    };
    let cost = round_even(float_or(row.get("cost_usd"), 0.0), 10);
    let first_byte = positive_int(row.get("first_byte_ms"), 0, None);
    // 阶段四 C-4:耗时分解三键 + 阶段四 C-7:实测 flow。缺失/为零 → null
    // (前端据此区分「没这个量」与「量是 0」)。
    let sent_ms = float_or(row.get("sent_ms"), 0.0);
    let queued_ms = float_or(row.get("queued_ms"), 0.0);
    let retried_ms = float_or(row.get("retried_ms"), 0.0);
    let flow_ms = float_or(row.get("flow_ms"), 0.0);
    // 20261108_01 §5.2:答案段计时(新行才有;缺失 → null)。
    let answer_end_ms = float_or(row.get("answer_end_ms"), 0.0);
    let first_answer_byte_ms = float_or(row.get("first_answer_byte_ms"), 0.0);
    let answer_flow_ms = float_or(row.get("answer_flow_ms"), 0.0);
    let reasoning_decode_ms = float_or(row.get("reasoning_decode_ms"), 0.0);
    let stream_tail_ms = float_or(row.get("stream_tail_ms"), 0.0);
// 状态三档(Rust 新增):客户端断开(中途取消,
    // 无论 HTTP 码)→ canceled;成功 → completed;其余(上游错误/策略
    // 拒绝/路由失败/预检失败…)→ failed。
    let ok = status != 0 && status < 400;
    let canceled = str_or_empty(row.get("stream_end_reason")) == "client_disconnected";
    let is_error = !ok || canceled;
    let status_text = if canceled {
        "canceled"
    } else if ok {
        "completed"
    } else {
        "failed"
    };
    // 20261108_01 §13:速度双指标由后端单一事实源计算。解码速度继承
    // 全部分支与守卫;输出速度只要求成功行、输出/耗时为正。
    let decode = decode_tps_sample(row, is_error);
    let decode_tps = decode.map(|sample| round_even(sample.tokens as f64 / (sample.ms / 1000.0), 2));
    let decode_source = decode
        .map(|sample| Value::String(sample.source.as_str().to_owned()))
        .unwrap_or(Value::Null);
    let output_tps = output_tps_sample(row, is_error).map(|tps| round_even(tps, 2));
    object(vec![
        ("id", row.get("id").cloned().unwrap_or(Value::Null)),
        ("createdAt", Value::String(created_text)),
        (
            "requestId",
            Value::String(str_or_empty(row.get("request_id"))),
        ),
        ("modelLabel", Value::String(label)),
        ("baseModel", Value::String(base_model)),
        ("requestedModel", opt_text("requested_model")),
        ("provider", opt_text("provider")),
        // 20260906_01 §4.7:本次请求实际走的出口(direct / system /
        // preset:<name>@<host:port>);凭据已在写入侧掩掉,历史行为 null。
        ("proxy", opt_text("proxy")),
        ("protocol", Value::String(str_or_empty(row.get("protocol")))),
        (
            "conversionChain",
            conversion_chain(
                row.get("protocol"),
                row.get("provider"),
                row.get("provider_attempts"),
                row.get("provider_protocol"),
                provider_protocols,
            )
            .map(Value::String)
            .unwrap_or(Value::Null),
        ),
        (
            "endpointPath",
            Value::String(endpoint_path(row.get("protocol"), row.get("path"))),
        ),
        ("method", Value::String(str_or_empty(row.get("method")))),
        ("path", Value::String(str_or_empty(row.get("path")))),
        // 20261104_01 阶段三 · B-1a:调用方标识(原始 UA)。payload 里一直有
        // `client` 键(写入侧 `request_meta`),但投影此前**不带**它 → 前端
        // 契约 `RequestLogEntry.client` 恒为 undefined。空值按 Python 的
        // `str(...) or None` 落 null。
        ("client", opt_text("client")),
        ("apiKeyId", opt_text("api_key_id")),
        ("apiKeyName", opt_text("api_key_name")),
        ("statusCode", Value::from(status)),
        ("ok", Value::Bool(ok)),
        ("status", Value::String(status_text.to_owned())),
        ("isStream", Value::Bool(truthy(row.get("stream")))),
        // 思考强度(Rust 新增键;登记在途时提取,汇点并入 payload)。
        ("reasoningEffort", opt_text("reasoning_effort")),
        ("inputTokens", Value::from(input_tokens)),
        ("outputTokens", Value::from(output_tokens)),
        ("cacheReadTokens", Value::from(cache_read)),
        ("cacheWriteTokens", Value::from(cache_write)),
        (
            "totalTokens",
            Value::from(if stored_total != 0 {
                stored_total
            } else {
                input_tokens + cache_read + cache_write + output_tokens
            }),
        ),
        (
            "reasoningTokens",
            Value::from(positive_int(row.get("reasoning_tokens"), 0, None)),
        ),
        (
            "costUsd",
            if cost == 0.0 {
                Value::Null
            } else {
                number(cost)
            },
        ),
        (
            "durationMs",
            Value::from(positive_int(row.get("duration_ms"), 0, None)),
        ),
        (
            "firstByteMs",
            if first_byte == 0 {
                Value::Null
            } else {
                Value::from(first_byte)
            },
        ),
        // 阶段四 C-4:出站偏移(sent_ms = 出站时刻 − 请求起点;`TTFT −
        // sent_ms` 即供应商侧耗时)、排队等待、重试累计。
        (
            "sentMs",
            if sent_ms == 0.0 {
                Value::Null
            } else {
                number(sent_ms)
            },
        ),
        (
            "queuedMs",
            if queued_ms == 0.0 {
                Value::Null
            } else {
                number(queued_ms)
            },
        ),
        (
            "retriedMs",
            if retried_ms == 0.0 {
                Value::Null
            } else {
                number(retried_ms)
            },
        ),
        // 阶段四 C-7:内容 10%→90% 跨度(实测;突发投递的样本 flow≈0)。
        (
            "flowMs",
            if flow_ms == 0.0 {
                Value::Null
            } else {
                number(flow_ms)
            },
        ),
        // 20261108_01 §5.2:答案段计时(毫秒;新行才有,缺失 → null)。
        (
            "answerEndMs",
            if answer_end_ms == 0.0 {
                Value::Null
            } else {
                number(answer_end_ms)
            },
        ),
        (
            "firstAnswerByteMs",
            if first_answer_byte_ms == 0.0 {
                Value::Null
            } else {
                number(first_answer_byte_ms)
            },
        ),
        (
            "answerFlowMs",
            if answer_flow_ms == 0.0 {
                Value::Null
            } else {
                number(answer_flow_ms)
            },
        ),
        (
            "reasoningDecodeMs",
            if reasoning_decode_ms == 0.0 {
                Value::Null
            } else {
                number(reasoning_decode_ms)
            },
        ),
        (
            "streamTailMs",
            if stream_tail_ms == 0.0 {
                Value::Null
            } else {
                number(stream_tail_ms)
            },
        ),
        // 20261108_01 §13:解码速度 / 输出速度(后端单一事实源;列表与
        // 统计同一公式,前端不再二次计算)。
        (
            "decodeTps",
            decode_tps.map(number).unwrap_or(Value::Null),
        ),
        (
            "outputTps",
            output_tps.map(number).unwrap_or(Value::Null),
        ),
        ("decodeSource", decode_source),
        ("endReason", opt_text("stream_end_reason")),
        ("error", opt_text("error")),
        ("providerAttempts", Value::Array(provider_attempts)),
        ("retryAttempts", Value::Array(retry_attempts)),
    ])
}

/// retryAttempts 投影(列表与详情读取共用):优先消费网关落库的结构化
/// 明细(`retry_attempts`,含等待时长/失败状态/错误/耗时 + 逐 attempt
/// 采集 —— 批5 后详情读取已从侧表并回);旧行无此键,回落按
/// `provider_attempts` 合成(delayMs 恒 0 —— 历史数据没记)。
fn project_retry_attempts(row: &Map<String, Value>) -> Vec<Value> {
    let attempts: Option<Vec<Value>> = row
        .get("provider_attempts")
        .and_then(Value::as_array)
        .cloned();
    let attempt_count = attempts.as_ref().map_or(0, |items| items.len());
    match row.get("retry_attempts").and_then(Value::as_array) {
        Some(items) if !items.is_empty() => items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let status = positive_int(item.get("status"), 0, None);
                // 2026-10-20:历史行 `wait_ms` 实为秒(旧写入 bug),没有
                // `wait_unit` 标记 —— 读取侧换算为毫秒,避免旧数据展示
                // 缩小 1000 倍;新行带 wait_unit="ms",直接消费并保留两位小数。
                let wait_raw = float_or(item.get("wait_ms"), 0.0);
                let wait_ms = match item.get("wait_unit").and_then(Value::as_str) {
                    Some("ms") => wait_raw,
                    _ => wait_raw * 1000.0,
                };
                let opt = |key: &str| -> Value {
                    match item.get(key) {
                        Some(Value::String(text)) if !text.is_empty() => {
                            Value::String(text.clone())
                        }
                        _ => Value::Null,
                    }
                };
                object(vec![
                    ("attempt", Value::from(index as i64 + 1)),
                    ("delayMs", number(round_even(wait_ms, 2))),
                    ("final", Value::Bool(index + 1 == items.len())),
                    ("statusCode", Value::from(status)),
                    ("provider", opt("provider")),
                    ("model", opt("model")),
                    ("account", opt("account")),
                    ("error", opt("error")),
                    ("durationMs", number(float_or(item.get("duration_ms"), 0.0))),
                    ("waitSource", opt("wait_source")),
                    ("startedAt", opt("started_at")),
                    // 阶段四 C-4:该次尝试的并发槽位等待与出站偏移
                    // (相对请求起点;未出站 → null)。
                    ("queuedMs", number(float_or(item.get("queued_ms"), 0.0))),
                    (
                        "sentOffsetMs",
                        match item.get("sent_offset_ms") {
                            Some(Value::Number(value)) => number(value.as_f64().unwrap_or(0.0)),
                            _ => Value::Null,
                        },
                    ),
                    // 阶段 D3:稳定 attempt id 与逐 attempt 采集(仅
                    // captureBodies 开启时后端才写这些键;缺失即未捕获,
                    // 前端据此显示 unavailable 而不是空正文)。
                    ("attemptId", opt("attempt_id")),
                    // 20261017_02 批5:列表行的采集正文已拆侧表,主行瘦身
                    // 项带 captured 标记供列表圆点判断;旧格式无此键 → null。
                    ("captured", item.get("captured").cloned().unwrap_or(Value::Null)),
                    (
                        "upstreamRequest",
                        item.get("upstream_request")
                            .cloned()
                            .unwrap_or(Value::Null),
                    ),
                    (
                        "upstreamRequestHeaders",
                        item.get("upstream_request_headers")
                            .cloned()
                            .unwrap_or(Value::Null),
                    ),
                    (
                        "upstreamResponse",
                        item.get("upstream_response")
                            .cloned()
                            .unwrap_or(Value::Null),
                    ),
                    (
                        "upstreamHeaders",
                        item.get("upstream_headers")
                            .cloned()
                            .unwrap_or(Value::Null),
                    ),
                    (
                        "conversion",
                        item.get("conversion").cloned().unwrap_or(Value::Null),
                    ),
                    (
                        "upstreamResponseSize",
                        item.get("upstream_response_size")
                            .cloned()
                            .unwrap_or(Value::Null),
                    ),
                    (
                        "upstreamResponseTruncated",
                        Value::Bool(truthy(item.get("upstream_response_truncated"))),
                    ),
                ])
            })
            .collect(),
        _ => attempts
            .map(|items| {
                items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| {
                        object(vec![
                            ("attempt", Value::from(index as i64 + 1)),
                            ("delayMs", Value::from(0)),
                            ("final", Value::Bool(index + 1 == attempt_count)),
                            ("status", Value::String(py_text(item))),
                        ])
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests;
