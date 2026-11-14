//! 应用层端口错误(镜像 Python 端口实现抛出的 `sqlite3.Error` / `ValueError`)。
//!
//! 端口不携带底层驱动错误类型:`wr-app` 不得依赖 rusqlite(D14 依赖方向),
//! 具体实现(`wr-infra`)负责把 `rusqlite::Error` 映射成这两个变体。

use thiserror::Error;

#[derive(Debug, Error)]
pub enum PortError {
    /// 存储层失败(SQLite I/O、约束、解析)。镜像 Python 侧直接冒泡的
    /// `sqlite3.Error`:调用方一律「记录并跳过」,不重试、不降级。
    #[error("storage error: {0}")]
    Storage(String),

    /// 端口契约违反(参数非法)。镜像 Python 侧的 `ValueError`
    /// (如 `save_health` 缺 `provider_id`、`upsert_account` 空 id)。
    #[error("{0}")]
    Invalid(String),
}

impl PortError {
    pub fn storage(message: impl Into<String>) -> Self {
        Self::Storage(message.into())
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }
}
