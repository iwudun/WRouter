//! Python `json.dumps` 默认参数的逐字节等价序列化(R2 relay 依赖)。
//!
//! relay 把上游 SSE 的 data JSON 解析后重新序列化回客户端;黄金样本
//! 逐行比对,因此输出必须与 Python `json.dumps(obj, ensure_ascii=False)`
//! 完全一致:分隔符 `", "` / `": "`(带空格)、非 ASCII 原样输出、
//! 键序保持插入序(serde_json preserve_order)。
//!
//! Python 的字符串转义规则与 JSON 标准在常见字符上一致(`\"` `\\`
//! `\n` `\t` `\r` `\b` `\f`,其余 < 0x20 用 `\uXXXX`);serde_json 的
//! escape 行为相同,故直接复用其标量转义,只自定义结构与分隔符。

use serde_json::Value;

/// `json.dumps` 的两个可选参数(R4 幂等指纹 / 日志预览需要)。
#[derive(Clone, Copy)]
struct DumpOpts {
    /// `sort_keys=True`:对象键按 Unicode 码点升序输出。
    sort_keys: bool,
    /// `separators=(",", ":")`:元素/键值之间不带空格。
    compact: bool,
}

const DEFAULT_OPTS: DumpOpts = DumpOpts {
    sort_keys: false,
    compact: false,
};
const SORTED_OPTS: DumpOpts = DumpOpts {
    sort_keys: true,
    compact: false,
};
const SORTED_COMPACT_OPTS: DumpOpts = DumpOpts {
    sort_keys: true,
    compact: true,
};

/// 序列化为 Python `json.dumps(value, ensure_ascii=False)` 的等价文本。
pub fn py_json_dumps(value: &Value) -> String {
    let mut out = String::new();
    write_value(value, &mut out, DEFAULT_OPTS);
    out
}

/// `json.dumps(value, ensure_ascii=False, separators=(",", ":"))` 等价文本。
pub fn py_json_dumps_compact(value: &Value) -> String {
    let mut out = String::new();
    write_value(
        value,
        &mut out,
        DumpOpts {
            sort_keys: false,
            compact: true,
        },
    );
    out
}

/// `json.dumps(value, ensure_ascii=False, sort_keys=True)` 等价文本。
///
/// 键序按 UTF-8 字节序比较,与 Python 的码点序一致(UTF-8 保序)。
pub fn py_json_dumps_sorted(value: &Value) -> String {
    let mut out = String::new();
    write_value(value, &mut out, SORTED_OPTS);
    out
}

/// `json.dumps(value, ensure_ascii=False, sort_keys=True,
/// separators=(",", ":"))` 等价文本(Python 指纹算法用这一形态)。
pub fn py_json_dumps_sorted_compact(value: &Value) -> String {
    let mut out = String::new();
    write_value(value, &mut out, SORTED_COMPACT_OPTS);
    out
}

fn write_value(value: &Value, out: &mut String, opts: DumpOpts) {
    let (item_sep, key_sep) = if opts.compact {
        (",", ":")
    } else {
        (", ", ": ")
    };
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => write_json_string(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(item_sep);
                }
                write_value(item, out, opts);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            if opts.sort_keys {
                entries.sort_by_key(|(key, _)| key.as_str());
            }
            out.push('{');
            for (i, (key, item)) in entries.into_iter().enumerate() {
                if i > 0 {
                    out.push_str(item_sep);
                }
                write_json_string(key, out);
                out.push_str(key_sep);
                write_value(item, out, opts);
            }
            out.push('}');
        }
    }
}

/// JSON 字符串字面量(带引号)。控制字符转义对齐 Python `json.dumps`:
/// `\b \t \n \f \r` 用短转义,其余 < 0x20 用 `\u00xx` 小写十六进制。
fn write_json_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mirrors_python_dumps_defaults() {
        // python -c "import json;print(json.dumps({'id':'c1','n':1,'ok':True,'x':None}))"
        let value = json!({"id": "c1", "n": 1, "ok": true, "x": null});
        assert_eq!(
            py_json_dumps(&value),
            r#"{"id": "c1", "n": 1, "ok": true, "x": null}"#
        );
    }

    #[test]
    fn keeps_non_ascii_raw_and_key_order() {
        // ensure_ascii=False:非 ASCII 原样;preserve_order:保持插入序。
        let value = json!({"delta": {"content": "你好"}, "b": 1, "a": 2});
        assert_eq!(
            py_json_dumps(&value),
            r#"{"delta": {"content": "你好"}, "b": 1, "a": 2}"#
        );
    }

    #[test]
    fn escapes_inner_quotes_like_python() {
        // partial_json 片段(C7):内嵌引号转义后与 Python 一致。
        let value = json!({"delta": {"content": "{\"loc\":\"SF\"}"}});
        assert_eq!(
            py_json_dumps(&value),
            r#"{"delta": {"content": "{\"loc\":\"SF\"}"}}"#
        );
    }
}
