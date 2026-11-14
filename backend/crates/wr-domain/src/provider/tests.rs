
use super::*;
use serde_json::json;

fn provider(value: Value) -> Result<Provider, String> {
    Provider::from_raw(value.as_object().unwrap())
}

#[test]
fn parses_defaults_like_python() {
    let provider = provider(json!({
        "id": "prov-x", "api_base_url": "http://127.0.0.1:3999/x", "protocol": "openai",
    }))
    .unwrap();
    assert_eq!(provider.timeout_seconds, 120.0);
    assert_eq!(provider.max_retries, 3);
    assert!(provider.enabled);
    assert_eq!(provider.name, "prov-x");
    assert_eq!(provider.api_key, "");
}

#[test]
fn retired_provider_aliases_are_ignored() {
    // 旧键不再读取:只有 base_url 不构成合法 provider。
    let legacy = provider(json!({
        "id": "prov-x",
        "base_url": "https://legacy.example",
        "protocol": "openai",
    }))
    .unwrap_err();
    assert!(legacy.contains("base_url"), "{legacy}");
    // 最新键正常读取;旧别名即使并存也不参与。
    let parsed = provider(json!({
        "id": "prov-x",
        "base_url": "https://legacy.example",
        "api_base_url": "https://current.example/anthropic",
        "apikey": "sk-old-spelling",
        "api_key": "sk-new",
        "protocol": "anthropic",
    }))
    .unwrap();
    assert_eq!(parsed.base_url, "https://current.example/anthropic");
    assert_eq!(parsed.api_key, "sk-new");
}

#[test]
fn effective_wire_protocol_merges_family_and_style() {
    // openai 家族按 type 细分;type 缺失/其余家族回落协议家族本身。
    let wire = |protocol: &str, style: Option<&str>| {
        let mut raw = json!({
            "id": "prov-x", "api_base_url": "http://x/v1", "protocol": protocol,
        });
        if let Some(style) = style {
            raw["type"] = json!(style);
        }
        provider(raw).unwrap().effective_wire_protocol().to_owned()
    };
    assert_eq!(wire("openai", Some("openai_responses")), "openai_responses");
    assert_eq!(
        wire("openai", Some("openai_chat_completions")),
        "openai"
    );
    assert_eq!(wire("openai", None), "openai");
    assert_eq!(wire("anthropic", Some("anthropic_messages")), "anthropic");
    assert_eq!(wire("gemini", None), "gemini");
    // type 大小写/空白宽容,与读取侧现算口径一致。
    assert_eq!(wire("openai", Some(" OpenAI_Responses ")), "openai_responses");
}

#[test]
fn parses_camel_and_overrides() {
    let provider = provider(json!({
        "id": "prov-x", "name": "X", "api_base_url": "https://api.x.com/v1",
        "protocol": "anthropic", "enabled": false, "models": ["m1"],
        "timeoutSeconds": 30, "maxRetries": 0, "maxInFlight": 4,
        "headers": {"anthropic-version": "2023-06-01"}, "api_key": "sk-1",
    }))
    .unwrap();
    assert_eq!(provider.timeout_seconds, 30.0);
    assert_eq!(provider.max_retries, 0);
    assert_eq!(provider.max_in_flight, Some(4));
    assert!(!provider.enabled);
    assert_eq!(provider.models, vec!["m1"]);
    assert_eq!(provider.api_key, "sk-1");
}

#[test]
fn string_zero_max_retries_is_zero_like_python_int() {
    let provider = provider(json!({
        "id": "prov-x", "api_base_url": "http://x/v1", "max_retries": "0",
    }))
    .unwrap();
    assert_eq!(provider.max_retries, 0);
}

#[test]
fn non_stream_timeout_optional_and_positive_guarded() {
    // 缺省 None(upstream 侧落 600s 默认);camelCase 别名同效。
    let bare = provider(json!({"id": "prov-x", "api_base_url": "http://x/v1"})).unwrap();
    assert_eq!(bare.non_stream_timeout_seconds, None);
    let camel = provider(json!({
        "id": "prov-x", "api_base_url": "http://x/v1", "nonStreamTimeoutSeconds": 900,
    }))
    .unwrap();
    assert_eq!(camel.non_stream_timeout_seconds, Some(900.0));
    // 非正数宽容视为未设置(0 会让 timeout 立即触发,等价于必败)。
    let zero = provider(json!({
        "id": "prov-x", "api_base_url": "http://x/v1", "non_stream_timeout_seconds": 0,
    }))
    .unwrap();
    assert_eq!(zero.non_stream_timeout_seconds, None);
    // to_dict 仅显式设置时出站。
    assert!(
        bare.to_dict(true)
            .get("non_stream_timeout_seconds")
            .is_none()
    );
    assert_eq!(
        camel.to_dict(true)["non_stream_timeout_seconds"],
        json!(900.0)
    );
}

#[test]
fn rejects_unsupported_provider_protocol() {
    let err = provider(json!({
        "id": "prov-x", "api_base_url": "http://x/v1", "protocol": "openai_responses",
    }))
    .unwrap_err();
    assert!(err.contains("Unsupported provider protocol"));
}

#[test]
fn rejects_bad_id_and_base_url() {
    assert!(provider(json!({"id": "", "api_base_url": "http://x"})).is_err());
    assert!(provider(json!({"id": "a/b", "api_base_url": "http://x"})).is_err());
    assert!(provider(json!({"id": "ok", "api_base_url": "ftp://x"})).is_err());
}

#[test]
fn rejects_wrong_typed_models_like_python_type_error() {
    let err = provider(json!({
        "id": "prov-x", "api_base_url": "http://x/v1", "models": null,
    }))
    .unwrap_err();
    assert!(err.contains("models"));
}

// ---- 20260905_02 K1:凭据策略字段(宽容解析) --------------------------

#[test]
fn credential_strategy_defaults_to_first() {
    let provider = provider(json!({
        "id": "prov-x", "api_base_url": "http://x/v1",
    }))
    .unwrap();
    assert_eq!(
        provider.credential_strategy,
        ProviderCredentialStrategy::First
    );
    assert_eq!(provider.pinned_account_id, None);
}

#[test]
fn credential_strategy_parses_snake_and_camel() {
    for (raw_key, raw_value, expected) in [
        (
            "credential_strategy",
            "pinned",
            ProviderCredentialStrategy::Pinned,
        ),
        (
            "credentialStrategy",
            "weighted",
            ProviderCredentialStrategy::Weighted,
        ),
        (
            "credential_strategy",
            "sticky",
            ProviderCredentialStrategy::Sticky,
        ),
        (
            "credentialStrategy",
            "FIRST",
            ProviderCredentialStrategy::First,
        ),
    ] {
        let provider = provider(json!({
            "id": "prov-x", "api_base_url": "http://x/v1",
            raw_key: raw_value,
        }))
        .unwrap();
        assert_eq!(
            provider.credential_strategy, expected,
            "{raw_key}={raw_value}"
        );
    }
}

#[test]
fn credential_strategy_invalid_value_falls_back_to_first() {
    // 宽容解析:非法值不报 Err,回退默认(方案 §3.1)。
    let provider = provider(json!({
        "id": "prov-x", "api_base_url": "http://x/v1", "credentialStrategy": "round-robin",
    }))
    .unwrap();
    assert_eq!(
        provider.credential_strategy,
        ProviderCredentialStrategy::First
    );
}

// ---- 20261014_01:URL 模式字段(宽容解析 + full 原样保留) ----------

#[test]
fn url_mode_defaults_to_base() {
    // 存量配置无该字段:缺省必须落 Base,行为与升级前完全一致。
    let provider = provider(json!({
        "id": "prov-x", "api_base_url": "http://x/anthropic/",
    }))
    .unwrap();
    assert_eq!(provider.url_mode, UrlMode::Base);
    // base 模式维持尾斜杠剥除(endpoint 拼接依赖)。
    assert_eq!(provider.base_url, "http://x/anthropic");
}

#[test]
fn url_mode_parses_snake_and_camel() {
    for (raw_key, expected) in [("url_mode", UrlMode::Full), ("urlMode", UrlMode::Full)] {
        let provider = provider(json!({
            "id": "prov-x", "api_base_url": "http://x/v1", raw_key: "full",
        }))
        .unwrap();
        assert_eq!(provider.url_mode, expected, "{raw_key}");
    }
}

#[test]
fn url_mode_invalid_value_falls_back_to_base() {
    // 宽容解析:非法值不报 Err,回退默认(与 credential_strategy 口径一致)。
    let provider = provider(json!({
        "id": "prov-x", "api_base_url": "http://x/v1", "urlMode": "verbatim",
    }))
    .unwrap();
    assert_eq!(provider.url_mode, UrlMode::Base);
}

#[test]
fn url_mode_full_preserves_trailing_slash_and_query() {
    // full 模式原样保留:尾斜杠与 query 都是用户语义,任何一层不得改写。
    let provider = provider(json!({
        "id": "prov-x", "api_base_url": "https://gw.example.com/llm/claude/?edition=us",
        "urlMode": "full",
    }))
    .unwrap();
    assert_eq!(provider.url_mode, UrlMode::Full);
    assert_eq!(
        provider.base_url,
        "https://gw.example.com/llm/claude/?edition=us"
    );
}

#[test]
fn pinned_account_id_accepts_aliases_and_ignores_garbage() {
    let camel = provider(json!({
        "id": "prov-x", "api_base_url": "http://x/v1",
        "pinnedAccountId": "acct-1",
    }))
    .unwrap();
    assert_eq!(camel.pinned_account_id.as_deref(), Some("acct-1"));
    let snake = provider(json!({
        "id": "prov-x", "api_base_url": "http://x/v1",
        "pinned_account_id": "acct-2",
    }))
    .unwrap();
    assert_eq!(snake.pinned_account_id.as_deref(), Some("acct-2"));
    // 空串与非字符串:宽容为未指定。
    for junk in [json!(""), json!(null), json!(17)] {
        let junk_provider = provider(json!({
            "id": "prov-x", "api_base_url": "http://x/v1", "pinnedAccountId": junk,
        }))
        .unwrap();
        assert_eq!(junk_provider.pinned_account_id, None);
    }
}

// ---- 20260906_01 B1:供应商级代理覆盖(宽松解析) ---------------------

#[test]
fn proxy_absent_means_inherit_none() {
    let provider = provider(json!({
        "id": "prov-x", "api_base_url": "http://x/v1",
    }))
    .unwrap();
    assert_eq!(provider.proxy, None);
    // to_dict 不产出 proxy 键(None = inherit,出站省略)。
    assert!(provider.to_dict(true).get("proxy").is_none());
}

#[test]
fn proxy_parses_direct_and_preset() {
    let direct = provider(json!({
        "id": "prov-x", "api_base_url": "http://x/v1",
        "proxy": {"mode": "direct"},
    }))
    .unwrap();
    assert_eq!(
        direct.proxy,
        Some(ProviderProxyOverride {
            mode: ProviderProxyMode::Direct,
            preset_id: None
        })
    );
    // presetId 的 camel / snake 别名都可读。
    for key in ["presetId", "preset_id"] {
        let preset = provider(json!({
            "id": "prov-x", "api_base_url": "http://x/v1",
            "proxy": {"mode": "preset", key: "clash-local"},
        }))
        .unwrap();
        assert_eq!(
            preset.proxy,
            Some(ProviderProxyOverride {
                mode: ProviderProxyMode::Preset,
                preset_id: Some("clash-local".to_owned())
            }),
            "{key}"
        );
    }
    // 出站形态:mode + presetId(camelCase)。
    let preset = provider(json!({
        "id": "prov-x", "api_base_url": "http://x/v1",
        "proxy": {"mode": "preset", "presetId": "p1"},
    }))
    .unwrap();
    assert_eq!(
        preset.to_dict(true).get("proxy"),
        Some(&json!({"mode": "preset", "presetId": "p1"}))
    );
}

#[test]
fn proxy_invalid_shapes_normalize_to_inherit() {
    // 决策 #7:代理配置错误不该让 provider 整体加载失败。
    for junk in [
        json!("direct"),
        json!(null),
        json!([]),
        json!({}),
        json!({"mode": "inherit"}),
        json!({"mode": "inherit", "presetId": "p1"}),
        json!({"mode": "round-robin"}),
        json!({"mode": "preset"}),
        json!({"mode": "preset", "presetId": "  "}),
        json!({"mode": "preset", "presetId": 17}),
    ] {
        let parsed = provider(json!({
            "id": "prov-x", "api_base_url": "http://x/v1", "proxy": junk,
        }))
        .unwrap();
        assert_eq!(parsed.proxy, None, "junk={junk}");
    }
}

#[test]
fn proxy_mode_parse_is_case_insensitive_and_strict() {
    assert_eq!(
        ProviderProxyMode::parse("DIRECT"),
        ProviderProxyMode::Direct
    );
    assert_eq!(
        ProviderProxyMode::parse(" preset "),
        ProviderProxyMode::Preset
    );
    assert_eq!(
        ProviderProxyMode::parse("custom"),
        ProviderProxyMode::Inherit
    );
    assert_eq!(ProviderProxyMode::Inherit.as_str(), "inherit");
}

// ---- 20261016_03:客户端指纹字段(宽松解析) -------------------------

#[test]
fn client_fingerprint_absent_means_off() {
    let provider = provider(json!({
        "id": "prov-x", "api_base_url": "http://x/v1",
    }))
    .unwrap();
    assert_eq!(provider.client_fingerprint, None);
    // to_dict 不产出 clientFingerprint 键(None = off,出站省略)。
    assert!(provider.to_dict(true).get("clientFingerprint").is_none());
}

#[test]
fn client_fingerprint_parses_passthrough_and_preset() {
    let passthrough = provider(json!({
        "id": "prov-x", "api_base_url": "http://x/v1",
        "clientFingerprint": {"mode": "passthrough"},
    }))
    .unwrap();
    assert_eq!(
        passthrough.client_fingerprint,
        Some(ClientFingerprintOverride {
            mode: ClientFingerprintMode::Passthrough,
            preset_id: None,
        })
    );
    // presetId 的 camel / snake 别名都可读。
    for key in ["presetId", "preset_id"] {
        let preset = provider(json!({
            "id": "prov-x", "api_base_url": "http://x/v1",
            "clientFingerprint": {"mode": "preset", key: "zcode"},
        }))
        .unwrap();
        assert_eq!(
            preset.client_fingerprint,
            Some(ClientFingerprintOverride {
                mode: ClientFingerprintMode::Preset,
                preset_id: Some("zcode".to_owned()),
            }),
            "{key}"
        );
    }
    // 出站形态:mode + presetId(camelCase)。
    let preset = provider(json!({
        "id": "prov-x", "api_base_url": "http://x/v1",
        "clientFingerprint": {"mode": "preset", "presetId": "claude-code"},
    }))
    .unwrap();
    assert_eq!(
        preset.to_dict(true).get("clientFingerprint"),
        Some(&json!({"mode": "preset", "presetId": "claude-code"}))
    );
}

#[test]
fn protocol_style_is_enum_validated() {
    for value in [
        "openai_responses",
        "openai_chat_completions",
        " OpenAI_Responses ",
    ] {
        let parsed = provider(json!({
            "id": "prov-x", "api_base_url": "http://x/v1", "type": value,
        }))
        .unwrap();
        assert!(parsed.protocol_style.is_some(), "{value}");
    }
    for junk in [json!("azure"), json!("responses"), json!(17), json!([])] {
        assert!(
            provider(json!({
                "id": "prov-x", "api_base_url": "http://x/v1", "type": junk,
            }))
            .is_err(),
            "type={junk}"
        );
    }
}

#[test]
fn client_fingerprint_invalid_shapes_normalize_to_off() {
    // 宽松解析:配置错误不该让 provider 整体加载失败。
    for junk in [
        json!("passthrough"),
        json!(null),
        json!([]),
        json!({}),
        json!({"mode": "off"}),
        json!({"mode": "off", "presetId": "zcode"}),
        json!({"mode": "impersonate"}),
        json!({"mode": "preset"}),
        json!({"mode": "preset", "presetId": "  "}),
        json!({"mode": "preset", "presetId": 17}),
    ] {
        let parsed = provider(json!({
            "id": "prov-x", "api_base_url": "http://x/v1", "clientFingerprint": junk,
        }))
        .unwrap();
        assert_eq!(parsed.client_fingerprint, None, "junk={junk}");
    }
}
