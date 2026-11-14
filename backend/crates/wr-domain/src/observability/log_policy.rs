//! C3 日志数据策略的门与正文工具。
//!
//! | 策略 | 门 | 默认 |
//! |---|---|---|
//! | 元数据永久保留 | `prune_logs(log_retention_days)`(0 = 永久) | 永久 |
//! | 正文默认不捕获 | [`capture_bodies_enabled`] | `false` |
//! | 正文默认 7 天过期 | [`body_retention_days`] / env | 7 |
//! | `callLogs` 只门控 request_logs | [`call_logs_enabled`] | `true` |
//!
//! `callLogs` 关掉时**整行不落**(元数据与正文都不落),而指标链路不受影响
//! —— 用量统计从 `request_logs` 派生,所以关掉后统计自然为空,这是 Python 侧
//! 的设计而非缺陷(见 `_record_request` 里 metrics 在 gate 之前调用)。

use serde_json::{Map, Value};

/// `observability.callLogs`(默认 True):关掉就不落 request_logs。
///
/// 读不到配置时按 Python 一样**保持记录** —— 审计链不能因为一次配置读取
/// 失败就静默丢行。
pub fn call_logs_enabled(observability: Option<&Map<String, Value>>) -> bool {
    match observability.and_then(|map| map.get("callLogs")) {
        Some(value) => truthy(value),
        None => true,
    }
}

/// `observability.captureBodies`(默认 False):正文捕获是显式 opt-in。
pub fn capture_bodies_enabled(observability: Option<&Map<String, Value>>) -> bool {
    observability
        .and_then(|map| map.get("captureBodies"))
        .is_some_and(truthy)
}

/// `observability.bodyRetentionDays`:只接受「非 bool 的整数且 >= 0」,
/// 否则回退 env 默认(Python `_current_body_retention_days`)。
pub fn body_retention_days(observability: Option<&Map<String, Value>>, fallback: i64) -> i64 {
    match observability.and_then(|map| map.get("bodyRetentionDays")) {
        Some(Value::Number(number)) => number
            .as_i64()
            .filter(|value| *value >= 0)
            .unwrap_or(fallback),
        _ => fallback,
    }
}

/// Python `bool(x)`。
fn truthy(value: &Value) -> bool {
    match value {
        Value::Bool(flag) => *flag,
        Value::Null => false,
        Value::Number(number) => number.as_f64().is_some_and(|raw| raw != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

/// `transformed_payload_preview` 的截断字符数(Python `PREVIEW_MAX_CHARS`)。
pub const PREVIEW_MAX_CHARS: usize = 256;

/// 正文捕获上限字节数(Python `RAW_BODY_MAX_BYTES` 默认值;
/// env `WROUTER_BODY_CAPTURE_MAX_BYTES` 的读取在 `wr-server::settings`)。
pub const RAW_BODY_MAX_BYTES: usize = 512 * 1024;

/// 预览里按字段名脱敏的键(可能携带凭据)。
pub const SENSITIVE_FIELDS: &[&str] = &[
    "api_key",
    "apikey",
    "api-key",
    "authorization",
    "auth",
    "token",
    "x-api-key",
    "x-goog-api-key",
    "anthropic-api-key",
    "openai-api-key",
    "x-codex-access-token",
    "x-codex-refresh-token",
    "cookie",
    "set-cookie",
];

/// 顶层敏感键替换成 `"***"`;嵌套结构原样保留(预览要的是形状,不是值)。
pub fn redact_for_preview(payload: &Map<String, Value>) -> Map<String, Value> {
    let mut redacted = payload.clone();
    for (key, value) in &mut redacted {
        if SENSITIVE_FIELDS
            .iter()
            .any(|field| key.to_lowercase() == *field)
        {
            *value = Value::String("***".to_owned());
        }
    }
    redacted
}

/// `transformed_payload_preview` 字段:脱敏后 `sort_keys=True` 文本的
/// SHA-256 指纹 + 截断视图 + 原始字符长度。
pub fn build_preview(payload: &Map<String, Value>) -> Value {
    let redacted = Value::Object(redact_for_preview(payload));
    let body = crate::json_py::py_json_dumps_sorted(&redacted);
    let char_count = body.chars().count();
    let preview = if char_count <= PREVIEW_MAX_CHARS {
        body.clone()
    } else {
        let mut truncated: String = body.chars().take(PREVIEW_MAX_CHARS).collect();
        truncated.push('…');
        truncated
    };
    let mut map = Map::new();
    map.insert(
        "sha256".to_owned(),
        Value::String(crate::digest::sha256_hex(body.as_bytes())),
    );
    map.insert("length".to_owned(), Value::from(char_count as i64));
    map.insert("preview".to_owned(), Value::String(preview));
    Value::Object(map)
}

/// 正文捕获结果:`(文本, 原始字节数, 是否截断)`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedBody {
    pub text: String,
    pub size_bytes: usize,
    pub truncated: bool,
}

impl CapturedBody {
    /// 空正文(Python `capture_body(None)` → `("", 0, False)`)。
    pub fn empty() -> Self {
        Self {
            text: String::new(),
            size_bytes: 0,
            truncated: false,
        }
    }
}

/// Python `capture_body`:文本按 UTF-8 字节数计,超限则**按字节边界**截断
/// (不劈开多字节字符)并补 `…`。
pub fn capture_text(text: &str, max_bytes: usize) -> CapturedBody {
    let size = text.len();
    if size <= max_bytes {
        return CapturedBody {
            text: text.to_owned(),
            size_bytes: size,
            truncated: false,
        };
    }
    let mut cut = max_bytes.min(size);
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut truncated = text[..cut].to_owned();
    truncated.push('…');
    CapturedBody {
        text: truncated,
        size_bytes: size,
        truncated: true,
    }
}

/// dict / list 这类结构化正文(Python 对 dict 走 `json.dumps`)。
pub fn capture_json(value: &Value, max_bytes: usize) -> CapturedBody {
    capture_text(&crate::json_py::py_json_dumps(value), max_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn map(value: Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap()
    }

    /// C3:callLogs 默认开、显式 false 才关。
    #[test]
    fn calllogs_gating_defaults_to_on() {
        assert!(call_logs_enabled(None));
        assert!(call_logs_enabled(Some(&map(json!({})))));
        assert!(!call_logs_enabled(Some(&map(json!({"callLogs": false})))));
        assert!(call_logs_enabled(Some(&map(json!({"callLogs": true})))));
    }

    /// C3:正文捕获默认关(opt-in)。
    #[test]
    fn capturebodies_gating_defaults_to_off() {
        assert!(!capture_bodies_enabled(None));
        assert!(!capture_bodies_enabled(Some(&map(json!({})))));
        assert!(capture_bodies_enabled(Some(&map(
            json!({"captureBodies": true})
        ))));
    }

    #[test]
    fn body_retention_accepts_only_non_bool_integers() {
        assert_eq!(body_retention_days(None, 7), 7);
        assert_eq!(body_retention_days(Some(&map(json!({}))), 7), 7);
        assert_eq!(
            body_retention_days(Some(&map(json!({"bodyRetentionDays": 0}))), 7),
            0,
            "0 是合法的「永久保留」显式值"
        );
        assert_eq!(
            body_retention_days(Some(&map(json!({"bodyRetentionDays": true}))), 7),
            7,
            "bool 是 int 的子类,Python 显式排除"
        );
        assert_eq!(
            body_retention_days(Some(&map(json!({"bodyRetentionDays": -1}))), 7),
            7
        );
        assert_eq!(
            body_retention_days(Some(&map(json!({"bodyRetentionDays": "5"}))), 7),
            7,
            "字符串数字不被接受(isinstance(val, int))"
        );
    }

    #[test]
    fn preview_redacts_sensitive_keys_and_fingerprints_sorted_text() {
        let payload = map(json!({
            "model": "m1",
            "api_key": "sk-secret-value",
            "messages": [{"role": "user", "content": "hi"}],
        }));
        let preview = build_preview(&payload);
        let expected_text =
            r#"{"api_key": "***", "messages": [{"content": "hi", "role": "user"}], "model": "m1"}"#;
        assert_eq!(
            preview["sha256"],
            json!(crate::digest::sha256_hex(expected_text.as_bytes())),
            "指纹必须是对 sort_keys=True 文本的 sha256"
        );
        assert_eq!(preview["length"], json!(expected_text.chars().count()));
        assert!(
            !preview["preview"]
                .as_str()
                .unwrap()
                .contains("sk-secret-value"),
            "脱敏后密钥值不得出现在预览里"
        );
    }

    #[test]
    fn preview_truncates_at_char_count_with_ellipsis() {
        let long = "x".repeat(PREVIEW_MAX_CHARS + 5);
        let mut payload = Map::new();
        payload.insert("v".to_owned(), Value::String(long));
        let preview = build_preview(&payload);
        let text = preview["preview"].as_str().unwrap();
        assert_eq!(text.chars().count(), PREVIEW_MAX_CHARS + 1);
        assert!(text.ends_with('…'));
    }

    #[test]
    fn capture_truncates_on_byte_boundary() {
        let captured = capture_text("中文测试", 7);
        assert!(captured.truncated);
        assert_eq!(captured.size_bytes, 12);
        assert_eq!(captured.text, "中文…");
        assert!(
            !captured.text.contains('\u{FFFD}'),
            "按字节切必须回退到字符边界,不留半个字符"
        );
        let whole = capture_text("中文测试", 12);
        assert!(!whole.truncated);
        assert_eq!(whole.text, "中文测试");
    }

    #[test]
    fn capture_json_uses_python_separators() {
        let captured = capture_json(&json!({"a": 1}), RAW_BODY_MAX_BYTES);
        assert_eq!(captured.text, r#"{"a": 1}"#);
        assert_eq!(captured.size_bytes, 8);
    }
}
