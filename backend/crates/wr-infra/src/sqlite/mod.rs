//! SQLite 存储层(方案 #14 R4;约束 C5 零迁移)。
//!
//! 结构镜像 Python `infrastructure/persistence/sqlite/`:一个
//! [`SqliteStore`] 门面实现 `wr-app` 声明的全部端口trait(Python 侧
//! `SqliteStore` 是多继承聚合六个 per-aggregate store,Rust 用
//! 「一个结构体 + 六个 `impl 端口`」等价呈现,方法逐文件切开便于
//! 评审对照)。
//!
//! - [`engine`]:R4.1 rusqlite 基建(专用写线程 + 读者 spawn_blocking);
//! - [`schema`]:R4.2 schema 兼容层(零 migration、透明建表);
//! - [`config_store`]:R4.2/R4.3 配置读写 + C2 api_key 三态 + metadata;
//! - [`secret_store`]:R4.3 密钥表与 A5 多账号凭据池行;
//! - [`request_log_store`]:R4.5 请求日志落库与分档保留;
//! - [`usage_store`]:R4.6 用量聚合(从 request_logs 派生,无独立用量表);
//! - [`breaker_health_store`]:R4.8 熔断/健康状态持久化;
//! - [`idempotency_store`]:R4.4 幂等条目持久化。
//!
//! [`crate::sqlite_config::SqliteConfigSource`] 保留为**只读**访问器
//! (parity 工具链经 Python `SqliteStore.save_config` 播种、R2/R3 网关快照
//! 读它);两者共用 [`inject_provider_secrets`] 以避免读写两条路径上的
//! 密钥注入语义漂移。

pub mod breaker_health_store;
pub mod config_store;
pub mod engine;
pub mod fingerprint_store;
pub mod idempotency_store;
pub mod request_log_store;
pub mod response_cache_store;
pub mod savings_store;
pub mod schema;
pub mod secret_store;
pub mod usage_store;

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::{Map, Value};

use wr_app::ports::PortError;

use crate::config::AppConfig;
pub use engine::DbEngine;

/// 存储层门面:独占一个 [`DbEngine`],实现全部持久化端口。
#[derive(Debug, Clone)]
pub struct SqliteStore {
    engine: DbEngine,
    /// provider 目录缓存(20261016_01 C-1):键 = `app_config` 原文,
    /// 配置未变时复用解析产物(此前每次调用日志/Usage 查询都重读并
    /// 重建目录)。
    catalog_cache: std::sync::Arc<std::sync::Mutex<Option<request_log_store::CatalogCacheSlot>>>,
    /// 调用日志投影缓存(20261016_01 C-5):键 = 最新 rowid + 短 TTL;
    /// prune/正文迁移显式失效。消除每次刷新对最近 2000 行的全量 JSON
    /// 解析与投影。
    call_log_cache:
        std::sync::Arc<std::sync::Mutex<Option<request_log_store::CallLogCacheSlot>>>,
}

impl SqliteStore {
    /// 打开(必要时创建)`<data_dir>/config.sqlite`:建表 + 空库播种默认
    /// 配置(C5:对既有库只补 `IF NOT EXISTS` 的新表,零迁移)。
    pub async fn open(data_dir: &Path) -> Result<Self, PortError> {
        let path: PathBuf = data_dir.join("config.sqlite");
        let engine = DbEngine::open(&path).await?;
        let default_payload = AppConfig::default().to_payload_text(true);
        engine
            .write_autocommit(move |connection: &Connection| {
                schema::bootstrap(connection, &default_payload)
            })
            .await?;
        Ok(Self::from_engine(engine))
    }

    /// 由既有引擎构造(测试与组合根共享引擎时用)。
    pub fn from_engine(engine: DbEngine) -> Self {
        Self {
            engine,
            catalog_cache: std::sync::Arc::new(std::sync::Mutex::new(None)),
            call_log_cache: std::sync::Arc::new(std::sync::Mutex::new(None)),
        }
    }

    pub fn engine(&self) -> &DbEngine {
        &self.engine
    }

    pub fn path(&self) -> &Path {
        self.engine.path()
    }

    /// 停机等价物:排空写队列后关闭写线程(R4.11「停机不丢」)。
    pub async fn close(&self) {
        self.engine.close().await;
    }
}

/// 把 `secrets` 里的 provider 密钥注入 payload 中 api_key 为空的
/// provider(镜像 Python `ProviderSelector.providers()` 的装配步)。
///
/// 判定与既有 `SqliteConfigSource::load_app_config` 保持一致:
/// `api_key` 缺失或为空串才算「需要注入」;非空字符串(含 inline 密钥)
/// 保留原值。
pub fn inject_provider_secrets(
    config: &mut AppConfig,
    mut secret_of: impl FnMut(&str) -> Option<String>,
) {
    for provider in &mut config.providers {
        let blank = match provider.raw.get("api_key") {
            None => true,
            Some(Value::String(text)) => text.is_empty(),
            Some(_) => false,
        };
        if !blank {
            continue;
        }
        let Some(id) = provider.raw.get("id").and_then(Value::as_str) else {
            continue;
        };
        if let Some(secret) = secret_of(id) {
            provider
                .raw
                .insert("api_key".to_owned(), Value::String(secret));
        }
    }
}

/// `rusqlite` 结果 → 端口错误的统一收口(Python 侧 `sqlite3.Error`
/// 直接冒泡,应用层一律「记录并跳过」,因此这里不需要更细的分类)。
pub(crate) trait SqlContext<T> {
    fn context(self, ctx: &str) -> Result<T, PortError>;
}

impl<T, E: std::fmt::Display> SqlContext<T> for Result<T, E> {
    fn context(self, ctx: &str) -> Result<T, PortError> {
        self.map_err(|error| PortError::storage(format!("{ctx}: {error}")))
    }
}

/// 轻量运行态计数(RPC `getStatus`)。端口面把它声明在 `UsageStore`,实现
/// 落在 config 域 —— 与 Python 的 MRO 事实一致(`runtime_status` 定义在
/// `SqliteConfigStore`,由聚合 `SqliteStore` 继承满足 `UsageStore` 协议)。
pub(crate) fn runtime_status_on(connection: &Connection) -> Result<Value, PortError> {
    let count = |sql: &str| -> Result<i64, PortError> {
        connection
            .query_row(sql, [], |row| row.get::<_, i64>(0))
            .map_err(|e| PortError::storage(format!("{sql}: {e}")))
    };
    Ok(object(vec![
        ("store", Value::String("sqlite".to_owned())),
        (
            "configCount",
            Value::from(count("SELECT COUNT(*) FROM app_config")?),
        ),
        (
            "providerSecretCount",
            Value::from(count(
                "SELECT COUNT(*) FROM secrets WHERE name LIKE 'provider:%'",
            )?),
        ),
        (
            "requestLogCount",
            Value::from(count("SELECT COUNT(*) FROM request_logs")?),
        ),
    ]))
}

/// 行对象便捷构造(保持 Python dict 的插入序语义)。
pub(crate) fn object(entries: Vec<(&str, Value)>) -> Value {
    let mut map = Map::new();
    for (key, value) in entries {
        map.insert(key.to_owned(), value);
    }
    Value::Object(map)
}
