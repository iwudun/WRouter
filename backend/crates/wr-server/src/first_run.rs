//! 首次使用初始化(数据面强制密钥 + 首启种入默认路由密钥)。
//!
//! dev-open 逃生门移除后(见 `gateway/auth.rs`),空注册表会拒绝一切匿名
//! 请求。为避免全新安装 / 老库升级后被锁死,组合根在启动时调用
//! [`ensure_default_routing_key`]:当且仅当注册表为空且未种过(一次性
//! 标志 `default_routing_key_seeded`)时,生成一个默认路由密钥并持久化。
//!
//! 决策 D2:标志位置位后不再自动重建——用户主动删光全部密钥即保持为空,
//! 网关对所有数据面请求返回 401,直到手动新建。

use serde_json::{Value, json};

use wr_app::ports::ConfigStore;
use wr_infra::config::AppConfig;
use wr_infra::sqlite::SqliteStore;

/// 一次性种入标志(metadata 键)。置位后 [`ensure_default_routing_key`] 恒短路。
const SEEDED_FLAG: &str = "default_routing_key_seeded";

/// 默认路由密钥的展示名。
const DEFAULT_KEY_NAME: &str = "Default Routing Key";

/// 首启种入默认路由密钥(幂等;见模块文档的 D2 语义)。
///
/// 与 admin `persist_snapshot` 同款写路径:明文 payload → `save_payload`
/// (路由密钥明文入 `app_config.payload`,与既有 `saveApiKeys` 行为一致)。
pub async fn ensure_default_routing_key(store: &SqliteStore) -> anyhow::Result<()> {
    // 已种过 → 尊重用户后续删除,直接短路(D2)。
    if store.get_metadata(SEEDED_FLAG).await?.as_deref() == Some("true") {
        return Ok(());
    }
    let payload = store.load_payload().await?;
    let mut config = AppConfig::from_json(&payload).unwrap_or_default();
    if config.api_keys.is_empty() {
        config.api_keys.push(generate_default_routing_key());
        let text = config.to_payload_text(true);
        let value: Value = serde_json::from_str(&text)?;
        store.save_payload(&value).await?;
        tracing::info!(target: "wrouter.cli", "首次使用:已创建默认路由密钥");
    }
    // 无论是否生成,都置标志(覆盖全新安装与老库升级;此后不再自动创建)。
    store.set_metadata(SEEDED_FLAG, "true").await?;
    Ok(())
}

/// 生成默认路由密钥条目(与前端 `ApiKeyConfig` 形态对齐:永不过期、不限
/// 模型、不限额,避免触发 `api_key_model_error` 白名单拒绝)。
fn generate_default_routing_key() -> Value {
    let key = format!("sk-{}", uuid::Uuid::new_v4().simple());
    let id_uuid = uuid::Uuid::new_v4().simple().to_string();
    json!({
        "id": format!("key_{}", &id_uuid[..12]),
        "key": key,
        "name": DEFAULT_KEY_NAME,
        "enabled": true,
        "createdAt": chrono::Utc::now().to_rfc3339(),
    })
}
