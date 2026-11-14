use super::AppConfig;
use serde_json::{Map, Value, json};
use wr_domain::policy::PolicyEffect;

#[test]
fn empty_object_yields_defaults() {
    // Python: from_dict({}) → version=max(2,1)=2,其余全默认。
    let cfg = AppConfig::from_json(&json!({})).unwrap();
    assert_eq!(cfg.version, 2);
    assert_eq!(cfg.preferred_provider, "");
    assert!(cfg.providers.is_empty());
    assert!(cfg.api_keys.is_empty());
    assert_eq!(cfg.theme, "system");
    assert!(!cfg.launch_at_login);
    assert!(cfg.policies.enabled);
    assert_eq!(cfg.policies.default_effect, PolicyEffect::Allow);
    let rb = &cfg.reliability.circuit_breaker;
    assert_eq!(rb.failure_threshold, 3);
    assert_eq!(rb.cooldown_ms, 30_000);
    assert_eq!(rb.failure_status_codes, vec![408, 429, 500, 502, 503, 504]);
    assert_eq!(cfg.reliability.concurrency.max_in_flight_per_provider, 8);
    assert_eq!(cfg.reliability.concurrency.queue_timeout_ms, 0);
    assert_eq!(cfg.reliability.idempotency.ttl_ms, 300_000);
    assert_eq!(cfg.reliability.idempotency.header_name, "idempotency-key");
    assert_eq!(cfg.reliability.health.initial_delay_ms, 5000);
    assert!(!cfg.reliability.precheck.enabled);
    assert!(cfg.reliability.metrics.include_provider_health);
}

#[test]
fn top_level_non_object_is_error() {
    assert!(AppConfig::from_json(&json!([1, 2])).is_err());
    assert!(AppConfig::from_json(&json!("x")).is_err());
}

/// 20261102_01 P4-3:开机自启开关必须能存能取。
///
/// 回归背景:该键此前**根本不在** `to_dict` 的写出集合里 → 前端
/// `saveConfig` 送来的 `launchAtLogin` 被静默丢弃 → `getConfig` 永远
/// 读不回来 → 桌面壳的「开机自启」开关点了等于没点。
#[test]
fn launch_at_login_round_trips() {
    let cfg = AppConfig::from_json(&json!({ "launchAtLogin": true })).unwrap();
    assert!(cfg.launch_at_login);
    let dict = cfg.to_dict(false);
    assert_eq!(dict.get("launchAtLogin"), Some(&json!(true)));

    // 落库文本 → 再解析一次,值不丢(模拟 save → load 往返)。
    let text = cfg.to_payload_text(false);
    let reloaded = AppConfig::from_json(&serde_json::from_str::<Value>(&text).unwrap()).unwrap();
    assert!(reloaded.launch_at_login);
}

/// 缺省 / 非布尔 → false(旧配置零迁移,不报错)。
#[test]
fn launch_at_login_defaults_to_false() {
    assert!(!AppConfig::from_json(&json!({})).unwrap().launch_at_login);
    assert!(!AppConfig::from_json(&json!({ "launchAtLogin": "yes" }))
        .unwrap()
        .launch_at_login);
}

#[test]
fn preferred_provider_reads_latest_key_only() {
    // 只认 to_dict 写出的 `preferredProvider`;旧 snake_case 键忽略。
    let cfg = AppConfig::from_json(
        &json!({"preferred_provider": "old", "preferredProvider": "x"}),
    )
    .unwrap();
    assert_eq!(cfg.preferred_provider, "x");
    let cfg = AppConfig::from_json(&json!({"preferred_provider": "old"})).unwrap();
    assert_eq!(cfg.preferred_provider, "");
    // 假值(0)→ x or "" → ""。
    let cfg = AppConfig::from_json(&json!({"preferredProvider": 0})).unwrap();
    assert_eq!(cfg.preferred_provider, "");
    // 数值真值 → str() 化。
    let cfg = AppConfig::from_json(&json!({"preferredProvider": 5})).unwrap();
    assert_eq!(cfg.preferred_provider, "5");
}

#[test]
fn provider_retired_aliases_are_ignored() {
    let cfg = AppConfig::from_json(&json!({
        "Providers": [
            {"name": "a", "baseUrl": "https://a", "apiKey": "sk-1", "icon": "x", "enabled": true},
            {"name": "b", "api_base_url": "https://b", "base_url": "https://b2", "apikey": "sk-old", "api_key": "sk-new"},
            {"name": "c", "base_url": "https://legacy-only", "credentials": [{"id": "k1"}]},
            {"name": "d", "credentials": [{"id": "old"}], "accounts": [{"id": "new"}]}
        ]
    }))
    .unwrap();
    let [a, b, c, d] = cfg.providers.as_slice() else {
        panic!("expected 4 providers")
    };
    // 旧别名不再读取(不迁移、不兜底)。
    assert_eq!(a.base_url(), None);
    assert_eq!(a.api_key(), None);
    assert_eq!(c.base_url(), None);
    assert!(c.raw.get("accounts").is_none());
    // 最新键正常读取。
    assert_eq!(b.base_url().as_deref(), Some("https://b"));
    assert_eq!(b.api_key().as_deref(), Some("sk-new"));
    assert_eq!(
        d.raw.get("accounts").and_then(|v| v.as_array()).map(|a| a[0]["id"].clone()),
        Some(json!("new"))
    );
    // raw 原样保留:加载不自动改写用户数据。
    assert_eq!(a.raw.get("icon"), Some(&json!("x")));
    assert!(a.raw.get("baseUrl").is_some());
    assert!(c.raw.get("credentials").is_some());
}

#[test]
fn version_clamped_and_coerced() {
    assert_eq!(
        AppConfig::from_json(&json!({"version": 0}))
            .unwrap()
            .version,
        2
    );
    assert_eq!(
        AppConfig::from_json(&json!({"version": 5}))
            .unwrap()
            .version,
        5
    );
    assert_eq!(
        AppConfig::from_json(&json!({"version": 5.9}))
            .unwrap()
            .version,
        5
    );
    assert_eq!(
        AppConfig::from_json(&json!({"version": "3"}))
            .unwrap()
            .version,
        3
    );
    assert!(AppConfig::from_json(&json!({"version": "abc"})).is_err());
}

#[test]
fn theme_reads_latest_key_and_normalizes() {
    assert_eq!(
        AppConfig::from_json(&json!({"theme": " Dark "}))
            .unwrap()
            .theme,
        "dark"
    );
    // 旧键 `themePreference` 不再读取。
    assert_eq!(
        AppConfig::from_json(&json!({"theme": "", "themePreference": "light"}))
            .unwrap()
            .theme,
        "system"
    );
    assert_eq!(
        AppConfig::from_json(&json!({"theme": "auto"}))
            .unwrap()
            .theme,
        "system"
    );
}

#[test]
fn policies_non_object_falls_back_to_default() {
    let cfg = AppConfig::from_json(&json!({"policies": "bogus"})).unwrap();
    assert_eq!(cfg.policies, super::PolicyConfig::default());
    let cfg =
        AppConfig::from_json(&json!({"policies": {"defaultEffect": "DENY", "enabled": false}}))
            .unwrap();
    assert_eq!(cfg.policies.default_effect, PolicyEffect::Deny);
    assert!(!cfg.policies.enabled);
}

#[test]
fn reliability_authoritative_keys_and_clamps() {
    let cfg = AppConfig::from_json(&json!({
        "reliability": {
            "circuit_breaker": {"failure_threshold": 0, "cooldown_ms": "60000"},
            "concurrency": {"max_in_flight_per_provider": 0},
            "idempotency": {"ttl_ms": 500, "header_name": null},
            "health": {"interval_ms": 0, "initial_delay_ms": -5, "timeout_ms": 100}
        }
    }))
    .unwrap();
    let rb = &cfg.reliability.circuit_breaker;
    assert_eq!(rb.failure_threshold, 1); // max(1, 0)
    assert_eq!(rb.cooldown_ms, 60_000); // 字符串数字
    assert_eq!(cfg.reliability.concurrency.max_in_flight_per_provider, 1);
    assert_eq!(cfg.reliability.idempotency.ttl_ms, 1000); // max(1000, 500)
    assert_eq!(cfg.reliability.idempotency.header_name, "None"); // str(None) 怪癖,Python 对齐
    assert_eq!(cfg.reliability.health.interval_ms, 1000);
    assert_eq!(cfg.reliability.health.initial_delay_ms, 0); // max(0, -5)
    assert_eq!(cfg.reliability.health.timeout_ms, 500);
}

#[test]
fn retired_reliability_aliases_are_ignored() {
    let cfg = AppConfig::from_json(&json!({
        "reliability": {
            "upstreamCircuitBreaker": {"failure_threshold": 0, "cooldownMs": "60000"},
            "upstreamConcurrency": {"maxInFlightPerProvider": 0},
            "idempotency": {"ttlMs": 500},
            "providerHealthCheck": {"intervalMs": 0}
        }
    }))
    .unwrap();
    // 旧块名/旧字段名整体忽略,落回默认值。
    assert_eq!(
        cfg.reliability.circuit_breaker,
        super::CircuitBreakerConfig::default()
    );
    assert_eq!(
        cfg.reliability.concurrency,
        super::ConcurrencyConfig::default()
    );
    assert_eq!(cfg.reliability.idempotency.ttl_ms, 300_000);
    assert_eq!(cfg.reliability.health.interval_ms, 30_000);
}

#[test]
fn reliability_falsy_sub_blocks_fall_back_to_defaults() {
    // Python: value = value or {} —— 假值子块静默落回默认,不报错。
    let cfg = AppConfig::from_json(&json!({
        "reliability": {"idempotency": false, "metrics": null}
    }))
    .unwrap();
    assert_eq!(
        cfg.reliability.idempotency,
        super::IdempotencyConfig::default()
    );
    assert_eq!(cfg.reliability.metrics, super::MetricsConfig::default());
}

#[test]
fn router_fallback_reads_latest_block_only() {
    let cfg = AppConfig::from_json(&json!({
        "Router": {"rules": [{"stale": true}], "fallback": {"provider": "x"}},
        "routingRules": [{"stale": true}]
    }))
    .unwrap();
    assert_eq!(
        cfg.router_fallback.get("provider").and_then(|v| v.as_str()),
        Some("x")
    );
    // 旧 `router.fallback` / `routerFallback` 链不再读取。
    let cfg = AppConfig::from_json(&json!({
        "routingRules": [{"match": "gpt-*"}],
        "routerFallback": {"provider": "legacy"},
        "router": {"fallback": {"provider": "legacy"}}
    }))
    .unwrap();
    assert!(cfg.router_fallback.is_empty());
    // Router.fallback 为 null(假值)→ 空表,不报错。
    let cfg = AppConfig::from_json(&json!({"Router": {"fallback": null}})).unwrap();
    assert!(cfg.router_fallback.is_empty());
}

#[test]
fn proxy_null_is_error_but_observability_null_is_default() {
    // Python: dict(migrated.get("proxy", {})) vs dict(migrated.get("observability") or {})。
    assert!(AppConfig::from_json(&json!({"proxy": null})).is_err());
    let cfg = AppConfig::from_json(&json!({"observability": null})).unwrap();
    assert!(cfg.observability.is_empty());
}

// ---- 20260905_03:savings 全局配置 --------------------------------------

#[test]
fn savings_defaults_are_off_and_disabled() {
    // 缺失 / 非 dict → 全默认:压缩 off(必须显式开启)、响应缓存关、
    // 粘性仅 TTL(全局无开关)。
    for raw in [
        json!({}),
        json!({"savings": "bogus"}),
        json!({"savings": null}),
    ] {
        let cfg = AppConfig::from_json(&raw).unwrap();
        assert_eq!(cfg.savings, super::SavingsConfig::default());
    }
    let cfg = AppConfig::from_json(&json!({})).unwrap();
    assert_eq!(cfg.savings.compression.mode, "off");
    assert_eq!(cfg.savings.compression.threshold_tokens, 32_000);
    assert!(!cfg.savings.response_cache.enabled);
    assert_eq!(cfg.savings.response_cache.memory_max_entries, 64);
    assert_eq!(cfg.savings.response_cache.memory_max_bytes, 5 * 1024 * 1024);
    assert_eq!(cfg.savings.response_cache.memory_ttl_secs, 1800);
    assert_eq!(cfg.savings.response_cache.disk_max_entries, 1000);
    assert_eq!(cfg.savings.response_cache.disk_ttl_secs, 3600);
    // 20261101_02 T3-B:粘性/钉扎全局开关默认关(规则域下线后的新契约)。
    assert!(!cfg.savings.sticky.enabled);
    assert_eq!(cfg.savings.sticky.ttl_secs, 900);
    assert!(!cfg.savings.prefix_pin.enabled);
}

#[test]
fn savings_parses_authoritative_keys_and_falls_back_leniently() {
    let cfg = AppConfig::from_json(&json!({
        "savings": {
            "compression": {"mode": "auto", "thresholdTokens": "1000"},
            "responseCache": {
                "enabled": true,
                "memoryMaxEntries": 8,
                "memoryMaxBytes": 1024,
                "memoryTtlSecs": -5,
                "diskMaxEntries": 0,
            },
            "sticky": {"ttlSecs": 60},
        }
    }))
    .unwrap();
    assert_eq!(cfg.savings.compression.mode, "auto");
    assert_eq!(cfg.savings.compression.threshold_tokens, 1000);
    assert!(cfg.savings.response_cache.enabled);
    assert_eq!(cfg.savings.response_cache.memory_max_entries, 8);
    assert_eq!(cfg.savings.response_cache.memory_max_bytes, 1024);
    assert_eq!(cfg.savings.response_cache.memory_ttl_secs, 0, "负值钳到 0");
    // 未覆盖的键保持默认。
    assert_eq!(cfg.savings.response_cache.disk_ttl_secs, 3600);
    assert_eq!(cfg.savings.sticky.ttl_secs, 60);
    // 坏值宽松回退:mode 未知 → off;非 int 数字 → 默认。
    let cfg = AppConfig::from_json(&json!({
        "savings": {
            "compression": {"mode": "bogus", "thresholdTokens": "abc"},
            "sticky": {"ttlSecs": null},
        }
    }))
    .unwrap();
    assert_eq!(cfg.savings.compression.mode, "off");
    assert_eq!(cfg.savings.compression.threshold_tokens, 32_000);
    assert_eq!(cfg.savings.sticky.ttl_secs, 900);
    // 旧 snake_case 子键不再读取。
    let cfg = AppConfig::from_json(&json!({
        "savings": {
            "compression": {"threshold_tokens": 1},
            "responseCache": {"memoryMaxBytes": 1024, "memory_max_entries": 8},
            "sticky": {"ttl_secs": 60}
        }
    }))
    .unwrap();
    assert_eq!(cfg.savings.compression.threshold_tokens, 32_000);
    assert_eq!(cfg.savings.response_cache.memory_max_entries, 64);
    assert_eq!(cfg.savings.sticky.ttl_secs, 900);
}

#[test]
fn savings_to_dict_key_order_and_roundtrip() {
    let cfg = AppConfig::from_json(&json!({
        "savings": {"compression": {"mode": "full"}, "sticky": {"ttlSecs": 30}}
    }))
    .unwrap();
    let dict = cfg.to_dict(true);
    let keys: Vec<&String> = dict.as_object().unwrap().keys().collect();
    // savings 插在 reliability 之后、theme 之前。
    let rel = keys.iter().position(|k| *k == "reliability").unwrap();
    let sav = keys.iter().position(|k| *k == "savings").unwrap();
    let theme = keys.iter().position(|k| *k == "theme").unwrap();
    assert!(
        rel < sav && sav < theme,
        "键序应为 reliability < savings < theme"
    );
    assert_eq!(dict["savings"]["compression"]["mode"], json!("full"));
    assert_eq!(
        dict["savings"]["compression"]["thresholdTokens"],
        json!(32_000)
    );
    assert_eq!(dict["savings"]["sticky"]["ttlSecs"], json!(30));
    assert_eq!(dict["savings"]["responseCache"]["enabled"], json!(false));
    // to_dict → from_json 往返。
    let parsed = AppConfig::from_json(&dict).unwrap();
    assert_eq!(parsed.savings, cfg.savings);
}

// ---- 20260906_01 B3:命名预设 + 保存期校验 ---------------------------

fn proxy_of(value: Value) -> Map<String, Value> {
    AppConfig::from_json(&json!({ "proxy": value }))
        .unwrap()
        .proxy
}

#[test]
fn retired_proxy_modes_are_preserved_but_resolve_direct() {
    // 旧 `custom` / `system` 不再迁移或构造代理;加载不崩溃、不改写,
    // 解析时按未知 mode 直连。
    for mode in ["custom", "system"] {
        let proxy = proxy_of(json!({
            "upstream": {
                "mode": mode,
                "custom": {"server": "127.0.0.1", "port": 7890}
            }
        }));
        assert_eq!(proxy["upstream"]["mode"], json!(mode));
        assert_eq!(
            crate::proxy::resolve_proxy_decision(Some(&proxy), "https://api.x.com/v1"),
            crate::proxy::ProxyDecision::Direct
        );
    }
}

#[test]
fn non_custom_proxy_modes_are_untouched() {
    for mode in ["none", "preset", "bogus", "system"] {
        let proxy = proxy_of(json!({"upstream": {"mode": mode, "presetId": "p1"}}));
        assert_eq!(proxy["upstream"]["mode"], json!(mode));
        assert_eq!(proxy["upstream"]["presetId"], json!("p1"));
        assert!(proxy.get("presets").is_none());
    }
    // 无 upstream 段 / proxy 段缺失都不报错。
    let proxy = proxy_of(json!({"mode": "gateway", "port": 7891}));
    assert_eq!(proxy["mode"], json!("gateway"));
    assert!(AppConfig::from_json(&json!({})).unwrap().proxy.is_empty());
}

#[test]
fn validate_presets_rejects_self_reference() {
    // 决策 #8 / 验收 #6:指向网关自身监听端口 → 拒绝保存。
    for server in ["127.0.0.1", "localhost", "[::1]", "127.0.0.7"] {
        let proxy = json!({
            "presets": [{"id": "p", "name": "n", "server": server, "port": 3460}]
        })
        .as_object()
        .unwrap()
        .clone();
        let err = super::validate_proxy_presets(&proxy, 3460)
            .unwrap_err()
            .to_string();
        assert!(err.contains("打环"), "{server} → {err}");
    }
    // 本机 Clash 等正常 loopback 代理不受影响。
    let ok = json!({
        "presets": [{"id": "p", "name": "n", "server": "127.0.0.1", "port": 7890}]
    })
    .as_object()
    .unwrap()
    .clone();
    assert!(super::validate_proxy_presets(&ok, 3460).is_ok());
    // server 自带端口时以它判定(不看 port 字段)。
    let self_ref = json!({
        "presets": [{"id": "p", "name": "n", "server": "http://127.0.0.1:3460", "port": 7890}]
    })
    .as_object()
    .unwrap()
    .clone();
    assert!(super::validate_proxy_presets(&self_ref, 3460).is_err());
    // 非 loopback 的同端口不算自指(远端机器上的同端口网关)。
    let remote = json!({
        "presets": [{"id": "p", "name": "n", "server": "gw.corp", "port": 3460}]
    })
    .as_object()
    .unwrap()
    .clone();
    assert!(super::validate_proxy_presets(&remote, 3460).is_ok());
}

#[test]
fn validate_presets_enforces_identity_and_shape() {
    let cases: Vec<(Value, &str)> = vec![
        (
            json!({"presets": [{"name": "n", "server": "a", "port": 1}]}),
            "id 不能为空",
        ),
        (
            json!({"presets": [{"id": "a/b", "name": "n", "server": "a", "port": 1}]}),
            "path-safe",
        ),
        (
            json!({"presets": [
                {"id": "p", "name": "n", "server": "a", "port": 1},
                {"id": "p", "name": "m", "server": "b", "port": 2}
            ]}),
            "id 重复",
        ),
        (
            json!({"presets": [
                {"id": "p", "name": "Clash", "server": "a", "port": 1},
                {"id": "q", "name": "clash", "server": "b", "port": 2}
            ]}),
            "名称已存在",
        ),
        (
            json!({"presets": [{"id": "p", "name": " ", "server": "a", "port": 1}]}),
            "名称不能为空",
        ),
        (
            json!({"presets": [{"id": "p", "name": "n", "server": "", "port": 1}]}),
            "代理地址不能为空",
        ),
        (
            json!({"presets": [{"id": "p", "name": "n", "server": "ftp://a", "port": 21}]}),
            "scheme 仅支持",
        ),
        (
            json!({"presets": [{"id": "p", "name": "n", "server": "a", "port": 0}]}),
            "端口须在 1-65535",
        ),
        (json!({"presets": ["junk"]}), "必须是对象"),
    ];
    for (value, expected) in cases {
        let proxy = value.as_object().unwrap().clone();
        let err = super::validate_proxy_presets(&proxy, 3460)
            .unwrap_err()
            .to_string();
        assert!(err.contains(expected), "期望含「{expected}」,实际「{err}」");
    }
    // 决策 #5:预设面接受 socks5/socks5h。
    for server in ["socks5://127.0.0.1", "socks5h://proxy.lan", "https://proxy.lan"] {
        let proxy = json!({
            "presets": [{"id": "p", "name": "n", "server": server, "port": 1080}]
        })
        .as_object()
        .unwrap()
        .clone();
        assert!(
            super::validate_proxy_presets(&proxy, 3460).is_ok(),
            "{server}"
        );
    }
    // 无 presets 段 → 通过(旧配置零迁移)。
    let proxy = json!({"upstream": {"mode": "none"}}).as_object().unwrap().clone();
    assert!(super::validate_proxy_presets(&proxy, 3460).is_ok());
}
