//! schema 兼容层(方案 #14 R4.2,约束 C5:零迁移直读写现有 db 文件)。
//!
//! DDL 逐语句镜像 Python `infrastructure/persistence/sqlite/sqlite_base.py::
//! _initialize`,包括:
//! - 全部 `CREATE TABLE IF NOT EXISTS` / `CREATE INDEX IF NOT EXISTS`
//!   形态(**Python 没有任何 `PRAGMA user_version` / schema_version 表,
//!   也没有编号 migration runner**——升级策略就是「只加新表 + IF NOT
//!   EXISTS」,老库首次打开透明升级。因此 Rust 侧同样不做版本判断、
//!   不做拒绝,见 14 号文档 R4 完成记录的决策回写);
//! - 空库播种 `app_config(id=1)` 一行默认配置 payload;
//! - 表名与列名保持持久化形式(`Providers` / `APIKEYS` 这类**配置 payload
//!   内**的大写键名由 `AppConfig::to_dict` 保证,C5;表名两侧本来就一致)。

use rusqlite::{Connection, OptionalExtension};

use wr_app::ports::PortError;

/// 用量小时卷积 DDL(20261108_01 §13:列名收敛为 `decode_ms_sum` /
/// `decode_tokens`,解码样本按 `decode_source` 分维度)。
pub(crate) const USAGE_ROLLUP_DDL: &str = "CREATE TABLE IF NOT EXISTS usage_rollup (bucket_epoch INTEGER NOT NULL,model TEXT NOT NULL,upstream_model TEXT NOT NULL DEFAULT '',requested_model TEXT NOT NULL DEFAULT '',provider TEXT NOT NULL,client TEXT NOT NULL,api_key_id TEXT NOT NULL,api_key_name TEXT NOT NULL DEFAULT '',request_count INTEGER NOT NULL DEFAULT 0,error_count INTEGER NOT NULL DEFAULT 0,input_tokens INTEGER NOT NULL DEFAULT 0,output_tokens INTEGER NOT NULL DEFAULT 0,total_tokens INTEGER NOT NULL DEFAULT 0,cache_tokens INTEGER NOT NULL DEFAULT 0,cost_usd REAL NOT NULL DEFAULT 0,duration_ms_sum REAL NOT NULL DEFAULT 0,first_byte_ms_sum REAL NOT NULL DEFAULT 0,first_byte_count INTEGER NOT NULL DEFAULT 0,decode_ms_sum REAL NOT NULL DEFAULT 0,decode_tokens INTEGER NOT NULL DEFAULT 0,decode_source TEXT NOT NULL DEFAULT '',PRIMARY KEY (bucket_epoch, model, upstream_model, requested_model, provider, client, api_key_id, api_key_name, decode_source))";
pub(crate) const USAGE_ROLLUP_INDEX_DDL: &str =
    "CREATE INDEX IF NOT EXISTS idx_usage_rollup_bucket ON usage_rollup(bucket_epoch)";

/// TTFT / 解码速度直方图 DDL(20261108_01 §13:`metric` 使用 `ttft_ms` /
/// `decode_tps`,并带 `decode_source` 维度)。
pub(crate) const USAGE_ROLLUP_HIST_DDL: &str = "CREATE TABLE IF NOT EXISTS usage_rollup_hist (bucket_epoch INTEGER NOT NULL,model TEXT NOT NULL,upstream_model TEXT NOT NULL DEFAULT '',requested_model TEXT NOT NULL DEFAULT '',provider TEXT NOT NULL,client TEXT NOT NULL,api_key_id TEXT NOT NULL,api_key_name TEXT NOT NULL DEFAULT '',metric TEXT NOT NULL,bucket INTEGER NOT NULL,sample_count INTEGER NOT NULL DEFAULT 0,decode_source TEXT NOT NULL DEFAULT '',PRIMARY KEY (bucket_epoch, model, upstream_model, requested_model, provider, client, api_key_id, api_key_name, metric, bucket, decode_source))";
pub(crate) const USAGE_ROLLUP_HIST_INDEX_DDL: &str =
    "CREATE INDEX IF NOT EXISTS idx_usage_rollup_hist_bucket ON usage_rollup_hist(bucket_epoch)";

/// 建表/建索引语句(顺序与 Python `_initialize` 一致)。
pub const SCHEMA_STATEMENTS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
    "CREATE TABLE IF NOT EXISTS app_config (id INTEGER PRIMARY KEY CHECK (id = 1), payload TEXT NOT NULL)",
    "CREATE TABLE IF NOT EXISTS request_logs (request_id TEXT PRIMARY KEY, created_at TEXT NOT NULL, payload TEXT NOT NULL)",
    "CREATE TABLE IF NOT EXISTS request_log_bodies (request_id TEXT PRIMARY KEY,created_at TEXT NOT NULL DEFAULT '',request_body TEXT,request_body_size INTEGER,request_body_truncated INTEGER,response_body TEXT,response_body_size INTEGER,response_body_truncated INTEGER,response_headers TEXT)",
    // 20261017_02 批5:attempt 采集侧表 —— 主行 retryAttempts 只留轻量
    // 元数据(captured 标记),上游请求/响应/响应头/转换报告按
    // (request_id, ordinal=attempt 数组下标)落这里,详情读取时按需并回。
    // 2026-11-07(W3-8):只读最新侧表格式;旧内嵌行不再合成,表结构保留。
    "CREATE TABLE IF NOT EXISTS request_log_attempts (request_id TEXT NOT NULL, ordinal INTEGER NOT NULL, created_at TEXT NOT NULL DEFAULT '', payload TEXT NOT NULL, PRIMARY KEY(request_id, ordinal))",
    "CREATE INDEX IF NOT EXISTS idx_request_logs_created_at ON request_logs(created_at)",
    "CREATE INDEX IF NOT EXISTS idx_request_log_bodies_created_at ON request_log_bodies(created_at)",
    "CREATE INDEX IF NOT EXISTS idx_request_log_attempts_created_at ON request_log_attempts(created_at)",
    "CREATE TABLE IF NOT EXISTS request_log_trace (request_id TEXT NOT NULL, ordinal INTEGER NOT NULL, created_at TEXT NOT NULL, kind TEXT NOT NULL, payload TEXT NOT NULL, PRIMARY KEY(request_id, ordinal))",
    "CREATE TABLE IF NOT EXISTS request_log_headers (request_id TEXT PRIMARY KEY, created_at TEXT NOT NULL, request_headers TEXT)",
    "CREATE INDEX IF NOT EXISTS idx_request_log_headers_created_at ON request_log_headers(created_at)",
    "CREATE INDEX IF NOT EXISTS idx_request_log_trace_created_at ON request_log_trace(created_at)",
    "CREATE INDEX IF NOT EXISTS idx_request_log_trace_request ON request_log_trace(request_id, ordinal)",
    "CREATE TABLE IF NOT EXISTS secrets (name TEXT PRIMARY KEY, value TEXT NOT NULL)",
    "CREATE TABLE IF NOT EXISTS circuit_breaker_state (provider_id TEXT PRIMARY KEY,consecutive_failures INTEGER NOT NULL DEFAULT 0,opened_until_ms INTEGER NOT NULL DEFAULT 0,last_state_change_ms INTEGER NOT NULL DEFAULT 0,updated_at TEXT NOT NULL)",
    "CREATE TABLE IF NOT EXISTS provider_health (provider_id TEXT PRIMARY KEY,status TEXT NOT NULL DEFAULT 'unknown',available INTEGER NOT NULL DEFAULT 0,latency_ms INTEGER NOT NULL DEFAULT 0,checked_at TEXT NOT NULL DEFAULT '',error TEXT)",
    "CREATE TABLE IF NOT EXISTS idempotency_entries (store_key TEXT PRIMARY KEY,request_hash TEXT NOT NULL,status_code INTEGER NOT NULL,payload TEXT NOT NULL,headers TEXT NOT NULL,expires_at INTEGER NOT NULL,created_at INTEGER NOT NULL)",
    "CREATE TABLE IF NOT EXISTS provider_accounts (provider_id TEXT NOT NULL,account_id TEXT NOT NULL,label TEXT NOT NULL DEFAULT '',enabled INTEGER NOT NULL DEFAULT 1,priority INTEGER NOT NULL DEFAULT 0,weight INTEGER NOT NULL DEFAULT 1,account_config TEXT NOT NULL DEFAULT '{}',created_at TEXT NOT NULL DEFAULT '',updated_at TEXT NOT NULL DEFAULT '',PRIMARY KEY (provider_id, account_id))",
    "CREATE TABLE IF NOT EXISTS account_snapshots (provider_id TEXT NOT NULL,account_id TEXT NOT NULL DEFAULT '',status TEXT NOT NULL DEFAULT 'unsupported',source TEXT NOT NULL DEFAULT 'unsupported',payload TEXT NOT NULL DEFAULT '{}',fetched_at TEXT NOT NULL DEFAULT '',expires_at INTEGER NOT NULL DEFAULT 0,PRIMARY KEY (provider_id, account_id))",
    // 20260905_03 S6:网关响应缓存磁盘层(内存 LRU 之外的二层;零迁移
    // 策略下新增表,老库首次打开透明建出)。
    "CREATE TABLE IF NOT EXISTS response_cache (cache_key TEXT PRIMARY KEY,payload TEXT NOT NULL,cost_usd REAL NOT NULL DEFAULT 0,model TEXT NOT NULL DEFAULT '',created_at_ms INTEGER NOT NULL DEFAULT 0,expires_at_ms INTEGER NOT NULL DEFAULT 0)",
    // 20260905_03 S8:节省事件(每请求一行,仅记发生了节省行为的;
    // kind = compression | response_cache,prefix_hit 保留不写 —— 上游
    // 缓存账由 request_logs.cache_read_tokens 折算,避免双账)。
    "CREATE TABLE IF NOT EXISTS savings_events (id INTEGER PRIMARY KEY,request_id TEXT NOT NULL,created_at TEXT NOT NULL,kind TEXT NOT NULL,tokens_saved INTEGER NOT NULL DEFAULT 0,estimated_usd_saved REAL NOT NULL DEFAULT 0,actual_tokens_saved INTEGER,engine_breakdown TEXT,skip_reason TEXT)",
    "CREATE INDEX IF NOT EXISTS idx_savings_events_created_at ON savings_events(created_at)",
    // 20261016_01 C-6:用量小时卷积。写日志时按行 UPSERT 增量(维度 =
    // 落库原始字段,展示归一在查询时做),Dashboard 读它替代每次现场
    // 全量聚合。覆盖范围 = MIN(bucket_epoch) 之后的全部行;之前的 历史
    // 行仍按原始日志现场聚合(hybrid,数值与现场聚合精确一致)。
    USAGE_ROLLUP_DDL,
    USAGE_ROLLUP_INDEX_DDL,
    // 20261104_01 阶段四 C-8:TTFT / 解码速度的**直方图**(分位数用)。
    // 与 `usage_rollup` 同维度、同小时桶;`metric` = ttft_ms | decode_tps,
    // `bucket` = 分桶下界,`sample_count` = 该桶样本数。
    // 20261108_01 §13:旧表由 `upgrade_usage_derived` 从原始行重建。
    USAGE_ROLLUP_HIST_DDL,
    USAGE_ROLLUP_HIST_INDEX_DDL,
    // 20261030_01 R11:用户自建模型指纹库(L2 全局候选库 / L3 供应商锚点)。
    // 样本单独成表 —— 重拟合需要
    // 原始样本(方案 §3.5 的全量重拟合语义)。
    "CREATE TABLE IF NOT EXISTS model_fingerprint (id TEXT PRIMARY KEY, kind TEXT NOT NULL, provider_id TEXT, model_name TEXT NOT NULL, family TEXT, label TEXT NOT NULL DEFAULT '', probe_suite TEXT NOT NULL, sample_count INTEGER NOT NULL DEFAULT 0, probe_seed TEXT, probe_env TEXT, cv_accuracy REAL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL)",
    "CREATE TABLE IF NOT EXISTS model_fingerprint_samples (entry_id TEXT NOT NULL, ordinal INTEGER NOT NULL, challenge_id TEXT NOT NULL, condition_id TEXT NOT NULL, expected_count INTEGER NOT NULL DEFAULT 0, text TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT '', PRIMARY KEY (entry_id, ordinal))",
    "CREATE INDEX IF NOT EXISTS idx_model_fingerprint_kind ON model_fingerprint(kind)",
    "CREATE INDEX IF NOT EXISTS idx_model_fingerprint_provider ON model_fingerprint(provider_id)",
];

/// 全部表名(R4.2 兼容性测试与 R4.12 dump 工具用它核对表集合)。
pub const TABLES: &[&str] = &[
    "account_snapshots",
    "app_config",
    "circuit_breaker_state",
    "idempotency_entries",
    "metadata",
    "model_fingerprint",
    "model_fingerprint_samples",
    "provider_accounts",
    "provider_health",
    "request_log_attempts",
    "request_log_bodies",
    "request_log_headers",
    "request_log_trace",
    "request_logs",
    "response_cache",
    "savings_events",
    "secrets",
    "usage_rollup",
    "usage_rollup_hist",
];

/// 建表 + 空库播种(镜像 `_initialize`)。`default_payload` 是播种进
/// `app_config` 的配置 JSON 文本(等价 Python 的
/// `json.dumps(AppConfig().to_dict(True))`)。
///
/// 逐语句 autocommit 执行(与 Python 一致:DDL 不在显式事务里)。
pub fn bootstrap(connection: &Connection, default_payload: &str) -> Result<(), PortError> {
    for statement in SCHEMA_STATEMENTS {
        connection
            .execute_batch(statement)
            .map_err(|e| PortError::storage(format!("schema bootstrap failed: {e}")))?;
    }
    upgrade_usage_derived(connection)?;
    let seeded: Option<i64> = connection
        .query_row("SELECT 1 FROM app_config WHERE id=1", [], |row| row.get(0))
        .optional()
        .map_err(|e| PortError::storage(format!("read app_config failed: {e}")))?;
    if seeded.is_none() {
        connection
            .execute(
                "INSERT INTO app_config(id, payload) VALUES(1, ?1)",
                [default_payload],
            )
            .map_err(|e| PortError::storage(format!("seed app_config failed: {e}")))?;
    }
    Ok(())
}

/// 派生表列名升级(20261108_01 §13)。
///
/// `usage_rollup` / `usage_rollup_hist` 是派生数据,不承载原始事实:
/// 检测到旧列时整表重建,并从 `request_logs` 重放。重建在一个事务内
/// 完成 —— 失败回滚会保留旧表,读取侧(`usage_summary`)检测到旧列后
/// 自动退回原始行聚合(raw-fallback),不影响数值正确性。
fn upgrade_usage_derived(connection: &Connection) -> Result<(), PortError> {
    if table_has_column(connection, "usage_rollup", "decode_tokens")?
        && table_has_column(connection, "usage_rollup_hist", "decode_source")?
    {
        return Ok(());
    }
    let rebuild = || -> Result<(), PortError> {
        let transaction = connection
            .unchecked_transaction()
            .map_err(|e| PortError::storage(format!("begin usage rollup rebuild failed: {e}")))?;
        transaction
            .execute_batch(
                "DROP TABLE IF EXISTS usage_rollup_hist; DROP TABLE IF EXISTS usage_rollup;",
            )
            .map_err(|e| PortError::storage(format!("drop usage derived tables failed: {e}")))?;
        transaction
            .execute_batch(USAGE_ROLLUP_DDL)
            .and_then(|_| transaction.execute_batch(USAGE_ROLLUP_INDEX_DDL))
            .and_then(|_| transaction.execute_batch(USAGE_ROLLUP_HIST_DDL))
            .and_then(|_| transaction.execute_batch(USAGE_ROLLUP_HIST_INDEX_DDL))
            .map_err(|e| {
                PortError::storage(format!("recreate usage derived tables failed: {e}"))
            })?;
        crate::sqlite::request_log_store::rebuild_usage_derived(&transaction)?;
        transaction
            .commit()
            .map_err(|e| PortError::storage(format!("commit usage rollup rebuild failed: {e}")))
    };
    if let Err(error) = rebuild() {
        tracing::warn!(
            target: "wrouter.storage",
            error = %error,
            "usage derived table rebuild failed; keeping legacy tables as backup and falling back to raw logs"
        );
        // 旧表保留为 `*_legacy` 备份(诊断用),新表建空 —— 后续写入
        // 必须用新列名,且空卷积表会让读取侧直接走原始行聚合。
        connection
            .execute_batch(
                "DROP TABLE IF EXISTS usage_rollup_legacy;\
                 DROP TABLE IF EXISTS usage_rollup_hist_legacy;\
                 DROP INDEX IF EXISTS idx_usage_rollup_bucket;\
                 DROP INDEX IF EXISTS idx_usage_rollup_hist_bucket;\
                 ALTER TABLE usage_rollup RENAME TO usage_rollup_legacy;\
                 ALTER TABLE usage_rollup_hist RENAME TO usage_rollup_hist_legacy;",
            )
            .and_then(|_| connection.execute_batch(USAGE_ROLLUP_DDL))
            .and_then(|_| connection.execute_batch(USAGE_ROLLUP_INDEX_DDL))
            .and_then(|_| connection.execute_batch(USAGE_ROLLUP_HIST_DDL))
            .and_then(|_| connection.execute_batch(USAGE_ROLLUP_HIST_INDEX_DDL))
            .map_err(|e| {
                PortError::storage(format!("preserve legacy usage derived tables failed: {e}"))
            })?;
    }
    Ok(())
}

fn table_has_column(connection: &Connection, table: &str, column: &str) -> Result<bool, PortError> {
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name=?2",
            rusqlite::params![table, column],
            |row| row.get(0),
        )
        .map_err(|e| PortError::storage(format!("probe {table}.{column} failed: {e}")))?;
    Ok(count > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn open_temp(name: &str) -> Connection {
        let dir = std::env::temp_dir().join(format!("wr-r4-schema-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.sqlite");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL;")
            .unwrap();
        connection
    }

    fn table_names(connection: &Connection) -> Vec<String> {
        let mut statement = connection
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap();
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap();
        rows.map(|row| row.unwrap()).collect()
    }

    /// R4.2:C5 零迁移——真实旧库夹具(`testdata/golden/sqlite-snapshot/
    /// schema.sql`)建出来的库,Rust bootstrap 必须原样接受(全部
    /// IF NOT EXISTS,不加列、不改表),且表集合与快照一致。
    #[test]
    fn bootstrap_accepts_legacy_snapshot_db_verbatim() {
        let connection = open_temp("legacy");
        let snapshot = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../testdata/golden/sqlite-snapshot/schema.sql");
        let ddl = std::fs::read_to_string(&snapshot)
            .unwrap_or_else(|e| panic!("read fixture {} failed: {e}", snapshot.display()));
        // 夹具是从 sqlite_master 导出的,索引语句排在建表语句之前;
        // 按「先表后索引」回放才能加载(这是夹具的加载顺序问题,与产品无关)。
        let statements: Vec<&str> = ddl
            .split(';')
            .map(str::trim)
            .filter(|statement| !statement.is_empty())
            .collect();
        // pass = false 先跑建表,pass = true 再跑索引。
        for pass in [false, true] {
            for statement in &statements {
                let is_index = statement.to_ascii_lowercase().starts_with("create index");
                if is_index != pass {
                    continue;
                }
                connection.execute_batch(statement).unwrap_or_else(|e| {
                    panic!(
                        "fixture statement failed: {e}
{statement}"
                    )
                });
            }
        }
        bootstrap(&connection, r#"{"version": 2}"#).unwrap();
        let mut expected: Vec<String> = TABLES.iter().map(|name| (*name).to_owned()).collect();
        expected.sort();
        assert_eq!(
            table_names(&connection),
            expected,
            "表集合必须与 R0.4 快照一致"
        );
    }

    #[test]
    fn bootstrap_is_idempotent_and_seeds_once() {
        let connection = open_temp("idempotent");
        bootstrap(&connection, r#"{"version": 2, "seed": 1}"#).unwrap();
        bootstrap(&connection, r#"{"version": 2, "seed": 2}"#).unwrap();
        let payload: String = connection
            .query_row("SELECT payload FROM app_config WHERE id=1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(
            payload.contains("\"seed\": 1"),
            "播种只发生在空库:第二次 bootstrap 不得覆盖既有 payload"
        );
    }

    /// 20261108_01 §13:旧列名派生表在 bootstrap 时从原始日志重放重建。
    #[test]
    fn bootstrap_rebuilds_legacy_usage_derived_tables_from_raw_logs() {
        let connection = open_temp("rollup-upgrade");
        // 旧版 usage_rollup / hist(旧列名、无 decode_source)。
        connection
            .execute_batch(
                "CREATE TABLE usage_rollup (bucket_epoch INTEGER NOT NULL,model TEXT NOT NULL,\
                 upstream_model TEXT NOT NULL DEFAULT '',requested_model TEXT NOT NULL DEFAULT '',\
                 provider TEXT NOT NULL,client TEXT NOT NULL,api_key_id TEXT NOT NULL,\
                 api_key_name TEXT NOT NULL DEFAULT '',request_count INTEGER NOT NULL DEFAULT 0,\
                 error_count INTEGER NOT NULL DEFAULT 0,input_tokens INTEGER NOT NULL DEFAULT 0,\
                 output_tokens INTEGER NOT NULL DEFAULT 0,total_tokens INTEGER NOT NULL DEFAULT 0,\
                 cache_tokens INTEGER NOT NULL DEFAULT 0,cost_usd REAL NOT NULL DEFAULT 0,\
                 duration_ms_sum REAL NOT NULL DEFAULT 0,first_byte_ms_sum REAL NOT NULL DEFAULT 0,\
                 first_byte_count INTEGER NOT NULL DEFAULT 0,gen_ms_sum REAL NOT NULL DEFAULT 0,\
                 throughput_output_tokens INTEGER NOT NULL DEFAULT 0,\
                 PRIMARY KEY (bucket_epoch, model, upstream_model, requested_model, provider, \
                 client, api_key_id, api_key_name));\
                 CREATE TABLE usage_rollup_hist (bucket_epoch INTEGER NOT NULL,model TEXT NOT NULL,\
                 upstream_model TEXT NOT NULL DEFAULT '',requested_model TEXT NOT NULL DEFAULT '',\
                 provider TEXT NOT NULL,client TEXT NOT NULL,api_key_id TEXT NOT NULL,\
                 api_key_name TEXT NOT NULL DEFAULT '',metric TEXT NOT NULL,bucket INTEGER NOT NULL,\
                 sample_count INTEGER NOT NULL DEFAULT 0,\
                 PRIMARY KEY (bucket_epoch, model, upstream_model, requested_model, provider, \
                 client, api_key_id, api_key_name, metric, bucket));\
                 CREATE TABLE request_logs (request_id TEXT PRIMARY KEY, created_at TEXT NOT NULL, \
                 payload TEXT NOT NULL);",
            )
            .unwrap();
        // 旧聚合里的脏行必须被丢弃(派生表不承载原始事实)。
        connection
            .execute(
                "INSERT INTO usage_rollup(bucket_epoch, model, provider, client, api_key_id, request_count) \
                 VALUES(1, 'stale', 'p0', 'c0', 'k0', 99)",
                [],
            )
            .unwrap();
        let payload = serde_json::json!({
            "protocol": "openai", "model": "prov/m1", "provider": "p1",
            "status": 200, "stream": true,
            "duration_ms": 10_000.0, "first_byte_ms": 1_000.0,
            "answer_end_ms": 10_000.0, "first_answer_byte_ms": 1_000.0,
            "flow_ms": 9_000.0, "answer_flow_ms": 9_000.0,
            "input_tokens": 100, "output_tokens": 900, "total_tokens": 1_000,
            "api_key_id": "k1", "api_key_name": "golden"
        });
        connection
            .execute(
                "INSERT INTO request_logs(request_id, created_at, payload) VALUES(?1, ?2, ?3)",
                rusqlite::params!["r1", "2026-11-08 10:05:00", payload.to_string()],
            )
            .unwrap();

        bootstrap(&connection, r#"{"version": 2}"#).unwrap();

        let has_decode_tokens: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('usage_rollup') WHERE name='decode_tokens'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(has_decode_tokens, 1, "旧列必须被新列替换");
        let has_source: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('usage_rollup_hist') WHERE name='decode_source'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(has_source, 1);
        let (model, tokens, ms, source, count): (String, i64, f64, String, i64) = connection
            .query_row(
                "SELECT model, decode_tokens, decode_ms_sum, decode_source, request_count FROM usage_rollup",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .unwrap();
        assert_eq!(model, "prov/m1", "旧聚合行被清掉,只保留原始行重放结果");
        assert_eq!(tokens, 900);
        assert!((ms - 9_000.0).abs() < 1e-9);
        assert_eq!(source, "full_flow");
        assert_eq!(count, 1);
        let decode_hist: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM usage_rollup_hist WHERE metric='decode_tps' AND decode_source='full_flow'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(decode_hist, 1, "直方图 metric 必须写新名 decode_tps");
    }

    /// 重建失败(原始表不可读)时:旧表改名为 `*_legacy` 备份,新表建空,
    /// 后续写入仍使用新列名;读取侧因空卷积表退回原始行聚合。
    #[test]
    fn bootstrap_falls_back_when_rebuild_fails() {
        let connection = open_temp("rollup-rebuild-fail");
        connection
            .execute_batch(
                "CREATE TABLE usage_rollup (bucket_epoch INTEGER NOT NULL,model TEXT NOT NULL,                 upstream_model TEXT NOT NULL DEFAULT '',requested_model TEXT NOT NULL DEFAULT '',                 provider TEXT NOT NULL,client TEXT NOT NULL,api_key_id TEXT NOT NULL,                 api_key_name TEXT NOT NULL DEFAULT '',request_count INTEGER NOT NULL DEFAULT 0,                 error_count INTEGER NOT NULL DEFAULT 0,input_tokens INTEGER NOT NULL DEFAULT 0,                 output_tokens INTEGER NOT NULL DEFAULT 0,total_tokens INTEGER NOT NULL DEFAULT 0,                 cache_tokens INTEGER NOT NULL DEFAULT 0,cost_usd REAL NOT NULL DEFAULT 0,                 duration_ms_sum REAL NOT NULL DEFAULT 0,first_byte_ms_sum REAL NOT NULL DEFAULT 0,                 first_byte_count INTEGER NOT NULL DEFAULT 0,gen_ms_sum REAL NOT NULL DEFAULT 0,                 throughput_output_tokens INTEGER NOT NULL DEFAULT 0,                 PRIMARY KEY (bucket_epoch, model, upstream_model, requested_model, provider,                  client, api_key_id, api_key_name));                 CREATE TABLE usage_rollup_hist (bucket_epoch INTEGER NOT NULL,model TEXT NOT NULL,                 upstream_model TEXT NOT NULL DEFAULT '',requested_model TEXT NOT NULL DEFAULT '',                 provider TEXT NOT NULL,client TEXT NOT NULL,api_key_id TEXT NOT NULL,                 api_key_name TEXT NOT NULL DEFAULT '',metric TEXT NOT NULL,bucket INTEGER NOT NULL,                 sample_count INTEGER NOT NULL DEFAULT 0,                 PRIMARY KEY (bucket_epoch, model, upstream_model, requested_model, provider,                  client, api_key_id, api_key_name, metric, bucket));                 CREATE TABLE request_logs (request_id TEXT PRIMARY KEY, created_at TEXT NOT NULL);",
            )
            .unwrap();

        bootstrap(&connection, r#"{"version": 2}"#).unwrap();

        let new_columns: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('usage_rollup') WHERE name IN ('decode_tokens','decode_ms_sum','decode_source')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(new_columns, 3, "兜底后新表必须使用新列名");
        let legacy_tables: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name LIKE 'usage_rollup%_legacy'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(legacy_tables, 2, "旧派生表以备份形式保留");
        let rows: i64 = connection
            .query_row("SELECT COUNT(*) FROM usage_rollup", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 0, "新卷积表为空 → 读取侧走原始行聚合");
    }
}
