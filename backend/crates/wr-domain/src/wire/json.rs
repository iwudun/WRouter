//! JSON 原始文档与解析视图(方案 20261106_01 P1-3、P1-5)。
//!
//! `serde_json::Value` 不保留重复键、数值词法和原始空白;这些事实由
//! [`JsonView`] 显式记录,原始字节始终是权威来源。视图只作为**共享**
//! 解释入口:同一原文经同一 [`JsonView`] 消费,避免各消费者各自重解析
//! 得到冲突含义。
//!
//! 策略:
//! - 重复键按 `serde_json`(preserve_order)最后写入生效,但所有重复
//!   路径进入 `duplicate_keys`,`structure_exact_safe()` 返回 false;
//! - 数值词法超出安全精度/范围时进入 `number_risks`,不据此静默改写;
//! - 非法 UTF-8、语法错误、深度超限均返回 `value = None` 与明确错误,
//!   原始字节不丢;
//! - 开头 UTF-8 BOM 只从解析视图中剥离,原始字节保留。

use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;

use serde_json::Value;

use crate::wire::error::WireLimits;

/// JSON 解析失败类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonSyntaxErrorKind {
    Syntax,
    InvalidUtf8,
    DepthLimit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonSyntaxError {
    pub kind: JsonSyntaxErrorKind,
    pub message: String,
    pub line: usize,
    pub column: usize,
}

impl fmt::Display for JsonSyntaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?} at {}:{}: {}",
            self.kind, self.line, self.column, self.message
        )
    }
}

/// 重复键报告。`path` 为诊断路径,不承诺是 RFC 6901 JSON Pointer。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateJsonKey {
    pub path: String,
    pub key: String,
}

/// 数值词法风险类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumberRiskKind {
    /// 整数超出 i64/u64 可表示范围。
    IntegerRange,
    /// 有效数字超过 17 位,经 f64 往返可能改变值。
    ManySignificantDigits,
    /// 使用指数形式,重序列化可能改变词法。
    ExponentNotation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NumberRisk {
    pub path: String,
    pub lexeme: String,
    pub kind: NumberRiskKind,
}

/// JSON 原文的解析视图。
#[derive(Debug, Clone, PartialEq)]
pub struct JsonView {
    value: Option<Value>,
    error: Option<JsonSyntaxError>,
    duplicate_keys: Vec<DuplicateJsonKey>,
    number_risks: Vec<NumberRisk>,
    reports_truncated: bool,
    max_depth: usize,
    had_bom: bool,
    utf8_valid: bool,
    byte_len: usize,
}

impl JsonView {
    pub fn value(&self) -> Option<&Value> {
        self.value.as_ref()
    }

    pub fn into_value(self) -> Option<Value> {
        self.value
    }

    pub fn error(&self) -> Option<&JsonSyntaxError> {
        self.error.as_ref()
    }

    pub fn duplicate_keys(&self) -> &[DuplicateJsonKey] {
        &self.duplicate_keys
    }

    pub fn number_risks(&self) -> &[NumberRisk] {
        &self.number_risks
    }

    pub fn reports_truncated(&self) -> bool {
        self.reports_truncated
    }

    pub fn max_depth(&self) -> usize {
        self.max_depth
    }

    pub fn had_bom(&self) -> bool {
        self.had_bom
    }

    pub fn utf8_valid(&self) -> bool {
        self.utf8_valid
    }

    pub fn byte_len(&self) -> usize {
        self.byte_len
    }

    pub fn is_valid(&self) -> bool {
        self.value.is_some()
    }

    /// 是否可把「重新序列化整条记录」声明为 StructureExact。
    ///
    /// 重复键、数值精度风险都会让 `serde_json::Value` 往返不再可靠;
    /// 需要保留这些原文时只能原字节转发或声明整条记录重写。
    pub fn structure_exact_safe(&self) -> bool {
        self.is_valid()
            && self.duplicate_keys.is_empty()
            && self.number_risks.is_empty()
            && !self.reports_truncated
    }
}

/// 解析一条完整 JSON 原文为共享视图。语法错误不会返回 `Err`;
/// 只有预算类错误由调用方在更外层处理。
pub fn parse_json_view(bytes: &[u8], limits: &WireLimits) -> Arc<JsonView> {
    let (body, had_bom) = strip_utf8_bom(bytes);
    let utf8_valid = std::str::from_utf8(body).is_ok();

    let scan = scan_json(body, limits);
    let (mut value, mut error) = (None, None);
    if !utf8_valid {
        error = Some(JsonSyntaxError {
            kind: JsonSyntaxErrorKind::InvalidUtf8,
            message: "JSON body is not valid UTF-8".to_owned(),
            line: 0,
            column: 0,
        });
    } else if scan.depth_exceeded {
        error = Some(JsonSyntaxError {
            kind: JsonSyntaxErrorKind::DepthLimit,
            message: format!(
                "JSON nesting depth exceeds configured limit {}",
                limits.max_json_depth
            ),
            line: 0,
            column: 0,
        });
    } else {
        match serde_json::from_slice::<Value>(body) {
            Ok(parsed) => value = Some(parsed),
            Err(err) => {
                error = Some(JsonSyntaxError {
                    kind: JsonSyntaxErrorKind::Syntax,
                    message: err.to_string(),
                    line: err.line(),
                    column: err.column(),
                });
            }
        }
    }

    Arc::new(JsonView {
        value,
        error,
        duplicate_keys: scan.duplicate_keys,
        number_risks: scan.number_risks,
        reports_truncated: scan.reports_truncated,
        max_depth: scan.max_depth,
        had_bom,
        utf8_valid,
        byte_len: bytes.len(),
    })
}

fn strip_utf8_bom(bytes: &[u8]) -> (&[u8], bool) {
    match bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        Some(rest) => (rest, true),
        None => (bytes, false),
    }
}

/// 定点替换顶层 `model` 字符串值的字节 span(方案 20261106_01 R8/P3-1)。
///
/// 只替换 `model` 值本身(含引号),其余字节 —— 空白、键序、数值词法、
/// 未知字段、嵌套业务对象里的同名 `model` —— 全部原样保留。用于原生
/// 请求面:网关剥前缀/逐候选改写模型名时,不重序列化整条请求体。
///
/// 返回 `None` 表示无法定点修改(无顶层 model、值非字符串、语法不完整),
/// 调用方据此回退到声明过的整体重序列化,不静默改写。
pub fn patch_top_level_model(bytes: &[u8], new_model: &str) -> Option<Vec<u8>> {
    // 开头 BOM 原样保留:扫描跳过它,span 仍按原始字节绝对偏移计算。
    let mut pos = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        3
    } else {
        0
    };
    pos = skip_ws(bytes, pos);
    if bytes.get(pos) != Some(&b'{') {
        return None;
    }
    pos += 1;
    loop {
        pos = skip_ws(bytes, pos);
        if bytes.get(pos) != Some(&b'"') {
            return None;
        }
        let (key_end, key_token) = scan_string(bytes, pos);
        let key = decode_json_string(key_token)?;
        pos = skip_ws(bytes, key_end);
        if bytes.get(pos) != Some(&b':') {
            return None;
        }
        pos = skip_ws(bytes, pos + 1);
        let value_start = pos;
        if key == "model" {
            if bytes.get(pos) != Some(&b'"') {
                return None;
            }
            let (value_end, _) = scan_string(bytes, pos);
            if value_end == 0 || bytes.get(value_end - 1) != Some(&b'"') {
                return None;
            }
            let replacement = serde_json::to_vec(new_model).ok()?;
            let mut out = Vec::with_capacity(
                bytes.len() - (value_end - value_start) + replacement.len(),
            );
            out.extend_from_slice(&bytes[..value_start]);
            out.extend_from_slice(&replacement);
            out.extend_from_slice(&bytes[value_end..]);
            return Some(out);
        }
        pos = skip_json_value(bytes, pos)?;
        pos = skip_ws(bytes, pos);
        match bytes.get(pos) {
            Some(b',') => pos += 1,
            _ => return None,
        }
    }
}

/// 跳过一个完整 JSON 值,返回其后位置;语法不完整返回 `None`。
fn skip_json_value(bytes: &[u8], mut pos: usize) -> Option<usize> {
    match bytes.get(pos)? {
        b'"' => {
            let (end, _) = scan_string(bytes, pos);
            (end <= bytes.len() && bytes.get(end.wrapping_sub(1)) == Some(&b'"')).then_some(end)
        }
        b'{' | b'[' => {
            let open = bytes[pos];
            let close = if open == b'{' { b'}' } else { b']' };
            let mut depth = 0usize;
            while pos < bytes.len() {
                match bytes[pos] {
                    b'"' => {
                        let (end, _) = scan_string(bytes, pos);
                        pos = end;
                        continue;
                    }
                    byte if byte == open => depth += 1,
                    byte if byte == close => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(pos + 1);
                        }
                    }
                    _ => {}
                }
                pos += 1;
            }
            None
        }
        _ => {
            while pos < bytes.len()
                && !matches!(bytes[pos], b',' | b'}' | b']' | b' ' | b'\t' | b'\r' | b'\n')
            {
                pos += 1;
            }
            (pos > 0).then_some(pos)
        }
    }
}

// ---------------------------------------------------------------------------
// 扫描器:重复键、数值词法、深度
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct ScanResult {
    duplicate_keys: Vec<DuplicateJsonKey>,
    number_risks: Vec<NumberRisk>,
    reports_truncated: bool,
    max_depth: usize,
    depth_exceeded: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContainerKind {
    Object,
    Array,
}

#[derive(Debug)]
struct Frame {
    kind: ContainerKind,
    /// 对象上下文:下一个字符串是否为键。
    key_position: bool,
    /// 已识别但尚未消费的键(供容器子节点构造路径)。
    pending_key: Option<String>,
    /// 该容器在父容器中的路径段:`foo` 或 `[0]`。
    segment: String,
    /// 对象已见键(用于重复检测)。
    seen: HashSet<String>,
    /// 数组下一个元素下标(仅用于诊断路径)。
    next_index: usize,
}

impl Frame {
    fn root() -> Self {
        Self {
            kind: ContainerKind::Object,
            key_position: false,
            pending_key: None,
            segment: "$".to_owned(),
            seen: HashSet::new(),
            next_index: 0,
        }
    }

    fn child(kind: ContainerKind, segment: String) -> Self {
        Self {
            kind,
            key_position: kind == ContainerKind::Object,
            pending_key: None,
            segment,
            seen: HashSet::new(),
            next_index: 0,
        }
    }
}

fn scan_json(bytes: &[u8], limits: &WireLimits) -> ScanResult {
    let mut result = ScanResult {
        duplicate_keys: Vec::new(),
        number_risks: Vec::new(),
        reports_truncated: false,
        max_depth: 0,
        depth_exceeded: false,
    };
    let mut stack = vec![Frame::root()];
    let mut pos = 0;

    while pos < bytes.len() {
        match bytes[pos] {
            b'"' => {
                let (end, token) = scan_string(bytes, pos);
                let top_is_object = stack
                    .last()
                    .is_some_and(|frame| frame.kind == ContainerKind::Object);
                let key_position =
                    top_is_object && stack.last().is_some_and(|frame| frame.key_position);
                let next = skip_ws(bytes, end);
                if key_position && bytes.get(next) == Some(&b':') {
                    let decoded = decode_json_string(token)
                        .unwrap_or_else(|| String::from_utf8_lossy(token).into_owned());
                    let path = path_with_leaf(&stack, &decoded);
                    let top = stack.last_mut().expect("stack is never empty");
                    top.key_position = false;
                    top.pending_key = Some(decoded.clone());
                    if !top.seen.insert(decoded.clone()) {
                        push_report(
                            &mut result.duplicate_keys,
                            DuplicateJsonKey { path, key: decoded },
                            limits.max_json_reports,
                            &mut result.reports_truncated,
                        );
                    }
                } else if top_is_object {
                    stack.last_mut().expect("stack is never empty").pending_key = None;
                } else if let Some(top) = stack.last_mut()
                    && top.kind == ContainerKind::Array
                {
                    top.next_index += 1;
                }
                pos = end;
            }
            b'{' | b'[' => {
                let kind = if bytes[pos] == b'{' {
                    ContainerKind::Object
                } else {
                    ContainerKind::Array
                };
                let segment = container_segment(&mut stack);
                stack.push(Frame::child(kind, segment));
                result.max_depth = result.max_depth.max(stack.len() - 1);
                if stack.len() - 1 > limits.max_json_depth {
                    result.depth_exceeded = true;
                    break;
                }
                pos += 1;
            }
            b'}' => {
                if stack.len() > 1
                    && stack
                        .last()
                        .is_some_and(|f| f.kind == ContainerKind::Object)
                {
                    stack.pop();
                }
                pos += 1;
            }
            b']' => {
                if stack.len() > 1 && stack.last().is_some_and(|f| f.kind == ContainerKind::Array) {
                    stack.pop();
                }
                pos += 1;
            }
            b',' => {
                if let Some(top) = stack.last_mut()
                    && top.kind == ContainerKind::Object
                {
                    top.key_position = true;
                    top.pending_key = None;
                }
                pos += 1;
            }
            b':' => {
                pos += 1;
            }
            b'-' | b'0'..=b'9' => {
                let end = scan_number(bytes, pos);
                let lexeme = String::from_utf8_lossy(&bytes[pos..end]).into_owned();
                if let Some(kind) = number_risk_kind(&lexeme) {
                    let path = scalar_path(&mut stack);
                    push_report(
                        &mut result.number_risks,
                        NumberRisk { path, lexeme, kind },
                        limits.max_json_reports,
                        &mut result.reports_truncated,
                    );
                } else {
                    consume_scalar_index(&mut stack);
                }
                pos = end;
            }
            b't' => {
                consume_scalar_index(&mut stack);
                pos = (pos + 4).min(bytes.len());
            }
            b'f' => {
                consume_scalar_index(&mut stack);
                pos = (pos + 5).min(bytes.len());
            }
            b'n' => {
                consume_scalar_index(&mut stack);
                pos = (pos + 4).min(bytes.len());
            }
            _ => pos += 1,
        }
    }

    result
}

fn container_segment(stack: &mut [Frame]) -> String {
    let Some(parent) = stack.last_mut() else {
        return "$".to_owned();
    };
    match parent.kind {
        ContainerKind::Object => parent.pending_key.take().unwrap_or_default(),
        ContainerKind::Array => {
            let segment = format!("[{}]", parent.next_index);
            parent.next_index += 1;
            segment
        }
    }
}

fn consume_scalar_index(stack: &mut [Frame]) {
    if let Some(top) = stack.last_mut() {
        top.pending_key = None;
        if top.kind == ContainerKind::Array {
            top.next_index += 1;
        }
    }
}

fn scalar_path(stack: &mut [Frame]) -> String {
    let (leaf, array_index) = match stack.last_mut() {
        Some(top) if top.kind == ContainerKind::Array => {
            let index = top.next_index;
            top.next_index += 1;
            (String::new(), Some(index))
        }
        Some(top) => (top.pending_key.take().unwrap_or_default(), None),
        None => (String::new(), None),
    };
    let mut path = path_with_leaf(stack, &leaf);
    if let Some(index) = array_index {
        path.push_str(&format!("[{index}]"));
    }
    path
}

fn path_with_leaf(stack: &[Frame], leaf: &str) -> String {
    let mut out = String::new();
    for (index, frame) in stack.iter().enumerate() {
        if index == 0 {
            out.push('$');
            continue;
        }
        append_segment(&mut out, &frame.segment);
    }
    if !leaf.is_empty() {
        append_segment(&mut out, leaf);
    }
    out
}

fn append_segment(out: &mut String, segment: &str) {
    if segment.starts_with('[') {
        out.push_str(segment);
        return;
    }
    if segment.is_empty() {
        return;
    }
    let simple = segment
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if simple {
        out.push('.');
        out.push_str(segment);
    } else {
        out.push('[');
        out.push_str(&serde_json::to_string(segment).unwrap_or_else(|_| "\"?\"".to_owned()));
        out.push(']');
    }
}

fn push_report<T>(reports: &mut Vec<T>, report: T, limit: usize, truncated: &mut bool) {
    if reports.len() < limit {
        reports.push(report);
    } else {
        *truncated = true;
    }
}

fn skip_ws(bytes: &[u8], mut pos: usize) -> usize {
    while let Some(byte) = bytes.get(pos) {
        if matches!(byte, b' ' | b'\t' | b'\r' | b'\n') {
            pos += 1;
        } else {
            break;
        }
    }
    pos
}

/// 返回闭合引号之后的位置与不含引号的原始 token。
fn scan_string(bytes: &[u8], start: usize) -> (usize, &[u8]) {
    let mut pos = start + 1;
    while pos < bytes.len() {
        match bytes[pos] {
            b'\\' => pos += 2,
            b'"' => {
                let end = pos + 1;
                return (end, &bytes[start + 1..pos]);
            }
            _ => pos += 1,
        }
    }
    (bytes.len(), &bytes[start + 1..])
}

fn decode_json_string(token: &[u8]) -> Option<String> {
    let quoted = [b"\"".as_slice(), token, b"\"".as_slice()].concat();
    serde_json::from_slice::<String>(&quoted).ok()
}

fn scan_number(bytes: &[u8], start: usize) -> usize {
    let mut pos = start;
    while let Some(byte) = bytes.get(pos) {
        if byte.is_ascii_digit() || matches!(byte, b'-' | b'+' | b'.' | b'e' | b'E') {
            pos += 1;
        } else {
            break;
        }
    }
    pos
}

fn number_risk_kind(lexeme: &str) -> Option<NumberRiskKind> {
    if lexeme.contains(['e', 'E']) {
        return Some(NumberRiskKind::ExponentNotation);
    }
    let unsigned = lexeme.strip_prefix('-').unwrap_or(lexeme);
    if let Some((int_part, frac_part)) = unsigned.split_once('.') {
        let significant = int_part.trim_start_matches('0').len() + frac_part.len();
        if significant > 17 {
            return Some(NumberRiskKind::ManySignificantDigits);
        }
        return None;
    }
    let overflow = if lexeme.starts_with('-') {
        lexeme.parse::<i64>().is_err()
    } else {
        lexeme.parse::<u64>().is_err()
    };
    overflow.then_some(NumberRiskKind::IntegerRange)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> WireLimits {
        WireLimits::default()
    }

    #[test]
    fn parses_json_and_marks_clean_document() {
        let view = parse_json_view(br#"{"a":1,"b":[true,null,"x"]}"#, &limits());
        assert!(view.is_valid());
        assert!(view.structure_exact_safe());
        assert_eq!(view.duplicate_keys().len(), 0);
        assert_eq!(view.max_depth(), 2);
    }

    #[test]
    fn preserves_null_vs_absent() {
        let view = parse_json_view(br#"{"a":null}"#, &limits());
        let value = view.value().unwrap();
        assert!(value.get("a").unwrap().is_null());
        assert!(value.get("b").is_none());
    }

    #[test]
    fn detects_duplicate_keys_and_marks_view_unsafe() {
        let view = parse_json_view(br#"{"a":1,"a":2,"b":{"c":1,"c":2}}"#, &limits());
        assert!(view.is_valid());
        assert!(!view.structure_exact_safe());
        let duplicates = view.duplicate_keys();
        assert_eq!(duplicates.len(), 2);
        assert_eq!(duplicates[0].path, "$.a");
        assert_eq!(duplicates[1].path, "$.b.c");
        // serde_json 最后写入生效
        assert_eq!(view.value().unwrap()["a"], serde_json::json!(2));
    }

    #[test]
    fn escaped_duplicate_keys_use_decoded_identity() {
        let view = parse_json_view(br#"{"a":1,"\u0061":2}"#, &limits());
        assert_eq!(view.duplicate_keys().len(), 1);
        assert_eq!(view.duplicate_keys()[0].key, "a");
    }

    #[test]
    fn flags_large_integer_and_exponent() {
        let view = parse_json_view(
            br#"{"n":184467440737095516160,"e":1e3,"d":3.141592653589793}"#,
            &limits(),
        );
        let risks = view.number_risks();
        assert_eq!(risks.len(), 2);
        assert_eq!(risks[0].path, "$.n");
        assert_eq!(risks[0].kind, NumberRiskKind::IntegerRange);
        assert_eq!(risks[1].path, "$.e");
        assert_eq!(risks[1].kind, NumberRiskKind::ExponentNotation);
        assert!(!view.structure_exact_safe());
    }

    #[test]
    fn invalid_utf8_is_flagged_but_view_absent() {
        let bytes = b"{\"a\":\"\xFF\"}";
        let view = parse_json_view(bytes, &limits());
        assert!(!view.is_valid());
        assert!(!view.utf8_valid());
        assert_eq!(view.error().unwrap().kind, JsonSyntaxErrorKind::InvalidUtf8);
    }

    #[test]
    fn bom_is_stripped_for_parse_only() {
        let view = parse_json_view(b"\xEF\xBB\xBF{\"a\":1}", &limits());
        assert!(view.is_valid());
        assert!(view.had_bom());
    }

    #[test]
    fn depth_limit_is_explicit() {
        let limits = WireLimits::default().with_max_json_depth(2);
        let view = parse_json_view(br#"{"a":{"b":{"c":1}}}"#, &limits);
        assert!(!view.is_valid());
        assert_eq!(view.error().unwrap().kind, JsonSyntaxErrorKind::DepthLimit);
    }

    #[test]
    fn syntax_error_keeps_position() {
        let view = parse_json_view(br#"{"a":}"#, &limits());
        assert!(!view.is_valid());
        let error = view.error().unwrap();
        assert_eq!(error.kind, JsonSyntaxErrorKind::Syntax);
        assert!(error.line >= 1);
    }

    #[test]
    fn reports_are_bounded() {
        let limits = WireLimits {
            max_json_reports: 2,
            ..WireLimits::default()
        };
        let view = parse_json_view(br#"{"a":1,"a":2,"a":3,"a":4}"#, &limits);
        assert_eq!(view.duplicate_keys().len(), 2);
        assert!(view.reports_truncated());
        assert!(!view.structure_exact_safe());
    }

    #[test]
    fn patch_model_preserves_all_other_bytes() {
        let raw = b"{ \"model\" : \"prov/old\" , \"messages\" : [{\"role\":\"user\",\"content\":\"hi\"}], \"temperature\" : 0.70 , \"n\": 1e3 }\n";
        let patched = patch_top_level_model(raw, "new-model").unwrap();
        let expected = b"{ \"model\" : \"new-model\" , \"messages\" : [{\"role\":\"user\",\"content\":\"hi\"}], \"temperature\" : 0.70 , \"n\": 1e3 }\n";
        assert_eq!(patched, expected);
    }

    #[test]
    fn patch_model_does_not_touch_nested_business_model() {
        let raw = br#"{"model":"old","tools":[{"function":{"parameters":{"model":"business"}}}]}"#;
        let patched = patch_top_level_model(raw, "new").unwrap();
        assert_eq!(
            patched,
            br#"{"model":"new","tools":[{"function":{"parameters":{"model":"business"}}}]}"#
        );
    }

    #[test]
    fn patch_model_escapes_replacement() {
        let raw = br#"{"model":"old","x":1}"#;
        let patched = patch_top_level_model(raw, "a\"b\\c").unwrap();
        let value: Value = serde_json::from_slice(&patched).unwrap();
        assert_eq!(value["model"], "a\"b\\c");
        assert_eq!(value["x"], 1);
    }

    #[test]
    fn patch_model_handles_escaped_key_and_leading_bom() {
        let raw = b"\xEF\xBB\xBF{\"\\u006dodel\":\"old\",\"z\":2}";
        let patched = patch_top_level_model(raw, "new").unwrap();
        assert_eq!(patched, b"\xEF\xBB\xBF{\"\\u006dodel\":\"new\",\"z\":2}");
    }

    #[test]
    fn patch_model_absent_non_string_or_malformed_returns_none() {
        assert!(patch_top_level_model(br#"{"x":1}"#, "new").is_none());
        assert!(patch_top_level_model(br#"{"model":42}"#, "new").is_none());
        assert!(patch_top_level_model(br#"{"model":"unterminated}"#, "new").is_none());
        assert!(patch_top_level_model(b"[1,2,3]", "new").is_none());
    }

    #[test]
    fn patch_model_skips_complex_preceding_values() {
        let raw = br#"{"a":{"b":[1,{"c":"}"}]},"model":"old","d":true}"#;
        let patched = patch_top_level_model(raw, "new").unwrap();
        assert_eq!(
            patched,
            br#"{"a":{"b":[1,{"c":"}"}]},"model":"new","d":true}"#
        );
    }
}
