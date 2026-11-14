//! 账号快照的纯表达式求值子集(Python 逐语义移植,自 `account_snapshot.rs` 纯搬移)。

use serde_json::{Value, json};

use wr_domain::python_semantics::py_str;

// 表达式求值子集(Python 94-296 行逐语义移植)
// ---------------------------------------------------------------------------

/// `_resolve_simple_path`:`$.a.b.c` / `$["k"]` / `$.arr[0]`;过滤器
/// 表达式返回 Err(必须走 [`resolve_filter`]);中途非 dict / 命中 null /
/// 索引越界 → Ok(None)。
pub(super) fn resolve_simple_path(payload: &Value, path: &str) -> Result<Option<Value>, String> {
    if path == "$" {
        return Ok(Some(payload.clone()));
    }
    let mut cursor = payload.clone();
    // lstrip("$") + lstrip(".")(剥掉全部前导 $ 与 .)。
    let rest = path.trim_start_matches('$').trim_start_matches('.');
    if rest.is_empty() {
        return Ok(Some(cursor));
    }
    // 分词:键与 [..] 交替。
    let mut tokens: Vec<String> = Vec::new();
    let mut buf = String::new();
    let chars: Vec<char> = rest.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if ch == '.' {
            if !buf.is_empty() {
                tokens.push(std::mem::take(&mut buf));
            }
        } else if ch == '[' {
            if !buf.is_empty() {
                tokens.push(std::mem::take(&mut buf));
            }
            let Some(end) = rest[i..].find(']').map(|off| i + off) else {
                return Err(format!("unclosed bracket in path: {path:?}"));
            };
            let inner: String = chars[i + 1..end].iter().collect();
            tokens.push(format!("[{}]", inner.trim()));
            i = end;
        } else {
            buf.push(ch);
        }
        i += 1;
    }
    if !buf.is_empty() {
        tokens.push(buf);
    }

    for tok in tokens {
        if tok.starts_with('[') && tok.ends_with(']') {
            let inner = tok[1..tok.len() - 1].trim();
            if inner.starts_with('?') {
                return Err("filter expressions must use _resolve_filter".to_owned());
            }
            if (inner.starts_with('"') && inner.ends_with('"') && inner.len() >= 2)
                || (inner.starts_with('\'') && inner.ends_with('\'') && inner.len() >= 2)
            {
                let key = &inner[1..inner.len() - 1];
                let Some(map) = cursor.as_object() else {
                    return Ok(None);
                };
                let Some(next) = map.get(key) else {
                    return Ok(None);
                };
                cursor = next.clone();
                continue;
            }
            let Ok(idx) = inner.parse::<i64>() else {
                return Err(format!("unsupported bracket content: {tok:?}"));
            };
            let Some(list) = cursor.as_array() else {
                return Ok(None);
            };
            let len = list.len() as i64;
            if idx >= len || idx < -len {
                return Ok(None);
            }
            cursor = list[if idx < 0 {
                (len + idx) as usize
            } else {
                idx as usize
            }]
            .clone();
            continue;
        }
        let Some(map) = cursor.as_object() else {
            return Ok(None);
        };
        let Some(next) = map.get(&tok) else {
            return Ok(None);
        };
        cursor = next.clone();
    }
    Ok(Some(cursor))
}

/// `_resolve_filter`:`$.x[?(@.k=="V" && @.n>2)]` — 返回首个匹配元素。
pub(super) fn resolve_filter(payload: &Value, expr: &str) -> Result<Option<Value>, String> {
    let Some(cut) = expr.find("[?(") else {
        return Err(format!("unsupported filter expression: {expr:?}"));
    };
    if !expr.ends_with(")]") {
        return Err(format!("unsupported filter expression: {expr:?}"));
    }
    let prefix = &expr[..cut];
    let condition = &expr[cut + 3..expr.len() - 2];

    let Some(parent) = resolve_simple_path(payload, prefix)? else {
        return Ok(None);
    };
    let Some(list) = parent.as_array() else {
        return Ok(None);
    };

    let clause_re = regex::Regex::new(r"^(@\.[\w.]+)\s*(==|!=|>=|<=|>|<)\s*(.+)$").unwrap();
    let mut compiled: Vec<(String, String, Value)> = Vec::new();
    for part in condition.split("&&") {
        let part = part.trim();
        let Some(captures) = clause_re.captures(part) else {
            return Err(format!("unsupported filter clause: {part:?}"));
        };
        let left = captures[1].to_owned();
        let op = captures[2].to_owned();
        let right_raw = captures[3].trim();
        let right: Value =
            if right_raw.starts_with('"') && right_raw.ends_with('"') && right_raw.len() >= 2 {
                Value::String(right_raw[1..right_raw.len() - 1].to_owned())
            } else {
                match right_raw.parse::<i64>() {
                    Ok(v) => json!(v),
                    Err(_) => match right_raw.parse::<f64>() {
                        Ok(v) => json!(v),
                        Err(_) => Value::String(right_raw.to_owned()),
                    },
                }
            };
        compiled.push((left, op, right));
    }

    for item in list {
        let mut ok = true;
        for (left, op, right) in &compiled {
            // "@.key" → 下钻 item(按 "." 逐段 get)。
            let sub_path = left.trim_start_matches('@').trim_start_matches('.');
            let mut actual: Value = item.clone();
            for tok in sub_path.split('.') {
                match actual.as_object().and_then(|m| m.get(tok)) {
                    Some(next) => actual = next.clone(),
                    None => {
                        actual = Value::Null;
                        break;
                    }
                }
            }
            if !compare(&actual, op, right) {
                ok = false;
                break;
            }
        }
        if ok {
            return Ok(Some(item.clone()));
        }
    }
    Ok(None)
}

/// `_compare`:`==`/`!=` 用 Python 值相等(bool 与数字互通,True==1);
/// 其余按 `float(left) op float(right)`,失败一律 False。
pub(super) fn compare(left: &Value, op: &str, right: &Value) -> bool {
    match op {
        "==" => py_eq(left, right),
        "!=" => !py_eq(left, right),
        ">" | "<" | ">=" | "<=" => {
            let Some(l) = py_to_f64(left) else {
                return false;
            };
            let Some(r) = py_to_f64(right) else {
                return false;
            };
            match op {
                ">" => l > r,
                "<" => l < r,
                ">=" => l >= r,
                "<=" => l <= r,
                _ => false,
            }
        }
        _ => false,
    }
}

/// Python `float()` 子集:数值/bool/可解析字符串;null/容器 → None。
pub(super) fn py_to_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Null => None, // float(None) → TypeError
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

/// Python 标量相等:bool 与数字互通(True==1),数值跨整型/浮点比较,
/// 字符串只与字符串相等,容器类型在此不出现。
pub(super) fn py_eq(left: &Value, right: &Value) -> bool {
    let num = |v: &Value| match v {
        Value::Number(_) => Some(v.as_f64()),
        Value::Bool(b) => Some(Some(if *b { 1.0 } else { 0.0 })),
        _ => Some(None),
    };
    match (left, right) {
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Null, Value::Null) => true,
        _ => match (num(left), num(right)) {
            (Some(Some(a)), Some(Some(b))) => a == b,
            _ => false,
        },
    }
}

/// `_eval_number_expression`:字面量数字 → JSON 路径 → 数组回退 → 减法
/// 形态 `"a - b"`(仅空白包裹的 ` - `)。
pub(super) fn eval_number_expression(expr: Option<&Value>, payload: &Value) -> Option<f64> {
    let expr = expr?;
    match expr {
        // isinstance(expr, (int, float)) 对 bool 亦真(True→1.0)。
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Array(items) => {
            for item in items {
                if let Some(value) = eval_number_expression(Some(item), payload) {
                    return Some(value);
                }
            }
            None
        }
        Value::String(text) => {
            let text = text.trim();
            if text.is_empty() {
                return None;
            }
            // 减法形态:仅 "a - b"(带空白);两侧各走一次求值。
            if let Some((left_str, right_str)) = text.split_once(" - ") {
                let left = eval_number_expression(
                    Some(&Value::String(left_str.trim().to_owned())),
                    payload,
                );
                let right = eval_number_expression(
                    Some(&Value::String(right_str.trim().to_owned())),
                    payload,
                );
                return match (left, right) {
                    (Some(l), Some(r)) => Some(l - r),
                    _ => None,
                };
            }
            if let Ok(value) = text.parse::<f64>() {
                return Some(value);
            }
            let resolved = if text.contains("[?(") {
                resolve_filter(payload, text)
            } else {
                resolve_simple_path(payload, text)
            };
            let value = resolved.ok()??;
            match &value {
                Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
                Value::Number(n) => n.as_f64(),
                Value::String(s) => s.parse::<f64>().ok(),
                _ => None,
            }
        }
        _ => None,
    }
}

/// `_eval_string_expression`:字面量字符串(含 `$` 时尝试按路径求值)→
/// JSON 路径 → 数组回退(首个非空)。非 str/list 形态 → None。
pub(super) fn eval_string_expression(expr: Option<&Value>, payload: &Value) -> Option<String> {
    let expr = expr?;
    match expr {
        Value::String(text) if !text.starts_with('$') => {
            if text.trim().is_empty() {
                return None;
            }
            if text.contains('$') {
                // 可能是内含 $ 的路径;失败即 None。
                let value = resolve_simple_path(payload, text).ok()??;
                return match &value {
                    Value::String(s) => Some(s.clone()),
                    other => py_str(other),
                };
            }
            Some(text.clone())
        }
        Value::Array(items) => {
            for item in items {
                if let Some(value) = eval_string_expression(Some(item), payload)
                    && !value.is_empty()
                {
                    return Some(value);
                }
            }
            None
        }
        Value::String(text) => {
            let value = resolve_simple_path(payload, text).ok()??;
            match &value {
                Value::String(s) => Some(s.clone()),
                other => py_str(other),
            }
        }
        _ => None,
    }
}

/// `_normalize_reset_at`:epoch → ISO 串(< 1e12 视为秒,否则毫秒);
/// 字符串原样;非正数 → None。当前仅测试覆盖,保留为语义参考。
#[cfg(test)]
pub(super) fn normalize_reset_at(value: Option<&Value>) -> Option<String> {
    let value = value?;
    match value {
        Value::String(s) => Some(s.clone()),
        other => {
            let number = match other {
                Value::Number(n) => n.as_f64(),
                Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
                _ => None,
            }?;
            let seconds = if number >= 1_000_000_000_000.0 {
                number / 1000.0
            } else {
                number
            };
            if seconds <= 0.0 {
                return None;
            }
            use chrono::TimeZone;
            let dt = chrono::Utc.timestamp_opt(seconds as i64, 0).single()?;
            Some(dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        }
    }
}
