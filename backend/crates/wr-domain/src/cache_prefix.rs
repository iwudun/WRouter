//! 缓存前缀键提取(20260905_03 §4.1 前缀钉扎;Rust 新增,无 Python 镜像)。
//!
//! 思路:带
//! `cache_control` 断点的请求,取「最后一个断点(含)之前的全部 messages
//! + tools」序列化后 SHA-256,作为前缀钉扎表的键 —— 相同前缀的请求粘到
//! 同一 (provider, account),提高上游 prompt cache 命中。
//!
//! 边界:断点位置只由 **messages 里的 content block**
//! 决定;tools 恒参与哈希输入但**不**驱动截断点(system 断点同理忽略)。
//! 全 body 递归搜 `cache_control` 的判定([`body_has_cache_control`])供
//! 路由条件 `request.meta.hasCacheControl` 复用。

use serde_json::Value;
use sha2::{Digest, Sha256};

/// 递归搜任意层级的 `cache_control` 键(对象键名精确匹配;数组下钻、
/// 对象值下钻)。畸形结构(标量/Null)自然返回 false,不 panic。
pub fn body_has_cache_control(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            if map.contains_key("cache_control") {
                return true;
            }
            map.values().any(body_has_cache_control)
        }
        Value::Array(items) => items.iter().any(body_has_cache_control),
        _ => false,
    }
}

/// 前缀钉扎键:Anthropic 形状请求体 → 「最后一个 message 断点(含)之前
/// 的 messages + tools」的 SHA-256 hex(64 位小写)。
///
/// 无 `messages` 数组、或 messages 中任何 content block 都不带
/// `cache_control` → None(无断点即无稳定前缀,不做钉扎)。
pub fn prefix_pin_key(body: &Value) -> Option<String> {
    let messages = body.get("messages")?.as_array()?;
    // 递归定位最后一个携带 cache_control 的消息(整条消息任意深度均算,
    // 覆盖 content block / tool_result 内层块等 Anthropic 形态)。
    let last_breakpoint = messages
        .iter()
        .rposition(|message| body_has_cache_control(message))?;
    let mut prefix = serde_json::Map::new();
    prefix.insert(
        "messages".to_owned(),
        Value::Array(messages[..=last_breakpoint].to_vec()),
    );
// tools 恒参与键(工具集变化必须 miss;缺省则不带)。
    if let Some(tools) = body.get("tools") {
        prefix.insert("tools".to_owned(), tools.clone());
    }
    // serde_json 默认(紧凑、插入序)序列化后 SHA-256;序列化 Value 不会失败。
    let text = serde_json::to_string(&Value::Object(prefix)).ok()?;
    Some(sha256_hex(text.as_bytes()))
}

/// 最后一个携带 cache_control 的消息下标(压缩管线的缓存感知门用:
/// `messages[..=index]` 为已断点前缀,强制不压)。无断点 → None。
pub fn last_breakpoint_message_index(body: &Value) -> Option<usize> {
    let messages = body.get("messages")?.as_array()?;
    messages
        .iter()
        .rposition(|message| body_has_cache_control(message))
}

/// SHA-256 → 64 位小写 hex(与 [`crate::gateway::extract_session_key`]
/// 的哈希口径同族,只是长度取全摘要)。
fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn no_breakpoint_yields_none() {
        // 无 messages / messages 非数组。
        assert_eq!(prefix_pin_key(&json!({})), None);
        assert_eq!(prefix_pin_key(&json!({"messages": "x"})), None);
        // 有 messages 但无任何 cache_control。
        assert_eq!(
            prefix_pin_key(&json!({"messages": [
                {"role": "user", "content": "hello"},
            ]})),
            None
        );
        // 断点只在 tools 上(messages 无断点 → 不驱动截断,返回 None)。
        assert_eq!(
            prefix_pin_key(&json!({
                "tools": [{"name": "t", "cache_control": {"type": "ephemeral"}}],
                "messages": [{"role": "user", "content": "hi"}],
            })),
            None
        );
    }

    fn body_with_breakpoint_at(index: usize) -> Value {
        let mut messages = vec![
            json!({"role": "user", "content": "m0"}),
            json!({"role": "assistant", "content": "m1"}),
            json!({"role": "user", "content": "m2"}),
            json!({"role": "assistant", "content": "m3"}),
        ];
        let block = json!({"type": "text", "text": "hit", "cache_control": {"type": "ephemeral"}});
        messages[index] = json!({"role": "user", "content": [block]});
        json!({"messages": messages, "tools": [{"name": "tool-a"}]})
    }

    #[test]
    fn breakpoint_position_drives_cut_and_key() {
        // 断点在第 0 条:前缀只含该条 + tools。
        let key0 = prefix_pin_key(&body_with_breakpoint_at(0)).unwrap();
        assert_eq!(key0.len(), 64);
        assert!(key0.chars().all(|c| c.is_ascii_hexdigit()));
        // 断点在中间条(第 2 条):前缀含 0..=2。
        let key2 = prefix_pin_key(&body_with_breakpoint_at(2)).unwrap();
        assert_ne!(key0, key2, "截断点不同 → 键不同");
        // 相同前缀 → 同键(确定性)。
        assert_eq!(key2, prefix_pin_key(&body_with_breakpoint_at(2)).unwrap());
    }

    #[test]
    fn tail_after_breakpoint_does_not_change_key() {
        let mut body = body_with_breakpoint_at(1);
        // 断点后追加消息 / 改写断点后内容:键不变。
        body["messages"]
            .as_array_mut()
            .unwrap()
            .push(json!({"role": "user", "content": "tail noise"}));
        let with_tail = prefix_pin_key(&body).unwrap();
        body["messages"][2]["content"] = json!("changed after breakpoint");
        assert_eq!(with_tail, prefix_pin_key(&body).unwrap());
    }

    #[test]
    fn tools_participate_in_key() {
        let mut a = body_with_breakpoint_at(0);
        let mut b = a.clone();
        // 仅 tools 不同 → 键不同。
        b["tools"] = json!([{"name": "tool-b"}]);
        assert_ne!(prefix_pin_key(&a).unwrap(), prefix_pin_key(&b).unwrap());
        // 无 tools 键的请求也能出键。
        a.as_object_mut().unwrap().remove("tools");
        assert!(prefix_pin_key(&a).is_some());
    }

    #[test]
    fn nested_and_system_breakpoints_are_found_by_has_check() {
        // 嵌套在 messages content block 中的 cache_control 能被找到。
        let nested = json!({"messages": [
            {"role": "user", "content": [
                {"type": "tool_result", "content": [
                    {"type": "text", "text": "x", "cache_control": {"type": "ephemeral"}},
                ]},
            ]},
        ]});
        assert!(body_has_cache_control(&nested));
        // system 数组元素上的断点。
        assert!(body_has_cache_control(&json!({
            "system": [{"type": "text", "text": "s", "cache_control": {}}],
        })));
        // 无断点形态。
        assert!(!body_has_cache_control(
            &json!({"messages": [], "system": "plain"})
        ));
        assert!(!body_has_cache_control(&json!(null)));
        // 键名精确匹配:cache_controlX 不算。
        assert!(!body_has_cache_control(&json!({"cache_controlX": {}})));
    }
}
