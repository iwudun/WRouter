//! Python 值语义助手(C9 对齐的底层支撑)。
//!
//! Python 版 config 解析大量依赖动态类型的隐式强转:`bool(x)` 的
//! truthiness、`int("60000")` 的宽容解析、`a or b` 的假值回退。
//! 手工编辑过的 config JSON(用户绕过 UI 直接改文件)可能出现
//! `"cooldown_ms": "60000"`、`"enabled": 0` 这类形态,Python 版照单
//! 全收;Rust 侧用本模块的助手逐条复刻,避免 C9 兼容性回归。
//!
//! 每个函数的文档都注明了对应 Python 语义与已知差异。

use serde_json::{Map, Value};

/// Python `bool(x)`:空串/0/空容器/None 为假,其余为真。
///
/// 注意:Python 里 `bool("false")` 是 `true`(非空字符串恒真),
/// 本函数与之完全一致 —— 这是刻意行为,有测试钉住。
pub fn py_bool(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Python `int(x)` 的常用子集;`None` 表示 Python 会抛
/// ValueError/TypeError(调用方把它转成 [`crate::config::ConfigError`])。
///
/// - `int(3.9)` 向零截断为 3;`int(true)` 为 1;
/// - `int(" 5 ")` 解析成功,`int("5.0")` 失败(Python 同样失败);
/// - `int(None)` 失败(Python 抛 TypeError)。
pub fn py_int(v: &Value) -> Option<i64> {
    match v {
        Value::Null => None,
        Value::Bool(b) => Some(i64::from(*b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(i)
            } else {
                n.as_f64().map(|f| f.trunc() as i64)
            }
        }
        Value::String(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
}

/// Python `float(x)` 的常用子集;`None` 表示 Python 会抛
/// ValueError/TypeError。`float(true)` 为 1.0。
pub fn py_float(v: &Value) -> Option<f64> {
    match v {
        Value::Null => None,
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

/// Python `str(x)` 的标量子集;数组/对象返回 `None`(Python 会给出
/// 单引号 repr,真实配置中不出现,不支持时由调用方报错)。
///
/// 注意与 JSON 风格的差异是刻意的:`str(None)` → `"None"`、
/// `str(true)` → `"True"`(Python `config.rs` 的 `header_name` 解析
/// 依赖这一语义 —— `header_name: null` 会变成字符串 `"None"`)。
pub fn py_str(v: &Value) -> Option<String> {
    match v {
        Value::Null => Some("None".to_owned()),
        Value::Bool(true) => Some("True".to_owned()),
        Value::Bool(false) => Some("False".to_owned()),
        Value::Number(n) => Some(n.to_string()),
        Value::String(s) => Some(s.clone()),
        Value::Array(_) | Value::Object(_) => None,
    }
}

/// Python `m.get(k1) or m.get(k2) or fallback`:沿键序找第一个
/// **truthy** 的值(空串/null/空数组都会跳到下一个键),全假回退 fallback。
pub fn get_truthy_chain<'a>(
    map: &'a Map<String, Value>,
    keys: &[&str],
    fallback: &'a Value,
) -> &'a Value {
    for key in keys {
        if let Some(v) = map.get(*key)
            && py_bool(v)
        {
            return v;
        }
    }
    fallback
}

/// 同 [`get_truthy_chain`],但全假返回 `None` 而不是固定 fallback。
pub fn try_truthy_chain<'a>(map: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a Value> {
    for key in keys {
        if let Some(v) = map.get(*key)
            && py_bool(v)
        {
            return Some(v);
        }
    }
    None
}

/// Python `m.get(k1, m.get(k2, default))`:沿键序找第一个**存在**的键
/// (哪怕值为 null 也采用 —— 与 truthy 链不同,Python 这里以键存在性
/// 为准,null 会让 `int(None)`/`dict(None)` 抛错)。
pub fn get_key_chain<'a>(map: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|key| map.get(*key))
}

/// Python `a == b` 的 JSON 值等价:
/// - `True == 1` / `False == 0`(bool 是 int 子类);数值按 f64 比较;
/// - 数组逐元素、对象逐键递归;
/// - 跨类型(null vs 数字、字符串 vs 数字)一律 False。
pub fn py_equal(a: &Value, b: &Value) -> bool {
    use Value::{Array, Bool, Null, Number, Object, String};
    match (a, b) {
        (Bool(x), Bool(y)) => x == y,
        (Null, Null) => true,
        (String(x), String(y)) => x == y,
        (Number(_), Number(_)) | (Bool(_), Number(_)) | (Number(_), Bool(_)) => {
            let as_num = |v: &Value| match v {
                Number(n) => n.as_f64(),
                Bool(true) => Some(1.0),
                Bool(false) => Some(0.0),
                _ => None,
            };
            match (as_num(a), as_num(b)) {
                (Some(x), Some(y)) => x == y,
                _ => false,
            }
        }
        (Array(x), Array(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(i, j)| py_equal(i, j))
        }
        (Object(x), Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|other| py_equal(v, other)))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{py_bool, py_float, py_int, py_str};
    use serde_json::json;

    #[test]
    fn py_bool_follows_python_truthiness() {
        assert!(!py_bool(&json!(null)));
        assert!(!py_bool(&json!(false)));
        assert!(!py_bool(&json!(0)));
        assert!(!py_bool(&json!(0.0)));
        assert!(!py_bool(&json!("")));
        assert!(!py_bool(&json!([])));
        assert!(!py_bool(&json!({})));
        // 非空字符串恒真 —— 包括 "false"/"0"(Python 同款怪癖,刻意钉住)。
        assert!(py_bool(&json!("false")));
        assert!(py_bool(&json!("0")));
        assert!(py_bool(&json!(1)));
        assert!(py_bool(&json!(-1)));
        assert!(py_bool(&json!([0])));
        assert!(py_bool(&json!({"a": 0})));
    }

    #[test]
    fn py_int_coerces_like_python() {
        assert_eq!(py_int(&json!(5)), Some(5));
        assert_eq!(py_int(&json!(3.9)), Some(3)); // int() 向零截断
        assert_eq!(py_int(&json!(-3.9)), Some(-3));
        assert_eq!(py_int(&json!(true)), Some(1));
        assert_eq!(py_int(&json!(false)), Some(0));
        assert_eq!(py_int(&json!("60000")), Some(60000));
        assert_eq!(py_int(&json!("  7 ")), Some(7));
        assert_eq!(py_int(&json!("+8")), Some(8));
        // Python 会抛 ValueError / TypeError 的形态。
        assert_eq!(py_int(&json!("5.0")), None);
        assert_eq!(py_int(&json!("abc")), None);
        assert_eq!(py_int(&json!(null)), None);
        assert_eq!(py_int(&json!([1])), None);
    }

    #[test]
    fn py_float_coerces_like_python() {
        assert_eq!(py_float(&json!(0.5)), Some(0.5));
        assert_eq!(py_float(&json!(2)), Some(2.0));
        assert_eq!(py_float(&json!(true)), Some(1.0));
        assert_eq!(py_float(&json!("1.5")), Some(1.5));
        assert_eq!(py_float(&json!("x")), None);
        assert_eq!(py_float(&json!(null)), None);
    }

    #[test]
    fn py_str_mirrors_python_str_for_scalars() {
        assert_eq!(py_str(&json!("hi")), Some("hi".to_owned()));
        assert_eq!(py_str(&json!(42)), Some("42".to_owned()));
        assert_eq!(py_str(&json!(1.5)), Some("1.5".to_owned()));
        // Python str() 风格(非 JSON 风格)—— header_name: null → "None" 的怪癖来源。
        assert_eq!(py_str(&json!(null)), Some("None".to_owned()));
        assert_eq!(py_str(&json!(true)), Some("True".to_owned()));
        assert_eq!(py_str(&json!(false)), Some("False".to_owned()));
        assert_eq!(py_str(&json!([1])), None);
    }
}
