//! 用量归一化(镜像 Python `domain/protocol/usage.py` 的 relay 子集)。
//!
//! relay 从流式终态事件里捕获 token 用量;上游命名惯例不一
//! (OpenAI `prompt_tokens` / Anthropic `input_tokens` / Gemini
//! `promptTokenCount`),归一后按「计数器取 max」合并——上游 usage
//! 事件是累计快照,重复终态块不得重复计数。

use serde_json::Value;

/// 归一化后的用量计数器(内部命名,镜像 Python `empty_usage()` 键;
/// `reasoning_tokens` 是 Rust 新增计数器,无 Python 镜像 —— OpenAI
/// `completion_tokens_details.reasoning_tokens` / Gemini
/// `thoughts_token_count` 方言,Anthropic 的思考计入 output 无独立键)。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub total_tokens: i64,
    pub reasoning_tokens: i64,
}

impl Usage {
    pub fn empty() -> Self {
        Self::default()
    }

    /// 按「逐计数器取 max」合并一次归一化用量(镜像 `record_usage`)。
    pub fn merge_max(&mut self, other: Usage) {
        self.input_tokens = self.input_tokens.max(other.input_tokens);
        self.output_tokens = self.output_tokens.max(other.output_tokens);
        self.cache_read_tokens = self.cache_read_tokens.max(other.cache_read_tokens);
        self.cache_write_tokens = self.cache_write_tokens.max(other.cache_write_tokens);
        self.reasoning_tokens = self.reasoning_tokens.max(other.reasoning_tokens);
        self.total_tokens = self
            .total_tokens
            .max(other.total_tokens)
            .max(self.input_tokens + self.output_tokens);
    }

    /// 逐计数器**相加**合并一次用量(阶段五 C-3 续写场景)。
    ///
    /// 与 [`Usage::merge_max`] 的语义相反:两次上游尝试的用量是**累加**
    /// 关系(各花各的额度),不是同一份快照的重复上报 —— 断流续写后
    /// 两次尝试的 input/output/cache token 都要计入总账。
    pub fn merge_add(&mut self, other: Usage) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(other.cache_read_tokens);
        self.cache_write_tokens = self
            .cache_write_tokens
            .saturating_add(other.cache_write_tokens);
        self.reasoning_tokens = self.reasoning_tokens.saturating_add(other.reasoning_tokens);
        self.total_tokens = self.total_tokens.saturating_add(other.total_tokens);
    }
}

/// Python `_as_int`:bool/None → 0;数值/数字字符串走 `int()` 并向零截断;
/// 负数夹到 0;解析失败 0。
fn py_as_int(value: Option<&Value>) -> i64 {
    match value {
        None | Some(Value::Null) | Some(Value::Bool(_)) => 0,
        Some(v) => crate::python_semantics::py_int(v).unwrap_or(0).max(0),
    }
}

/// `field` 支持点路径(元组形态);任一环节非 dict/缺键 → None。
fn lookup<'a>(data: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut node = data;
    for key in path {
        node = node.get(*key)?;
    }
    Some(node)
}

/// `_first_int`:沿字段序找第一个**非零**值。
fn first_int(data: &Value, paths: &[&[&str]]) -> i64 {
    for path in paths {
        let n = py_as_int(lookup(data, path));
        if n != 0 {
            return n;
        }
    }
    0
}

/// `_cache_read`:全部已知方言按序取首个非零。
fn cache_read(raw: &Value) -> i64 {
    first_int(
        raw,
        &[
            &["cache_read_tokens"],
            &["cache_read_input_tokens"],
            &["prompt_tokens_details", "cached_tokens"],
            &["input_tokens_details", "cached_tokens"],
            &["cached_tokens"],
            &["cachedContentTokenCount"],
            &["total_cached_tokens"],
        ],
    )
}

/// `_cache_write`:总键优先,回退 Anthropic per-TTL 分解。
fn cache_write(raw: &Value) -> i64 {
    let total = first_int(
        raw,
        &[
            &["cache_write_tokens"],
            &["cache_creation_input_tokens"],
            &["input_tokens_details", "cache_write_tokens"],
        ],
    );
    if total != 0 {
        return total;
    }
    let Some(creation) = raw.get("cache_creation").filter(|v| v.is_object()) else {
        return 0;
    };
    let breakdown = py_as_int(creation.get("ephemeral_5m_input_tokens"))
        + py_as_int(creation.get("ephemeral_1h_input_tokens"));
    if breakdown != 0 {
        breakdown
    } else {
        py_as_int(creation.get("ephemeral_input_tokens"))
    }
}

/// 推理(思考)token:输出中用于推理的部分。方言链:OpenAI chat 的
/// `completion_tokens_details.reasoning_tokens`、Responses 的
/// `output_tokens_details.reasoning_tokens`、SGLang 等的顶层
/// `reasoning_tokens`、Gemini 的 `thoughts_token_count` /
/// `thoughtsTokenCount`。Anthropic 无独立字段(思考已计入 output)。
fn reasoning_tokens(raw: &Value) -> i64 {
    first_int(
        raw,
        &[
            &["completion_tokens_details", "reasoning_tokens"],
            &["output_tokens_details", "reasoning_tokens"],
            &["reasoning_tokens"],
            &["thoughts_token_count"],
            &["thoughtsTokenCount"],
        ],
    )
}

/// 从任意协议的 usage dict 提取归一化用量;非 dict 返回空
/// (镜像 `normalize_usage`:逐字段「首个非零」方言链)。
pub fn normalize_usage(value: Option<&Value>) -> Usage {
    let Some(map) = value.filter(|v| v.is_object()) else {
        return Usage::empty();
    };
    let input_tokens = first_int(
        map,
        &[
            &["input_tokens"],
            &["prompt_tokens"],
            &["promptTokenCount"],
            &["total_input_tokens"],
        ],
    );
    let output_tokens = first_int(
        map,
        &[
            &["output_tokens"],
            &["completion_tokens"],
            &["candidatesTokenCount"],
            &["total_output_tokens"],
        ],
    );
    // Python `_first_int(...) or (...)`:0 时回退,非零直接采用。
    let total_tokens = first_int(map, &[&["total_tokens"], &["totalTokenCount"]]);
    let total_tokens = if total_tokens != 0 {
        total_tokens
    } else if input_tokens != 0 || output_tokens != 0 {
        input_tokens + output_tokens
    } else {
        0
    };
    Usage {
        input_tokens,
        output_tokens,
        cache_read_tokens: cache_read(map),
        cache_write_tokens: cache_write(map),
        total_tokens,
        reasoning_tokens: reasoning_tokens(map),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalizes_openai_names() {
        let usage = normalize_usage(Some(&json!({
            "prompt_tokens": 3, "completion_tokens": 5, "total_tokens": 8,
        })));
        assert_eq!(usage.input_tokens, 3);
        assert_eq!(usage.output_tokens, 5);
        assert_eq!(usage.total_tokens, 8);
    }

    #[test]
    fn normalizes_anthropic_and_cache_fields() {
        let usage = normalize_usage(Some(&json!({
            "input_tokens": 8, "output_tokens": 3,
            "cache_read_input_tokens": 4, "cache_creation_input_tokens": 2,
        })));
        assert_eq!(usage.input_tokens, 8);
        assert_eq!(usage.cache_read_tokens, 4);
        assert_eq!(usage.cache_write_tokens, 2);
    }

    /// Rust 新增计数器:OpenAI details 嵌套与 Gemini 方言;Anthropic 形态
    /// 无独立键,推理留在 output(不得误报)。
    #[test]
    fn normalizes_reasoning_token_dialects() {
        let openai = normalize_usage(Some(&json!({
            "prompt_tokens": 10, "completion_tokens": 20,
            "completion_tokens_details": {"reasoning_tokens": 7},
        })));
        assert_eq!(openai.reasoning_tokens, 7);
        let responses = normalize_usage(Some(&json!({
            "input_tokens": 10, "output_tokens": 20,
            "output_tokens_details": {"reasoning_tokens": 5},
        })));
        assert_eq!(responses.reasoning_tokens, 5);
        let sglang = normalize_usage(Some(&json!({
            "prompt_tokens": 10, "completion_tokens": 20, "reasoning_tokens": 3,
        })));
        assert_eq!(sglang.reasoning_tokens, 3);
        let gemini = normalize_usage(Some(&json!({
            "promptTokenCount": 10, "candidatesTokenCount": 20,
            "thoughtsTokenCount": 9,
        })));
        assert_eq!(gemini.reasoning_tokens, 9);
        let anthropic = normalize_usage(Some(&json!({
            "input_tokens": 8, "output_tokens": 3,
        })));
        assert_eq!(anthropic.reasoning_tokens, 0);
    }

    #[test]
    fn merge_max_never_double_counts() {
        let mut usage = Usage::empty();
        usage.merge_max(normalize_usage(Some(&json!({"input_tokens": 8}))));
        usage.merge_max(normalize_usage(Some(
            &json!({"input_tokens": 8, "output_tokens": 3}),
        )));
        usage.merge_max(normalize_usage(Some(&json!(
            {"output_tokens": 9, "completion_tokens_details": {"reasoning_tokens": 4}}
        ))));
        assert_eq!(usage.input_tokens, 8);
        assert_eq!(usage.output_tokens, 9);
        assert_eq!(usage.reasoning_tokens, 4);
        assert_eq!(usage.total_tokens, 17);
    }
}
