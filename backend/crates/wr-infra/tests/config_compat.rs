//! C9 兼容集成测试:fixtures 驱动(方案 #14 R1.4 / R1.9)。
//!
//! fixtures 位于 `tests/fixtures/`(路径约定沿用 R7 迁移前的
//! `backend/tests/fixtures`);每份 fixture 对应一类真实/极端配置形态。

use serde_json::Value;
use wr_domain::policy::PolicyEffect;
use wr_infra::config::AppConfig;

fn load_fixture(name: &str) -> Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures").to_owned() + "/" + name;
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("读取 fixture 失败 {path}: {e}"));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("fixture JSON 非法 {path}: {e}"))
}

#[test]
fn defaults_fixture_yields_python_defaults() {
    let cfg = AppConfig::from_json(&load_fixture("config-defaults.json")).unwrap();
    let expected = AppConfig::default();
    assert_eq!(cfg, expected);
}

#[test]
fn current_shape_roundtrip_keys() {
    let cfg = AppConfig::from_json(&load_fixture("config-current-shape.json")).unwrap();
    assert_eq!(cfg.version, 2);
    assert_eq!(cfg.preferred_provider, "deepseek");
    let provider = &cfg.providers[0];
    assert_eq!(provider.name(), Some("DeepSeek"));
    assert_eq!(provider.base_url().as_deref(), Some("https://api.deepseek.com"));
    assert_eq!(provider.protocol(), Some("openai"));
    assert_eq!(cfg.api_keys.len(), 1);
    // 20261101_02 T3:规则键不再读取(Router 仅存 fallback)。
    assert_eq!(
        cfg.router_fallback.get("provider").and_then(Value::as_str),
        Some("deepseek")
    );
    assert_eq!(cfg.theme, "dark");
    assert_eq!(cfg.reliability.circuit_breaker.failure_threshold, 3);
    assert!(cfg.policies.enabled);
    assert_eq!(cfg.policies.default_effect, PolicyEffect::Allow);
    // observability 是不透明 dict(R0 行为契约:callLogs/captureBodies/bodyRetentionDays)。
    assert_eq!(
        cfg.observability
            .get("captureBodies")
            .and_then(Value::as_bool),
        Some(false)
    );
}

#[test]
fn legacy_camel_shape_is_ignored_without_crash() {
    // 旧格式不迁移、不崩溃:唯一可读的顶层键是 `preferredProvider`。
    let cfg = AppConfig::from_json(&load_fixture("config-legacy-camel.json")).unwrap();
    assert_eq!(cfg.version, 2); // max(2, 1)
    assert_eq!(cfg.preferred_provider, "legacy-provider");
    // 旧 `providers` / `API_KEYS` / `routerFallback` / `themePreference`
    // 全部忽略,落回默认。
    assert!(cfg.providers.is_empty());
    assert!(cfg.api_keys.is_empty());
    assert!(cfg.router_fallback.is_empty());
    assert_eq!(cfg.theme, "system");
}

#[test]
fn legacy_camel_reliability_blocks_are_ignored() {
    let cfg = AppConfig::from_json(&load_fixture("config-reliability-camel.json")).unwrap();
    // 旧块名/字段名整体忽略 → 默认值。
    assert_eq!(cfg.reliability.circuit_breaker.failure_threshold, 3);
    assert_eq!(cfg.reliability.circuit_breaker.cooldown_ms, 30_000);
    assert_eq!(cfg.reliability.concurrency.max_in_flight_per_provider, 8);
    assert_eq!(cfg.reliability.idempotency.ttl_ms, 300_000);
    // 块内旧字段忽略,但同一块里的权威字段仍生效(null → str(None) 怪癖)。
    assert_eq!(cfg.reliability.idempotency.header_name, "None");
    assert_eq!(cfg.reliability.health.interval_ms, 30_000);
    // 权威字段 `enabled` 仍生效;旧 camel 字段忽略。
    assert!(cfg.reliability.precheck.enabled);
    assert!(cfg.reliability.precheck.rate_limit_rules.is_empty());
    assert_eq!(cfg.reliability.precheck.budget_max_cost_usd, 0.0);
}

#[test]
fn messy_types_still_parse_like_python() {
    let cfg = AppConfig::from_json(&load_fixture("config-messy-types.json")).unwrap();
    assert_eq!(cfg.version, 3); // 字符串数字
    assert_eq!(cfg.preferred_provider, ""); // 旧 snake_case 键忽略
    assert!(
        !cfg.providers[0]
            .raw
            .get("enabled")
            .map(wr_infra::python_semantics::py_bool)
            .unwrap_or(true)
    );
    assert!(
        cfg.providers[1]
            .raw
            .get("enabled")
            .map(wr_infra::python_semantics::py_bool)
            .unwrap_or(false)
    );
    assert_eq!(cfg.theme, "system"); // 非字符串 → system
    assert_eq!(
        cfg.observability
            .get("callLogs")
            .map(wr_infra::python_semantics::py_bool),
        Some(true)
    );
}

#[test]
fn router_legacy_keys_are_ignored() {
    let cfg = AppConfig::from_json(&load_fixture("config-router-legacy.json")).unwrap();
    // 旧 `router.fallback` / `routerFallback` 链不再读取。
    assert!(cfg.router_fallback.is_empty());
}

#[test]
fn r0_capture_shape_matches_golden_config_writer() {
    // 与 scripts/rebuild/capture_golden.py::_write_config 写入的
    // AppConfig.Providers 形状一致(R0 黄金样本配置样本)。
    let cfg = AppConfig::from_json(&load_fixture("config-r0-capture-shape.json")).unwrap();
    let provider = &cfg.providers[0];
    assert_eq!(provider.name(), Some("gold-1"));
    assert_eq!(provider.base_url().as_deref(), Some("http://127.0.0.1:3999/gold-1"));
    assert_eq!(provider.api_key().as_deref(), Some("upstream-key"));
    assert_eq!(provider.protocol(), Some("openai"));
    assert!(cfg.api_keys.is_empty()); // 该 R0 样本未配置路由密钥
}
