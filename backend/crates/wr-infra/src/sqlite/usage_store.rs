//! Usage store 实现(方案 #14 R4.6)。
//!
//! 镜像 Python `infrastructure/persistence/sqlite/usage_store.py`。
//! **没有独立用量表**:一切从 `request_logs.payload` 现场聚合,所以
//! 「入账」= 请求日志落库时带上 usage/cost 字段(见 `request_log_store.rs`)。
//!
//! 三条 Python 侧已固化的语义,parity 与 R5 接口都依赖:
//! 1. **有效请求判定**:`provider` 为空(归一成 `"unknown"`)的行是「从未
//!    真正调用过供应商」的被拒请求(认证失败/密钥模型限制/限流/策略拒绝/
//!    幂等冲突/无供应商可选)。它们留在 `request_logs` 供审计,但**不进任何
//!    聚合**(totals / models / series / recentRequests 全排除)。因此
//!    `callLogs` 关掉后统计自然为空——设计如此。
//! 2. **展示归一**:标签恒为 `应用商/模型名`。只有当模型串的 `前缀/` 命中
//!    某个已配置 provider 的 id 或 name(小写别名)时才剥前缀,上游原生带
//!    `/` 的模型名(`meta-llama/Llama-3.1`)不会被误剥;确定不了供应商时用
//!    原始模型名做标签,不臆造供应商。
//! 3. **桶与补零**:分桶按 UTC epoch 下取整(键用 Python `isoformat()` 的
//!    `+00:00` 形态),标签按**本地时区**格式化,范围内每个空桶补零行
//!    (否则稀疏活动在折线图上被拉成直线)。
//!
//! JSON 键序与 Python dict 字面量逐位对齐(R5.9 管理面 parity 比对响应体)。

use chrono::{DateTime, Local, TimeZone, Utc};
use rusqlite::{Connection, params};
use serde_json::{Map, Value};

use wr_app::ports::{PortError, UsageStore};
use wr_domain::protocol::client_identity::normalize_client_identity;

use crate::python_semantics::{py_bool, py_float, py_int, py_str};

/// 范围 → 桶粒度秒数(未知名按 86400)。
pub(crate) const BUCKET_SECONDS: &[(&str, i64)] = &[
    ("today", 3600),
    ("24h", 3600),
    ("7d", 86400),
    ("30d", 86400),
];

/// `recentRequests` 条数。
pub(crate) const RECENT_LIMIT: usize = 20;

/// 卷积生效期 `recentRequests` 的最新行候选数(C-6):最新 200 行里
/// 按凭据/供应商过滤后取前 20 —— 避免为 recent 再扫全量 payload。
pub(crate) const RECENT_CANDIDATES: usize = 200;

// ---------------------------------------------------------------------------
// Python 值语义小工具
// ---------------------------------------------------------------------------

/// Python `str(x or "")`:假值(含缺失)归空串。
pub(crate) fn str_or_empty(value: Option<&Value>) -> String {
    match value {
        Some(raw) if py_bool(raw) => py_str(raw).unwrap_or_default(),
        _ => String::new(),
    }
}

/// Python `int(x or default)`。
pub(crate) fn int_or(value: Option<&Value>, default: i64) -> i64 {
    match value {
        Some(raw) if py_bool(raw) => py_int(raw).unwrap_or(default),
        _ => default,
    }
}

/// Python `float(x or default)`。
pub(crate) fn float_or(value: Option<&Value>, default: f64) -> f64 {
    match value {
        Some(raw) if py_bool(raw) => py_float(raw).unwrap_or(default),
        _ => default,
    }
}

/// Python `round(x, n)`:round-half-even(CPython 行为,非四舍五入)。
pub(crate) fn round_even(value: f64, digits: i32) -> f64 {
    let factor = 10f64.powi(digits);
    (value * factor).round_ties_even() / factor
}

pub(crate) fn number(value: f64) -> Value {
    serde_json::Number::from_f64(value)
        .map(Value::Number)
        .unwrap_or(Value::Null)
}

pub(crate) fn string(value: impl Into<String>) -> Value {
    Value::String(value.into())
}

// ---------------------------------------------------------------------------
// 供应商目录与模型展示归一(request_log_store 共用,镜像 Python 的 import)
// ---------------------------------------------------------------------------

/// provider id → 展示名,以及小写别名(id / name)→ id。
#[derive(Debug, Default, Clone)]
pub(crate) struct ProviderCatalog {
    display: Map<String, Value>,
    by_alias: Map<String, Value>,
}

impl ProviderCatalog {
    pub(crate) fn display_name(&self, provider_id: &str) -> String {
        match self.display.get(provider_id).and_then(Value::as_str) {
            Some(name) if !name.is_empty() => name.to_owned(),
            _ => provider_id.to_owned(),
        }
    }

    fn alias(&self, prefix: &str) -> Option<String> {
        self.by_alias
            .get(&prefix.to_lowercase())
            .and_then(Value::as_str)
            .map(str::to_owned)
    }
}

/// 从 payload 形态的 provider 列表建目录(Python 跳过非对象与空 id;
/// `name` 缺失回退 id;别名按 `setdefault` 语义先注册者占位)。
pub(crate) fn catalog_from_providers(providers: &[Value]) -> ProviderCatalog {
    let mut catalog = ProviderCatalog::default();
    for item in providers {
        let Some(map) = item.as_object() else {
            continue;
        };
        let pid = str_or_empty(map.get("id")).trim().to_owned();
        if pid.is_empty() {
            continue;
        }
        let raw_name = str_or_empty(map.get("name")).trim().to_owned();
        let name = if raw_name.is_empty() {
            pid.clone()
        } else {
            raw_name
        };
        catalog.display.insert(pid.clone(), string(name.clone()));
        catalog
            .by_alias
            .entry(pid.to_lowercase())
            .or_insert_with(|| string(pid.clone()));
        catalog
            .by_alias
            .entry(name.to_lowercase())
            .or_insert_with(|| string(pid.clone()));
    }
    catalog
}

/// 单行模型归一为 `应用商/模型名`,返回 `(label, base_model, label_provider)`。
pub(crate) fn normalize_model_display(
    model: &str,
    provider_id: &str,
    catalog: &ProviderCatalog,
) -> (String, String, String) {
    let mut base = model.to_owned();
    let mut prefix_provider = String::new();
    if let Some((prefix, rest)) = model.split_once('/')
        && !rest.is_empty()
        && !prefix.trim().is_empty()
        && let Some(hit) = catalog.alias(prefix.trim())
    {
        prefix_provider = hit;
        base = rest.to_owned();
    }
    let row_provider = if provider_id.is_empty() || provider_id == "unknown" {
        String::new()
    } else {
        provider_id.to_owned()
    };
    let label_provider = if row_provider.is_empty() {
        prefix_provider
    } else {
        row_provider
    };
    if label_provider.is_empty() {
        return (model.to_owned(), model.to_owned(), String::new());
    }
    (
        format!("{}/{base}", catalog.display_name(&label_provider)),
        base,
        label_provider,
    )
}

/// 库里 `created_at` 同时存在两种形态:`datetime('now')` 的
/// `YYYY-MM-DD HH:MM:SS` 与 Python `isoformat()` 的 `+00:00` / `Z`。
pub(crate) fn parse_iso(value: &str) -> Option<DateTime<Utc>> {
    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return Some(parsed.with_timezone(&Utc));
    }
    let normalized = value.trim().replace('T', " ").replace("Z", "");
    let naive = chrono::NaiveDateTime::parse_from_str(&normalized, "%Y-%m-%d %H:%M:%S%.f").ok()?;
    Some(Utc.from_utc_datetime(&naive))
}

/// Python `datetime.isoformat()`(UTC → `+00:00` 后缀)。
pub(crate) fn iso_format(moment: &DateTime<Utc>) -> String {
    moment.to_rfc3339()
}

/// 范围时间窗(UTC):下界**含**、上界**不含**;`None` = 该侧不限。
pub(crate) type RangeBounds = (Option<DateTime<Utc>>, Option<DateTime<Utc>>);

/// 范围 → 时间窗 `(下界, 上界)`(UTC):下界**含**、上界**不含**。
///
/// - 预设:`today`(本地日历零点)/`24h`/`7d`/`30d` → (起点, 无上界)
/// - `all` → (无下界, 无上界)= 全表
/// - 自定义:`YYYY-MM-DD..YYYY-MM-DD` → (首日本地零点, 末日**次日**本地零点)
///   —— 两端都含,按**本地**日历切(与 `today` 同口径)
/// - 其它未知名 → (无下界, 无上界)(全表,旧语义)
///
/// 自定义窗口的线格式与前端一致(`from..to` 两个本地日)。
pub(crate) fn range_bounds(range_key: &str) -> RangeBounds {
    match range_key {
        "today" => (local_midnight(Local::now().date_naive()), None),
        "24h" => (Some(Utc::now() - chrono::Duration::hours(24)), None),
        "7d" => (Some(Utc::now() - chrono::Duration::days(7)), None),
        "30d" => (Some(Utc::now() - chrono::Duration::days(30)), None),
        "all" => (None, None),
        _ => match parse_custom_range(range_key) {
            Some(bounds) => bounds,
            // 形如自定义却解析不出来 → **空窗口**,不是全表:把用户要的窄
            // 窗口静默变成全量,比显示空更糟。
            None if range_key.contains("..") => {
                let start = local_midnight(Local::now().date_naive());
                (start, start)
            }
            None => (None, None),
        },
    }
}

/// 本地日历某日零点 → UTC(本地日历口径,与 `today` 一致)。
fn local_midnight(date: chrono::NaiveDate) -> Option<DateTime<Utc>> {
    let naive = date.and_hms_opt(0, 0, 0)?;
    Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|moment| moment.with_timezone(&Utc))
}

/// 自定义窗口 `YYYY-MM-DD..YYYY-MM-DD` → (首日零点, 末日次日零点)。
/// 起止颠倒时按先后交换(与前端选日器同口径)。
fn parse_custom_range(range_key: &str) -> Option<RangeBounds> {
    let (from_raw, to_raw) = range_key.split_once("..")?;
    let from = chrono::NaiveDate::parse_from_str(from_raw.trim(), "%Y-%m-%d").ok()?;
    let to = chrono::NaiveDate::parse_from_str(to_raw.trim(), "%Y-%m-%d").ok()?;
    let (from, to) = if from <= to { (from, to) } else { (to, from) };
    let start = local_midnight(from)?;
    let end = local_midnight(to + chrono::Duration::days(1))?;
    Some((Some(start), Some(end)))
}

/// 一行已解析的请求日志(时间 + payload)。
pub(crate) struct UsageRow {
    pub(crate) created: Option<DateTime<Utc>>,
    pub(crate) item: Map<String, Value>,
}

fn collect_usage_rows(
    rows: impl Iterator<Item = rusqlite::Result<(String, String)>>,
) -> Result<Vec<UsageRow>, PortError> {
    let mut loaded = Vec::new();
    for row in rows {
        let (created_raw, payload_text) = row.context("row usage rows")?;
        let Ok(Value::Object(item)) = serde_json::from_str::<Value>(&payload_text) else {
            continue;
        };
        loaded.push(UsageRow {
            created: parse_iso(&created_raw),
            item,
        });
    }
    Ok(loaded)
}

/// `_load_rows`:范围下推到 SQL(`YYYY-MM-DD HH:MM:SS` 绑定串,能吃到
/// `idx_request_logs_created_at`),再逐行回检时间戳(挡住畸形历史行)。
/// 20261016_01 C-6:上界 `max_created` 由卷积混合边界给出(`None` 开区间)。
pub(crate) fn load_rows(
    connection: &Connection,
    min_created: Option<&str>,
    max_created: Option<&str>,
) -> Result<Vec<UsageRow>, PortError> {
    let pair = |row: &rusqlite::Row<'_>| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?));
    match (min_created, max_created) {
        (Some(bind), Some(hi_bind)) => {
            let mut statement = connection
                .prepare(
                    "SELECT created_at, payload FROM request_logs \
                     WHERE created_at >= ?1 AND created_at < ?2 ORDER BY created_at DESC",
                )
                .context("prepare usage rows")?;
            let rows = statement
                .query_map(params![bind, hi_bind], pair)
                .context("query usage rows")?;
            collect_usage_rows(rows)
        }
        (Some(bind), None) => {
            let mut statement = connection
                .prepare(
                    "SELECT created_at, payload FROM request_logs                      WHERE created_at >= ?1 ORDER BY created_at DESC",
                )
                .context("prepare usage rows")?;
            let rows = statement
                .query_map(params![bind], pair)
                .context("query usage rows")?;
            collect_usage_rows(rows)
        }
        (None, Some(hi_bind)) => {
            let mut statement = connection
                .prepare(
                    "SELECT created_at, payload FROM request_logs \
                     WHERE created_at < ?1 ORDER BY created_at DESC",
                )
                .context("prepare usage rows")?;
            let rows = statement
                .query_map(params![hi_bind], pair)
                .context("query usage rows")?;
            collect_usage_rows(rows)
        }
        (None, None) => {
            let mut statement = connection
                .prepare("SELECT created_at, payload FROM request_logs ORDER BY created_at DESC")
                .context("prepare usage rows")?;
            let rows = statement.query_map([], pair).context("query usage rows")?;
            collect_usage_rows(rows)
        }
    }
}

/// 派生表是否已是当前列名(20261108_01 §13)。旧表在升级重建失败时会被
/// 保留;此时读取侧必须退回原始行聚合,不能拿旧列名去查新表。
fn table_has_column(
    connection: &Connection,
    table: &str,
    column: &str,
) -> Result<bool, PortError> {
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name=?2",
            params![table, column],
            |row| row.get(0),
        )
        .context("probe table column")?;
    Ok(count > 0)
}

/// 卷积行读取(C-6):`bucket >= min_bucket`,桶内行按 (桶, rowid) 倒序,
/// 与现场聚合「最新行先进 models 表」的次序近似。
/// `max_bucket` = 自定义窗口上界(不含);`None` = 不限。
fn load_rollup_rows(
    connection: &Connection,
    min_bucket: i64,
    max_bucket: Option<i64>,
) -> Result<Vec<RollupRow>, PortError> {
    let hi_bucket = max_bucket.unwrap_or(i64::MAX);
    let mut statement = connection
        .prepare(
            "SELECT bucket_epoch, model, upstream_model, requested_model, provider, client, \
             api_key_id, api_key_name, request_count, error_count, input_tokens, output_tokens, \
             total_tokens, cache_tokens, cost_usd, duration_ms_sum, first_byte_ms_sum, \
             first_byte_count, decode_ms_sum, decode_tokens, decode_source \
             FROM usage_rollup WHERE bucket_epoch >= ?1 AND bucket_epoch < ?2 \
             ORDER BY bucket_epoch DESC, rowid DESC",
        )
        .context("prepare usage rollup rows")?;
    let rows = statement
        .query_map(params![min_bucket, hi_bucket], |row| {
            let model: String = row.get(1)?;
            let upstream_model: String = row.get(2)?;
            let accounting_model = if upstream_model.is_empty() {
                model.clone()
            } else {
                upstream_model
            };
            Ok(RollupRow {
                bucket_epoch: row.get(0)?,
                dims: RowDims {
                    model,
                    accounting_model,
                    requested_model: row.get(3)?,
                    provider: row.get(4)?,
                    // 20261104_01 阶段三 · B-1b:列里可能是**原始 UA**
                    // (归一落地前写入的历史行),读侧再归一一次 →
                    // 与新写入的归一 id 收敛到同一维度(归一幂等)。
                    client: normalize_client_identity(&row.get::<_, String>(5)?),
                    api_key_id: row.get(6)?,
                    api_key_name: row.get(7)?,
                },
                request_count: row.get(8)?,
                sums: UsageSums {
                    error_count: row.get(9)?,
                    input_tokens: row.get(10)?,
                    output_tokens: row.get(11)?,
                    total_tokens: row.get(12)?,
                    cache_tokens: row.get(13)?,
                    cost_usd: row.get(14)?,
                    duration_ms_sum: row.get(15)?,
                    first_byte_ms_sum: row.get(16)?,
                    first_byte_count: row.get(17)?,
                    decode_ms_sum: row.get(18)?,
                    decode_tokens: row.get(19)?,
                    decode_source: row
                        .get::<_, Option<String>>(20)?
                        .as_deref()
                        .and_then(DecodeSource::parse),
                },
            })
        })
        .context("query usage rollup rows")?;
    let mut loaded = Vec::new();
    for row in rows {
        loaded.push(row.context("row usage rollup")?);
    }
    Ok(loaded)
}

/// 阶段四 C-8:直方图行(`usage_rollup_hist` 的一组维度)。
#[derive(Debug, Clone)]
struct HistRow {
    dims: RowDims,
    /// 解码样本来源(TTFT 行为空串);旧口径样本读取侧排除。
    source: String,
    /// 指标 → 桶 → 计数。
    hist: Histogram,
}

/// 维度归组键(直方图行按同一组维度合并成一行)。
fn dims_key(dims: &RowDims) -> String {
    format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
        dims.model,
        dims.accounting_model,
        dims.requested_model,
        dims.provider,
        dims.client,
        dims.api_key_id,
        dims.api_key_name
    )
}

/// 直方图行读取(阶段四 C-8):`bucket >= min_bucket`、`< max_bucket`。
///
/// 同一组维度的多个 `(指标, 桶)` 行合并成一行(直方图)—— 维度归组
/// 与原始行/卷积行共用 `absorb` 的那一份。
fn load_hist_rows(
    connection: &Connection,
    min_bucket: i64,
    max_bucket: Option<i64>,
) -> Result<Vec<HistRow>, PortError> {
    let hi_bucket = max_bucket.unwrap_or(i64::MAX);
    let mut statement = connection
        .prepare(
            "SELECT bucket_epoch, model, upstream_model, requested_model, provider, client, \
             api_key_id, api_key_name, metric, bucket, sample_count, decode_source \
             FROM usage_rollup_hist WHERE bucket_epoch >= ?1 AND bucket_epoch < ?2 \
             ORDER BY bucket_epoch DESC, rowid DESC",
        )
        .context("prepare usage rollup hist rows")?;
    let rows = statement
        .query_map(params![min_bucket, hi_bucket], |row| {
            let model: String = row.get(1)?;
            let upstream_model: String = row.get(2)?;
            let accounting_model = if upstream_model.is_empty() {
                model.clone()
            } else {
                upstream_model
            };
            Ok((
                RowDims {
                    model,
                    accounting_model,
                    requested_model: row.get(3)?,
                    provider: row.get(4)?,
                    // 20261104_01 阶段三 · B-1b:同 `load_rollup_rows`,
                    // 历史行存的是原始 UA → 读侧归一。
                    client: normalize_client_identity(&row.get::<_, String>(5)?),
                    api_key_id: row.get(6)?,
                    api_key_name: row.get(7)?,
                },
                row.get::<_, String>(8)?,
                row.get::<_, i64>(9)?,
                row.get::<_, i64>(10)?,
                row.get::<_, String>(11)?,
            ))
        })
        .context("query usage rollup hist rows")?;
    let mut grouped: std::collections::BTreeMap<String, (RowDims, String, Histogram)> =
        std::collections::BTreeMap::new();
    for row in rows {
        let (dims, metric, bucket, count, source) = row.context("row usage rollup hist")?;
        // 旧口径解码样本单独成组:读取侧整体跳过(见 `merge_hist_rows`)。
        let key = format!("{}\u{1f}{source}", dims_key(&dims));
        let entry = grouped
            .entry(key)
            .or_insert_with(|| (dims.clone(), source, Histogram::default()));
        entry.2.add_metric(&metric, bucket, count);
    }
    Ok(grouped
        .into_values()
        .map(|(dims, source, hist)| HistRow { dims, source, hist })
        .collect())
}

/// 最新 N 行 payload(卷积生效期 `recentRequests` 的候选集)。
/// `max_created` = 自定义窗口上界(不含);`None` = 不限。
fn load_recent_payloads(
    connection: &Connection,
    min_created: Option<&str>,
    max_created: Option<&str>,
    limit: i64,
) -> Result<Vec<Map<String, Value>>, PortError> {
    fn collect<T: Iterator<Item = rusqlite::Result<String>>>(
        rows: T,
    ) -> Result<Vec<Map<String, Value>>, PortError> {
        let mut items = Vec::new();
        for row in rows {
            let text: String = row.context("row recent payloads")?;
            if let Ok(Value::Object(item)) = serde_json::from_str::<Value>(&text) {
                items.push(item);
            }
        }
        Ok(items)
    }
    match (min_created, max_created) {
        (Some(bind), Some(hi)) => {
            let mut statement = connection
                .prepare(
                    "SELECT payload FROM request_logs \
                     WHERE created_at >= ?1 AND created_at < ?2 \
                     ORDER BY created_at DESC, rowid DESC LIMIT ?3",
                )
                .context("prepare recent payloads")?;
            let rows = statement
                .query_map(params![bind, hi, limit], |row| row.get::<_, String>(0))
                .context("query recent payloads")?;
            collect(rows)
        }
        (Some(bind), None) => {
            let mut statement = connection
                .prepare(
                    "SELECT payload FROM request_logs WHERE created_at >= ?1 \
                     ORDER BY created_at DESC, rowid DESC LIMIT ?2",
                )
                .context("prepare recent payloads")?;
            let rows = statement
                .query_map(params![bind, limit], |row| row.get::<_, String>(0))
                .context("query recent payloads")?;
            collect(rows)
        }
        (None, Some(hi)) => {
            let mut statement = connection
                .prepare(
                    "SELECT payload FROM request_logs WHERE created_at < ?1 \
                     ORDER BY created_at DESC, rowid DESC LIMIT ?2",
                )
                .context("prepare recent payloads")?;
            let rows = statement
                .query_map(params![hi, limit], |row| row.get::<_, String>(0))
                .context("query recent payloads")?;
            collect(rows)
        }
        (None, None) => {
            let mut statement = connection
                .prepare(
                    "SELECT payload FROM request_logs \
                     ORDER BY created_at DESC, rowid DESC LIMIT ?1",
                )
                .context("prepare recent payloads")?;
            let rows = statement
                .query_map(params![limit], |row| row.get::<_, String>(0))
                .context("query recent payloads")?;
            collect(rows)
        }
    }
}

// ---------------------------------------------------------------------------
// 保序聚合表(Python 用 dict,插入序即输出序)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub(crate) struct OrderedTable<V> {
    entries: Vec<(String, V)>,
}

impl<V> OrderedTable<V> {
    fn get_or_insert_with(&mut self, key: &str, make: impl FnOnce() -> V) -> &mut V {
        if !self.entries.iter().any(|(name, _)| name == key) {
            self.entries.push((key.to_owned(), make()));
        }
        &mut self
            .entries
            .iter_mut()
            .find(|(name, _)| name == key)
            .expect("key present")
            .1
    }

    fn iter(&self) -> impl Iterator<Item = &(String, V)> {
        self.entries.iter()
    }

    /// 按键字典序输出(Python 的 `sorted(series)` / `sorted(model_series)`)。
    fn sorted(&self) -> Vec<&(String, V)> {
        let mut refs: Vec<&(String, V)> = self.entries.iter().collect();
        refs.sort_by(|left, right| left.0.cmp(&right.0));
        refs
    }
}

/// 单行用量计数(20261016_01 C-6):写入侧 rollup 与读取侧
/// [`Accumulator`] 共用同一套取数口径,防止两路口径漂移。
/// `request_count` 不在其中 —— 每原始行恒为 1,rollup 行单独携带。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct UsageSums {
    pub(crate) error_count: i64,
    pub(crate) input_tokens: i64,
    pub(crate) output_tokens: i64,
    pub(crate) total_tokens: i64,
    pub(crate) cache_tokens: i64,
    pub(crate) cost_usd: f64,
    pub(crate) duration_ms_sum: f64,
    pub(crate) first_byte_ms_sum: f64,
    pub(crate) first_byte_count: i64,
    pub(crate) decode_ms_sum: f64,
    pub(crate) decode_tokens: i64,
    /// 本行解码样本的分母来源;`None` = 无解码样本。
    pub(crate) decode_source: Option<DecodeSource>,
}

impl UsageSums {
    /// 取负(REPLACE 同 id 旧行时从卷积里扣回)。
    pub(crate) fn negated(mut self) -> Self {
        self.error_count = -self.error_count;
        self.input_tokens = -self.input_tokens;
        self.output_tokens = -self.output_tokens;
        self.total_tokens = -self.total_tokens;
        self.cache_tokens = -self.cache_tokens;
        self.cost_usd = -self.cost_usd;
        self.duration_ms_sum = -self.duration_ms_sum;
        self.first_byte_ms_sum = -self.first_byte_ms_sum;
        self.first_byte_count = -self.first_byte_count;
        self.decode_ms_sum = -self.decode_ms_sum;
        self.decode_tokens = -self.decode_tokens;
        self
    }
}

/// 解码速度样本的分母来源(20261108_01 §13)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DecodeSource {
    /// 新行:全内容流(分子为全输出,含可见思考/工具参数)。
    FullFlow,
    /// 新行:隐藏思考,分子剔除 reasoning、分母只取答案流。
    AnswerFlow,
    /// 旧行:解码窗口 `duration_ms − first_byte_ms`(断代口径)。
    LegacyDecodeWindow,
}

impl DecodeSource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::FullFlow => "full_flow",
            Self::AnswerFlow => "answer_flow",
            Self::LegacyDecodeWindow => "legacy_decode_window",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "full_flow" => Some(Self::FullFlow),
            "answer_flow" => Some(Self::AnswerFlow),
            "legacy_decode_window" => Some(Self::LegacyDecodeWindow),
            _ => None,
        }
    }
}

/// 单行解码速度样本:`tokens ÷ (ms/1000)`,并记录分母来源。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct DecodeSample {
    pub(crate) tokens: i64,
    pub(crate) ms: f64,
    pub(crate) source: DecodeSource,
}

/// 失败/取消行不产出速度样本(20261108_01 §13)。历史行可能缺 `status`:
/// 缺省按成功处理;客户端断开即使未落 499 也按取消行处理。
pub(crate) fn is_error_item(item: &Map<String, Value>) -> bool {
    int_or(item.get("status"), 200) >= 400
        || str_or_empty(item.get("stream_end_reason")) == "client_disconnected"
}

/// 该行是否应把 `reasoning_tokens` 从解码分子中剥离。
///
/// Responses 的 reasoning 以加密块返回时只有 usage 计数、没有可测内容流
/// (`reasoning_decode_ms` 为空),分子必须与 `answer_flow_ms` 同集合。
/// Chat 仅在思考未以可测文本流出现时才剥离:无 `reasoning_decode_ms`,
/// 或可见思考字节不足以覆盖上报 token(摘要/单块 thinking)。DeepSeek 等
/// reasoning_content 完整可见上游保持全输出。Gemini 的 output 本就不含
/// thoughts,不能剥离。
fn strips_hidden_reasoning(item: &Map<String, Value>) -> bool {
    let reasoning = int_or(item.get("reasoning_tokens"), 0);
    if reasoning <= 0 {
        return false;
    }
    match item.get("provider_protocol").and_then(Value::as_str) {
        Some("openai_responses") => true,
        Some("openai") => {
            let decode_missing = item
                .get("reasoning_decode_ms")
                .and_then(Value::as_f64)
                .map(|ms| ms <= 0.0)
                .unwrap_or(true);
            if decode_missing {
                return true;
            }
            // 有思考计时仍可能只有摘要/单块(加密思考被折叠):可见字节
            // 少于上报 token 时按隐藏处理。缺字段的旧行保守维持全输出。
            match item.get("reasoning_content_bytes").and_then(Value::as_f64) {
                Some(bytes) => (bytes as i64) < reasoning,
                None => false,
            }
        }
        _ => false,
    }
}

/// 单行解码速度样本(20261108_01 §7/§13 规范公式的唯一实现)。
///
/// `None` = 该行不产出解码样本(失败/取消行、分子为空、窗口不可测)。
///
/// **分子分母必须覆盖同一集合**。默认分子是 `output_tokens`(含思考与
/// 工具参数),分母是 `flow_ms`(同样含可见思考/工具参数)。Ling 等不报
/// `reasoning_tokens` 的上游、以及思考内容可见的 Chat 上游走此口径。
///
/// **隐藏思考**(`strips_hidden_reasoning`):`output_tokens` 含加密
/// reasoning 的 token,但 `flow_ms` 只统计可见文本/工具参数;此时改用
/// `(output_tokens − reasoning_tokens) / answer_flow_ms`,否则分子多出
/// 一段没有对应时间的 token,吞吐被放大到数百甚至上千。
///
/// **新行**(有 `answer_end_ms` 键)守卫:
/// - 分母缺失/≤0(单点或零跨度,FlowMeter 判不可测)→ 无样本;
/// - 窗口短于 [`MIN_DECODE_MS`](计时粒度不足)→ 无样本;
/// - 窗口短于 [`HELD_WINDOW_MS`] 且首个内容前等待超过窗口 4 倍
///   (短答案/短工具参数在长思考后整段倒出)→ 无样本;
/// - 内容在窗口 1/4 内倒完(`flow × HELD_SHARE < window`)→ 无样本;
/// - 速率超过 [`BURST_MIN_TOKENS_PER_SECOND`](物理上限,批量投递)→
///   无样本。
///
/// **旧行**(无 `answer_end_ms`):沿用解码窗口口径
/// (`duration_ms − first_byte_ms`)与既有突发守卫;旧数据不迁移,
/// 口径断代见方案 §8。旧行来源标记为 [`DecodeSource::LegacyDecodeWindow`],
/// 统计页只聚合新口径样本。
pub(crate) fn decode_tps_sample(
    item: &Map<String, Value>,
    is_error: bool,
) -> Option<DecodeSample> {
    if is_error {
        return None;
    }
    let output = int_or(item.get("output_tokens"), 0);
    if output <= 0 {
        return None;
    }
    // 新行以 `answer_end_ms` 为断代标记(旧行没有该键)。新行的
    // 分母不可测时**不回落**旧解码窗口 —— 否则同一请求在列表与统计里
    // 会拿到不同分母。
    if item.contains_key("answer_end_ms") {
        let hidden_reasoning = strips_hidden_reasoning(item);
        let numerator = if hidden_reasoning {
            (output - int_or(item.get("reasoning_tokens"), 0)).max(0)
        } else {
            output
        };
        if numerator <= 0 {
            return None;
        }
        // 隐藏思考的分子已剔除 reasoning,分母也必须只取可见答案流;
        // 无答案流(纯思考轮次)时不可测。
        let (flow_ms, window_start) = if hidden_reasoning {
            (
                item.get("answer_flow_ms").and_then(Value::as_f64)?,
                match item.get("first_answer_byte_ms").and_then(Value::as_f64) {
                    Some(anchor) if anchor > 0.0 => anchor,
                    _ => float_or(item.get("first_byte_ms"), 0.0),
                },
            )
        } else {
            (
                item.get("flow_ms").and_then(Value::as_f64)?,
                float_or(item.get("first_byte_ms"), 0.0),
            )
        };
        if flow_ms <= 0.0 {
            return None;
        }
        let answer_end = float_or(item.get("answer_end_ms"), 0.0);
        let gen_window = answer_end - window_start;
        if window_start <= 0.0 || gen_window <= 0.0 {
            return None;
        }
        if flow_ms < MIN_DECODE_MS {
            return None;
        }
        if flow_ms < HELD_WINDOW_MS && window_start > HELD_SHARE * flow_ms {
            return None;
        }
        if flow_ms * HELD_SHARE < gen_window {
            return None;
        }
        // 物理上限:真实 LLM 解码不会持续超过该速率;超过说明内容被上游
        // 批量倒出或推进点过少(1-2ms 窗口),测不出解码速度。
        if numerator as f64 / (flow_ms / 1000.0) > BURST_MIN_TOKENS_PER_SECOND {
            return None;
        }
        return Some(DecodeSample {
            tokens: numerator,
            ms: flow_ms,
            source: if hidden_reasoning {
                DecodeSource::AnswerFlow
            } else {
                DecodeSource::FullFlow
            },
        });
    }
    let first_byte = float_or(item.get("first_byte_ms"), 0.0);
    if first_byte <= 0.0 {
        return None;
    }
    let duration = float_or(item.get("duration_ms"), 0.0);
    let gen_ms = (duration - first_byte).max(0.0);
    if gen_ms <= 0.0 {
        return None;
    }
    let flow_ms = item.get("flow_ms").and_then(|value| value.as_f64());
    if burst_sample(output, gen_ms, duration, flow_ms) {
        return None;
    }
    Some(DecodeSample {
        tokens: output,
        ms: gen_ms,
        source: DecodeSource::LegacyDecodeWindow,
    })
}

/// 输出速度(20261108_01 §13):全部上报输出 token ÷ 请求总耗时。
///
/// 不套用内容流守卫 —— 它衡量的是「含首字、思考等待、排队与代理」的
/// 整体输出速率。只要求成功行、`output_tokens > 0`、`duration_ms > 0`;
/// 失败/取消行与在途行不产出。
pub(crate) fn output_tps_sample(item: &Map<String, Value>, is_error: bool) -> Option<f64> {
    if is_error {
        return None;
    }
    let output = int_or(item.get("output_tokens"), 0);
    if output <= 0 {
        return None;
    }
    let duration = float_or(item.get("duration_ms"), 0.0);
    if duration <= 0.0 {
        return None;
    }
    Some(output as f64 / (duration / 1000.0))
}

/// 从 payload 字段抽单行用量计数(与原 `Accumulator::add` 逐位同口径:
/// cacheTokens 只计 cache_read;首字仅 >0 且**成功**计入;吞吐样本仅流式
/// + 解码窗口有效 + 非缓冲突发)。
pub(crate) fn usage_sums_from_item(item: &Map<String, Value>, is_error: bool) -> UsageSums {
    // 首字延迟仅统计上报了 first_byte_ms 的请求(非流式通常无)。
    let first_byte = float_or(item.get("first_byte_ms"), 0.0);
    let first_byte_positive = first_byte > 0.0;
    // 20261104_01 阶段一 · B-3:失败/中断行**也带** first_byte_ms
    // (502 `mid_stream_error` / 499 `client_disconnected`),计入会把首字
    // 延迟均值整体拉高(用户误以为 WRouter 变慢),且 TTFT 的样本集严格
    // 大于吞吐的样本集 → 两个指标无法互相校验。分子与分母**同时**门限:
    // 只门限分母会让分子含失败行而分母不含,均值反而更偏。
    let first_byte_counted = first_byte_positive && !is_error;
    // 解码样本:仅成功行、输出非零、窗口有效、非缓冲突发者计入。规范
    // 公式与突发判定统一收敛在 [`decode_tps_sample`](20261108_01 §13)。
    let output = int_or(item.get("output_tokens"), 0);
    let decode = decode_tps_sample(item, is_error);
    let (decode_tokens, decode_ms_sum) = match decode {
        Some(sample) => (sample.tokens, sample.ms),
        None => (0, 0.0),
    };
    UsageSums {
        error_count: i64::from(is_error),
        input_tokens: int_or(item.get("input_tokens"), 0),
        output_tokens: output,
        total_tokens: int_or(item.get("total_tokens"), 0),
        // cacheTokens 只累计 cache_read_tokens(Python 语义)。
        cache_tokens: int_or(item.get("cache_read_tokens"), 0),
        cost_usd: float_or(item.get("cost_usd"), 0.0),
        duration_ms_sum: float_or(item.get("duration_ms"), 0.0),
        first_byte_ms_sum: if first_byte_counted { first_byte } else { 0.0 },
        first_byte_count: i64::from(first_byte_counted),
        decode_ms_sum,
        decode_tokens,
        decode_source: decode.map(|sample| sample.source),
    }
}

/// 一行的分组维度(20261016_01 C-6:原始行与 rollup 行共用)。
#[derive(Debug, Clone)]
pub(crate) struct RowDims {
    /// 落库原始模型名(空归 unknown)。
    pub(crate) model: String,
    /// 计价/记账模型(聚合行为 `upstream_model`,否则同 model)。
    pub(crate) accounting_model: String,
    pub(crate) requested_model: String,
    pub(crate) provider: String,
    pub(crate) client: String,
    pub(crate) api_key_id: String,
    pub(crate) api_key_name: String,
}

/// 从 payload 抽维度(空 → unknown,与现场聚合口径一致)。
pub(crate) fn dims_from_item(item: &Map<String, Value>) -> RowDims {
    let defaulted = |key: &str| -> String {
        let raw = str_or_empty(item.get(key));
        if raw.is_empty() {
            "unknown".to_owned()
        } else {
            raw
        }
    };
    let model = defaulted("model");
    let upstream_model = str_or_empty(item.get("upstream_model"));
    let accounting_model = if upstream_model.is_empty() {
        model.clone()
    } else {
        upstream_model
    };
    RowDims {
        model,
        accounting_model,
        requested_model: str_or_empty(item.get("requested_model")),
        provider: defaulted("provider"),
        // 20261104_01 阶段三 · B-1b:调用方维度存**归一 id**(产品名),
        // 不是原始 UA → 同一客户端换版本号不会裂成多条维度行。
        // ⚠️ 归一在**写入侧**做 → `usage_rollup.client` 列里存的是 id;
        // 归一前的历史行仍是原始 UA,读侧(`load_rollup_rows` /
        // `load_hist_rows`)再归一一次即可收敛(归一是幂等的)。
        client: normalize_client_identity(&defaulted("client")),
        api_key_id: defaulted("api_key_id"),
        api_key_name: str_or_empty(item.get("api_key_name")),
    }
}

/// 逐计数器累计(Python 的 totals 与各维度行共用同一套累加)。
#[derive(Debug, Clone, Default)]
struct Accumulator {
    request_count: i64,
    error_count: i64,
    input_tokens: i64,
    output_tokens: i64,
    total_tokens: i64,
    cache_tokens: i64,
    cost_usd: f64,
    duration_sum: f64,
    // 2026-09-11 健康度浮层:首字延迟(first_byte_ms)与生成吞吐(output_tokens/生成秒)。
    first_byte_sum: f64,
    first_byte_count: i64,
    // 20261108_01 §13:解码速度只聚合新口径样本(full_flow / answer_flow);
    // legacy_decode_window 分母语义不同,单独排除,避免均值混合分母。
    decode_seconds_sum: f64,
    decode_tokens: i64,
    /// 阶段四 C-8:分位数用的直方图(TTFT / 解码速度的桶计数)。
    hist: Histogram,
}

impl Accumulator {
    fn add_sums(&mut self, request_count: i64, sums: &UsageSums) {
        self.request_count += request_count;
        self.error_count += sums.error_count;
        self.input_tokens += sums.input_tokens;
        self.output_tokens += sums.output_tokens;
        self.total_tokens += sums.total_tokens;
        self.cache_tokens += sums.cache_tokens;
        self.cost_usd += sums.cost_usd;
        self.duration_sum += sums.duration_ms_sum;
        self.first_byte_sum += sums.first_byte_ms_sum;
        self.first_byte_count += sums.first_byte_count;
        if matches!(
            sums.decode_source,
            Some(DecodeSource::FullFlow | DecodeSource::AnswerFlow)
        ) {
            self.decode_seconds_sum += sums.decode_ms_sum / 1000.0;
            self.decode_tokens += sums.decode_tokens;
        }
    }

    /// 阶段四 C-8:并入直方图桶(分位数用)。
    fn add_hist(&mut self, hist: &Histogram) {
        self.hist.merge(hist);
    }

    /// 计数与直方图一起并入(原始行 / 卷积行 / 直方图行走同一条路)。
    fn add_with_hist(&mut self, request_count: i64, sums: &UsageSums, hist: Option<&Histogram>) {
        self.add_sums(request_count, sums);
        if let Some(hist) = hist {
            self.add_hist(hist);
        }
    }

    /// 单行入口(读取侧已改走 [`Self::add_sums`] + `usage_sums_from_item`;
    /// 保留给吞吐口径单元测试复用)。
    #[cfg(test)]
    fn add(&mut self, row: &UsageRow, is_error: bool) {
        let sums = usage_sums_from_item(&row.item, is_error);
        self.add_sums(1, &sums);
    }

    fn success_rate(&self) -> f64 {
        round_even(
            (self.request_count - self.error_count) as f64 / self.request_count.max(1) as f64,
            4,
        )
    }

    fn cache_ratio(&self) -> f64 {
        if self.input_tokens > 0 {
            round_even(self.cache_tokens as f64 / self.input_tokens as f64, 4)
        } else {
            0.0
        }
    }

    fn avg_duration(&self) -> f64 {
        round_even(self.duration_sum / self.request_count.max(1) as f64, 2)
    }

    fn avg_first_byte_ms(&self) -> f64 {
        if self.first_byte_count > 0 {
            round_even(self.first_byte_sum / self.first_byte_count as f64, 2)
        } else {
            0.0
        }
    }

    fn avg_decode_tps(&self) -> f64 {
        if self.decode_seconds_sum > 0.0 {
            round_even(self.decode_tokens as f64 / self.decode_seconds_sum, 2)
        } else {
            0.0
        }
    }
}

// ---------------------------------------------------------------------------
// 吞吐口径共享判定(2026-10-13):后端聚合与前端逐行展示必须同一套规则,
// 否则列表与统计页会对同一请求给出不同速率。
// ---------------------------------------------------------------------------

/// 缓冲突发判定(对齐 minimax-code `bufferedBurst`):解码窗口占整段请求
/// 时长过小(< 10%)且据此推算的速率畸高(> 1000 tok/s)时,说明上游把输出
/// 攒成一批下发,窗口测的是网络批量投递而非模型解码速度,该样本作废。
pub(crate) const BURST_MIN_TOKENS_PER_SECOND: f64 = 1000.0;
pub(crate) const BURST_MAX_DECODE_SHARE: f64 = 0.1;

pub(crate) fn buffered_burst(output_tokens: i64, gen_ms: f64, duration_ms: f64) -> bool {
    if duration_ms <= 0.0 || gen_ms >= duration_ms * BURST_MAX_DECODE_SHARE {
        return false;
    }
    output_tokens as f64 > (gen_ms / 1000.0) * BURST_MIN_TOKENS_PER_SECOND
}

/// `HeldShare = 4`:内容在解码
/// 窗口的**四分之一**以内倒出来 → 测的是批量投递,不是解码速度。
pub(crate) const HELD_SHARE: f64 = 4.0;

/// `MinDecodeMs = 100`:窗口短于
/// 该值,毫秒级计时粒度不足以判断解码速度。
pub(crate) const MIN_DECODE_MS: f64 = 100.0;

/// `HeldWindowMs = 1000`:窗口
/// 不足 1s、且首个内容前的等待超过窗口 4 倍 → 上游攒住后整段倒出,
/// 短样本不代表解码速度。
pub(crate) const HELD_WINDOW_MS: f64 = 1000.0;

/// 阶段四 C-7:吞吐样本的突发判定。
///
/// **有实测 `flow`**(内容 10%→90% 跨度,payload 键 `flow_ms`)时用它 ——
/// 形态:`flow > 0 && flow *
/// HeldShare < w` → 作废(`w` = 解码窗口 = `duration_ms − first_byte_ms`)。
/// 这条判据**不依赖任何速率阈值**:内容一次性倒出来时,窗口再长、
/// 速率再低也测不出解码速度。
///
/// **没有 flow** 的行(旧行 / 非流式 / 无内容)沿用速率阈值启发式
/// [`buffered_burst`] —— 保持历史行与既有聚合口径不变。
pub(crate) fn burst_sample(
    output_tokens: i64,
    gen_ms: f64,
    duration_ms: f64,
    flow_ms: Option<f64>,
) -> bool {
    match flow_ms {
        Some(flow) if flow > 0.0 => flow * HELD_SHARE < gen_ms,
        _ => buffered_burst(output_tokens, gen_ms, duration_ms),
    }
}

// ---------------------------------------------------------------------------
// 阶段四 C-8:TTFT / 吞吐的 P50 / P95(直方图)
// ---------------------------------------------------------------------------

/// 直方图指标名:首字延迟(毫秒)。
pub(crate) const METRIC_TTFT: &str = "ttft_ms";
/// 直方图指标名:解码速度(可见 token / 解码秒;仅新口径样本)。
pub(crate) const METRIC_DECODE_TPS: &str = "decode_tps";

/// 一行的直方图样本:`(指标, 来源, 桶)`(每对计数 1)。
///
/// `source` 只对解码速度有意义(TTFT 恒为空串);卷积表中作为
/// `decode_source` 维度,读取侧据此排除旧口径样本。
#[derive(Debug, Clone, Copy)]
pub(crate) struct HistogramSample {
    pub(crate) metric: &'static str,
    pub(crate) source: &'static str,
    pub(crate) bucket: i64,
}

/// 一行的直方图样本集合。
pub(crate) type HistogramSamples = Vec<HistogramSample>;

/// TTFT(毫秒)分桶:0–1s 每 10ms 一桶,1–10s 每 100ms,10s 以上每 1s。
///
/// 桶是**闭下开**区间(`b` 覆盖 `[lower(b), upper(b))`),分位数取桶的
/// **上界**(保守口径:真实值 ≤ 上界)。
pub(crate) fn ttft_bucket(ms: f64) -> i64 {
    let value = if ms.is_finite() && ms > 0.0 { ms } else { 0.0 };
    if value < 1000.0 {
        (value / 10.0).floor() as i64
    } else if value < 10000.0 {
        100 + ((value - 1000.0) / 100.0).floor() as i64
    } else {
        190 + ((value - 10000.0) / 1000.0).floor() as i64
    }
}

/// TTFT 桶的上界(毫秒)。
pub(crate) fn ttft_bucket_upper(bucket: i64) -> f64 {
    if bucket < 100 {
        (bucket + 1) as f64 * 10.0
    } else if bucket < 190 {
        1000.0 + (bucket - 99) as f64 * 100.0
    } else {
        10000.0 + (bucket - 189) as f64 * 1000.0
    }
}

/// 解码速度(token / 秒)分桶:0–100 每 1,100–500 每 5,500–2000 每 25,
/// 2000 以上每 100。
pub(crate) fn decode_tps_bucket(tps: f64) -> i64 {
    let value = if tps.is_finite() && tps > 0.0 {
        tps
    } else {
        0.0
    };
    if value < 100.0 {
        value.floor() as i64
    } else if value < 500.0 {
        100 + ((value - 100.0) / 5.0).floor() as i64
    } else if value < 2000.0 {
        180 + ((value - 500.0) / 25.0).floor() as i64
    } else {
        250 + ((value - 2000.0) / 100.0).floor() as i64
    }
}

/// 解码速度桶的上界(token / 秒)。
pub(crate) fn decode_tps_bucket_upper(bucket: i64) -> f64 {
    if bucket < 100 {
        (bucket + 1) as f64
    } else if bucket < 180 {
        100.0 + (bucket - 99) as f64 * 5.0
    } else if bucket < 250 {
        500.0 + (bucket - 179) as f64 * 25.0
    } else {
        2000.0 + (bucket - 249) as f64 * 100.0
    }
}

/// 指标 → 桶上界。
fn bucket_upper(metric: &str, bucket: i64) -> f64 {
    match metric {
        METRIC_TTFT => ttft_bucket_upper(bucket),
        METRIC_DECODE_TPS => decode_tps_bucket_upper(bucket),
        _ => bucket as f64,
    }
}

/// 从 payload 抽直方图样本(阶段四 C-8)。
///
/// 样本集与既有口径**同源**:
/// - TTFT:上报了 `first_byte_ms` 且**成功**的行(与 `first_byte_counted`
///   同门限);
/// - 解码速度:通过守卫的成功流式行(与 `decode_tokens` 同门限),值 =
///   可见 token / 解码秒;旧口径样本不进直方图。
pub(crate) fn histogram_samples_for_item(
    item: &Map<String, Value>,
    is_error: bool,
) -> HistogramSamples {
    let mut samples: HistogramSamples = Vec::new();
    let first_byte = float_or(item.get("first_byte_ms"), 0.0);
    if first_byte > 0.0 && !is_error {
        samples.push(HistogramSample {
            metric: METRIC_TTFT,
            source: "",
            bucket: ttft_bucket(first_byte),
        });
    }
    // 20261108_01 §13:解码直方图与均值/逐行走同一个规范样本函数;
    // 旧口径样本不进入新口径分位(与均值排除口径一致)。
    if let Some(sample) = decode_tps_sample(item, is_error)
        && sample.source != DecodeSource::LegacyDecodeWindow
    {
        let tps = sample.tokens as f64 / (sample.ms / 1000.0);
        if tps > 0.0 {
            samples.push(HistogramSample {
                metric: METRIC_DECODE_TPS,
                source: sample.source.as_str(),
                bucket: decode_tps_bucket(tps),
            });
        }
    }
    samples
}

/// 累计直方图(阶段四 C-8):指标 → 桶 → 计数。
#[derive(Debug, Clone, Default)]
pub(crate) struct Histogram {
    buckets: std::collections::BTreeMap<String, std::collections::BTreeMap<i64, i64>>,
}

impl Histogram {
    /// 并入单个桶的计数。
    pub(crate) fn add_metric(&mut self, metric: &str, bucket: i64, count: i64) {
        let entry = self.buckets.entry(metric.to_owned()).or_default();
        *entry.entry(bucket).or_insert(0) += count;
    }

    /// 累加一批样本(`weight` 为 +1 增量 / -1 扣回)。
    pub(crate) fn add_samples(&mut self, samples: &HistogramSamples, weight: i64) {
        for sample in samples {
            self.add_metric(sample.metric, sample.bucket, weight);
        }
    }

    /// 累加另一个直方图。
    pub(crate) fn merge(&mut self, other: &Histogram) {
        for (metric, buckets) in &other.buckets {
            let entry = self.buckets.entry(metric.clone()).or_default();
            for (bucket, count) in buckets {
                *entry.entry(*bucket).or_insert(0) += count;
            }
        }
    }

    /// 分位数(保守口径:取所在桶的**上界**;无样本时 `None`)。
    pub(crate) fn quantile(&self, metric: &str, q: f64) -> Option<f64> {
        let buckets = self.buckets.get(metric)?;
        let total: i64 = buckets.values().sum();
        if total <= 0 {
            return None;
        }
        let target = (total as f64 * q).ceil() as i64;
        let mut cumulative = 0;
        for (bucket, count) in buckets {
            cumulative += count;
            if cumulative >= target {
                return Some(bucket_upper(metric, *bucket));
            }
        }
        None
    }
}

/// 维度行(models / clientModels / providerModels)。
#[derive(Debug, Clone, Default)]
struct GroupRow {
    total: Accumulator,
    label: String,
    key: String,
    client: Option<String>,
    model: Option<String>,
    provider: Option<String>,
    credential_id: Option<String>,
    credential: Option<String>,
}

impl GroupRow {
    fn to_json(&self, max_share: f64) -> Value {
        let total = &self.total;
        let mut map = Map::new();
        map.insert("caption".to_owned(), string(self.label.clone()));
        map.insert("errorCount".to_owned(), Value::from(total.error_count));
        map.insert("inputTokens".to_owned(), Value::from(total.input_tokens));
        map.insert("outputTokens".to_owned(), Value::from(total.output_tokens));
        map.insert("requestCount".to_owned(), Value::from(total.request_count));
        map.insert("totalTokens".to_owned(), Value::from(total.total_tokens));
        map.insert("costUsd".to_owned(), number(round_even(total.cost_usd, 10)));
        map.insert("avgDurationMs".to_owned(), number(total.avg_duration()));
        map.insert("cacheRatio".to_owned(), number(total.cache_ratio()));
        map.insert("cacheTokens".to_owned(), Value::from(total.cache_tokens));
        map.insert("successRate".to_owned(), number(total.success_rate()));
        map.insert(
            "avgFirstByteMs".to_owned(),
            number(total.avg_first_byte_ms()),
        );
        map.insert(
            "avgDecodeTps".to_owned(),
            number(total.avg_decode_tps()),
        );
        map.insert("key".to_owned(), string(self.key.clone()));
        map.insert("label".to_owned(), string(self.label.clone()));
        map.insert("maxShare".to_owned(), number(max_share));
        for (name, value) in [
            ("client", &self.client),
            ("model", &self.model),
            ("provider", &self.provider),
            ("credentialId", &self.credential_id),
            ("credential", &self.credential),
        ] {
            if let Some(value) = value {
                map.insert(name.to_owned(), string(value.clone()));
            }
        }
        // 阶段四 C-8:该维度行的分位数(桶上界;无样本 → 0)。
        map.insert(
            "p50FirstByteMs".to_owned(),
            number(total.hist.quantile(METRIC_TTFT, 0.50).unwrap_or(0.0)),
        );
        map.insert(
            "p95FirstByteMs".to_owned(),
            number(total.hist.quantile(METRIC_TTFT, 0.95).unwrap_or(0.0)),
        );
        map.insert(
            "p50DecodeTps".to_owned(),
            number(total.hist.quantile(METRIC_DECODE_TPS, 0.50).unwrap_or(0.0)),
        );
        map.insert(
            "p95DecodeTps".to_owned(),
            number(total.hist.quantile(METRIC_DECODE_TPS, 0.95).unwrap_or(0.0)),
        );
        Value::Object(map)
    }
}

/// 时间桶行(series / modelSeries 共用;`_empty_bucket` 的键序)。
#[derive(Debug, Clone)]
struct SeriesRow {
    bucket: String,
    label: String,
    total: Accumulator,
}

impl Default for SeriesRow {
    fn default() -> Self {
        Self::new("")
    }
}

impl SeriesRow {
    fn new(bucket: &str) -> Self {
        Self {
            bucket: bucket.to_owned(),
            label: bucket.to_owned(),
            total: Accumulator::default(),
        }
    }

    fn to_json(&self) -> Value {
        object(vec![
            ("bucket", string(self.bucket.clone())),
            ("label", string(self.label.clone())),
            ("requestCount", Value::from(self.total.request_count)),
            ("errorCount", Value::from(self.total.error_count)),
            ("inputTokens", Value::from(self.total.input_tokens)),
            ("outputTokens", Value::from(self.total.output_tokens)),
            ("totalTokens", Value::from(self.total.total_tokens)),
            ("costUsd", number(round_even(self.total.cost_usd, 10))),
            // 20261104_01 阶段一 · A-2:桶级耗时与缓存命中率不再是硬编码 0,
            // 改与窗口级 totals 同口径(avg_duration / cache_ratio)。所需四列
            // (duration_ms_sum / request_count / cache_tokens / input_tokens)
            // 早已在 DDL 里且早已被写入侧累加,此处是纯读侧修正。
            // 补零桶 request_count = 0 → avg_duration 分母 max(1) → 0,无除零。
            ("avgDurationMs", number(self.total.avg_duration())),
            ("cacheRatio", number(self.total.cache_ratio())),
            ("cacheTokens", Value::from(self.total.cache_tokens)),
            ("successRate", number(self.total.success_rate())),
            (
                "avgFirstByteMs",
                number(self.total.avg_first_byte_ms()),
            ),
            (
                "avgDecodeTps",
                number(self.total.avg_decode_tps()),
            ),
        ])
    }
}

/// 一次聚合的全部中间结果。
#[derive(Default)]
pub(crate) struct UsageAggregate {
    totals: Accumulator,
    models: OrderedTable<GroupRow>,
    client_models: OrderedTable<GroupRow>,
    provider_models: OrderedTable<GroupRow>,
    series: OrderedTable<SeriesRow>,
    model_series: OrderedTable<OrderedTable<SeriesRow>>,
    recent: Vec<Value>,
}

/// 聚合过滤条件(20261016_01 C-6:原始行与 rollup 行共用)。
#[derive(Debug, Clone, Default)]
pub(crate) struct UsageFilters {
    pub(crate) credential: String,
    pub(crate) provider: String,
    pub(crate) model: String,
    /// 20261104_01 阶段三 · B-1b:调用方标识(**归一 id**,如 `ZCode`)。
    /// 原始行走 `dims_from_item` 的 `client` 键,卷积行走 `usage_rollup.client`
    /// 列 —— 两路同口径(都归一到 id),故同一 filter 在两段上的筛选结果一致。
    /// 空串 = 不按调用方过滤(装配处显式保留空串,归一会把空变成 "unknown")。
    pub(crate) client: String,
}

impl UsageAggregate {
    /// 一行进聚合(20261016_01 C-6 重构):过滤(凭据/供应商/有效请求/
    /// 模型)→ 维度归组 → 时间桶。原始行与 rollup 行走同一条路,过滤与
    /// 展示归一逻辑只此一份,不会漂移。返回该行是否被计入。
    fn absorb(
        &mut self,
        filters: &UsageFilters,
        dims: &RowDims,
        request_count: i64,
        sums: &UsageSums,
        // 阶段四 C-8:该行的直方图样本(原始行现算;直方图行读表;
        // 卷积行没有逐行分布 → None)。
        hist: Option<&Histogram>,
        created: Option<DateTime<Utc>>,
        bucket_seconds: i64,
        catalog: &ProviderCatalog,
    ) -> bool {
        if !filters.credential.is_empty() && dims.api_key_id != filters.credential {
            return false;
        }
        if !filters.provider.is_empty() && dims.provider != filters.provider {
            return false;
        }
        // 20261104_01 阶段三 · B-1b:调用方过滤(归一 id 精确匹配;两侧都归一)。
        if !filters.client.is_empty() && dims.client != filters.client {
            return false;
        }
        // 有效请求判定:被拒请求不入账(但仍留在 request_logs 供审计)。
        if dims.provider == "unknown" {
            return false;
        }
        let (model_label, base_model, label_provider) =
            normalize_model_display(&dims.accounting_model, &dims.provider, catalog);
        // 筛选值六选一:展示 label("供应商/模型名",前端下拉即 models[]
        // .label)、落库裸名(聚合行为聚合名)、剥前缀 base_model、记账模型
        // 名、requestedModel(聚合名维度);兼容旧调用方与聚合双维度筛选。
        if !filters.model.is_empty()
            && dims.model != filters.model
            && base_model != filters.model
            && model_label != filters.model
            && dims.accounting_model != filters.model
            && dims.requested_model != filters.model
        {
            return false;
        }
        self.totals.add_with_hist(request_count, sums, hist);

        self.models
            .get_or_insert_with(&model_label, || GroupRow {
                total: Accumulator::default(),
                label: model_label.clone(),
                key: tuple_key(&model_label),
                client: None,
                model: None,
                provider: Some(dims.provider.clone()),
                credential_id: None,
                credential: None,
            })
            .total
            .add_with_hist(request_count, sums, hist);

        let client_key = format!("{}\u{1f}{}\u{1f}{}", dims.client, base_model, dims.provider);
        let client_label = format!("{} / {}", dims.client, model_label);
        self.client_models
            .get_or_insert_with(&client_key, || GroupRow {
                total: Accumulator::default(),
                label: client_label.clone(),
                key: tuple_key(&client_key),
                client: Some(dims.client.clone()),
                model: Some(base_model.clone()),
                provider: Some(dims.provider.clone()),
                credential_id: None,
                credential: None,
            })
            .total
            .add_with_hist(request_count, sums, hist);

        let provider_key = format!(
            "{}\u{1f}{}\u{1f}{}",
            dims.provider, dims.api_key_id, base_model
        );
        let display_provider = if label_provider.is_empty() {
            dims.provider.clone()
        } else {
            catalog.display_name(&label_provider)
        };
        let provider_label = format!("{} / {} / {}", display_provider, dims.api_key_id, base_model);
        self.provider_models
            .get_or_insert_with(&provider_key, || GroupRow {
                total: Accumulator::default(),
                label: provider_label.clone(),
                key: tuple_key(&provider_key),
                client: None,
                model: Some(base_model.clone()),
                provider: Some(dims.provider.clone()),
                credential_id: Some(dims.api_key_id.clone()),
                credential: Some(if dims.api_key_name.is_empty() {
                    dims.api_key_id.clone()
                } else {
                    dims.api_key_name.clone()
                }),
            })
            .total
            .add_with_hist(request_count, sums, hist);

        if let Some(moment) = created {
            let bucket = bucket_key(moment, bucket_seconds);
            self.series
                .get_or_insert_with(&bucket, || SeriesRow::new(&bucket))
                .total
                .add_sums(request_count, sums);
            let table = self
                .model_series
                .get_or_insert_with(&model_label, OrderedTable::default);
            table
                .get_or_insert_with(&bucket, || SeriesRow::new(&bucket))
                .total
                .add_sums(request_count, sums);
        }
        true
    }
}

/// Python 的聚合键是元组,`str(key)` 落成 `('a', 'b')` 文本。
/// 内部键用 `\u{1f}` 分隔,输出时还原成元组形态。
fn tuple_key(internal: &str) -> String {
    let parts: Vec<&str> = internal.split('\u{1f}').collect();
    let joined = parts
        .iter()
        .map(|part| format!("'{part}'"))
        .collect::<Vec<_>>()
        .join(", ");
    if parts.len() == 1 {
        format!("({joined},)")
    } else {
        format!("({joined})")
    }
}

fn bucket_key(moment: DateTime<Utc>, bucket_seconds: i64) -> String {
    let epoch = moment.timestamp();
    let floored = epoch - epoch.rem_euclid(bucket_seconds);
    Utc.timestamp_opt(floored, 0)
        .single()
        .map(|value| iso_format(&value))
        .unwrap_or_default()
}

/// 桶键 → 本地时区轴标签(小时桶 `MM-DD HH:00`,天桶 `MM-DD`)。
fn bucket_label(bucket: &str, bucket_seconds: i64) -> String {
    let Some(parsed) = parse_iso(bucket) else {
        return bucket.to_owned();
    };
    let local = parsed.with_timezone(&Local);
    if bucket_seconds < 86400 {
        local.format("%m-%d %H:00").to_string()
    } else {
        local.format("%m-%d").to_string()
    }
}

/// 从范围起点(或最早非空桶)补零到当前,并顺带给每个桶打标签。
/// `max_created` = 自定义窗口上界(不含):补零只补到窗口末尾那一桶,
/// 不再往窗口外延伸(否则图表会拖出一条不属于该窗口的尾巴)。
fn complete_buckets(
    buckets: &mut OrderedTable<SeriesRow>,
    bucket_seconds: i64,
    min_created: Option<DateTime<Utc>>,
    max_created: Option<DateTime<Utc>>,
) {
    if buckets.entries.is_empty() {
        return;
    }
    let now_epoch = Utc::now().timestamp();
    let start_epoch = match min_created {
        Some(moment) => moment.timestamp(),
        None => buckets
            .iter()
            .filter_map(|(key, _)| parse_iso(key).map(|moment| moment.timestamp()))
            .min()
            .unwrap_or(now_epoch),
    };
    // 上界不含 → 最后一桶的起点必须 < 上界;桶起点按粒度对齐,故取
    // `上界 - 1s` 作为「可补到的最后一个桶起点」的上限。
    let last_allowed = match max_created {
        Some(moment) => moment.timestamp().saturating_sub(1).min(now_epoch),
        None => now_epoch,
    };
    let mut current = start_epoch - start_epoch.rem_euclid(bucket_seconds);
    while current <= last_allowed {
        if let Some(moment) = Utc.timestamp_opt(current, 0).single() {
            let key = iso_format(&moment);
            let entry = buckets.get_or_insert_with(&key, || SeriesRow::new(&key));
            entry.label = bucket_label(&key, bucket_seconds);
        }
        current += bucket_seconds;
    }
}

/// `recentRequests` 行:单请求快照(Python `_recent_row`)。
fn recent_row(item: &Map<String, Value>, catalog: &ProviderCatalog) -> Value {
    let status = int_or(item.get("status"), 200);
    let raw_model = str_or_empty(item.get("model"));
    // 20260905_04:聚合行按实际调用模型展示(计价维度同源)。
    let upstream_model = str_or_empty(item.get("upstream_model"));
    let raw_model = if upstream_model.is_empty() {
        raw_model
    } else {
        upstream_model
    };
    let model = if raw_model.is_empty() {
        "unknown".to_owned()
    } else {
        raw_model.to_owned()
    };
    let raw_provider = str_or_empty(item.get("provider"));
    let provider = if raw_provider.is_empty() {
        "unknown".to_owned()
    } else {
        raw_provider
    };
    let (label, base_model, _) = normalize_model_display(&model, &provider, catalog);
    // key = request_id or model or "unknown"(Python `or` 链)。
    let key = item
        .get("request_id")
        .filter(|value| py_bool(value))
        .or_else(|| item.get("model").filter(|value| py_bool(value)))
        .map(|value| py_str(value).unwrap_or_default())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown".to_owned());
    object(vec![
        ("caption", string(label.clone())),
        ("errorCount", Value::from(i64::from(status >= 400))),
        (
            "inputTokens",
            Value::from(int_or(item.get("input_tokens"), 0)),
        ),
        (
            "outputTokens",
            Value::from(int_or(item.get("output_tokens"), 0)),
        ),
        ("requestCount", Value::from(1)),
        (
            "totalTokens",
            Value::from(int_or(item.get("total_tokens"), 0)),
        ),
        (
            "costUsd",
            number(round_even(float_or(item.get("cost_usd"), 0.0), 10)),
        ),
        (
            "avgDurationMs",
            number(round_even(float_or(item.get("duration_ms"), 0.0), 2)),
        ),
        ("cacheRatio", Value::from(0)),
        ("cacheTokens", Value::from(0)),
        ("successRate", number(if status >= 400 { 0.0 } else { 1.0 })),
        ("key", string(key)),
        ("label", string(label)),
        ("maxShare", number(0.0)),
        ("model", string(base_model)),
        ("provider", string(provider)),
    ])
}

use async_trait::async_trait;

use super::SqliteStore;
use super::{SqlContext as _, object};

impl SqliteStore {
    /// `usage_summary` 的 provider 目录(C-1 缓存:配置原文未变即复用;
    /// provider 可运行期改名/新增,读失败退化成空目录而不报错,镜像
    /// Python 的 try/except)。
    pub(crate) async fn provider_catalog(&self) -> ProviderCatalog {
        self.catalog_snapshot().await.0.clone()
    }
}

/// Rollup 行(20261016_01 C-6):小时桶 × 原始维度的一行加和。
pub(crate) struct RollupRow {
    pub(crate) bucket_epoch: i64,
    pub(crate) dims: RowDims,
    pub(crate) request_count: i64,
    pub(crate) sums: UsageSums,
}

/// 混合边界(20261016_01 C-6):卷积按**小时桶**聚合,范围起点若落在桶
/// 中段,该桶不能用桶加和(会带入起点之前的行)—— 原始行覆盖
/// `[min, boundary)`,卷积覆盖 `bucket >= boundary`,两段拼起来与现场
/// 全量聚合**精确一致**。返回 (原始行上界 bind, 卷积下界 epoch)。
pub(crate) fn hybrid_split(
    min_moment: Option<DateTime<Utc>>,
    rollup_floor: i64,
) -> (Option<String>, i64) {
    let rollup_min = match min_moment {
        Some(min) => {
            let epoch = min.timestamp();
            let aligned_up = if epoch.rem_euclid(3600) == 0 {
                epoch
            } else {
                epoch - epoch.rem_euclid(3600) + 3600
            };
            aligned_up.max(rollup_floor)
        }
        None => rollup_floor,
    };
    let hi_bind = Utc.timestamp_opt(rollup_min, 0)
        .single()
        .map(|moment| moment.format("%Y-%m-%d %H:%M:%S").to_string());
    (hi_bind, rollup_min)
}

#[allow(clippy::too_many_arguments)]
fn accumulate_rows(
    rows: Vec<UsageRow>,
    min_created: Option<DateTime<Utc>>,
    max_created: Option<DateTime<Utc>>,
    filters: &UsageFilters,
    bucket_seconds: i64,
    catalog: &ProviderCatalog,
) -> UsageAggregate {
    let mut aggregate = UsageAggregate::default();
    for row in rows {
        if let (Some(min), Some(moment)) = (min_created, row.created)
            && moment < min
        {
            continue;
        }
        // 自定义窗口上界(不含):窗口之后的行不入账。
        if let (Some(max), Some(moment)) = (max_created, row.created)
            && moment >= max
        {
            continue;
        }
        let dims = dims_from_item(&row.item);
        let status = int_or(row.item.get("status"), 200);
        let is_error = status >= 400;
        let sums = usage_sums_from_item(&row.item, is_error);
        // 阶段四 C-8:直方图样本(与 sums 同口径现算 —— 同一行既进均值
        // 也进分位数桶;卷积生效期之前的原始行走这条路)。
        let mut hist = Histogram::default();
        hist.add_samples(&histogram_samples_for_item(&row.item, is_error), 1);
        // recentRequests 的历史插入点:凭据/供应商/调用方/有效请求前置过滤
        // 之后、模型过滤之前(不随模型筛选收窄 —— 行为保留)。
        let passes_pre = (filters.credential.is_empty() || dims.api_key_id == filters.credential)
            && (filters.provider.is_empty() || dims.provider == filters.provider)
            // 20261104_01 阶段三 · B-1b:调用方过滤与凭据/供应商同级
            // (前置过滤 → recent 行也随调用方收窄)。
            && (filters.client.is_empty() || dims.client == filters.client)
            && dims.provider != "unknown";
        if passes_pre && aggregate.recent.len() < RECENT_LIMIT {
            aggregate.recent.push(recent_row(&row.item, catalog));
        }
        aggregate.absorb(
            filters,
            &dims,
            1,
            &sums,
            Some(&hist),
            row.created,
            bucket_seconds,
            catalog,
        );
    }
    aggregate
}

/// 卷积行并入聚合(过滤/归组与原始行同一条 absorb 路径)。
/// `max_epoch` = 自定义窗口上界(不含):窗口之后的卷积桶不入账。
#[allow(clippy::too_many_arguments)]
fn merge_rollup_rows(
    aggregate: &mut UsageAggregate,
    rows: Vec<RollupRow>,
    max_epoch: Option<i64>,
    filters: &UsageFilters,
    bucket_seconds: i64,
    catalog: &ProviderCatalog,
) {
    for row in rows {
        if let Some(max) = max_epoch
            && row.bucket_epoch >= max
        {
            continue;
        }
        let created = Utc.timestamp_opt(row.bucket_epoch, 0).single();
        aggregate.absorb(
            filters,
            &row.dims,
            row.request_count,
            &row.sums,
            // 卷积行只有累加量,没有逐行分布 → 不带直方图(直方图走
            // 独立的 `usage_rollup_hist` 表)。
            None,
            created,
            bucket_seconds,
            catalog,
        );
    }
}

/// 阶段四 C-8:直方图行并入聚合(过滤/归组与原始行、卷积行同一条
/// `absorb` 路径 —— 维度归组只有一份,不会漂移)。
fn merge_hist_rows(
    aggregate: &mut UsageAggregate,
    rows: Vec<HistRow>,
    filters: &UsageFilters,
    bucket_seconds: i64,
    catalog: &ProviderCatalog,
) {
    for row in rows {
        // 旧口径解码样本(duration − first_byte)与解码速度不同分母,
        // 不参与新口径均值/分位(方案 §13.1/§13.4)。
        if DecodeSource::parse(&row.source) == Some(DecodeSource::LegacyDecodeWindow) {
            continue;
        }
        // 直方图行只带分布,不带计数 → request_count = 0、计数全零。
        aggregate.absorb(
            filters,
            &row.dims,
            0,
            &UsageSums::default(),
            Some(&row.hist),
            // 不进时间序列(直方图行不是请求)。
            None,
            bucket_seconds,
            catalog,
        );
    }
}

/// `recentRequests`:卷积生效期从最新原始行现取(200 行候选足够;
/// 与现场聚合同口径:凭据/供应商过滤 + 被拒请求不入选)。
fn build_recent_rows(
    items: Vec<Map<String, Value>>,
    filters: &UsageFilters,
    catalog: &ProviderCatalog,
) -> Vec<Value> {
    let mut recent = Vec::with_capacity(RECENT_LIMIT);
    for item in items {
        if recent.len() >= RECENT_LIMIT {
            break;
        }
        let dims = dims_from_item(&item);
        if !filters.credential.is_empty() && dims.api_key_id != filters.credential {
            continue;
        }
        if !filters.provider.is_empty() && dims.provider != filters.provider {
            continue;
        }
        // 20261104_01 阶段三 · B-1b:调用方过滤(与凭据/供应商同级)。
        if !filters.client.is_empty() && dims.client != filters.client {
            continue;
        }
        if dims.provider == "unknown" {
            continue;
        }
        recent.push(recent_row(&item, catalog));
    }
    recent
}

fn render_groups(groups: &OrderedTable<GroupRow>, max_tokens: i64) -> Value {
    Value::Array(
        groups
            .iter()
            .map(|(_, row)| {
                row.to_json(round_even(
                    row.total.total_tokens as f64 / max_tokens.max(1) as f64,
                    4,
                ))
            })
            .collect(),
    )
}

fn render_buckets(buckets: &OrderedTable<SeriesRow>) -> Value {
    Value::Array(
        buckets
            .sorted()
            .into_iter()
            .map(|(_, row)| row.to_json())
            .collect(),
    )
}

#[async_trait]
impl UsageStore for SqliteStore {
    async fn usage_summary(&self, range: Option<&str>, filter: &Value) -> Result<Value, PortError> {
        let range_key = match range {
            Some(value) if !value.is_empty() => value.to_owned(),
            _ => "today".to_owned(),
        };
        let bucket_seconds = BUCKET_SECONDS
            .iter()
            .find(|(name, _)| *name == range_key.as_str())
            .map(|(_, seconds)| *seconds)
            .unwrap_or(86400);
        let filter_map = filter.as_object().cloned().unwrap_or_default();
        let filters = UsageFilters {
            credential: str_or_empty(filter_map.get("credential")).trim().to_owned(),
            provider: str_or_empty(filter_map.get("provider")).trim().to_owned(),
            model: str_or_empty(filter_map.get("model")).trim().to_owned(),
            // 20261104_01 阶段三 · B-1b:调用方过滤按**归一 id** 比对 →
            // 前端下拉给的就是归一 id;这里再归一一次是为了「拿原始 UA
            // 来查」也能命中(归一幂等)。
            // ⚠️ 空串必须保持空串:空 = 「不按调用方过滤」,归一会把它变成
            // "unknown"(那是「没有 UA 头」这个具体客户端,不是「不过滤」)。
            client: {
                let raw = str_or_empty(filter_map.get("client")).trim().to_owned();
                if raw.is_empty() {
                    String::new()
                } else {
                    normalize_client_identity(&raw)
                }
            },
        };
        let (min_moment, max_moment) = range_bounds(&range_key);
        let max_epoch = max_moment.map(|moment| moment.timestamp());
        let catalog = self.provider_catalog().await;
        // 20261017_02 批2:卷积覆盖探测 + 原始行 + 卷积行 + recent 候选
        // 四个读取步骤合并到**一条**连接(旧实现每步各开一条 SQLite 连接;
        // 步骤间本无事务需求,单连接顺序执行语义不变)。
        let (rows, rollup_rows, recent_candidates, hist_rows, rollup_min) = {
            let bind = min_moment.map(|moment| moment.format("%Y-%m-%d %H:%M:%S").to_string());
            let max_bind = max_moment.map(|moment| moment.format("%Y-%m-%d %H:%M:%S").to_string());
            self.engine
                .read(move |connection| {
                    // 旧派生表(升级重建失败被保留)不做卷积读取 —— 列名
                    // 已变,查询必然失败;退回原始行聚合保证数值正确。
                    let rollup_current = table_has_column(connection, "usage_rollup", "decode_tokens")?;
                    let rollup_floor: Option<i64> = if rollup_current {
                        connection
                            .query_row(
                                "SELECT MIN(bucket_epoch) FROM usage_rollup",
                                [],
                                |row| row.get::<_, Option<i64>>(0),
                            )
                            .context("probe usage_rollup coverage")?
                    } else {
                        None
                    };
                    let (raw_hi_bind, rollup_min) = match rollup_floor {
                        Some(floor) => {
                            let (hi, min) = hybrid_split(min_moment, floor);
                            // 自定义窗口上界落在卷积段起点之前 → 整段卷积都在
                            // 窗口外,不用卷积,原始行一路读到上界为止。
                            match max_epoch {
                                Some(upper) if upper <= min => (max_bind.clone(), None),
                                _ => (hi, Some(min)),
                            }
                        }
                        None => (max_bind.clone(), None),
                    };
                    let rows =
                        load_rows(connection, bind.as_deref(), raw_hi_bind.as_deref())?;
                    let (rollup_rows, recent_candidates) = match rollup_min {
                        Some(min) => {
                            let rollup = load_rollup_rows(connection, min, max_epoch)?;
                            let min_bind = min_moment
                                .map(|moment| moment.format("%Y-%m-%d %H:%M:%S").to_string());
                            let candidates = load_recent_payloads(
                                connection,
                                min_bind.as_deref(),
                                max_bind.as_deref(),
                                RECENT_CANDIDATES as i64,
                            )?;
                            (rollup, Some(candidates))
                        }
                        None => (Vec::new(), None),
                    };
                    // 阶段四 C-8:直方图行(与卷积行同一时间范围)。
                    let hist_current =
                        table_has_column(connection, "usage_rollup_hist", "decode_source")?;
                    let hist_rows = match (rollup_min, hist_current) {
                        (Some(min), true) => load_hist_rows(connection, min, max_epoch)?,
                        _ => Vec::new(),
                    };
                    Ok((rows, rollup_rows, recent_candidates, hist_rows, rollup_min))
                })
                .await?
        };

        let mut aggregate = accumulate_rows(
            rows,
            min_moment,
            max_moment,
            &filters,
            bucket_seconds,
            &catalog,
        );
        if let (Some(_), rollup_rows, Some(candidates)) =
            (rollup_min, rollup_rows, recent_candidates)
        {
            merge_rollup_rows(
                &mut aggregate,
                rollup_rows,
                max_epoch,
                &filters,
                bucket_seconds,
                &catalog,
            );
            // recentRequests:卷积行没有单请求快照 → 最新原始行现取。
            aggregate.recent = build_recent_rows(candidates, &filters, &catalog);
        }
        // 阶段四 C-8:直方图行并入(P50 / P95 的样本分布)。
        if !hist_rows.is_empty() {
            merge_hist_rows(
                &mut aggregate,
                hist_rows,
                &filters,
                bucket_seconds,
                &catalog,
            );
        }
        let max_tokens = aggregate
            .models
            .iter()
            .map(|(_, row)| row.total.total_tokens)
            .max()
            .unwrap_or(0);
        let models = render_groups(&aggregate.models, max_tokens);
        let client_models = render_groups(&aggregate.client_models, max_tokens);
        let provider_models = render_groups(&aggregate.provider_models, max_tokens);
        complete_buckets(
            &mut aggregate.series,
            bucket_seconds,
            min_moment,
            max_moment,
        );
        for (_, table) in &mut aggregate.model_series.entries {
            complete_buckets(table, bucket_seconds, min_moment, max_moment);
        }
        let totals = &aggregate.totals;
        Ok(object(vec![
            ("clientModels", client_models),
            ("generatedAt", string(Utc::now().to_rfc3339())),
            ("models", models),
            (
                "modelSeries",
                Value::Array(
                    aggregate
                        .model_series
                        .sorted()
                        .into_iter()
                        .map(|(label, table)| {
                            object(vec![
                                ("model", string(label.clone())),
                                ("points", render_buckets(table)),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("providerModels", provider_models),
            ("range", string(range_key)),
            ("recentRequests", Value::Array(aggregate.recent)),
            ("series", render_buckets(&aggregate.series)),
            (
                "totals",
                object(vec![
                    ("avgDurationMs", number(totals.avg_duration())),
                    ("cacheRatio", number(totals.cache_ratio())),
                    ("cacheTokens", Value::from(totals.cache_tokens)),
                    ("costUsd", number(round_even(totals.cost_usd, 10))),
                    ("errorCount", Value::from(totals.error_count)),
                    ("inputTokens", Value::from(totals.input_tokens)),
                    ("outputTokens", Value::from(totals.output_tokens)),
                    ("requestCount", Value::from(totals.request_count)),
                    ("successRate", number(totals.success_rate())),
                    (
                        "avgFirstByteMs",
                        number(totals.avg_first_byte_ms()),
                    ),
                    (
                        "avgDecodeTps",
                        number(totals.avg_decode_tps()),
                    ),
                    ("totalTokens", Value::from(totals.total_tokens)),
                    // 阶段四 C-8:首字延迟 / 吞吐的分位数(无样本 → 0,
                    // 与 `avgFirstByteMs` 的空样本口径一致)。分位数取
                    // 直方图桶的**上界**(保守口径:真实值 ≤ 上界)。
                    (
                        "p50FirstByteMs",
                        number(totals.hist.quantile(METRIC_TTFT, 0.50).unwrap_or(0.0)),
                    ),
                    (
                        "p95FirstByteMs",
                        number(totals.hist.quantile(METRIC_TTFT, 0.95).unwrap_or(0.0)),
                    ),
                    (
                        "p50DecodeTps",
                        number(totals.hist.quantile(METRIC_DECODE_TPS, 0.50).unwrap_or(0.0)),
                    ),
                    (
                        "p95DecodeTps",
                        number(totals.hist.quantile(METRIC_DECODE_TPS, 0.95).unwrap_or(0.0)),
                    ),
                ]),
            ),
        ]))
    }

    /// `runtime_status` 声明在 `UsageStore` 端口、实现在 config store
    /// (Python 的 MRO 事实);`path` 字段在这里补上(需要持有库路径)。
    async fn runtime_status(&self) -> Result<Value, PortError> {
        let path = self.path().display().to_string();
        self.engine
            .read(move |connection| {
                let mut status = super::runtime_status_on(connection)?;
                if let Some(map) = status.as_object_mut() {
                    map.insert("path".to_owned(), string(path));
                }
                Ok(status)
            })
            .await
    }

    /// 纯 SQL 聚合(`json_extract` + `GROUP BY`):路由密钥页每次渲染都走
    /// 这条热路径,不能把整表 payload 拉进内存解析。
    async fn last_used_by_api_key(&self) -> Result<Value, PortError> {
        self.engine
            .read(|connection| {
                let mut statement = connection
                    .prepare(
                        "SELECT json_extract(payload, '$.api_key_id') AS key_id, \
                         MAX(created_at) AS last_used FROM request_logs \
                         WHERE json_extract(payload, '$.api_key_id') IS NOT NULL \
                         AND json_extract(payload, '$.api_key_id') != '' \
                         GROUP BY key_id",
                    )
                    .context("prepare last_used")?;
                let rows = statement
                    .query_map(params![], |row| {
                        Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?))
                    })
                    .context("query last_used")?;
                let mut latest = Map::new();
                for row in rows {
                    let (key_id, last_used) = row.context("row last_used")?;
                    let Some(key_id) = key_id.filter(|value| !value.is_empty()) else {
                        continue;
                    };
                    if let Some(parsed) = parse_iso(&last_used) {
                        latest.insert(key_id, string(iso_format(&parsed)));
                    }
                }
                Ok(Value::Object(latest))
            })
            .await
    }
}

#[cfg(test)]
mod speed_tests;

/// 20261104_01 阶段一 · 计量口径修正(A-2 / A-3 / B-3)的回归断言。
#[cfg(test)]
mod stage_one_metrics_tests;

/// 阶段四 C-8:分位数(直方图)口径。
#[cfg(test)]
mod quantile_tests;
