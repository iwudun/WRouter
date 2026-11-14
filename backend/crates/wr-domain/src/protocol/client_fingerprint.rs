//! 客户端指纹(方案 20261016_03):出站请求的客户端身份层。
//!
//! 三级优先级的中间层(`provider.headers` 显式配置 > 指纹 passthrough /
//! preset > 自动注入认证/版本),由 [`super::upstream_headers_with_fingerprint`]
//! 以 `or_insert` 语义并入 —— 指纹不覆盖显式配置,又不让位给自动注入。
//!
//! 域模型:白名单按**上游**线格式分域(通用域 / anthropic 域 / responses
//! 域),跨协议转发时 anthropic 特征头不外带。认证红线:任何指纹源
//! (preset 模板 / 入站透传)都不得携带认证头 —— 客户端打 WRouter 的
//! 网关凭据绝不能上送上游(§3.3,硬编码 + 白名单双重防线)。
//!
//! 预设为数据而非逻辑:内置指纹表 [`builtin_presets`] 来自
//! `presets/*.json`(`include_str!` 内嵌,单二进制分发不变);值腐化
//! (版本号/beta 列表过期)由数据文件随版本维护 + 用户经
//! `provider.headers` 覆盖单个键兜底。

use std::sync::OnceLock;

use serde_json::Value;

use crate::provider::{ClientFingerprintMode, Provider};

// ---------------------------------------------------------------------------
// 白名单与红线(§3.3)
// ---------------------------------------------------------------------------

/// 认证红线:任何指纹源都不得携带(白名单之外的纵深防御,防预设数据
/// 腐化带出凭据)。
pub const FORBIDDEN_HEADERS: &[&str] = &["authorization", "x-api-key", "api-key", "cookie"];

/// 通用域(任意上游线格式)。
const UNIVERSAL_HEADERS: &[&str] = &["user-agent", "x-app"];

/// anthropic 域(上游 protocol == anthropic)。`x-claude-code-*` 会话头
/// 属真实 Claude Code 指纹(决策 #3:passthrough 含)。
const ANTHROPIC_HEADERS: &[&str] = &[
    "anthropic-beta",
    "anthropic-version",
    "x-claude-code-session-id",
    "x-claude-code-agent-id",
    "x-claude-code-parent-agent-id",
];

/// anthropic 域前缀规则:Anthropic SDK 遥测头族(`x-stainless-*`)是真实
/// Claude Code 指纹的一部分,透传保真需要;preset 模板同样按此族声明。
const ANTHROPIC_HEADER_PREFIXES: &[&str] = &["x-stainless-"];

/// responses 域(上游为原生 Responses 线格式;Codex 第一方白名单头)。
const RESPONSES_HEADERS: &[&str] = &["originator", "version"];

/// passthrough query 白名单:仅复制这些键(Claude Code 系 `?beta=true`)。
pub const PASSTHROUGH_QUERY_KEYS: &[&str] = &["beta"];

/// 透传头的值级上限(字节)。超长值截断到该长度 —— 防上游侧头部长度爆量
/// 与日志爆量(20261104_01 阶段三 · B-2)。
pub const PASSTHROUGH_VALUE_MAX_BYTES: usize = 256;

/// 透传头的**值级**净化(20261104_01 阶段三 · B-2)。
///
/// `passthrough_header_allowed` 是纯头名谓词,承载不了值级检查 —— 而
/// `user-agent` 在通用域(`UNIVERSAL_HEADERS`)里对任何上游线格式都
/// 无条件放行,入站 UA 的**值**此前零约束地出站。
///
/// 判定(两条独立规则):
/// - **含控制字符**(`< 0x20` 或 `0x7f`,含 `\r` `\n` `\0`)→ 返回 `None`,
///   调用方**丢弃该头**。控制字符是头注入的载体(回车换行能把一次请求
///   拆成两条);不猜测客户端本意,整头丢弃比静默改写更可预期。
/// - **超长**(> [`PASSTHROUGH_VALUE_MAX_BYTES`])→ 截断到该字节数
///   (按字符边界回退,不切碎 UTF-8 多字节字符)。
/// - 其余原样放行。
///
/// 只作用于 **passthrough 臂**(入站透传);preset 臂读的是内置可信数据
/// 表,不做净化。
pub fn sanitize_passthrough_value(value: &str) -> Option<String> {
    if value
        .chars()
        .any(|ch| (ch as u32) < 0x20 || (ch as u32) == 0x7f)
    {
        return None;
    }
    if value.len() <= PASSTHROUGH_VALUE_MAX_BYTES {
        return Some(value.to_owned());
    }
    let mut end = PASSTHROUGH_VALUE_MAX_BYTES;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    Some(value[..end].to_owned())
}

/// passthrough 模式下某入站头(小写名)是否可出站。按**上游**线格式取域
/// —— 跨协议转发(anthropic 客户端 → openai 上游)时 UA 照常,anthropic
/// 特征头不外带。
pub fn passthrough_header_allowed(provider: &Provider, name: &str) -> bool {
    let name = name.trim().to_ascii_lowercase();
    if name.is_empty() || FORBIDDEN_HEADERS.contains(&name.as_str()) {
        return false;
    }
    if UNIVERSAL_HEADERS.contains(&name.as_str()) {
        return true;
    }
    match provider.protocol {
        crate::provider::ProviderProtocol::Anthropic => {
            ANTHROPIC_HEADERS.contains(&name.as_str())
                || ANTHROPIC_HEADER_PREFIXES
                    .iter()
                    .any(|prefix| name.starts_with(prefix))
        }
        crate::provider::ProviderProtocol::OpenAi => {
            provider.serves_openai_responses_natively()
                && RESPONSES_HEADERS.contains(&name.as_str())
        }
        crate::provider::ProviderProtocol::Gemini => false,
    }
}

// ---------------------------------------------------------------------------
// 内置预设表(数据而非逻辑;附录 A)
// ---------------------------------------------------------------------------

/// 内置客户端指纹预设(`presets/*.json` 的解析形态)。
#[derive(Debug, Clone, PartialEq)]
pub struct ClientFingerprintPreset {
    pub id: String,
    pub label: String,
    /// 兼容的上游线格式(有效线格式口径 = [`Provider::effective_wire_protocol`];
    /// 保存侧门禁与注入期兜底共用)。
    pub compatible_protocols: Vec<String>,
    /// 稳定身份头(动态头属 passthrough 职责;不含任何认证头)。
    pub headers: serde_json::Map<String, Value>,
    /// 端点键 → 静态 query(`"messages" → "beta=true"`)。
    pub query: serde_json::Map<String, Value>,
}

impl ClientFingerprintPreset {
    fn from_json(text: &'static str) -> Self {
        let value: Value = serde_json::from_str(text).expect("builtin preset json must be valid");
        let map = value.as_object().expect("builtin preset must be an object");
        let string_array = |key: &str| -> Vec<String> {
            map.get(key)
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        };
        let object = |key: &str| -> serde_json::Map<String, Value> {
            map.get(key)
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default()
        };
        Self {
            id: map
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            label: map
                .get("label")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            compatible_protocols: string_array("compatibleProtocols"),
            headers: object("headers"),
            query: object("query"),
        }
    }

    /// 预设是否兼容该上游线格式。
    pub fn compatible_with(&self, wire_protocol: &str) -> bool {
        self.compatible_protocols
            .iter()
            .any(|protocol| protocol == wire_protocol)
    }
}

macro_rules! builtin_presets {
    ($($file:expr),* $(,)?) => {
        &[ $(include_str!(concat!("presets/", $file))),* ]
    };
}

/// 内置指纹表(首次调用解析,此后进程内恒定 —— §3.7 稳定性:解析结果
/// 按 provider 固定,不随请求抖动)。
pub fn builtin_presets() -> &'static [ClientFingerprintPreset] {
    static PRESETS: OnceLock<Vec<ClientFingerprintPreset>> = OnceLock::new();
    PRESETS.get_or_init(|| {
        builtin_presets!("zcode.json", "claude-code.json", "codex.json", "opencode.json")
            .iter()
            .map(|text| ClientFingerprintPreset::from_json(text))
            .collect()
    })
}

/// 按 id 查内置预设;未知名由调用方按 off 处理(§3.1)。
pub fn find_preset(id: &str) -> Option<&'static ClientFingerprintPreset> {
    builtin_presets().iter().find(|preset| preset.id == id)
}

// ---------------------------------------------------------------------------
// resolve:指纹层注入头(§3.2 三级优先级的中间层)
// ---------------------------------------------------------------------------

/// 按供应商指纹配置解析出**指纹层**请求头。
///
/// 返回 `(小写头名, 值)` 列表;调用方以 `or_insert` 语义并入出站头
/// (不覆盖 provider.headers,又先于自动注入落位)。`inbound` 为路由
/// 改写前的原始入站头快照(passthrough 的指纹源;preset 不消费)。
///
/// 无指纹 / passthrough 无入站上下文(探测面)/ 未知 presetId / 协议
/// 不匹配 → 空列表(= off 行为,注入期兜底;保存侧门禁是主防线)。
pub fn resolve_fingerprint_headers(
    provider: &Provider,
    inbound: Option<&[(String, String)]>,
) -> Vec<(String, String)> {
    let Some(config) = &provider.client_fingerprint else {
        return Vec::new();
    };
    match config.mode {
        ClientFingerprintMode::Passthrough => {
            let Some(inbound) = inbound else {
                return Vec::new();
            };
            inbound
                .iter()
                // 20261104_01 阶段三 · B-2:头名过白名单后再做**值级**净化
                // (控制字符整头丢弃 / 超长截断)。UA 在通用域里无条件放行,
                // 入站 UA 的值此前零约束出站。
                .filter_map(|(name, value)| {
                    if value.is_empty() || !passthrough_header_allowed(provider, name) {
                        return None;
                    }
                    let value = sanitize_passthrough_value(value)?;
                    // 防御性小写化:dispatch 快照构造已小写,但直构调用方
                    // (测试/探测)可能传入原始大小写。
                    Some((name.trim().to_ascii_lowercase(), value))
                })
                .collect()
        }
        ClientFingerprintMode::Preset => {
            let Some(preset_id) = &config.preset_id else {
                return Vec::new();
            };
            let Some(preset) = find_preset(preset_id) else {
                return Vec::new();
            };
            if !preset.compatible_with(provider.effective_wire_protocol()) {
                return Vec::new();
            }
            preset
                .headers
                .iter()
                .filter_map(|(name, value)| {
                    let name = name.trim().to_ascii_lowercase();
                    if name.is_empty() || FORBIDDEN_HEADERS.contains(&name.as_str()) {
                        return None;
                    }
                    let text = value.as_str()?;
                    if text.is_empty() {
                        return None;
                    }
                    Some((name, text.to_owned()))
                })
                .collect()
        }
    }
}

// ---------------------------------------------------------------------------
// query 透传(§3.4)
// ---------------------------------------------------------------------------

/// passthrough query:allowlist 复制(`beta=true` 保留原样,其余剥除;
/// 无命中 → None)。入参为原始入站 query 串(不带 `?`)。
pub fn passthrough_query(inbound: Option<&str>) -> Option<String> {
    let inbound = inbound?;
    let kept: Vec<&str> = inbound
        .split('&')
        .map(str::trim)
        .filter(|pair| !pair.is_empty())
        .filter(|pair| {
            let key = pair.split('=').next().unwrap_or("").to_ascii_lowercase();
            PASSTHROUGH_QUERY_KEYS.contains(&key.as_str())
        })
        .collect();
    if kept.is_empty() {
        None
    } else {
        Some(kept.join("&"))
    }
}

/// preset 静态 query:按上游线格式取端点键(anthropic → `"messages"`;
/// 其余线格式无预置 query)。full URL 模式不适用(用户原样语义,由
/// `endpoint_with_query` 短路保证)。
pub fn preset_query(preset: &ClientFingerprintPreset, provider: &Provider) -> Option<String> {
    let key = match provider.effective_wire_protocol() {
        "anthropic" => "messages",
        _ => return None,
    };
    preset
        .query
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// 指纹层 query 总入口(转发与探测共用):passthrough 按 allowlist 复制
/// 入站 query(`beta=true`);preset 取预设端点键静态 query。off / 未知
/// presetId / 协议不匹配 → None(与头注入的归 off 口径一致)。
pub fn resolve_fingerprint_query(provider: &Provider, inbound_query: Option<&str>) -> Option<String> {
    let config = provider.client_fingerprint.as_ref()?;
    match config.mode {
        ClientFingerprintMode::Passthrough => passthrough_query(inbound_query),
        ClientFingerprintMode::Preset => {
            let preset = find_preset(config.preset_id.as_deref()?)?;
            if !preset.compatible_with(provider.effective_wire_protocol()) {
                return None;
            }
            preset_query(preset, provider)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ClientFingerprintOverride, UrlMode};
    use serde_json::json;

    fn provider(protocol: &str, style: Option<&str>) -> Provider {
        let mut raw = json!({
            "id": "prov-x", "api_base_url": "https://up.example/v1", "protocol": protocol,
        });
        if let Some(style) = style {
            raw["type"] = json!(style);
        }
        Provider::from_raw(raw.as_object().unwrap()).unwrap()
    }

    fn with_fingerprint(provider: &mut Provider, value: Value) {
        provider.client_fingerprint = ClientFingerprintOverride::from_value(Some(&value));
    }

    // ---- 白名单与红线 ------------------------------------------------------

    #[test]
    fn universal_domain_passes_any_upstream() {
        for protocol in ["openai", "anthropic", "gemini"] {
            let p = provider(protocol, None);
            assert!(passthrough_header_allowed(&p, "user-agent"), "{protocol}");
            assert!(passthrough_header_allowed(&p, "x-app"), "{protocol}");
        }
    }

    #[test]
    fn anthropic_domain_scoped_to_anthropic_upstream() {
        let p = provider("anthropic", None);
        assert!(passthrough_header_allowed(&p, "anthropic-beta"));
        assert!(passthrough_header_allowed(&p, "anthropic-version"));
        assert!(passthrough_header_allowed(&p, "x-claude-code-session-id"));
        assert!(passthrough_header_allowed(&p, "x-stainless-lang"));
        // openai chat 上游:anthropic 特征头不外带(跨协议转发)。
        let chat = provider("openai", None);
        assert!(!passthrough_header_allowed(&chat, "anthropic-beta"));
        assert!(!passthrough_header_allowed(&chat, "x-claude-code-session-id"));
        // gemini 上游同剥。
        let gemini = provider("gemini", None);
        assert!(!passthrough_header_allowed(&gemini, "anthropic-beta"));
    }

    #[test]
    fn responses_domain_scoped_to_native_responses() {
        let responses = provider("openai", Some("openai_responses"));
        assert!(passthrough_header_allowed(&responses, "originator"));
        assert!(passthrough_header_allowed(&responses, "version"));
        // 非 Responses 线格式的 openai 上游:responses 特征头不外带。
        let chat = provider("openai", None);
        assert!(!passthrough_header_allowed(&chat, "originator"));
    }

    #[test]
    fn auth_red_line_never_passes() {
        // 客户端打 WRouter 的网关凭据绝不能上送上游(任何上游线格式)。
        for name in ["authorization", "x-api-key", "api-key", "cookie"] {
            for protocol in ["openai", "anthropic", "gemini"] {
                let p = provider(protocol, None);
                assert!(
                    !passthrough_header_allowed(&p, name),
                    "{name} must never pass ({protocol})"
                );
            }
        }
    }

    // ---- resolve:三级优先级中间层 -----------------------------------------

    #[test]
    fn resolve_off_yields_nothing() {
        let mut p = provider("anthropic", None);
        with_fingerprint(&mut p, json!({"mode": "off"}));
        assert!(resolve_fingerprint_headers(&p, Some(&[])).is_empty());
        // 无配置同效。
        p.client_fingerprint = None;
        let inbound = vec![("user-agent".to_owned(), "UA".to_owned())];
        assert!(resolve_fingerprint_headers(&p, Some(&inbound)).is_empty());
    }

    #[test]
    fn passthrough_copies_inbound_by_domain_and_drops_auth() {
        let mut p = provider("anthropic", None);
        with_fingerprint(&mut p, json!({"mode": "passthrough"}));
        let inbound = vec![
            ("User-Agent".to_owned(), "claude-cli/2.1.123".to_owned()),
            ("x-app".to_owned(), "cli".to_owned()),
            ("anthropic-beta".to_owned(), "beta-1".to_owned()),
            ("authorization".to_owned(), "Bearer client-gw-key".to_owned()),
            ("x-api-key".to_owned(), "sk-client".to_owned()),
            ("x-custom".to_owned(), "noise".to_owned()),
        ];
        let resolved = resolve_fingerprint_headers(&p, Some(&inbound));
        assert_eq!(
            resolved,
            vec![
                ("user-agent".to_owned(), "claude-cli/2.1.123".to_owned()),
                ("x-app".to_owned(), "cli".to_owned()),
                ("anthropic-beta".to_owned(), "beta-1".to_owned()),
            ]
        );
    }

    #[test]
    fn passthrough_without_inbound_context_degrades_to_off() {
        // 探测面:无入站上下文 → 空列表(仅自动注入,由 upstream_headers 变体保证)。
        let mut p = provider("anthropic", None);
        with_fingerprint(&mut p, json!({"mode": "passthrough"}));
        assert!(resolve_fingerprint_headers(&p, None).is_empty());
    }

    // ---- B-2 值级净化(20261104_01 阶段三)-----------------------------------

    #[test]
    fn sanitize_keeps_normal_value_untouched() {
        let value = "claude-cli/2.1.123 (external, cli)";
        assert_eq!(sanitize_passthrough_value(value), Some(value.to_owned()));
    }

    #[test]
    fn sanitize_drops_value_with_control_characters() {
        // 头注入载体:回车 / 换行 / NUL / 其它 C0 控制符 / DEL。
        for bad in [
            "ua\r\nX-Injected: 1",
            "ua\nX-Injected: 1",
            "ua\r",
            "ua\0",
            "ua\t",
            "ua\u{7f}",
            "ua\u{01}",
        ] {
            assert_eq!(
                sanitize_passthrough_value(bad),
                None,
                "控制字符值必须被丢弃: {bad:?}"
            );
        }
    }

    #[test]
    fn sanitize_truncates_overlong_value_at_char_boundary() {
        // 超长值截断到 256 字节。
        let long = "a".repeat(PASSTHROUGH_VALUE_MAX_BYTES + 64);
        let sanitized = sanitize_passthrough_value(&long).unwrap();
        assert_eq!(sanitized.len(), PASSTHROUGH_VALUE_MAX_BYTES);
        // 边界不切碎多字节字符(此处用中文填充,截断点回退到字符边界)。
        let multibyte = "中".repeat(200);
        let sanitized = sanitize_passthrough_value(&multibyte).unwrap();
        assert!(sanitized.len() <= PASSTHROUGH_VALUE_MAX_BYTES);
        assert!(multibyte.starts_with(&sanitized));
        assert!(std::str::from_utf8(sanitized.as_bytes()).is_ok());
    }

    #[test]
    fn passthrough_drops_inbound_header_with_control_characters() {
        // 端到端:含 CR/LF 的入站 UA 不出站(整头丢弃,不静默改写)。
        let mut p = provider("openai", None);
        with_fingerprint(&mut p, json!({"mode": "passthrough"}));
        let inbound = vec![
            (
                "user-agent".to_owned(),
                "ok/1.0\r\nX-Injected: 1".to_owned(),
            ),
            ("x-app".to_owned(), "cli".to_owned()),
        ];
        let resolved = resolve_fingerprint_headers(&p, Some(&inbound));
        assert_eq!(resolved, vec![("x-app".to_owned(), "cli".to_owned())]);
    }

    #[test]
    fn passthrough_truncates_inbound_overlong_value() {
        let mut p = provider("openai", None);
        with_fingerprint(&mut p, json!({"mode": "passthrough"}));
        let inbound = vec![(
            "user-agent".to_owned(),
            "u".repeat(PASSTHROUGH_VALUE_MAX_BYTES + 10),
        )];
        let resolved = resolve_fingerprint_headers(&p, Some(&inbound));
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].1.len(), PASSTHROUGH_VALUE_MAX_BYTES);
    }

    #[test]
    fn preset_unknown_id_degrades_to_off() {
        let mut p = provider("anthropic", None);
        with_fingerprint(&mut p, json!({"mode": "preset", "presetId": "no-such"}));
        let inbound = vec![("user-agent".to_owned(), "UA".to_owned())];
        assert!(resolve_fingerprint_headers(&p, Some(&inbound)).is_empty());
    }

    #[test]
    fn preset_incompatible_protocol_degrades_to_off() {
        // claude-code 只兼容 anthropic 线格式;打到 openai 上游静默归 off
        // (保存侧门禁是主防线,这里是注入期兜底)。
        let mut p = provider("openai", None);
        with_fingerprint(&mut p, json!({"mode": "preset", "presetId": "claude-code"}));
        assert!(resolve_fingerprint_headers(&p, None).is_empty());
    }

    #[test]
    fn preset_zcode_injects_identity_headers_without_auth() {
        let mut p = provider("anthropic", None);
        with_fingerprint(&mut p, json!({"mode": "preset", "presetId": "zcode"}));
        let resolved = resolve_fingerprint_headers(&p, None);
        let map: std::collections::HashMap<_, _> = resolved.iter().cloned().collect();
        assert_eq!(
            map.get("user-agent").map(String::as_str),
            Some("ZCode/3.15.3 ai-sdk/provider-utils/4.0.39 runtime/node.js/24")
        );
        assert_eq!(map.get("x-zcode-agent").map(String::as_str), Some("glm"));
        // 红线:预设永不携带认证头。
        for name in FORBIDDEN_HEADERS {
            assert!(!map.contains_key(*name), "{name} must not be in preset");
        }
    }

    // ---- query 透传 ---------------------------------------------------------

    #[test]
    fn passthrough_query_allowlist_keeps_beta_only() {
        assert_eq!(
            passthrough_query(Some("beta=true")),
            Some("beta=true".to_owned())
        );
        assert_eq!(
            passthrough_query(Some("debug=1&beta=true&trace=x")),
            Some("beta=true".to_owned())
        );
        assert_eq!(passthrough_query(Some("debug=1")), None);
        assert_eq!(passthrough_query(None), None);
        assert_eq!(passthrough_query(Some("")), None);
    }

    #[test]
    fn preset_query_scoped_by_wire_protocol() {
        let claude_code = find_preset("claude-code").unwrap();
        let anthropic = provider("anthropic", None);
        assert_eq!(
            preset_query(claude_code, &anthropic),
            Some("beta=true".to_owned())
        );
        // openai 上游无预置 query(claude-code 亦不兼容,注入侧已归 off)。
        let openai = provider("openai", None);
        assert_eq!(preset_query(claude_code, &openai), None);
    }

    #[test]
    fn builtin_presets_carry_no_auth_and_have_ids() {
        let presets = builtin_presets();
        assert_eq!(presets.len(), 4);
        for preset in presets {
            assert!(!preset.id.is_empty(), "{} has id", preset.id);
            assert!(!preset.compatible_protocols.is_empty());
            for name in preset.headers.keys() {
                let lower = name.to_ascii_lowercase();
                assert!(
                    !FORBIDDEN_HEADERS.contains(&lower.as_str()),
                    "preset {} carries auth header {name}",
                    preset.id
                );
            }
        }
        assert_eq!(find_preset("zcode").unwrap().label, "ZCode");
        assert_eq!(find_preset("claude-code").unwrap().label, "Claude Code");
        assert_eq!(find_preset("codex").unwrap().label, "Codex CLI");
        assert_eq!(find_preset("opencode").unwrap().label, "OpenCode");
    }

    #[test]
    fn codex_preset_only_compatible_with_responses() {
        let codex = find_preset("codex").unwrap();
        assert_eq!(codex.compatible_protocols, vec!["openai_responses".to_owned()]);
        assert!(codex.compatible_with("openai_responses"));
        assert!(!codex.compatible_with("openai"));
    }

    // ---- full URL 模式由 endpoint_with_query 测(见 super::endpoint_tests)--

    #[test]
    fn url_mode_field_remains_default_here() {
        let p = provider("anthropic", None);
        assert_eq!(p.url_mode, UrlMode::Base);
    }
}
