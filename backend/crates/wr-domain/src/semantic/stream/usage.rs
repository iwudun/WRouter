//! 各上游协议的 usage 提取。

use serde_json::Value;

use crate::semantic::model::Usage;

pub(super) fn openai_usage(value: &Value) -> Usage {
    let mut usage = Usage::reported();
    usage.input = nn(value.get("prompt_tokens").and_then(Value::as_i64));
    usage.output = nn(value.get("completion_tokens").and_then(Value::as_i64));
    usage.total = nn(value.get("total_tokens").and_then(Value::as_i64));
    usage.reasoning = nn(value
        .pointer("/completion_tokens_details/reasoning_tokens")
        .and_then(Value::as_i64));
    // 缓存读写:prompt/input 两个 details 桶都要看(方言不一),对齐
    // `crate::usage::normalize_usage` 的方言链,否则流式请求缓存恒 0。
    usage.cache_read = nn(first_i64(
        value,
        &[
            &["prompt_tokens_details", "cached_tokens"],
            &["input_tokens_details", "cached_tokens"],
            &["cached_tokens"],
            &["total_cached_tokens"],
        ],
    ));
    usage.cache_write = nn(first_i64(
        value,
        &[
            &["prompt_tokens_details", "cache_write_tokens"],
            &["input_tokens_details", "cache_write_tokens"],
        ],
    ));
    usage
}
pub(super) fn openai_responses_usage(value: &Value) -> Usage {
    let mut usage = Usage::reported();
    usage.input = nn(value.get("input_tokens").and_then(Value::as_i64));
    usage.output = nn(value.get("output_tokens").and_then(Value::as_i64));
    usage.total = nn(value.get("total_tokens").and_then(Value::as_i64));
    usage.reasoning = nn(value
        .pointer("/output_tokens_details/reasoning_tokens")
        .and_then(Value::as_i64));
    usage.cache_read = nn(first_i64(
        value,
        &[
            &["input_tokens_details", "cached_tokens"],
            &["prompt_tokens_details", "cached_tokens"],
            &["cached_tokens"],
        ],
    ));
    usage.cache_write = nn(first_i64(
        value,
        &[
            &["input_tokens_details", "cache_write_tokens"],
            &["prompt_tokens_details", "cache_write_tokens"],
        ],
    ));
    usage
}
/// usage 负值防御:负数视为未报告(个别兼容端点以负数表达 cache 折扣)。
fn nn(v: Option<i64>) -> Option<i64> {
    v.filter(|n| *n >= 0)
}
pub(super) fn anthropic_usage(value: &Value) -> Usage {
    let mut usage = Usage::reported();
    usage.input = nn(value.get("input_tokens").and_then(Value::as_i64));
    usage.output = nn(value.get("output_tokens").and_then(Value::as_i64));
    usage.cache_read = nn(value.get("cache_read_input_tokens").and_then(Value::as_i64));
    usage.cache_write = nn(value
        .get("cache_creation_input_tokens")
        .and_then(Value::as_i64));
    usage
}
pub(super) fn gemini_usage(value: &Value) -> Usage {
    let mut usage = Usage::reported();
    usage.input = nn(value.get("promptTokenCount").and_then(Value::as_i64));
    usage.output = nn(value.get("candidatesTokenCount").and_then(Value::as_i64));
    usage.total = nn(value.get("totalTokenCount").and_then(Value::as_i64));
    usage.reasoning = nn(value.get("thoughtsTokenCount").and_then(Value::as_i64));
    usage.cache_read = nn(value.get("cachedContentTokenCount").and_then(Value::as_i64));
    usage
}

/// 沿点路径取首个 i64(元组形态);任一环节非 dict/缺键/非数值 → None。
fn first_i64(value: &Value, paths: &[&[&str]]) -> Option<i64> {
    for path in paths {
        let mut node = value;
        let mut ok = true;
        for key in *path {
            match node.get(*key) {
                Some(next) => node = next,
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if ok && let Some(n) = node.as_i64() {
            return Some(n);
        }
    }
    None
}
