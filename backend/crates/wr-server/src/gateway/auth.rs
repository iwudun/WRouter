//! Gateway API Key 认证(镜像 Python `application/gateway/auth.py`,R2.2)。
//!
//! 两条放行路径:
//! - **internal**:携带管理 token(`settings.auth_token`)——MITM 回环
//!   与本地工具使用,恒放行;
//! - **api_key**:命中 `config.api_keys` 注册表(enabled / expiresAt)。
//!
//! 数据面**强制密钥**:即使 `api_keys` 为空(注册表空),匿名请求也一律
//! 拒绝(401)——不再有 dev-open 逃生门。首次使用的默认密钥由组合根
//! `ensure_default_routing_key` 在启动时种入(见 wr-server `lib.rs`)。
//!
//! 失败响应(Python `app.py:340-344`):
//! `{"error": {"type": "authentication_error", "message": ...}}`,
//! 401(缺失/未知/过期)或 403(禁用),带 `x-wrouter-request-id`。

use std::collections::HashMap;

use axum::http::HeaderMap;
use chrono::{DateTime, Utc};
use serde_json::Value;

use super::ConfigSnapshot;

/// 认证身份(镜像 `GatewayAuthIdentity`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayIdentity {
    pub key_id: String,
    pub key_name: String,
    /// "api_key" | "internal"。
    pub source: String,
}

/// 认证失败(镜像 `GatewayAuthResult` 的失败形态)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthFailure {
    pub status: u16,
    pub message: String,
}

/// internal 身份哨兵(镜像 `_INTERNAL_IDENTITY`)。
fn internal_identity() -> GatewayIdentity {
    GatewayIdentity {
        key_id: "internal".to_owned(),
        key_name: "Internal".to_owned(),
        source: "internal".to_owned(),
    }
}

/// 从 header 集提取 API key(镜像 `extract_api_key`):
/// `x-api-key` → `api-key` → `Authorization: Bearer|Token <token>`;
/// header 名大小写不敏感;返回的 token 不做规范化。
pub fn extract_api_key(headers: &HeaderMap) -> String {
    for header in ["x-api-key", "api-key"] {
        if let Some(value) = header_value(headers, header) {
            let value = value.trim();
            if !value.is_empty() {
                return value.to_owned();
            }
        }
    }
    let Some(auth) = header_value(headers, "authorization").map(str::trim) else {
        return String::new();
    };
    if auth.is_empty() {
        return String::new();
    }
    for prefix in ["bearer ", "token "] {
        if auth.len() >= prefix.len() && auth[..prefix.len()].eq_ignore_ascii_case(prefix) {
            return auth[prefix.len()..].trim().to_owned();
        }
    }
    String::new()
}

fn header_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

/// 认证一条网关请求(镜像 `GatewayAuthService.authenticate`)。
/// `snapshot` 提供缓存好的 api_keys 注册表;`auth_token` 是管理 token。
pub fn authenticate(
    snapshot: &ConfigSnapshot,
    auth_token: &str,
    headers: &HeaderMap,
) -> Result<GatewayIdentity, AuthFailure> {
    let registry = build_registry(&snapshot.config.api_keys);
    let key = extract_api_key(headers);
    if !key.is_empty() {
        if !auth_token.is_empty() && key == auth_token {
            return Ok(internal_identity());
        }
        if let Some(entry) = registry.get(&key) {
            if !entry.enabled {
                return Err(AuthFailure {
                    status: 403,
                    message: "API key is disabled.".to_owned(),
                });
            }
            if is_expired(&entry.expires_at, Utc::now()) {
                return Err(AuthFailure {
                    status: 401,
                    message: "API key has expired.".to_owned(),
                });
            }
            return Ok(GatewayIdentity {
                key_id: entry.key_id.clone(),
                key_name: entry.key_name.clone(),
                source: "api_key".to_owned(),
            });
        }
    }
    if key.is_empty() {
        return Err(AuthFailure {
            status: 401,
            message: "Missing credentials: provide x-api-key or Authorization Bearer.".to_owned(),
        });
    }
    Err(AuthFailure {
        status: 401,
        message: "Invalid API key.".to_owned(),
    })
}

struct RegistryEntry {
    key_id: String,
    key_name: String,
    enabled: bool,
    expires_at: String,
}

/// 构建 key → 身份注册表(镜像 `_load_registry`):
/// 空/非 dict 条目跳过;key 空 strip 跳过;id 缺省取 key;name 缺省
/// "API Key";`enabled is not False`(仅字面 false 禁用);
/// `expiresAt` 缺省永不过期。
fn build_registry(api_keys: &[Value]) -> HashMap<String, RegistryEntry> {
    let mut registry = HashMap::new();
    for entry in api_keys {
        let Some(entry) = entry.as_object() else {
            continue;
        };
        let key = match entry.get("key").and_then(Value::as_str) {
            Some(key) => key.trim(),
            None => continue,
        };
        if key.is_empty() {
            continue;
        }
        let key_id = entry
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(key)
            .to_owned();
        let key_name = entry
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("API Key")
            .to_owned();
        let enabled = !matches!(entry.get("enabled"), Some(Value::Bool(false)));
        let expires_at = entry
            .get("expiresAt")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default()
            .to_owned();
        registry.insert(
            key.to_owned(),
            RegistryEntry {
                key_id,
                key_name,
                enabled,
                expires_at,
            },
        );
    }
    registry
}

/// ISO-8601 过期判定(镜像 `_is_expired`):空/不可解析一律视为
/// 永不过期(畸形字段不得锁死合法 key);无时区按 UTC。
pub fn is_expired(expires_at: &str, now: DateTime<Utc>) -> bool {
    let text = expires_at.trim();
    if text.is_empty() {
        return false;
    }
    let normalized = text.replacen('Z', "+00:00", 1);
    let Some(parsed) = DateTime::parse_from_rfc3339(&normalized)
        .or_else(|_| {
            // Python fromisoformat 也接受 "2026-01-01 10:00:00" 形态;
            // 无时区时 Python 会补 UTC,这里一致。
            chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S")
                .map(|naive| naive.and_utc().fixed_offset())
        })
        .ok()
    else {
        return false;
    };
    let parsed: DateTime<Utc> = parsed.into();
    parsed <= now
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn headers_with(entries: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in entries {
            headers.insert(
                axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                axum::http::HeaderValue::from_str(value).unwrap(),
            );
        }
        headers
    }

    fn snapshot(api_keys: Value) -> ConfigSnapshot {
        let payload = json!({"version": 2, "APIKEYS": api_keys});
        ConfigSnapshot::from_parts(
            wr_infra::config::AppConfig::from_json(&payload).unwrap(),
            Vec::new(),
            None,
        )
    }

    #[test]
    fn extracts_key_from_supported_headers() {
        assert_eq!(
            extract_api_key(&headers_with(&[("X-Api-Key", " sk-1 ")])),
            "sk-1"
        );
        assert_eq!(
            extract_api_key(&headers_with(&[("API-KEY", "sk-2")])),
            "sk-2"
        );
        assert_eq!(
            extract_api_key(&headers_with(&[("Authorization", "Bearer sk-3")])),
            "sk-3"
        );
        // 前缀大小写不敏感(镜像 Python lower().startswith)。
        assert_eq!(
            extract_api_key(&headers_with(&[("Authorization", "BeArEr sk-4")])),
            "sk-4"
        );
        assert_eq!(
            extract_api_key(&headers_with(&[("Authorization", "Token sk-5")])),
            "sk-5"
        );
        // 非 Bearer 前缀不解析。
        assert_eq!(
            extract_api_key(&headers_with(&[("Authorization", "Basic abc")])),
            ""
        );
        assert_eq!(extract_api_key(&headers_with(&[])), "");
    }

    #[test]
    fn empty_registry_requires_key() {
        // dev-open 已移除:未配置任何 key 时,匿名请求一律 401。
        let snapshot = snapshot(json!([]));
        let failure = authenticate(&snapshot, "", &headers_with(&[])).unwrap_err();
        assert_eq!(failure.status, 401);
        assert_eq!(
            failure.message,
            "Missing credentials: provide x-api-key or Authorization Bearer."
        );
        // 空注册表 + 未知 key → 401 Invalid。
        let failure = authenticate(
            &snapshot,
            "",
            &headers_with(&[("Authorization", "Bearer sk-nope")]),
        )
        .unwrap_err();
        assert_eq!(failure.status, 401);
        assert_eq!(failure.message, "Invalid API key.");
    }

    #[test]
    fn valid_key_admits_and_internal_token_wins() {
        let snapshot = snapshot(json!([{"key": "sk-golden-1", "enabled": true, "name": "golden"}]));
        let identity = authenticate(
            &snapshot,
            "",
            &headers_with(&[("Authorization", "Bearer sk-golden-1")]),
        )
        .unwrap();
        assert_eq!(identity.key_id, "sk-golden-1"); // id 缺省取 key
        assert_eq!(identity.key_name, "golden");
        assert_eq!(identity.source, "api_key");

        // 管理 token 恒放行(internal)。
        let identity = authenticate(
            &snapshot,
            "r0-capture-token",
            &headers_with(&[("X-Api-Key", "r0-capture-token")]),
        )
        .unwrap();
        assert_eq!(identity.source, "internal");
    }

    #[test]
    fn missing_and_invalid_keys_fail_with_401() {
        let snapshot = snapshot(json!([{"key": "sk-golden-1", "name": "golden"}]));
        let failure = authenticate(&snapshot, "", &headers_with(&[])).unwrap_err();
        assert_eq!(failure.status, 401);
        assert_eq!(
            failure.message,
            "Missing credentials: provide x-api-key or Authorization Bearer."
        );
        let failure = authenticate(
            &snapshot,
            "",
            &headers_with(&[("Authorization", "Bearer sk-wrong-key")]),
        )
        .unwrap_err();
        assert_eq!(failure.status, 401);
        assert_eq!(failure.message, "Invalid API key.");
    }

    #[test]
    fn disabled_key_fails_with_403_and_keeps_gateway_closed() {
        let snapshot = snapshot(json!([{"key": "sk-off", "enabled": false}]));
        let failure = authenticate(
            &snapshot,
            "",
            &headers_with(&[("Authorization", "Bearer sk-off")]),
        )
        .unwrap_err();
        assert_eq!(failure.status, 403);
        assert_eq!(failure.message, "API key is disabled.");
        // 禁用 key 占住注册表,匿名请求仍 401。
        let failure = authenticate(&snapshot, "", &headers_with(&[])).unwrap_err();
        assert_eq!(failure.status, 401);
    }

    #[test]
    fn expired_key_fails_with_401_and_malformed_never_expires() {
        let snapshot = snapshot(json!([
            {"key": "sk-old", "expiresAt": "2000-01-01T00:00:00Z"},
            {"key": "sk-weird", "expiresAt": "not-a-date"},
        ]));
        let failure = authenticate(
            &snapshot,
            "",
            &headers_with(&[("Authorization", "Bearer sk-old")]),
        )
        .unwrap_err();
        assert_eq!(failure.status, 401);
        assert_eq!(failure.message, "API key has expired.");
        let identity = authenticate(
            &snapshot,
            "",
            &headers_with(&[("Authorization", "Bearer sk-weird")]),
        )
        .unwrap();
        assert_eq!(identity.key_id, "sk-weird");
    }

    #[test]
    fn is_expired_boundaries() {
        let now = Utc::now();
        assert!(!is_expired("", now));
        assert!(!is_expired("junk", now));
        assert!(!is_expired("9999-01-01T00:00:00Z", now));
        assert!(is_expired("2000-01-01T00:00:00+00:00", now));
        // 恰好等于 now 视为过期(Python `<=`)。
        assert!(is_expired(&now.to_rfc3339(), now));
    }
}
