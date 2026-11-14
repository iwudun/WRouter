//! SQLite 只读配置源(R2 前置;镜像 Python `infrastructure/persistence/
//! sqlite/` 的读取路径)。
//!
//! golden 工具链(seed_config.py,原镜像 Python `SqliteStore.save_config`
//! 的播种行为)在 `<data_dir>/config.sqlite` 播种配置,因此 Rust 网关必须
//! 能直接读同一个文件(C5 零迁移):
//! - `app_config` 表 id=1 行的 `payload` 列 = 完整配置 JSON
//!   (Python `to_dict(True)` 形态,provider 的 `api_key` 恒为空串);
//! - `secrets` 表 `provider:{id}:api_key` = 真实上游密钥
//!   (`providers()` 装配时注入,镜像 Python ProviderSelector)。
//!
//! 完整读写(R4)与 request_logs 落库(R4)不在本模块。

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::{Map, Value};

use crate::config::AppConfig;

/// `<data_dir>/config.sqlite` 的只读访问器。
#[derive(Debug, Clone)]
pub struct SqliteConfigSource {
    path: PathBuf,
}

impl SqliteConfigSource {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join("config.sqlite"),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 读 `app_config` payload。文件不存在或行不存在时返回空对象
    /// (Python 侧 SqliteStore 初始化空库即默认配置;C9 兼容)。
    pub fn load_payload(&self) -> Result<Value, String> {
        if !self.path.exists() {
            return Ok(Value::Object(Map::new()));
        }
        let conn = self.open_readonly()?;
        let text: Option<String> = conn
            .query_row("SELECT payload FROM app_config WHERE id=1", [], |row| {
                row.get(0)
            })
            .optional()
            .map_err(|e| format!("read app_config failed: {e}"))?;
        match text {
            Some(text) => serde_json::from_str(&text)
                .map_err(|e| format!("app_config payload is not valid JSON: {e}")),
            None => Ok(Value::Object(Map::new())),
        }
    }

    /// 读单个 provider 密钥(镜像 `SecretStore.provider_secret`)。
    pub fn provider_secret(&self, provider_id: &str) -> Option<String> {
        if !self.path.exists() {
            return None;
        }
        let conn = self.open_readonly().ok()?;
        conn.query_row(
            "SELECT value FROM secrets WHERE name=?",
            [format!("provider:{provider_id}:api_key")],
            |row| row.get::<_, String>(0),
        )
        .ok()
    }

    /// 装配完整 `AppConfig`:payload → C9 兼容解析,再为 api_key 为空的
    /// provider 注入 secrets 密钥(镜像 Python `ProviderSelector.providers`)。
    pub fn load_app_config(&self) -> Result<AppConfig, String> {
        let payload = self.load_payload()?;
        let mut config = AppConfig::from_json(&payload).map_err(|e| e.to_string())?;
        for provider in &mut config.providers {
            let has_key = provider
                .raw
                .get("api_key")
                .and_then(Value::as_str)
                .map(str::is_empty)
                != Some(true)
                && provider
                    .raw
                    .get("api_key")
                    .and_then(Value::as_str)
                    .is_some();
            if has_key {
                continue;
            }
            let Some(id) = provider.raw.get("id").and_then(Value::as_str) else {
                continue;
            };
            if let Some(secret) = self.provider_secret(id) {
                provider
                    .raw
                    .insert("api_key".to_owned(), Value::String(secret));
            }
        }
        Ok(config)
    }

    fn open_readonly(&self) -> Result<Connection, String> {
        Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| format!("open {} failed: {e}", self.path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 用 rusqlite 造一个与 Python SqliteStore 同形态的库。
    fn seed(dir: &Path, payload: &Value, secret: Option<(&str, &str)>) -> PathBuf {
        let path = dir.join("config.sqlite");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE app_config (id INTEGER PRIMARY KEY CHECK (id = 1), payload TEXT NOT NULL);
             CREATE TABLE secrets (name TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO app_config(id, payload) VALUES(1, ?1)",
            [payload.to_string()],
        )
        .unwrap();
        if let Some((name, value)) = secret {
            conn.execute(
                "INSERT INTO secrets(name, value) VALUES(?1, ?2)",
                [(name.to_string()), (value.to_string())],
            )
            .unwrap();
        }
        path
    }

    #[test]
    fn reads_payload_and_injects_secrets() {
        let dir = std::env::temp_dir().join(format!("wr-r2-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let payload = json!({
            "version": 2,
            "Providers": [
                {"id": "prov-a", "name": "a", "api_base_url": "http://127.0.0.1:1/a",
                 "protocol": "openai", "models": ["m-golden"], "api_key": ""},
                {"id": "prov-b", "name": "b", "api_base_url": "http://127.0.0.1:1/b",
                 "protocol": "openai", "models": ["m-golden"], "api_key": "inline-key"},
            ],
            "APIKEYS": [{"key": "sk-golden-1", "enabled": true, "name": "golden"}],
        });
        seed(
            &dir,
            &payload,
            Some(("provider:prov-a:api_key", "secret-a")),
        );
        let source = SqliteConfigSource::new(&dir);
        let config = source.load_app_config().unwrap();
        assert_eq!(config.providers.len(), 2);
        assert_eq!(config.providers[0].api_key().as_deref(), Some("secret-a"));
        assert_eq!(config.providers[1].api_key().as_deref(), Some("inline-key"));
        assert_eq!(config.api_keys.len(), 1);
        assert_eq!(
            source.provider_secret("prov-a"),
            Some("secret-a".to_owned())
        );
        assert_eq!(source.provider_secret("missing"), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_db_behaves_like_default_config() {
        let dir = std::env::temp_dir().join(format!("wr-r2-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = SqliteConfigSource::new(&dir);
        let config = source.load_app_config().unwrap();
        assert!(config.providers.is_empty());
        assert!(config.api_keys.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
