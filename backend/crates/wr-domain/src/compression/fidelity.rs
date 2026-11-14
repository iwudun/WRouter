//! 保真门(20260905_03 §4.3):每引擎 apply 后校验硬性不可丢集合,失败回退该引擎改动并记 `skip: fidelity`。
//!
//! 每引擎 apply 后校验**硬性不可丢集合**(方案原文:数字、URL、错误
//! 信息、代码块;错误信息以文本行形态归属数字/URL/围栏之外的一般文本,
//! 这里按可机械校验的四类落地):
//! - 数字字面量(含 JSON 结构数字与文本内数字);
//! - URL(http/https);
//! - 文本内 JSON key(`"key":` 形态);
//! - 代码围栏(``` 计数不减)。
//!
//! 判定用**存在性集合**(before ⊆ after),不是计数多重集 —— 列式
//! 引擎合法去重 key(表头保留一份)必须放行,逐项计数会误杀。
//!
//! 校验器自身出错(正则构造失败等)→ fail-open:放行压缩,不阻塞请求
//! (方案 §2;保真门是护栏不是故障点)。

use std::collections::HashSet;
use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;

/// 硬性不可丢集合(序列化全文上提取;存在性语义)。
#[derive(Debug, Default, PartialEq)]
pub struct Vitals {
    pub numbers: HashSet<String>,
    pub urls: HashSet<String>,
    pub keys: HashSet<String>,
    pub fence_count: usize,
}

fn number_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"\d+(?:\.\d+)?").expect("内置正则必然可编译"))
}

fn url_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    // 排除尾部标点/引号/反斜杠(JSON 转义边界)。
    REGEX.get_or_init(|| Regex::new(r#"https?://[^\s"'\\]+"#).expect("内置正则必然可编译"))
}

fn json_key_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r#""([^"\\]{1,64})"\s*:"#).expect("内置正则必然可编译"))
}

/// 提取序列化 JSON 文本的不可丢集合。正则构造失败 → 空集合(空集合
/// 与任何 after 均满足子集关系 → fail-open)。
pub fn vitals_of(serialized: &str) -> Vitals {
    let numbers = number_regex()
        .find_iter(serialized)
        .map(|m| m.as_str().to_owned())
        .collect();
    let urls = url_regex()
        .find_iter(serialized)
        .map(|m| m.as_str().to_owned())
        .collect();
    let keys = json_key_regex()
        .captures_iter(serialized)
        .filter_map(|c| c.get(1).map(|k| k.as_str().to_owned()))
        .collect();
    let fence_count = serialized.matches("```").count();
    Vitals {
        numbers,
        urls,
        keys,
        fence_count,
    }
}

/// 保真判定:数字/URL/key 全部存活(存在性),围栏计数不减。
/// fail-open 取反事实:提取端异常不会走到这里(构造失败返回空集合,
/// 空集合 ⊆ 任何集合恒真)。
pub fn vitals_preserved(before: &Value, after: &Value) -> bool {
    let before_text = serde_json::to_string(before).unwrap_or_default();
    let after_text = serde_json::to_string(after).unwrap_or_default();
    let before = vitals_of(&before_text);
    let after = vitals_of(&after_text);
    after.numbers.is_superset(&before.numbers)
        && after.urls.is_superset(&before.urls)
        && after.keys.is_superset(&before.keys)
        && after.fence_count >= before.fence_count
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lossless_edits_preserve_vitals() {
        let before = json!({
            "messages": [{"role": "user", "content":
                "见 https://a.b/c  重试 3.5 次,返回 {\"status\": 404} ```code```"}],
        });
        // 双空格折叠(无损引擎语义)。
        let mut after = before.clone();
        after["messages"][0]["content"] =
            json!("见 https://a.b/c 重试 3.5 次,返回 {\"status\": 404} ```code```");
        assert!(vitals_preserved(&before, &after));
    }

    #[test]
    fn lost_number_url_or_key_is_detected() {
        let base = json!({"messages": [{"content": "v 12345 u https://x.y/z k {\"alpha\": 1}"}]});
        // 数字丢失。
        let mut after = base.clone();
        after["messages"][0]["content"] = json!("v  u https://x.y/z k {\"alpha\": 1}");
        assert!(!vitals_preserved(&base, &after));
        // URL 丢失。
        let mut after = base.clone();
        after["messages"][0]["content"] = json!("v 12345 u  k {\"alpha\": 1}");
        assert!(!vitals_preserved(&base, &after));
        // 文本内 JSON key 丢失。
        let mut after = base.clone();
        after["messages"][0]["content"] = json!("v 12345 u https://x.y/z k {}");
        assert!(!vitals_preserved(&base, &after));
    }

    #[test]
    fn losing_one_of_two_numbers_is_detected() {
        // 存在性语义:两个不同数字丢一个也算失真。
        let before = json!({"messages": [{"content": "a 111 b 222"}]});
        let mut after = before.clone();
        after["messages"][0]["content"] = json!("a 111 b");
        assert!(!vitals_preserved(&before, &after));
        // 重复出现减为一次 → 放行(存在性,非计数)。
        let before = json!({"messages": [{"content": "a 111 b 111"}]});
        let mut after = before.clone();
        after["messages"][0]["content"] = json!("a 111");
        assert!(vitals_preserved(&before, &after));
    }

    #[test]
    fn fence_count_must_not_shrink() {
        let before = json!({"messages": [{"content": "```py\ncode\n```"}]});
        let mut after = before.clone();
        after["messages"][0]["content"] = json!("```py\ncode");
        assert!(!vitals_preserved(&before, &after), "围栏配对被破坏 → 拒绝");
    }

    #[test]
    fn columnar_key_dedup_is_allowed() {
        // 列式引擎合法去重:key 在表头保留一份,行内不再重复 → 放行。
        let before = json!({"messages": [{"content": "[{\"name\":\"a\",\"age\":1},{\"name\":\"b\",\"age\":2}]"}]});
        let mut after = before.clone();
        after["messages"][0]["content"] =
            json!("[JSON table: 2 cols × 2 rows]\n{\"name\":null,\"age\":null}\na|1\nb|2\n[2 rows]");
        assert!(vitals_preserved(&before, &after));
    }

    #[test]
    fn serializing_entity_values_stays_subset() {
        // 数字出现在 JSON 结构里(如 max_tokens)与文本里,一视同仁。
        let before = json!({"max_tokens": 4096, "messages": [{"content": "n=7"}]});
        let mut after = before.clone();
        after["messages"][0]["content"] = json!("n=");
        assert!(!vitals_preserved(&before, &after));
    }
}
