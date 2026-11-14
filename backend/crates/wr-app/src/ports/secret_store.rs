//! Secret store 端口(镜像 `application/ports/secret_store.py`)。
//!
//! 覆盖两个域:单凭据 provider secret(`secrets` 表的
//! `provider:{id}:api_key`)与 A5 多账号凭据池(`provider_accounts` +
//! `provider:{pid}:account:{aid}:api_key` + `account_snapshots`)。
//! 读取方法恒返回空串表示「无密钥」(镜像 Python 的 `-> str` 契约);
//! 密钥值一律不落普通日志(全局 C 系列约束)。

use async_trait::async_trait;
use serde_json::{Map, Value};

use super::error::PortError;

/// 多账号 upsert 入参(镜像 `upsert_account` 的关键字参数)。
#[derive(Debug, Clone)]
pub struct AccountUpsert<'a> {
    pub provider_id: &'a str,
    pub account_id: &'a str,
    /// 空串 → 保留既有密钥(C2 同族语义)。
    pub api_key: &'a str,
    pub label: &'a str,
    pub enabled: bool,
    pub priority: i64,
    pub weight: i64,
    /// `account_config` 列的 JSON 形态;`None` 等价于 `null`(落 `{}`)。
    pub account_config: Option<&'a Value>,
}

/// 账号余额/配额快照入参(镜像 `upsert_account_snapshot`)。
#[derive(Debug, Clone)]
pub struct AccountSnapshotInput<'a> {
    pub provider_id: &'a str,
    pub account_id: &'a str,
    pub status: &'a str,
    pub source: &'a str,
    pub payload: &'a Map<String, Value>,
    /// 快照有效时长(毫秒),决定 `expires_at`。
    pub ttl_ms: i64,
}

#[async_trait]
pub trait SecretStore: Send + Sync {
    /// 写入/更新 provider 级 api_key;**空 api_key 不写**(镜像 Python 的
    /// `if api_key:` 短路)。
    async fn upsert_provider_secret(
        &self,
        provider_id: &str,
        api_key: &str,
    ) -> Result<(), PortError>;

    /// 读 provider 级 api_key;缺失返回空串。
    async fn provider_secret(&self, provider_id: &str) -> Result<String, PortError>;

    async fn delete_provider_secret(&self, provider_id: &str) -> Result<(), PortError>;

    /// 多账号池 upsert。`provider_id`/`account_id` 任一为空串 → 抛
    /// [`PortError::Invalid`](镜像 Python 的 `ValueError("upsert_account
    /// requires non-empty provider_id and account_id")`)。
    async fn upsert_account(&self, input: &AccountUpsert<'_>) -> Result<(), PortError>;

    async fn account_secret(
        &self,
        provider_id: &str,
        account_id: &str,
    ) -> Result<String, PortError>;

    /// 列出某 provider 的全部账号行(不含密钥)。
    async fn list_accounts(&self, provider_id: &str) -> Result<Vec<Value>, PortError>;

    async fn delete_account(&self, provider_id: &str, account_id: &str) -> Result<(), PortError>;

    async fn delete_provider_accounts(&self, provider_id: &str) -> Result<(), PortError>;

    async fn upsert_account_snapshot(
        &self,
        input: &AccountSnapshotInput<'_>,
    ) -> Result<(), PortError>;

    /// 返回缓存快照(`{status, source, payload, fetched_at, expires_at}`)
    /// 或 `None`;**过期判定留给调用方**。
    async fn get_account_snapshot(
        &self,
        provider_id: &str,
        account_id: &str,
    ) -> Result<Option<Value>, PortError>;
}
