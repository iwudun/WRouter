//! `ModelRegistry` + `normalizeRouteSelector` + `parseProviderModelSelector`
//! (镜像 Python `domain/routing/model_registry.py`;方案计划中的
//! `model_resolution.rs` 与 `model_registry.py` 在 Python 侧是同一文件,
//! 故选择器解析与注册表合并于此,见 14 号文档 R3.2 完成记录)。
//!
//! 解析顺序(**以实现为准**,2026-11-06 复核;此前此处列的是已被下线的
//! 六步旧序,与 `resolve_with_provider` 的实际三步不符):
//!
//! 1. `provider/model` 分解(`parse_provider_model_selector`);
//! 2. `wrouter/<聚合名>` 显式聚合前缀(需 `group.enabled`);
//! 3. 显式指定 provider 时的 provider 域内匹配。
//!
//! 网关级别名通道与跨 provider 唯一匹配(裸名全池兜底)已随
//! 20261101_02 T3 一并下线 —— 裸模型名/裸聚合名不再解析,未命中由
//! 调用方返回 400 `model_not_found`。权威顺序见 `resolve_with_provider`
//! 的函数级文档(本文件下方)。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::routing::contracts::{AggregateModelRef, ModelGroup, ProviderModelRef, RouteModelRef};

/// 内部后缀协议白名单:`provider::openai_responses::cred:xx` 形态的
/// provider 选择器里,`::` 后的首段必须命中此集合才被视为内部后缀。
const PROVIDER_INTERNAL_PROTOCOLS: &[&str] = &[
    "anthropic_messages",
    "gemini_generate_content",
    "gemini_interactions",
    "openai_chat_completions",
    "openai_responses",
];

const CREDENTIAL_MARKER: &str = "::cred:";

/// upstream `isKnownProviderInternalSuffix`:按凭据标记切分 + 协议白名单检查。
fn is_known_provider_internal_suffix(value: &str) -> bool {
    let has_credential = value.contains(CREDENTIAL_MARKER);
    if has_credential {
        let after = &value[value.find(CREDENTIAL_MARKER).unwrap() + CREDENTIAL_MARKER.len()..];
        if after.trim().is_empty() {
            return false;
        }
    }
    let protocol = if has_credential {
        &value[..value.find(CREDENTIAL_MARKER).unwrap()]
    } else {
        value
    };
    PROVIDER_INTERNAL_PROTOCOLS.contains(&protocol)
}

/// `_provider_selector_base`:剥掉 `::<内部后缀>` 还原 provider 部分。
fn provider_selector_base(value: Option<&str>) -> String {
    let normalized = value.unwrap_or("").trim();
    let Some(separator) = normalized.find("::") else {
        return normalized.to_owned();
    };
    let provider = normalized[..separator].trim();
    let suffix = normalized[separator + 2..].trim();
    if !provider.is_empty() && is_known_provider_internal_suffix(suffix) {
        provider.to_owned()
    } else {
        normalized.to_owned()
    }
}

/// upstream `providerBaseUrl`:统一走 [`crate::provider::provider_base_url`]
/// (2026-10-14 整治 —— 私有版顺序 `baseurl > baseUrl > api_base_url` 且缺
/// snake `base_url`,纯老数据供应商会在此取空)。
fn provider_base_url(provider: &Map<String, Value>) -> Option<String> {
    crate::provider::provider_base_url(provider)
}

// ---------------------------------------------------------------------------
// 公开选择器
// ---------------------------------------------------------------------------

/// `normalizeRouteSelector`:trim + 首个逗号转斜杠;空/全空白 → None。
/// 逗号在首/尾位置不算分解信号(Python `0 < idx < len-1` 条件)。
pub fn normalize_route_selector(value: Option<&str>) -> Option<String> {
    let trimmed = value?.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(comma_index) = trimmed.find(',')
        && comma_index > 0
        && comma_index < trimmed.len() - 1
    {
        let provider = trimmed[..comma_index].trim();
        let model = trimmed[comma_index + 1..].trim();
        if !provider.is_empty() && !model.is_empty() {
            return Some(format!("{provider}/{model}"));
        }
        return None;
    }
    Some(trimmed.to_owned())
}

/// `parseProviderModelSelector`:`Provider/model` 形态分解;两段非空才命中。
pub fn parse_provider_model_selector(value: Option<&str>) -> Option<(String, String)> {
    let normalized = normalize_route_selector(value)?;
    let separator = normalized.find('/')?;
    if separator == 0 || separator >= normalized.len() - 1 {
        return None;
    }
    let provider = normalized[..separator].trim();
    let model = normalized[separator + 1..].trim();
    if provider.is_empty() || model.is_empty() {
        return None;
    }
    Some((provider.to_owned(), model.to_owned()))
}

/// `re.sub(r"[^a-z0-9_.-]+", "-", s)`:连续非法字符合并成一个 `-`。
fn sanitize_replaced(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut pending_dash = false;
    for c in input.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
            if pending_dash {
                out.push('-');
                pending_dash = false;
            }
            out.push(c);
        } else {
            pending_dash = true;
        }
    }
    if pending_dash {
        out.push('-');
    }
    out
}

/// `sanitizeProviderHeaderId`:小写 + 非 `[a-z0-9_.-]` 段替换为 `-` + 剥边 `-`。
fn sanitize_provider_header_id(value: Option<&str>) -> Option<String> {
    let value = value?;
    let normalized = sanitize_replaced(&value.trim().to_ascii_lowercase())
        .trim_matches('-')
        .to_owned();
    (!normalized.is_empty()).then_some(normalized)
}

/// `providerRuntimeId`:显式 id 优先;否则 `provider-{name}-{sha256(name\nbase)[..10]}`。
pub fn provider_runtime_id(provider: &Map<String, Value>) -> String {
    let explicit = sanitize_provider_header_id(provider.get("id").and_then(Value::as_str));
    if let Some(id) = explicit {
        return id;
    }
    let name = provider
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_owned();
    let base = provider_base_url(provider).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(format!("{name}\n{base}").as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    let digest = &digest[..10];
    let normalized = sanitize_replaced(&name.to_ascii_lowercase());
    let normalized = normalized.trim_matches('-');
    let normalized = &normalized[..normalized.len().min(48)];
    let normalized = if normalized.is_empty() {
        "provider"
    } else {
        normalized
    };
    format!("provider-{normalized}-{digest}")
}

/// `providerAliases`:{name, id, provider} trim 小写 + runtime_id 小写。
fn provider_aliases(provider: &Map<String, Value>) -> HashSet<String> {
    let mut aliases: HashSet<String> = HashSet::new();
    for key in ["name", "id", "provider"] {
        if let Some(value) = provider.get(key).and_then(Value::as_str) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                aliases.insert(trimmed.to_ascii_lowercase());
            }
        }
    }
    aliases.insert(provider_runtime_id(provider).to_ascii_lowercase());
    aliases.retain(|a| !a.is_empty());
    aliases
}

/// `configuredProviderModel`:在 provider.models 中找大小写不敏感匹配,
/// 返回配置里的原始大小写形态。
fn configured_provider_model(provider: &Map<String, Value>, model: &str) -> Option<String> {
    let target = model.trim().to_ascii_lowercase();
    let candidates = provider.get("models")?.as_array()?;
    for candidate in candidates {
        let Some(cleaned) = candidate.as_str().map(str::trim) else {
            continue;
        };
        if cleaned.is_empty() {
            continue;
        }
        if cleaned.to_ascii_lowercase() == target {
            return Some(cleaned.to_owned());
        }
    }
    None
}

/// `providerModelRef`:构造 provider 域模型引用。
fn provider_model_ref(
    provider: Arc<Map<String, Value>>,
    model: &str,
    selector: &str,
) -> RouteModelRef {
    let name = provider
        .get("name")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .or_else(|| {
            provider
                .get("id")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
        })
        .unwrap_or("");
    let canonical_selector = if !name.is_empty() {
        format!("{name}/{model}")
    } else {
        let fallback_id = provider.get("id").and_then(Value::as_str).unwrap_or("?");
        format!("{fallback_id}/{model}")
    };
    RouteModelRef::Provider(ProviderModelRef {
        canonical_selector,
        model: model.to_owned(),
        provider,
        selector: selector.to_owned(),
    })
}

// ---------------------------------------------------------------------------
// ModelRegistry
// ---------------------------------------------------------------------------

/// provider dict 列表 + 聚合组构成的纯内存注册表(2026-09-26 调用契约:
/// 网关级别名通道已下线,选择器只认 `Provider/模型` 与 `wrouter/<聚合名>`)。
#[derive(Debug, Clone, Default)]
pub struct ModelRegistry {
    providers: Vec<Arc<Map<String, Value>>>,
    /// key: 小写聚合名;value: 聚合组(20260905_04)。
    model_groups: HashMap<String, ModelGroup>,
}

impl ModelRegistry {
    pub fn new(providers: Vec<Arc<Map<String, Value>>>, model_groups: &[ModelGroup]) -> Self {
        Self {
            providers,
            model_groups: model_groups
                .iter()
                .map(|group| (group.name.trim().to_ascii_lowercase(), group.clone()))
                .collect(),
        }
    }

    pub fn providers(&self) -> &[Arc<Map<String, Value>>] {
        &self.providers
    }

    /// 聚合组表(20260905_04;`/v1/models` 列表与健康摘要消费)。
    pub fn model_groups(&self) -> &HashMap<String, ModelGroup> {
        &self.model_groups
    }

    /// `resolve`:显式选择器解析(2026-09-26 调用契约)。输入 None/空白 → None。
    ///
    /// 1. `Provider/模型` 分解 —— 供应商模型唯一入口;
    /// 1.5 `wrouter/<聚合名>` 显式聚合前缀;
    /// 2. 显式 provider 提示下的域内匹配(内部编译/规则目标解析用)。
    ///
    /// 裸模型名/裸聚合名/网关级别名不再解析 —— 裸名全池兜底与隐式遮蔽
    /// 一并下线,未命中由调用方返回明确的 400 格式提示。
    pub fn resolve(&self, value: Option<&str>) -> Option<RouteModelRef> {
        self.resolve_with_provider(value, None)
    }

    /// `resolve(value, provider_name=...)`。
    pub fn resolve_with_provider(
        &self,
        value: Option<&str>,
        provider_name: Option<&str>,
    ) -> Option<RouteModelRef> {
        let normalized = normalize_route_selector(value)?;

        // 1. `Provider/model` 分解。
        if let Some((provider_part, model_part)) = parse_provider_model_selector(Some(&normalized))
            && let Some(provider) = self.find_provider(Some(&provider_part))
            && let Some(model) = configured_provider_model(&provider, &model_part)
        {
            return Some(provider_model_ref(provider, &model, &normalized));
        }

        // 1.5 `wrouter/<聚合名>` 显式聚合前缀(2026-09-15):"wrouter" 是聚合
        // 专属命名空间,供应商名已在前端预留禁止占用。前缀 + enabled 聚合名
        // 命中即返回;未命中不拦截、继续走后续级别——既兼容把 "wrouter/x"
        // 当模型名的上游供应商,也让存量名为 wrouter 的供应商保持一级优先。
        if let Some((provider_part, model_part)) = parse_provider_model_selector(Some(&normalized))
            && provider_part.eq_ignore_ascii_case("wrouter")
            && let Some(group) = self
                .model_groups
                .get(&model_part.to_ascii_lowercase())
                .filter(|group| group.enabled)
        {
            return Some(RouteModelRef::Aggregate(AggregateModelRef {
                canonical_selector: group.name.clone(),
                model: group.name.clone(),
                group: group.clone(),
            }));
        }

        // 2. 显式 provider 提示下的域内匹配。
        if let Some(hint) = provider_name
            && let Some(provider) = self.find_provider(Some(hint))
            && let Some(model) = configured_provider_model(&provider, &normalized)
        {
            return Some(provider_model_ref(provider, &model, &normalized));
        }
        None
    }

    pub fn is_configured(&self, value: Option<&str>) -> bool {
        self.resolve(value).is_some()
    }

    /// `findProvider`:按 name/id/provider/runtime_id 别名匹配(小写)。
    pub fn find_provider(&self, value: Option<&str>) -> Option<Arc<Map<String, Value>>> {
        let normalized = provider_selector_base(value).to_ascii_lowercase();
        if normalized.is_empty() {
            return None;
        }
        self.providers
            .iter()
            .find(|provider| provider_aliases(provider).contains(&normalized))
            .cloned()
    }

    /// `resolveProviderModel`:仅 kind == provider 时返回 {model, provider}。
    pub fn resolve_provider_model(
        &self,
        value: Option<&str>,
    ) -> Option<(String, Arc<Map<String, Value>>)> {
        match self.resolve(value)? {
            RouteModelRef::Provider(r) => Some((r.model.clone(), r.provider.clone())),
            RouteModelRef::Aggregate(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn provider_raw(id: &str, name: &str, models: &[&str]) -> Arc<Map<String, Value>> {
        let mut map = Map::new();
        map.insert("id".to_owned(), json!(id));
        map.insert("name".to_owned(), json!(name));
        map.insert("models".to_owned(), json!(models));
        Arc::new(map)
    }

    fn registry() -> ModelRegistry {
        ModelRegistry::new(
            vec![
                provider_raw("p1", "Primary", &["GPT-4o", "deepseek-v4-flash", "dup"]),
                provider_raw("p2", "Secondary", &["dup", "x"]),
            ],
            &[],
        )
    }

    fn group(name: &str, enabled: bool, targets: &[(&str, &str)]) -> crate::routing::contracts::ModelGroup {
        crate::routing::contracts::ModelGroup {
            id: name.to_owned(),
            name: name.to_owned(),
            enabled,
            strategy: crate::routing::contracts::ModelGroupStrategy::First,
            targets: targets
                .iter()
                .map(|(provider_id, model)| crate::routing::contracts::ModelGroupTarget {
                    provider_id: (*provider_id).to_owned(),
                    model: (*model).to_owned(),
                    weight: 1.0,
                    priority: 0,
                    enabled: true,
                })
                .collect(),
        }
    }

    #[test]
    fn resolve_aggregate_requires_wrouter_prefix() {
        let registry = ModelRegistry::new(
            vec![
                provider_raw("p1", "Primary", &["GPT-4o", "dup"]),
                provider_raw("p2", "Secondary", &["dup", "x"]),
            ],
            &[
                group("mixed", true, &[("p1", "GPT-4o"), ("p2", "x")]),
                // 与供应商模型重名:统一契约下互不干扰(裸名归模型,前缀归聚合)。
                group("dup", true, &[("p2", "dup")]),
                group("off-group", false, &[("p1", "GPT-4o")]),
            ],
        );
        // 裸聚合名不再解析(2026-09-26 调用契约)。
        assert!(registry.resolve(Some("mixed")).is_none());
        // `wrouter/<聚合名>` 前缀命中 enabled 聚合(大小写不敏感),kind=aggregate。
        let resolved = registry.resolve(Some("wrouter/MIXED")).unwrap();
        assert_eq!(resolved.kind(), "aggregate");
        assert_eq!(resolved.model(), "mixed");
        let crate::routing::contracts::RouteModelRef::Aggregate(reference) = &resolved else {
            panic!("expected aggregate");
        };
        assert_eq!(reference.group.targets.len(), 2);
        let resolved = registry.resolve(Some("WRouter/mixed")).unwrap();
        assert_eq!(resolved.kind(), "aggregate");
        // 与供应商模型重名的聚合:裸名不再解析,`wrouter/dup` 命中聚合。
        assert!(registry.resolve(Some("dup")).is_none());
        assert_eq!(registry.resolve(Some("wrouter/dup")).unwrap().kind(), "aggregate");
        // 显式 Provider/model 选择器最高(聚合名含 `/` 时也不接管)。
        let resolved = registry.resolve(Some("Primary/gpt-4o")).unwrap();
        assert_eq!(resolved.kind(), "provider");
        // disabled 的组不参与解析。
        assert!(registry.resolve(Some("wrouter/off-group")).is_none());
        // 聚合不产出 (model, provider) 对(仅 kind=provider 产出)。
        assert!(registry.resolve_provider_model(Some("wrouter/mixed")).is_none());
    }

    #[test]
    fn resolve_wrouter_prefix_namespace() {
        let registry = ModelRegistry::new(
            vec![
                provider_raw("p1", "Primary", &["GPT-4o", "wrouter/gpt"]),
                // 存量:名为 wrouter 的供应商(前端已预留,旧配置不强制迁移)。
                provider_raw("p9", "wrouter", &["legacy"]),
            ],
            &[
                group("mixed", true, &[("p1", "GPT-4o"), ("p2", "x")]),
                group("off-group", false, &[("p1", "GPT-4o")]),
            ],
        );
        // `wrouter/<聚合名>` 前缀命中 enabled 聚合(大小写不敏感)。
        let resolved = registry.resolve(Some("wrouter/MIXED")).unwrap();
        assert_eq!(resolved.kind(), "aggregate");
        assert_eq!(resolved.model(), "mixed");
        let resolved = registry.resolve(Some("WRouter/mixed")).unwrap();
        assert_eq!(resolved.kind(), "aggregate");
        // 前缀但组 disabled → 未配置。
        assert!(registry.resolve(Some("wrouter/off-group")).is_none());
        // 前缀但无此聚合 → 未命中(裸模型名通道已下线)。
        assert!(registry.resolve(Some("wrouter/gpt")).is_none());
        // 存量名为 wrouter 的供应商:一级 Provider/model 仍最优先。
        let resolved = registry.resolve(Some("wrouter/legacy")).unwrap();
        assert_eq!(resolved.kind(), "provider");
        assert_eq!(resolved.model(), "legacy");
    }

    #[test]
    fn normalize_and_parse_selectors() {
        assert_eq!(
            normalize_route_selector(Some(" Primary,alpha ")).as_deref(),
            Some("Primary/alpha")
        );
        assert_eq!(normalize_route_selector(Some("  ")).as_deref(), None);
        assert_eq!(normalize_route_selector(Some("a,")).as_deref(), Some("a,"));
        assert_eq!(
            parse_provider_model_selector(Some("p/m")),
            Some(("p".to_owned(), "m".to_owned()))
        );
        assert_eq!(parse_provider_model_selector(Some("/m")), None);
        assert_eq!(parse_provider_model_selector(Some("p/")), None);
    }

    #[test]
    fn resolve_explicit_selector_contract() {
        let registry = registry();
        // 1. Provider/model 分解(name 匹配 + 模型大小写不敏感命中)。
        let resolved = registry.resolve(Some("Primary/gpt-4o")).unwrap();
        assert_eq!(resolved.kind(), "provider");
        assert_eq!(resolved.model(), "GPT-4o");
        // 裸模型名不再解析(跨 provider 唯一匹配已下线;歧义/唯一均 None)。
        assert!(registry.resolve(Some("dup")).is_none());
        assert!(registry.resolve(Some("deepseek-v4-flash")).is_none());
        assert!(registry.resolve(Some("GPT-4O")).is_none());
        assert!(registry.resolve(Some("x")).is_none());
        // 未配置。
        assert!(registry.resolve(Some("nope")).is_none());
        assert!(!registry.is_configured(Some("nope")));
    }

    #[test]
    fn find_provider_matches_alias_and_runtime_id() {
        let registry = registry();
        assert!(registry.find_provider(Some("primary")).is_some());
        assert!(registry.find_provider(Some("P1")).is_some());
        // `::openai_responses` 内部后缀剥离后匹配。
        assert!(
            registry
                .find_provider(Some("Primary::openai_responses"))
                .is_some()
        );
        // 未知后缀不剥离 → 不匹配。
        assert!(registry.find_provider(Some("Primary::bogus")).is_none());
    }

    #[test]
    fn runtime_id_prefers_sanitized_explicit_id() {
        let mut map = Map::new();
        map.insert("id".to_owned(), json!("  Weird ID/!!  "));
        assert_eq!(provider_runtime_id(&map), "weird-id");
        map.remove("id");
        map.insert("name".to_owned(), json!("Deep Seek"));
        map.insert("api_base_url".to_owned(), json!("https://api.deepseek.com/v1"));
        let derived = provider_runtime_id(&map);
        assert!(derived.starts_with("provider-deep-seek-"));
        assert_eq!(derived.len(), "provider-deep-seek-".len() + 10);
    }
}
