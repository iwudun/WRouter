//! Gemini path-style 适配器(镜像 Python `domain/routing/protocol_adapter.py`)。
//!
//! 纯 URL 改写,无 I/O。R3.11:模型名改写规则 —— 客户端把模型放在
//! URL 段(`{model}:{action}`)时,body.model 由路径折叠而来;路由引擎
//! 改写模型后,URL 段按同样的正则重新百分号编码回写。

use serde_json::{Map, Value};

/// Gemini generateContent 路径正则(大小写不敏感,镜像
/// `_GEMINI_GENERATE_CONTENT_PATH`)。
fn gemini_path_regex() -> regex::Regex {
    regex::Regex::new(
        r"(?i)(/v1(?:beta)?/models/)([^/:]+)(:(?:generatecontent|streamgeneratecontent))",
    )
    .unwrap()
}

/// `routeModelFromPath`:从 URL 提取百分号解码后的模型段;无命中 None。
pub fn route_model_from_path(path: &str) -> Option<String> {
    let captures = gemini_path_regex().captures(path)?;
    let segment = captures.get(2)?.as_str();
    if segment.is_empty() {
        return None;
    }
    Some(percent_decode_lossy(segment))
}

/// `adaptRouteRequestBody`:把路径模型折叠进 body,返回
/// (body, model_location)。model_location == "path" 时调用方应在
/// 转发前 `restore_route_request_body` 再剥掉 body.model。
pub fn adapt_route_request_body(
    path: &str,
    body: &Map<String, Value>,
) -> (Map<String, Value>, String) {
    match route_model_from_path(path) {
        Some(model) => {
            let mut next = body.clone();
            next.insert("model".to_owned(), Value::String(model));
            (next, "path".to_owned())
        }
        None => (body.clone(), "body".to_owned()),
    }
}

/// `restoreRouteRequestBody`:path-owned 时剥离 body.model。
pub fn restore_route_request_body(
    body: &Map<String, Value>,
    model_location: &str,
) -> Map<String, Value> {
    if model_location != "path" {
        return body.clone();
    }
    let mut next = body.clone();
    next.remove("model");
    next
}

/// `rewriteRouteModelInUrl`:命中 gemini 路径时以百分号编码后的新模型
/// 重写路径段;否则原样返回。
pub fn rewrite_route_model_in_url(url: &str, model: Option<&str>) -> String {
    let Some(model) = model.filter(|m| !m.is_empty()) else {
        return url.to_owned();
    };
    let regex = gemini_path_regex();
    if regex.find(url).is_none() {
        return url.to_owned();
    }
    let encoded = crate::protocol::uri_encode(model);
    regex
        .replace_all(url, |caps: &regex::Captures| {
            format!("{}{}{}", &caps[1], encoded, &caps[3])
        })
        .into_owned()
}

/// `percent_decode_lossy`:unquote 容错非法转义(与 Python
/// `_safe_decode` 的 strict→replace 兜底一致)。
fn percent_decode_lossy(value: &str) -> String {
    let mut bytes: Vec<u8> = Vec::new();
    let chars: Vec<char> = value.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '%'
            && i + 2 < chars.len()
            && let (Some(h), Some(l)) = (chars[i + 1].to_digit(16), chars[i + 2].to_digit(16))
        {
            bytes.push((h * 16 + l) as u8);
            i += 3;
            continue;
        }
        // 非 ASCII 字面字符按 UTF-8 直接入缓冲(对齐 unquote 行为)。
        let mut buf = [0u8; 4];
        bytes.extend_from_slice(chars[i].encode_utf8(&mut buf).as_bytes());
        i += 1;
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_model_from_gemini_path() {
        assert_eq!(
            route_model_from_path("/v1beta/models/gemini-2.5-flash:streamGenerateContent")
                .as_deref(),
            Some("gemini-2.5-flash")
        );
        assert_eq!(
            route_model_from_path("/v1/models/my%2Fmodel:generateContent").as_deref(),
            Some("my/model")
        );
        assert_eq!(route_model_from_path("/v1/chat/completions"), None);
    }

    #[test]
    fn adapt_and_restore_round_trip() {
        let body = json!({"contents": []}).as_object().unwrap().clone();
        let (adapted, location) =
            adapt_route_request_body("/v1beta/models/m-golden:generateContent", &body);
        assert_eq!(location, "path");
        assert_eq!(adapted["model"], json!("m-golden"));
        let restored = restore_route_request_body(&adapted, &location);
        assert!(restored.get("model").is_none());
    }

    #[test]
    fn rewrite_url_reencodes_model_segment() {
        let rewritten = rewrite_route_model_in_url(
            "https://g.example/v1beta/models/old-model:generateContent",
            Some("new/model"),
        );
        assert_eq!(
            rewritten,
            "https://g.example/v1beta/models/new%2Fmodel:generateContent"
        );
        // 非 gemini URL 原样。
        assert_eq!(
            rewrite_route_model_in_url("https://x/v1/chat/completions", Some("m")),
            "https://x/v1/chat/completions"
        );
    }
}
