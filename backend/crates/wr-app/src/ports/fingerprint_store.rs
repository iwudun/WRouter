//! 模型指纹库端口(方案 20261030_01 R10)。
//!
//! 覆盖用户自建指纹库的持久化面(L2 全局候选库 / L3 供应商锚点)。
//! 条目与样本全部来自用户自建库。
//!
//! 端口约定(与全仓一致):
//! - async 方法一律 `#[async_trait]`(dyn 对象安全,装配注入与测试替身需要);
//! - `Send + Sync` 超界:管理面在 tokio 多线程运行时下并发持有 `Arc<dyn …>`;
//! - 载荷以 `serde_json::Value` 传递(镜像 Python 侧的 `dict` 入参/返回)。
//!
//! **样本与条目分表**:条目行存元数据,样本行存原始回答文本。分开存是为了
//! 支持**全量重拟合**(方案 §3.5):新增/删除条目后全局库与该供应商的全部
//! 锚点都要重算,而重算需要原始样本 —— 只存拟合产物无法重拟合。

use async_trait::async_trait;
use serde_json::Value;

use super::error::PortError;

/// 指纹条目类别:L2 全局候选库。
pub const KIND_GLOBAL: &str = "global";
/// 指纹条目类别:L3 供应商锚点。
pub const KIND_ANCHOR: &str = "anchor";

#[async_trait]
pub trait FingerprintStore: Send + Sync {
    /// 列出条目。
    ///
    /// `kind` 为 `Some("global"|"anchor")` 时按类别过滤;`provider_id` 为
    /// `Some(id)` 时只列该供应商下的锚点;两者都 `None` 时列全部。
    async fn list_entries(
        &self,
        kind: Option<&str>,
        provider_id: Option<&str>,
    ) -> Result<Vec<Value>, PortError>;

    /// 读单条条目(含其样本);不存在返回 `None`。
    async fn get_entry(&self, id: &str) -> Result<Option<Value>, PortError>;

    /// 写入/更新一个条目及其样本(整体替换语义)。
    ///
    /// 事务语义:条目行与样本行同事务写入,失败不留半成品。
    async fn upsert_entry(&self, entry: &Value) -> Result<(), PortError>;

    /// 删除条目及其样本。返回是否真的删掉了东西。
    async fn delete_entry(&self, id: &str) -> Result<bool, PortError>;

    /// 列出全部条目(含样本)——建库时一次性取全量。
    async fn all_entries(&self) -> Result<Vec<Value>, PortError>;
}
