//! 首启默认路由密钥种入测试(需求 B;决策 D2)。
//!
//! 覆盖:空库种入一条默认密钥 + 置标志;二次调用不重复;已有密钥不新增
//! (仅置标志);用户删光后(标志已置)不重建。

use serde_json::{Value, json};

use wr_app::ports::ConfigStore;
use wr_infra::config::AppConfig;
use wr_infra::sqlite::SqliteStore;
use wr_server::first_run::ensure_default_routing_key;

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "wr-first-run-tests-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn api_keys_of(store: &SqliteStore) -> Vec<Value> {
    let payload = store.load_payload().await.unwrap();
    payload
        .get("APIKEYS")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

async fn seeded_flag(store: &SqliteStore) -> Option<String> {
    store
        .get_metadata("default_routing_key_seeded")
        .await
        .unwrap()
}

async fn write_config(store: &SqliteStore, config: &AppConfig) {
    let value: Value = serde_json::from_str(&config.to_payload_text(true)).unwrap();
    store.save_payload(&value).await.unwrap();
}

#[tokio::test]
async fn empty_db_seeds_one_default_key_and_sets_flag() {
    let dir = temp_dir("seed");
    let store = SqliteStore::open(&dir).await.unwrap();
    // 空库 bootstrap 后注册表为空、标志缺省。
    assert!(api_keys_of(&store).await.is_empty());
    assert!(seeded_flag(&store).await.is_none());

    ensure_default_routing_key(&store).await.unwrap();

    let keys = api_keys_of(&store).await;
    assert_eq!(keys.len(), 1, "空库首启应种入恰一条默认密钥");
    let key = keys[0].as_object().unwrap();
    assert_eq!(
        key.get("name").and_then(Value::as_str),
        Some("Default Routing Key")
    );
    assert_eq!(key.get("enabled").and_then(Value::as_bool), Some(true));
    assert!(key.get("key").and_then(Value::as_str).unwrap().starts_with("sk-"));
    // 不设 expiresAt / models / limits(不限模型,避免白名单拒绝)。
    assert!(key.get("expiresAt").is_none());
    assert!(key.get("models").is_none());
    assert!(key.get("limits").is_none());
    assert_eq!(seeded_flag(&store).await.as_deref(), Some("true"));

    store.close().await;
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn second_call_does_not_duplicate() {
    let dir = temp_dir("dup");
    let store = SqliteStore::open(&dir).await.unwrap();
    ensure_default_routing_key(&store).await.unwrap();
    ensure_default_routing_key(&store).await.unwrap();
    assert_eq!(api_keys_of(&store).await.len(), 1, "二次调用不得重复种入");
    store.close().await;
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn existing_keys_are_preserved_and_flag_set() {
    let dir = temp_dir("existing");
    let store = SqliteStore::open(&dir).await.unwrap();
    // 模拟用户已自行配置密钥(标志尚未置位)。
    let mut config = AppConfig::default();
    config.api_keys.push(json!({
        "id": "key_user", "key": "sk-user-1", "name": "User Key", "enabled": true
    }));
    write_config(&store, &config).await;
    assert!(seeded_flag(&store).await.is_none());

    ensure_default_routing_key(&store).await.unwrap();

    let keys = api_keys_of(&store).await;
    assert_eq!(keys.len(), 1, "已有密钥时不得新增默认密钥");
    assert_eq!(keys[0].get("id").and_then(Value::as_str), Some("key_user"));
    assert_eq!(
        seeded_flag(&store).await.as_deref(),
        Some("true"),
        "已有密钥也应置标志(此后不再自动创建)"
    );

    store.close().await;
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn emptied_registry_with_flag_set_is_not_reseeded() {
    let dir = temp_dir("d2");
    let store = SqliteStore::open(&dir).await.unwrap();
    ensure_default_routing_key(&store).await.unwrap();
    assert_eq!(api_keys_of(&store).await.len(), 1);

    // 模拟用户删光全部密钥:写入空 APIKEYS(标志仍为 true)。
    write_config(&store, &AppConfig::default()).await;
    assert!(api_keys_of(&store).await.is_empty());

    // D2:标志已置 → 不重建。
    ensure_default_routing_key(&store).await.unwrap();
    assert!(
        api_keys_of(&store).await.is_empty(),
        "D2:删光后不得自动重建默认密钥"
    );

    store.close().await;
    std::fs::remove_dir_all(&dir).ok();
}
