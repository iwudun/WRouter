//! 出站代理解析(镜像 Python `application/gateway/proxy_config.py`
//! 的 `resolve_proxy_kwargs_for_url` 决策树,R2 子集;方案 20260906_01
//! 在其上叠加「命名代理预设 + 供应商级覆盖」)。
//!
//! 决策链(按序,见 [`resolve_proxy_decision_with_override`]):
//! 1. URL 是 loopback(localhost / 127.0.0.0/8 / ::1 / 0.0.0.0)→ 直连,
//!    防止本地端点(Ollama 等)被代理打环;
//! 2. 供应商覆盖 `direct` → 直连(压过一切全局配置);
//! 3. 供应商覆盖 `preset(id)` → 用该预设构造代理 URL(scheme 白名单
//!    http/https/socks5/socks5h);预设被删或非法 → warn 后落到第 4 步;
//! 4. 全局默认 `proxy.upstream.mode`:
//!    - `none` 或未知 → 直连(不读进程环境);
//!    - `preset` + `presetId` → 同上构造;缺失 → warn + 直连;
//!    - 旧 `custom` / `system`(均已退役)→ 按未知 mode 直连,不再迁移。
//!
//! 代理来源可经 [`describe_proxy_source`] 转成日志标签,凭据永不落日志
//! (决策 #10)。

use serde_json::{Map, Value};

use wr_domain::provider::{ProviderProxyMode, ProviderProxyOverride};

/// 代理解析结果(映射到 reqwest ClientBuilder 的配置)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyDecision {
    /// `trust_env=False` 且无 proxy:显式直连。
    Direct,
    /// 显式代理 URL(`trust_env=False`)。
    Url(String),
}

/// 镜像 `should_bypass_proxy_for_url`:loopback 一律直连。
pub fn should_bypass_proxy_for_url(url: &str) -> bool {
    let Some(host) = host_of(url) else {
        return false;
    };
    is_loopback_host(&host)
}

/// host 是否 loopback(localhost / ::1 / 0.0.0.0 / 127.0.0.0/8)。
/// 方案 20260906_01 §4.8 的自指校验复用同一判定。
pub fn is_loopback_host(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    if host == "localhost" || host == "::1" || host == "[::1]" || host == "0.0.0.0" {
        return true;
    }
    // 127.0.0.0/8(IPv6 字面量带方括号时先剥掉)。
    let bare = host.trim_matches(|c| c == '[' || c == ']');
    bare.parse::<std::net::Ipv4Addr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

/// 取 URL 的 host 部分(轻量解析;含端口/凭据剔除)。
fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let rest = rest.split(['/', '?', '#']).next()?;
    // user:pass@host:port → host:port。
    let host_port = rest.rsplit_once('@').map(|(_, h)| h).unwrap_or(rest);
    // IPv6 字面量 [::1]:8080。
    if let Some(close) = host_port.rfind(']') {
        return Some(host_port[..=close].to_owned());
    }
    Some(
        host_port
            .rsplit_once(':')
            .map(|(h, _)| h)
            .unwrap_or(host_port)
            .to_owned(),
    )
}

/// 预设面接受的 scheme 白名单(决策 #5:仅**预设**放开 socks5/socks5h ——
/// reqwest 已启用 `socks` feature;legacy `custom` 路径仍只收 http/https,
/// 不动 Python 镜像面)。
const PRESET_SCHEME_WHITELIST: &[&str] = &["http", "https", "socks5", "socks5h"];

/// 命名代理预设(方案 20260906_01 §4.1a):设置页统一管理的「自定义代理」,
/// 供应商与全局默认都**按 id 引用**(决策 #2 —— 改预设即对所有引用方生效,
/// 修复回填值不生效的弱点)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyPreset {
    pub id: String,
    pub name: String,
    pub server: String,
    pub port: i64,
    pub username: String,
    pub password: String,
}

impl ProxyPreset {
    /// 宽松解析:非对象 / id 空 → None(跳过该项);name 缺省回退 id
    /// (与 `Provider.name` 回退 id 的惯例一致)。
    pub fn from_value(value: &Value) -> Option<Self> {
        let map = value.as_object()?;
        let text = |keys: &[&str]| -> String {
            keys.iter()
                .find_map(|key| map.get(*key).and_then(Value::as_str))
                .unwrap_or_default()
                .trim()
                .to_owned()
        };
        let id = text(&["id", "presetId"]);
        if id.is_empty() {
            return None;
        }
        let name = text(&["name", "label"]);
        let port = map
            .get("port")
            .and_then(crate::python_semantics::py_int)
            .unwrap_or(0);
        Some(Self {
            id: id.clone(),
            name: if name.is_empty() { id } else { name },
            server: text(&["server", "host"]),
            port,
            username: text(&["username", "user"]),
            password: text(&["password"]),
        })
    }

    /// 构造代理 URL。`server` 可自带 scheme(白名单校验)与端口;
    /// 未带端口时用 `port` 补全,凭据只在 server 未自带 userinfo 时注入。
    /// 结构非法(server 空 / scheme 不在白名单 / 无端口可补)→ None,
    /// 由调用方 warn 后回落。
    pub fn to_proxy_url(&self) -> Option<String> {
        let server = self.server.trim();
        if server.is_empty() {
            return None;
        }
        let (scheme, authority) = match server.split_once("://") {
            Some((scheme, rest)) => (scheme.trim().to_ascii_lowercase(), rest),
            None => ("http".to_owned(), server),
        };
        if !PRESET_SCHEME_WHITELIST.contains(&scheme.as_str()) {
            return None;
        }
        // 预设只表达 host:port,server 里误带的 path/query 一律丢弃。
        let authority = authority.split(['/', '?', '#']).next().unwrap_or("");
        let (userinfo, host_port) = match authority.rsplit_once('@') {
            Some((user, host)) => (Some(user), host),
            None => (None, authority),
        };
        if host_port.is_empty() {
            return None;
        }
        let authority = match userinfo {
            // server 自带凭据时以它为准(不再叠加 username/password)。
            Some(_) => authority.to_owned(),
            None if !self.username.is_empty() && !self.password.is_empty() => {
                format!("{}:{}@{host_port}", self.username, self.password)
            }
            None => host_port.to_owned(),
        };
        let authority = if authority_has_port(
            authority
                .rsplit_once('@')
                .map(|(_, host)| host)
                .unwrap_or(&authority),
        ) {
            authority
        } else {
            if self.port < 1 || self.port > 65535 {
                return None;
            }
            format!("{authority}:{}", self.port)
        };
        Some(format!("{scheme}://{authority}"))
    }
}

/// host:port 是否已带显式端口(支持 IPv6 字面量 `[::1]:1080`)。
fn authority_has_port(host_port: &str) -> bool {
    if let Some(open) = host_port.find('[') {
        let Some(close) = host_port[open..].find(']') else {
            return false;
        };
        return host_port[open + close..].contains(':');
    }
    host_port
        .rsplit_once(':')
        .map(|(_, port)| port.parse::<u16>().is_ok())
        .unwrap_or(false)
}

/// 解析 `proxy.presets`(宽松:缺失 / 非数组 → 空表;非法项跳过;
/// id 重复时首个胜出,与 `parse_model_groups` 的去重惯例一致)。
pub fn parse_proxy_presets(proxy_config: Option<&Map<String, Value>>) -> Vec<ProxyPreset> {
    let Some(Value::Array(items)) = proxy_config.and_then(|config| config.get("presets")) else {
        return Vec::new();
    };
    let mut presets = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for item in items {
        if let Some(preset) = ProxyPreset::from_value(item)
            && seen.insert(preset.id.clone())
        {
            presets.push(preset);
        }
    }
    presets
}

/// 按 id 查预设(大小写敏感 —— id 由前端生成,不做归一)。
pub fn preset_by_id(proxy_config: Option<&Map<String, Value>>, id: &str) -> Option<ProxyPreset> {
    parse_proxy_presets(proxy_config)
        .into_iter()
        .find(|preset| preset.id == id)
}

/// 预设实际生效的 (host, port)(方案 §4.8 自指校验用;host 已去 scheme、
/// userinfo 与 path)。server 自带端口时以它为准,否则取 `port` 字段。
pub fn preset_host_port(preset: &ProxyPreset) -> Option<(String, u16)> {
    let authority = mask_proxy_authority(&preset.to_proxy_url()?);
    if let Some(open) = authority.find('[') {
        // IPv6 字面量 `[::1]:1080`。
        let close = authority[open..].find(']')? + open;
        let host = authority[..=close].to_owned();
        let port = authority[close + 1..].strip_prefix(':')?.parse::<u16>().ok()?;
        return Some((host, port));
    }
    let (host, port) = authority.rsplit_once(':')?;
    Some((host.to_owned(), port.parse::<u16>().ok()?))
}

/// 镜像 `resolve_proxy_kwargs_for_url`(全局默认路径;供应商级覆盖见
/// [`resolve_proxy_decision_with_override`])。
pub fn resolve_proxy_decision(
    proxy_config: Option<&Map<String, Value>>,
    url: &str,
) -> ProxyDecision {
    resolve_proxy_decision_with_override(proxy_config, None, url)
}

/// 方案 20260906_01 §4.3 决策链:
/// 1. 目标 loopback → Direct(最高优先,防本地端点被打环);
/// 2. 供应商 `direct` → Direct(显式直连压过一切全局配置);
/// 3. 供应商 `preset(id)` → 命中即 Url;预设被删/非法 → warn 后落到第 4 步
///    (决策 #6:回落行为与「预设缺失」运行时路径一致);
/// 4. 全局默认:`none` → Direct / `preset(id)` → Url 或 warn + Direct /
///    legacy `custom` → 原 Python 镜像逻辑。(`system` 已退役 → Direct。)
pub fn resolve_proxy_decision_with_override(
    proxy_config: Option<&Map<String, Value>>,
    provider_override: Option<&ProviderProxyOverride>,
    url: &str,
) -> ProxyDecision {
    if should_bypass_proxy_for_url(url) {
        tracing::info!(target: "wrouter.gateway.proxy", "upstream proxy: bypassing for loopback URL {url}");
        return ProxyDecision::Direct;
    }
    let empty = Map::new();
    let config = proxy_config.unwrap_or(&empty);
    if provider_override.is_some_and(|item| item.mode == ProviderProxyMode::Direct) {
        tracing::info!(target: "wrouter.gateway.proxy", "upstream proxy: provider override forces direct connection");
        return ProxyDecision::Direct;
    }
    if let Some(preset_id) = provider_override
        .filter(|item| item.mode == ProviderProxyMode::Preset)
        .and_then(|item| item.preset_id.as_deref())
    {
        match preset_by_id(Some(config), preset_id).and_then(|preset| preset.to_proxy_url()) {
            Some(proxy_url) => return ProxyDecision::Url(proxy_url),
            None => {
                tracing::warn!(target: "wrouter.gateway.proxy", "upstream proxy: provider preset '{preset_id}' is missing or invalid; falling back to the global default");
            }
        }
    }
    global_decision(config)
}

/// 全局默认(`proxy.upstream`)决策。
fn global_decision(config: &Map<String, Value>) -> ProxyDecision {
    let upstream = config.get("upstream");
    let mode = upstream
        .and_then(|u| u.get("mode"))
        .and_then(Value::as_str)
        .unwrap_or("none");
    match mode {
        // 20260906_01 §4.1b:全局默认改为引用预设(legacy custom 由
        // `AppConfig::from_json` 迁移到这里,见方案 §4.2)。
        "preset" => {
            let preset_id = upstream
                .and_then(|u| u.get("presetId"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim();
            if preset_id.is_empty() {
                tracing::warn!(target: "wrouter.gateway.proxy", "upstream proxy: preset mode without presetId; falling back to direct connection");
                return ProxyDecision::Direct;
            }
            match preset_by_id(Some(config), preset_id).and_then(|preset| preset.to_proxy_url()) {
                Some(proxy_url) => ProxyDecision::Url(proxy_url),
                None => {
                    tracing::warn!(target: "wrouter.gateway.proxy", "upstream proxy: global preset '{preset_id}' is missing or invalid; falling back to direct connection");
                    ProxyDecision::Direct
                }
            }
        }
        "custom" | "system" => {
            tracing::warn!(
                target: "wrouter.gateway.proxy",
                "upstream proxy: retired mode '{mode}' treated as direct connection"
            );
            ProxyDecision::Direct
        }
        _ => ProxyDecision::Direct,
    }
}

/// 请求日志用的代理来源标签(方案 §4.7):`direct |
/// preset:<name>@<host:port> | custom@<host:port>`(2026-09-13 起 `system`
/// 不再产生,历史日志中仍可能出现该旧值)。
/// **凭据一律不落日志**(决策 #10,按 `mask_url` 脱敏)。
pub fn describe_proxy_source(
    proxy_config: Option<&Map<String, Value>>,
    decision: &ProxyDecision,
) -> String {
    match decision {
        ProxyDecision::Direct => "direct".to_owned(),
        ProxyDecision::Url(proxy_url) => {
            let authority = mask_proxy_authority(proxy_url);
            let matched = parse_proxy_presets(proxy_config)
                .into_iter()
                .find(|preset| preset.to_proxy_url().as_deref() == Some(proxy_url.as_str()));
            match matched {
                Some(preset) => format!("preset:{}@{authority}", preset.name),
                None => format!("custom@{authority}"),
            }
        }
    }
}

/// 去 scheme 与 userinfo,只留 host:port。
fn mask_proxy_authority(proxy_url: &str) -> String {
    let rest = proxy_url
        .split_once("://")
        .map(|(_, tail)| tail)
        .unwrap_or(proxy_url);
    let rest = rest.split(['/', '?', '#']).next().unwrap_or("");
    rest.rsplit_once('@')
        .map(|(_, host)| host)
        .unwrap_or(rest)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config(mode: &str, custom: Value) -> Map<String, Value> {
        json!({"upstream": {"mode": mode, "custom": custom}})
            .as_object()
            .unwrap()
            .clone()
    }

    #[test]
    fn loopback_bypasses_any_mode() {
        let cfg = config("custom", json!({"server": "proxy.lan", "port": 7890}));
        assert_eq!(
            resolve_proxy_decision(Some(&cfg), "http://127.0.0.1:3999/x"),
            ProxyDecision::Direct
        );
        assert_eq!(
            resolve_proxy_decision(Some(&cfg), "http://localhost:11434/v1"),
            ProxyDecision::Direct
        );
        assert_eq!(
            resolve_proxy_decision(Some(&cfg), "http://[::1]:1234/v1"),
            ProxyDecision::Direct
        );
    }

    #[test]
    fn none_and_unknown_modes_are_direct() {
        let cfg = config("none", json!({}));
        assert_eq!(
            resolve_proxy_decision(Some(&cfg), "https://api.x.com/v1"),
            ProxyDecision::Direct
        );
        let cfg = config("bogus", json!({}));
        assert_eq!(
            resolve_proxy_decision(Some(&cfg), "https://api.x.com/v1"),
            ProxyDecision::Direct
        );
        assert_eq!(
            resolve_proxy_decision(None, "https://api.x.com/v1"),
            ProxyDecision::Direct
        );
    }

    // ---- 20260906_01 B2:命名预设 + 供应商级覆盖 -------------------------

    fn preset(id: &str, name: &str, server: &str, port: i64) -> ProxyPreset {
        ProxyPreset {
            id: id.to_owned(),
            name: name.to_owned(),
            server: server.to_owned(),
            port,
            username: String::new(),
            password: String::new(),
        }
    }

    fn presets_config(mode: &str, preset_id: &str, presets: Value) -> Map<String, Value> {
        json!({
            "upstream": {"mode": mode, "presetId": preset_id},
            "presets": presets,
        })
        .as_object()
        .unwrap()
        .clone()
    }

    fn override_preset(preset_id: &str) -> ProviderProxyOverride {
        ProviderProxyOverride {
            mode: ProviderProxyMode::Preset,
            preset_id: Some(preset_id.to_owned()),
        }
    }

    fn override_direct() -> ProviderProxyOverride {
        ProviderProxyOverride {
            mode: ProviderProxyMode::Direct,
            preset_id: None,
        }
    }

    #[test]
    fn preset_builds_proxy_url() {
        // 裸 host + port → 默认 http scheme。
        assert_eq!(
            preset("p1", "Clash", "127.0.0.1", 7890).to_proxy_url().as_deref(),
            Some("http://127.0.0.1:7890")
        );
        // 凭据注入。
        let mut with_auth = preset("p2", "Corp", "gw.corp", 8080);
        with_auth.username = "u".to_owned();
        with_auth.password = "p".to_owned();
        assert_eq!(
            with_auth.to_proxy_url().as_deref(),
            Some("http://u:p@gw.corp:8080")
        );
        // server 自带 scheme 与端口:port 字段不再叠加。
        assert_eq!(
            preset("p3", "Socks", "socks5://127.0.0.1:1080", 0)
                .to_proxy_url()
                .as_deref(),
            Some("socks5://127.0.0.1:1080")
        );
        // 决策 #5:预设面放开 socks5h。
        assert_eq!(
            preset("p4", "SocksH", "socks5h://proxy.lan", 1080)
                .to_proxy_url()
                .as_deref(),
            Some("socks5h://proxy.lan:1080")
        );
        // server 自带 userinfo 时以它为准。
        let mut inline = preset("p5", "Inline", "http://a:b@proxy.lan", 3128);
        inline.username = "ignored".to_owned();
        inline.password = "ignored".to_owned();
        assert_eq!(
            inline.to_proxy_url().as_deref(),
            Some("http://a:b@proxy.lan:3128")
        );
        // 误带的 path 丢弃。
        assert_eq!(
            preset("p6", "Path", "proxy.lan/extra", 7890)
                .to_proxy_url()
                .as_deref(),
            Some("http://proxy.lan:7890")
        );
    }

    #[test]
    fn preset_rejects_invalid_shapes() {
        // 空 server / 非白名单 scheme / 无端口可补。
        assert_eq!(preset("p", "n", "  ", 7890).to_proxy_url(), None);
        assert_eq!(preset("p", "n", "ftp://proxy.lan", 21).to_proxy_url(), None);
        assert_eq!(preset("p", "n", "proxy.lan", 0).to_proxy_url(), None);
        assert_eq!(preset("p", "n", "proxy.lan", 70000).to_proxy_url(), None);
        // legacy custom 仍拒绝 socks(Python 镜像面不变)。
        let cfg = config(
            "custom",
            json!({"server": "socks5://proxy.lan", "port": 1080}),
        );
        assert_eq!(
            resolve_proxy_decision(Some(&cfg), "https://api.x.com/v1"),
            ProxyDecision::Direct
        );
    }

    #[test]
    fn parse_presets_is_lenient_and_dedupes_ids() {
        assert!(parse_proxy_presets(None).is_empty());
        let cfg = json!({"presets": "nope"}).as_object().unwrap().clone();
        assert!(parse_proxy_presets(Some(&cfg)).is_empty());
        let cfg = json!({
            "presets": [
                {"id": "p1", "name": "First", "server": "a", "port": 1},
                {"id": "p1", "name": "Dup", "server": "b", "port": 2},
                {"name": "No id", "server": "c", "port": 3},
                "junk",
                {"id": "p2", "server": "d", "port": 4}
            ]
        })
        .as_object()
        .unwrap()
        .clone();
        let presets = parse_proxy_presets(Some(&cfg));
        assert_eq!(presets.len(), 2);
        assert_eq!(presets[0].name, "First", "id 重复时首个胜出");
        // name 缺省回退 id。
        assert_eq!(presets[1].name, "p2");
        assert_eq!(preset_by_id(Some(&cfg), "p2").map(|p| p.server).as_deref(), Some("d"));
        assert_eq!(preset_by_id(Some(&cfg), "missing"), None);
    }

    #[test]
    fn loopback_beats_provider_override() {
        let cfg = presets_config(
            "none",
            "",
            json!([{"id": "p1", "name": "Clash", "server": "127.0.0.1", "port": 7890}]),
        );
        assert_eq!(
            resolve_proxy_decision_with_override(
                Some(&cfg),
                Some(&override_preset("p1")),
                "http://127.0.0.1:11434/v1"
            ),
            ProxyDecision::Direct
        );
    }

    #[test]
    fn provider_direct_beats_global_preset() {
        let cfg = presets_config(
            "preset",
            "p1",
            json!([{"id": "p1", "name": "Clash", "server": "127.0.0.1", "port": 7890}]),
        );
        // 全局默认走预设,供应商显式直连压过它。
        assert_eq!(
            resolve_proxy_decision_with_override(Some(&cfg), None, "https://api.x.com/v1"),
            ProxyDecision::Url("http://127.0.0.1:7890".to_owned())
        );
        assert_eq!(
            resolve_proxy_decision_with_override(
                Some(&cfg),
                Some(&override_direct()),
                "https://api.x.com/v1"
            ),
            ProxyDecision::Direct
        );
    }

    #[test]
    fn provider_preset_beats_global_default() {
        let cfg = presets_config(
            "preset",
            "global",
            json!([
                {"id": "global", "name": "Global", "server": "gw.corp", "port": 8080},
                {"id": "clash", "name": "Clash", "server": "127.0.0.1", "port": 7890}
            ]),
        );
        assert_eq!(
            resolve_proxy_decision_with_override(
                Some(&cfg),
                Some(&override_preset("clash")),
                "https://api.x.com/v1"
            ),
            ProxyDecision::Url("http://127.0.0.1:7890".to_owned())
        );
    }

    #[test]
    fn missing_provider_preset_falls_back_to_global() {
        // 决策 #6:预设被删 → 回落全局默认(none → 直连)。
        let cfg = presets_config("none", "", json!([]));
        assert_eq!(
            resolve_proxy_decision_with_override(
                Some(&cfg),
                Some(&override_preset("deleted")),
                "https://api.x.com/v1"
            ),
            ProxyDecision::Direct
        );
        // 2026-11-07:system/custom 模式退役 —— 与未知 mode 同样直连。
        let cfg = presets_config("system", "", json!([]));
        assert_eq!(
            resolve_proxy_decision_with_override(
                Some(&cfg),
                Some(&override_preset("deleted")),
                "https://api.x.com/v1"
            ),
            ProxyDecision::Direct
        );
    }

    #[test]
    fn global_preset_mode_requires_existing_preset() {
        let cfg = presets_config(
            "preset",
            "gone",
            json!([{"id": "other", "name": "Other", "server": "proxy.lan", "port": 3128}]),
        );
        assert_eq!(
            resolve_proxy_decision(Some(&cfg), "https://api.x.com/v1"),
            ProxyDecision::Direct
        );
        // presetId 缺失同样直连。
        let cfg = presets_config("preset", "", json!([]));
        assert_eq!(
            resolve_proxy_decision(Some(&cfg), "https://api.x.com/v1"),
            ProxyDecision::Direct
        );
        // 未知 mode 仍是直连(降级兼容:旧版本读到 preset 也安全)。
        let cfg = presets_config("bogus", "", json!([]));
        assert_eq!(
            resolve_proxy_decision(Some(&cfg), "https://api.x.com/v1"),
            ProxyDecision::Direct
        );
    }

    #[test]
    fn describe_proxy_source_masks_credentials() {
        let cfg = presets_config(
            "preset",
            "corp",
            json!([{"id": "corp", "name": "公司网关", "server": "gw.corp", "port": 8080, "username": "u", "password": "secret"}]),
        );
        let decision = resolve_proxy_decision(Some(&cfg), "https://api.x.com/v1");
        assert_eq!(decision, ProxyDecision::Url("http://u:secret@gw.corp:8080".to_owned()));
        // 决策 #10:标签只留 名称 + host:port,凭据不落日志。
        let label = describe_proxy_source(Some(&cfg), &decision);
        assert_eq!(label, "preset:公司网关@gw.corp:8080");
        assert!(!label.contains("secret") && !label.contains("u:"));
        assert_eq!(describe_proxy_source(Some(&cfg), &ProxyDecision::Direct), "direct");
    }
}
