//! Config store 端口(镜像 `application/ports/config_store.py`)。
//!
//! 与 Python 的一处端口级差异:Python 的 `load_config/save_config` 收发
//! `AppConfig` 实体,而 Rust 侧 `AppConfig` 住在 `wr-infra`(R1.4 落点)。
//! 端口若引用它就会让 `wr-app → wr-infra`,破坏 D14 依赖方向,因此本端口
//! 收发 config payload JSON(C9/C5 键名形态),`AppConfig` 的解析与序列化
//! 留在装配层(`wr-server`)。

use async_trait::async_trait;
use serde_json::Value;

use super::error::PortError;

/// `app_config` 单行 + `metadata` 键值位的持久化端口。
#[async_trait]
pub trait ConfigStore: Send + Sync {
    /// 读 `app_config` payload(C9/C5 键名形态)。库内无该行时返回空对象
    /// (镜像 Python 侧「空库即默认配置」)。
    async fn load_payload(&self) -> Result<Value, PortError>;

    /// 写配置 payload,并按 C2 三态语义同步 `secrets` 与 `provider_accounts`
    /// (镜像 `SqliteConfigStore.save_config`:非空非 `"***"` → upsert;
    /// `null` → 删除;缺失或空串 → 不动)。落库 payload 的 `api_key` 一律
    /// 置空串——真实密钥只存在于 `secrets`。
    async fn save_payload(&self, payload: &Value) -> Result<(), PortError>;

    /// 读 `metadata` 单键;缺失返回 `None`。
    async fn get_metadata(&self, key: &str) -> Result<Option<String>, PortError>;

    /// upsert `metadata` 单键。
    async fn set_metadata(&self, key: &str, value: &str) -> Result<(), PortError>;

    async fn get_onboarding_finished(&self) -> Result<bool, PortError>;

    async fn set_onboarding_finished(&self, finished: bool) -> Result<(), PortError>;
}
