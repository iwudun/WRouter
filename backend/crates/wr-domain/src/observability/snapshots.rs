//! 健康 / 熔断只读快照实体(镜像 `domain/observability/entities.py` 中
//! 有生产写入点的两个)。
//!
//! 序列化形态有两处 Python 怪癖必须保持:
//! - `ProviderHealthSnapshot.to_dict()` 的键是 **`provider`** 而不是
//!   `provider_id`,且 `error` 为 `None` 时**整个键缺席**;
//! - `CircuitBreakerSnapshot.to_dict()` 的键是 `provider` / `breaker_state`
//!   / `failures` / `open_until`(注意最后一个是 `open_until` 而非
//!   `opened_until_ms`)。

use serde_json::{Map, Value, json};

/// 一次 provider 健康探测结果(`provider_health` 表的一行来源)。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderHealthSnapshot {
    pub provider_id: String,
    /// `healthy | degraded | down | unknown`。
    pub status: String,
    pub available: bool,
    pub latency_ms: i64,
    /// ISO-8601 串,保证 SQLite 往返不带时区意外。
    pub checked_at: String,
    pub error: Option<String>,
}

impl ProviderHealthSnapshot {
    pub fn to_dict(&self) -> Value {
        let mut map = Map::new();
        map.insert(
            "provider".to_owned(),
            Value::String(self.provider_id.clone()),
        );
        map.insert("status".to_owned(), Value::String(self.status.clone()));
        map.insert("available".to_owned(), Value::Bool(self.available));
        map.insert("latency_ms".to_owned(), Value::from(self.latency_ms));
        map.insert(
            "checked_at".to_owned(),
            Value::String(self.checked_at.clone()),
        );
        if let Some(error) = &self.error {
            map.insert("error".to_owned(), Value::String(error.clone()));
        }
        Value::Object(map)
    }

    /// 反序列化:同时接受 `provider_id` 与 `provider` 两个键(Python
    /// `from_dict` 的 or 链)。
    pub fn from_dict(value: &Value) -> Self {
        let Some(map) = value.as_object() else {
            return Self::default();
        };
        let text = |key: &str| -> String {
            map.get(key)
                .and_then(Value::as_str)
                .filter(|raw| !raw.is_empty())
                .unwrap_or_default()
                .to_owned()
        };
        let provider_id = {
            let by_id = text("provider_id");
            if by_id.is_empty() {
                text("provider")
            } else {
                by_id
            }
        };
        let status = {
            let raw = text("status");
            if raw.is_empty() {
                "unknown".to_owned()
            } else {
                raw
            }
        };
        Self {
            provider_id,
            status,
            available: map
                .get("available")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            latency_ms: map.get("latency_ms").and_then(Value::as_i64).unwrap_or(0),
            checked_at: text("checked_at"),
            error: map.get("error").and_then(Value::as_str).map(str::to_owned),
        }
    }
}

/// 一个 provider 的熔断视图(`state` ∈ `closed|open|half_open`)。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CircuitBreakerSnapshot {
    pub provider_id: String,
    pub state: String,
    pub failures: i64,
    pub opened_until_ms: i64,
}

impl CircuitBreakerSnapshot {
    /// 出现在 503 `breaker_open` 响应体的 `details` 里。
    pub fn to_dict(&self) -> Value {
        json!({
            "provider": self.provider_id,
            "breaker_state": self.state,
            "failures": self.failures,
            "open_until": self.opened_until_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_dict_uses_provider_key_and_omits_null_error() {
        let snapshot = ProviderHealthSnapshot {
            provider_id: "p1".to_owned(),
            status: "healthy".to_owned(),
            available: true,
            latency_ms: 12,
            checked_at: "2026-08-29T00:00:00Z".to_owned(),
            error: None,
        };
        let value = snapshot.to_dict();
        assert_eq!(value["provider"], json!("p1"));
        assert!(
            value.get("provider_id").is_none(),
            "Python to_dict 的键是 provider"
        );
        assert!(value.get("error").is_none(), "error 为 None 时整键缺席");
        let round = ProviderHealthSnapshot::from_dict(&value);
        assert_eq!(round, snapshot);
    }

    #[test]
    fn breaker_dict_keeps_python_field_names() {
        let snapshot = CircuitBreakerSnapshot {
            provider_id: "p1".to_owned(),
            state: "open".to_owned(),
            failures: 3,
            opened_until_ms: 1234,
        };
        assert_eq!(
            snapshot.to_dict(),
            json!({"provider": "p1", "breaker_state": "open", "failures": 3, "open_until": 1234})
        );
    }
}
