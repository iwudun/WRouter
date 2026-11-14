//! 原生三面字节决策(方案 20261106_01 P3-1)。
//!
//! 请求面:无修改 → 原字节;原生且仅顶层 model 不同 → 定点替换 model
//! 值 span(其余字节不变);其余(Translate / 压缩 / 结构变更)→ 声明过
//! 的整体重序列化。响应面与流式面在各自管线内消费同一规划。

use serde_json::Value;
use wr_domain::protocol::contract::BodyMode;
use wr_domain::wire::json::patch_top_level_model;

/// 请求面最终出站字节。`inbound` 必须是 `inbound_raw` 的解析视图(同一原文),
/// `outbound` 是路由/压缩/逐候选改写后的最终值。
pub(super) fn request_wire_bytes(
    inbound_raw: &[u8],
    inbound: &Value,
    outbound: &Value,
    mode: BodyMode,
) -> Vec<u8> {
    if inbound == outbound {
        return inbound_raw.to_vec();
    }
    if mode != BodyMode::Translate
        && let Some(new_model) = outbound.get("model").and_then(Value::as_str)
    {
        let mut probe = inbound.clone();
        if let Some(map) = probe.as_object_mut() {
            map.insert("model".to_owned(), Value::String(new_model.to_owned()));
        }
        if &probe == outbound
            && let Some(patched) = patch_top_level_model(inbound_raw, new_model)
        {
            return patched;
        }
    }
    serde_json::to_vec(outbound).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unchanged_request_keeps_original_bytes() {
        let raw = b"{ \"model\" : \"m\", \"messages\" : [] , \"temperature\":0.70 }\n";
        let value: Value = serde_json::from_slice(raw).unwrap();
        let out = request_wire_bytes(raw, &value, &value, BodyMode::Raw);
        assert_eq!(out, raw);
    }

    #[test]
    fn model_only_change_patches_value_span() {
        let raw =
            br#"{"model":"prov/old","messages":[{"role":"user","content":"hi"}],"stream":true}"#;
        let inbound: Value = serde_json::from_slice(raw).unwrap();
        let mut outbound = inbound.clone();
        outbound["model"] = json!("old");
        let out = request_wire_bytes(raw, &inbound, &outbound, BodyMode::Raw);
        assert_eq!(
            out,
            br#"{"model":"old","messages":[{"role":"user","content":"hi"}],"stream":true}"#
        );
    }

    #[test]
    fn structural_change_falls_back_to_serialization() {
        let raw = br#"{"model":"m","messages":[]}"#;
        let inbound: Value = serde_json::from_slice(raw).unwrap();
        let mut outbound = inbound.clone();
        outbound["messages"] = json!([{"role":"user","content":"compressed"}]);
        let out = request_wire_bytes(raw, &inbound, &outbound, BodyMode::Raw);
        let reparsed: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(reparsed, outbound);
    }

    #[test]
    fn translate_mode_always_serializes() {
        let raw = br#"{"model":"m","x":1}"#;
        let inbound: Value = serde_json::from_slice(raw).unwrap();
        let mut outbound = inbound.clone();
        outbound["model"] = json!("n");
        let out = request_wire_bytes(raw, &inbound, &outbound, BodyMode::Translate);
        // 重序列化不保证原空白,但值一致。
        let reparsed: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(reparsed, outbound);
    }

    #[test]
    fn gemini_url_model_injection_serializes() {
        // 入站体无 model(URL 携带),网关注入 model → 无既有 span 可定点修改。
        let raw = br#"{"contents":[{"parts":[{"text":"hi"}]}]}"#;
        let inbound: Value = serde_json::from_slice(raw).unwrap();
        let mut outbound = inbound.clone();
        outbound["model"] = json!("gemini-x");
        let out = request_wire_bytes(raw, &inbound, &outbound, BodyMode::Raw);
        let reparsed: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(reparsed, outbound);
    }
}
