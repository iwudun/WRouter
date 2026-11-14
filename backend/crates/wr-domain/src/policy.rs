//! 策略契约(镜像 Python `domain/policy/`:contracts.py + rules.py +
//! evaluator.py + principal.py;方案 #14 R3.6 的访问控制半部)。
//!
//! 路由策略链在 [`crate::routing::executor`](同为 first-match-wins,
//! Python 侧 `RoutePolicyEngine` 即本文件 `PolicyEngine` 的灵感来源);
//! 本模块面向 **身份访问控制**:按 Principal 对 provider/model/tool
//! 资源做 allow/deny,deny 相先于 allow 相评估(RBAC 语义)。
//!
//! `extract_principal` 镜像 `application/gateway/precheck.py` 的同名
//! 函数(保留完整凭据供 glob 匹配;`extract_identity` 的 12 字符截断
//! 版本服务于 precheck 限流,随 R4 迁入)。

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::python_semantics::{py_int, py_str};

/// 策略裁决效果(Python `PolicyEffect` 类型别名 `"allow" | "deny"`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PolicyEffect {
    Allow,
    Deny,
}

impl PolicyEffect {
    /// 解析 `defaultEffect` 字段(Python `PolicyConfig.from_dict` 语义):
    /// trim + 小写后匹配 `allow` / `deny`;非法值一律回退 `allow`,
    /// 保证存量配置缺省时放行全部流量。
    pub fn parse_default_effect(raw: Option<&str>) -> Self {
        match raw.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
            Some("deny") => PolicyEffect::Deny,
            _ => PolicyEffect::Allow,
        }
    }
}

// ---------------------------------------------------------------------------
// 资源 / 身份 / 决策
// ---------------------------------------------------------------------------

/// 被检查的资源(Python `PolicyResource`)。kind: provider|model|tool。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyResource {
    pub kind: String,
    pub name: String,
}

/// 身份主体(Python `Principal`:key 为 `key:<credential>` 或 `anonymous`)。
#[derive(Debug, Clone, PartialEq)]
pub struct Principal {
    pub key: String,
    pub source: String,
    pub claims: Map<String, Value>,
}

/// 单条规则裁决结果(Python `PolicyDecision`)。
#[derive(Debug, Clone, PartialEq)]
pub struct PolicyDecision {
    pub effect: PolicyEffect,
    pub rule_id: String,
    pub reason: String,
    pub principal_key: String,
}

/// `extractPrincipal`:从请求头构造完整凭据 Principal(A3)。
pub fn extract_principal(headers: &[(String, String)]) -> Principal {
    let get = |name: &str| {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
    };
    // `x-api-key` 优先 `api-key`(Python truthy or 链:空串跳下一键)。
    let api_key = get("x-api-key")
        .filter(|v| !v.is_empty())
        .or_else(|| get("api-key").filter(|v| !v.is_empty()));
    if let Some(api_key) = api_key {
        return Principal {
            key: format!("key:{api_key}"),
            source: "gateway".to_owned(),
            claims: Map::from_iter([("credential".to_owned(), Value::String(api_key))]),
        };
    }
    let auth = get("authorization").unwrap_or_default();
    if auth.to_lowercase().starts_with("bearer ") && auth.chars().count() > 7 {
        let token = auth[7..].to_owned();
        return Principal {
            key: format!("key:{token}"),
            source: "gateway".to_owned(),
            claims: Map::from_iter([("credential".to_owned(), Value::String(token))]),
        };
    }
    Principal {
        key: "anonymous".to_owned(),
        source: "anonymous".to_owned(),
        claims: Map::new(),
    }
}

// ---------------------------------------------------------------------------
// 规则(解析 + 匹配)
// ---------------------------------------------------------------------------

const VALID_RESOURCE_KINDS: &[&str] = &["provider", "model", "tool"];

#[derive(Debug, Clone, PartialEq)]
pub struct PolicyRule {
    pub id: String,
    pub identity_match: String,
    pub effect: PolicyEffect,
    pub resource_kind: String,
    pub resource_patterns: Vec<String>,
    pub priority: i64,
    pub reason: String,
}

/// `identity_matches`:空 pattern 或 `*` 全匹配;principal_key 空 → False;
/// 其余走 fnmatchcase。
pub fn identity_matches(principal_key: &str, pattern: &str) -> bool {
    if pattern.is_empty() || pattern == "*" {
        return true;
    }
    if principal_key.is_empty() {
        return false;
    }
    fnmatchcase(principal_key, pattern)
}

/// `resource_matches`:kind 必须相等;patterns 空 → 恒 False(安全默认);
/// 任一 pattern fnmatch 命中即 True。
pub fn resource_matches(resource: &PolicyResource, rule_kind: &str, patterns: &[String]) -> bool {
    if resource.kind != rule_kind {
        return false;
    }
    if patterns.is_empty() {
        return false;
    }
    patterns
        .iter()
        .any(|p| !p.is_empty() && fnmatchcase(&resource.name, p))
}

/// `fnmatch.fnmatchcase` 的大小写敏感 glob:`*` `?` `[seq]`(`!` 取反),
/// 其余逐字元义,整串锚定。
pub fn fnmatchcase(name: &str, pattern: &str) -> bool {
    let mut regex = String::from("\\A");
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' => {
                regex.push_str(".*");
                i += 1;
            }
            '?' => {
                regex.push('.');
                i += 1;
            }
            '[' => {
                // 找闭合 ']'(Python:首字符 '!' 后紧跟 ']' 仍算闭合?fnmatch
                // 的 _translate_worker 把 '[...]' 未闭合时按字面 '[' 处理)。
                let mut j = i + 1;
                if j < chars.len() && chars[j] == '!' {
                    j += 1;
                }
                if j < chars.len() && chars[j] == ']' {
                    j += 1;
                }
                match chars[j..].iter().position(|c| *c == ']') {
                    None => {
                        regex.push_str(&regex::escape("["));
                        i += 1;
                    }
                    Some(offset) => {
                        let close = j + offset;
                        let negated = chars[i + 1] == '!';
                        let contents: String = chars[if negated { i + 2 } else { i + 1 }..close]
                            .iter()
                            .collect();
                        regex.push('[');
                        if negated {
                            regex.push('^');
                        }
                        // fnmatch 把类内内容原样交给 regex(支持 a-z 区间);
                        // 仅转义会破坏类结构的字符。
                        for c in contents.chars() {
                            if c == '\\' || c == '[' || c == ']' {
                                regex.push('\\');
                            }
                            regex.push(c);
                        }
                        regex.push(']');
                        i = close + 1;
                    }
                }
            }
            c => {
                let mut buf = [0u8; 4];
                regex.push_str(&regex::escape(c.encode_utf8(&mut buf)));
                i += 1;
            }
        }
    }
    regex.push_str("\\z");
    regex::Regex::new(&regex).is_ok_and(|re| re.is_match(name))
}

/// `parse_policy_rule`:结构性无效(缺 id / id 空白)返回 None。
pub fn parse_policy_rule(raw: &Value) -> Option<PolicyRule> {
    let map = raw.as_object()?;
    let rule_id = py_str_or_empty_default(map.get("id")).trim().to_owned();
    if rule_id.is_empty() {
        return None;
    }
    let effect_raw = py_str_or_empty_default(map.get("effect"))
        .trim()
        .to_ascii_lowercase();
    let effect = if effect_raw == "deny" {
        PolicyEffect::Deny
    } else {
        PolicyEffect::Allow
    };
    let resource_kind_raw = py_str_or_empty_default(map.get("resource"))
        .trim()
        .to_ascii_lowercase();
    let resource_kind = if VALID_RESOURCE_KINDS.contains(&resource_kind_raw.as_str()) {
        resource_kind_raw
    } else {
        "model".to_owned()
    };
    let priority = match map.get("priority") {
        Some(v) => py_int(v).unwrap_or(0),
        None => 0,
    };
    // `patterns_raw` 为字符串时包成单元素列表;仅收非空字符串项。
    let patterns = match map.get("patterns") {
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() {
                Vec::new()
            } else {
                vec![t.to_owned()]
            }
        }
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    };
    // identity:缺键 → "*";str(None)="None" 的 Python 怪癖由 py_str 复刻。
    let identity_match = match map.get("identity") {
        Some(v) => {
            let text = py_str_or_repr(v);
            let trimmed = text.trim().to_owned();
            if trimmed.is_empty() {
                "*".to_owned()
            } else {
                trimmed
            }
        }
        None => "*".to_owned(),
    };
    Some(PolicyRule {
        id: rule_id,
        identity_match,
        effect,
        resource_kind,
        resource_patterns: patterns,
        priority,
        reason: py_str_or_empty_default(map.get("reason")),
    })
}

/// `parse_policy_rules`:批量解析并丢弃无效项,保序。
pub fn parse_policy_rules(raw_list: &[Value]) -> Vec<PolicyRule> {
    raw_list.iter().filter_map(parse_policy_rule).collect()
}

fn py_str_or_repr(value: &Value) -> String {
    py_str(value).unwrap_or_else(|| crate::json_py::py_json_dumps(value))
}

/// `str(raw.get(k, ""))`:缺键 → 空串;null → "None"(Python str(None))。
fn py_str_or_empty_default(value: Option<&Value>) -> String {
    match value {
        None => String::new(),
        Some(v) => py_str_or_repr(v),
    }
}

// ---------------------------------------------------------------------------
// PolicyEngine(deny 相 → allow 相 → default_effect)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct PolicyEngine {
    deny_rules: Vec<PolicyRule>,
    allow_rules: Vec<PolicyRule>,
    default_effect: PolicyEffect,
}

impl PolicyEngine {
    pub fn new(rules: Vec<PolicyRule>, default_effect: PolicyEffect) -> Self {
        let mut deny_rules: Vec<PolicyRule> = rules
            .iter()
            .filter(|r| r.effect == PolicyEffect::Deny)
            .cloned()
            .collect();
        let mut allow_rules: Vec<PolicyRule> = rules
            .iter()
            .filter(|r| r.effect == PolicyEffect::Allow)
            .cloned()
            .collect();
        // priority 降序稳定排序(Python sorted(key, reverse=True) 同为稳定)。
        deny_rules.sort_by_key(|r| std::cmp::Reverse(r.priority));
        allow_rules.sort_by_key(|r| std::cmp::Reverse(r.priority));
        Self {
            deny_rules,
            allow_rules,
            default_effect,
        }
    }

    pub fn rule_count(&self) -> usize {
        self.deny_rules.len() + self.allow_rules.len()
    }

    /// `evaluate`:deny 相先;同相首个「身份 glob ∧ 资源 fnmatch」命中
    /// 短路;全不命中回 default_effect。
    pub fn evaluate(&self, principal_key: &str, resource: &PolicyResource) -> PolicyDecision {
        for rule in &self.deny_rules {
            if identity_matches(principal_key, &rule.identity_match)
                && resource_matches(resource, &rule.resource_kind, &rule.resource_patterns)
            {
                return PolicyDecision {
                    effect: PolicyEffect::Deny,
                    rule_id: if rule.id.is_empty() {
                        "<anonymous-deny>".to_owned()
                    } else {
                        rule.id.clone()
                    },
                    reason: if rule.reason.is_empty() {
                        format!("denied by rule {}", rule.id)
                    } else {
                        rule.reason.clone()
                    },
                    principal_key: principal_key.to_owned(),
                };
            }
        }
        for rule in &self.allow_rules {
            if identity_matches(principal_key, &rule.identity_match)
                && resource_matches(resource, &rule.resource_kind, &rule.resource_patterns)
            {
                return PolicyDecision {
                    effect: PolicyEffect::Allow,
                    rule_id: if rule.id.is_empty() {
                        "<anonymous-allow>".to_owned()
                    } else {
                        rule.id.clone()
                    },
                    reason: if rule.reason.is_empty() {
                        format!("allowed by rule {}", rule.id)
                    } else {
                        rule.reason.clone()
                    },
                    principal_key: principal_key.to_owned(),
                };
            }
        }
        PolicyDecision {
            effect: self.default_effect,
            rule_id: "<default>".to_owned(),
            reason: format!(
                "no rule matched, default_effect={}",
                match self.default_effect {
                    PolicyEffect::Allow => "allow",
                    PolicyEffect::Deny => "deny",
                }
            ),
            principal_key: principal_key.to_owned(),
        }
    }
}

/// `build_policy_engine`(evaluator.py):`enabled=false` 时构造
/// 恒放行引擎(忽略规则);否则解析 + 指定 default_effect。
pub fn build_policy_engine(
    raw_rules: &[Value],
    default_effect: PolicyEffect,
    enabled: bool,
) -> PolicyEngine {
    if !enabled {
        return PolicyEngine::new(Vec::new(), PolicyEffect::Allow);
    }
    PolicyEngine::new(parse_policy_rules(raw_rules), default_effect)
}

/// 便捷:把策略裁决渲染成网关 403 错误体(Python forwarding.py 的
/// `policy_denied` 信封)。
pub fn policy_denied_body(decision: &PolicyDecision) -> Value {
    json!({
        "error": {
            "type": "policy_denied",
            "message": format!("access denied by policy '{}'", decision.rule_id),
            "code": "policy_denied",
            "policy_id": decision.rule_id,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_allow_and_deny() {
        assert_eq!(
            PolicyEffect::parse_default_effect(Some("allow")),
            PolicyEffect::Allow
        );
        assert_eq!(
            PolicyEffect::parse_default_effect(Some("deny")),
            PolicyEffect::Deny
        );
    }

    #[test]
    fn case_and_whitespace_insensitive() {
        assert_eq!(
            PolicyEffect::parse_default_effect(Some(" DENY ")),
            PolicyEffect::Deny
        );
        assert_eq!(
            PolicyEffect::parse_default_effect(Some("Allow")),
            PolicyEffect::Allow
        );
    }

    #[test]
    fn invalid_or_missing_falls_back_to_allow() {
        assert_eq!(
            PolicyEffect::parse_default_effect(Some("bogus")),
            PolicyEffect::Allow
        );
        assert_eq!(
            PolicyEffect::parse_default_effect(Some("")),
            PolicyEffect::Allow
        );
        assert_eq!(
            PolicyEffect::parse_default_effect(None),
            PolicyEffect::Allow
        );
    }

    #[test]
    fn serde_roundtrip_uses_lowercase() {
        assert_eq!(
            serde_json::to_string(&PolicyEffect::Deny).unwrap(),
            "\"deny\""
        );
        let back: PolicyEffect = serde_json::from_str("\"allow\"").unwrap();
        assert_eq!(back, PolicyEffect::Allow);
    }

    #[test]
    fn fnmatchcase_semantics() {
        assert!(fnmatchcase("key:sk-abc", "key:sk-*"));
        assert!(fnmatchcase("anonymous", "*"));
        assert!(!fnmatchcase("key:xyz", "key:abc"));
        assert!(fnmatchcase("gpt-4o", "g?t-4o"));
        assert!(fnmatchcase("a-x", "[abc]*-[a-z]"));
        assert!(fnmatchcase("z-model", "[!a-y]-model"));
        assert!(!fnmatchcase("m-model", "[!a-y]-model"));
        // 大小写敏感(fnmatchcase 而非 fnmatch)。
        assert!(!fnmatchcase("GPT", "gpt"));
    }

    #[test]
    fn rule_parsing_defaults_and_coercion() {
        let rule = parse_policy_rule(&json!({"id": "r1"})).unwrap();
        assert_eq!(rule.identity_match, "*");
        assert_eq!(rule.effect, PolicyEffect::Allow);
        assert_eq!(rule.resource_kind, "model");
        assert!(rule.resource_patterns.is_empty());
        // 非法 kind/effect 回退;priority 宽容 int();字符串 patterns 包装。
        let rule = parse_policy_rule(&json!({
            "id": 7, "effect": "WTF", "resource": "bogus",
            "priority": "3", "patterns": "a*", "identity": "",
        }))
        .unwrap();
        assert_eq!(rule.id, "7");
        assert_eq!(rule.effect, PolicyEffect::Allow);
        assert_eq!(rule.resource_kind, "model");
        assert_eq!(rule.priority, 3);
        assert_eq!(rule.resource_patterns, vec!["a*".to_owned()]);
        assert_eq!(rule.identity_match, "*");
        // 无 id / 空白 id → None。
        assert!(parse_policy_rule(&json!({"name": "x"})).is_none());
        assert!(parse_policy_rule(&json!({"id": "  "})).is_none());
    }

    fn model_rule(
        id: &str,
        effect: &str,
        identity: &str,
        patterns: &[&str],
        priority: i64,
    ) -> PolicyRule {
        parse_policy_rule(&json!({
            "id": id, "effect": effect, "resource": "model",
            "identity": identity, "patterns": patterns, "priority": priority,
        }))
        .unwrap()
    }

    #[test]
    fn engine_deny_precedence_and_priority() {
        let engine = PolicyEngine::new(
            vec![
                model_rule("allow-all", "allow", "*", &["*"], 0),
                model_rule("deny-gpt", "deny", "key:vip", &["gpt-*"], 0),
                model_rule("allow-gpt", "allow", "*", &["gpt-*"], 5),
            ],
            PolicyEffect::Allow,
        );
        // deny 相先于 allow 相(与 priority 无关 —— allow-gpt 的 5 只在 allow 相内比较)。
        let decision = engine.evaluate(
            "key:vip",
            &PolicyResource {
                kind: "model".into(),
                name: "gpt-4o".into(),
            },
        );
        assert_eq!(decision.effect, PolicyEffect::Deny);
        assert_eq!(decision.rule_id, "deny-gpt");
        assert_eq!(decision.reason, "denied by rule deny-gpt");
        // 非 vip 身份 → allow 相按 priority 降序命中 allow-gpt。
        let decision = engine.evaluate(
            "key:other",
            &PolicyResource {
                kind: "model".into(),
                name: "gpt-4o".into(),
            },
        );
        assert_eq!(decision.effect, PolicyEffect::Allow);
        assert_eq!(decision.rule_id, "allow-gpt");
    }

    #[test]
    fn empty_patterns_never_match_and_default_effect_applies() {
        let engine = build_policy_engine(
            &[json!({"id": "empty", "effect": "allow"})],
            PolicyEffect::Deny,
            true,
        );
        let decision = engine.evaluate(
            "anyone",
            &PolicyResource {
                kind: "model".into(),
                name: "m".into(),
            },
        );
        assert_eq!(decision.effect, PolicyEffect::Deny);
        assert_eq!(decision.rule_id, "<default>");
        // disabled → 恒 allow。
        let engine = build_policy_engine(
            &[json!({"id": "d", "effect": "deny", "patterns": ["*"]})],
            PolicyEffect::Deny,
            false,
        );
        assert_eq!(engine.rule_count(), 0);
        let decision = engine.evaluate(
            "anyone",
            &PolicyResource {
                kind: "model".into(),
                name: "m".into(),
            },
        );
        assert_eq!(decision.effect, PolicyEffect::Allow);
    }

    #[test]
    fn extract_principal_mirrors_python_header_rules() {
        let headers = vec![("x-api-key".to_owned(), "sk-abc".to_owned())];
        let principal = extract_principal(&headers);
        assert_eq!(principal.key, "key:sk-abc");
        assert_eq!(principal.source, "gateway");
        // bearer 大小写不敏感。
        let headers = vec![("Authorization".to_owned(), "Bearer tok123".to_owned())];
        assert_eq!(extract_principal(&headers).key, "key:tok123");
        // "bearer " 恰 7 字符 → len>7 不成立 → anonymous。
        let headers = vec![("authorization".to_owned(), "bearer ".to_owned())];
        assert_eq!(extract_principal(&headers).key, "anonymous");
        let headers: Vec<(String, String)> = Vec::new();
        assert_eq!(extract_principal(&headers).key, "anonymous");
    }
}
