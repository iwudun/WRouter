//! 首期 3 个无损压缩引擎(20260905_03 §4.3):
//!
//! - [`WhitespaceLite`](priority 10):连续空白折叠;代码围栏内不动;
//! - [`TableColumnar`](priority 20):同构 JSON 数组 → 列式紧凑块 +
//!   `[N rows]` 审计标记;
//! - [`ToolOutputTrim`](priority 30):超长工具输出保头尾 + 省略桩。
//!
//! 全部纯 CPU 字符串处理,单请求毫秒级(方案 §8);畸形结构静默跳过,
//! 未改动时必须给 skip_reason(管线契约,见 [`super::CompressionEngine`])。

use serde_json::Value;

use super::{CompressionCtx, CompressionEngine, EngineStats, EngineTargets};

// ---------------------------------------------------------------------------
// 文本槽位遍历(引擎共用)
// ---------------------------------------------------------------------------

/// `&mut Value` → `&mut String`(serde_json 无 as_string_mut;模式匹配取内层)。
fn text_mut(value: &mut Value) -> Option<&mut String> {
    match value {
        Value::String(text) => Some(text),
        _ => None,
    }
}

/// 槽位类别(引擎 targets 的判定依据)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotKind {
    /// system 段(缓存感知门默认整段保护,引擎 targets 均不含)。
    System,
    /// 普通 messages 文本。
    Message,
    /// 工具输出(role=tool 消息 / Anthropic tool_result 块内容)。
    ToolResult,
}

/// 遍历请求体的全部可变文本槽位,对每个槽位调用 `f(下标, 类别, 文本)`,
/// 返回 true 表示文本被修改。缓存感知门在此落地:system 受
/// `ctx.protect_system` 保护,`messages[..=protect_messages_up_to]`
/// 受断点前缀保护。畸形结构(content 为 null、块非对象等)静默跳过。
pub fn for_each_text_slot(
    body: &mut Value,
    ctx: &CompressionCtx,
    f: &mut impl FnMut(usize, SlotKind, &mut String) -> bool,
) {
    let Some(map) = body.as_object_mut() else {
        return;
    };
    // system 段(缓存感知门:默认整段不压)。
    if !ctx.protect_system {
        match map.get_mut("system") {
            Some(Value::Array(items)) => {
                for item in items.iter_mut() {
                    if let Some(text) = item.get_mut("text").and_then(text_mut) {
                        f(usize::MAX, SlotKind::System, text);
                    }
                }
            }
            Some(Value::String(text)) => {
                f(usize::MAX, SlotKind::System, text);
            }
            _ => {}
        }
    }
    // messages。
    let Some(Value::Array(messages)) = map.get_mut("messages") else {
        return;
    };
    for (index, message) in messages.iter_mut().enumerate() {
        if let Some(bound) = ctx.protect_messages_up_to
            && index <= bound
        {
            continue;
        }
        walk_message_slots(index, message, f);
    }
}

/// 单条消息的槽位下钻(openai role=tool / Anthropic tool_result /
/// 普通 content string / block 数组)。
fn walk_message_slots(
    index: usize,
    message: &mut Value,
    f: &mut impl FnMut(usize, SlotKind, &mut String) -> bool,
) {
    let Some(map) = message.as_object_mut() else {
        return;
    };
    let is_tool_message = map.get("role").and_then(Value::as_str) == Some("tool");
    let Some(content) = map.get_mut("content") else {
        return;
    };
    match content {
        // 字符串 content:openai role=tool → 工具输出;否则普通消息。
        Value::String(text) => {
            let kind = if is_tool_message {
                SlotKind::ToolResult
            } else {
                SlotKind::Message
            };
            f(index, kind, text);
        }
        Value::Array(blocks) => {
            for block in blocks.iter_mut() {
                walk_content_block(index, is_tool_message, block, f);
            }
        }
        _ => {}
    }
}

/// 单个 content block:tool_result 块的 content(string/内层块数组)→
/// 工具输出;其余块的 `text` 字段 → 普通消息文本。
fn walk_content_block(
    index: usize,
    is_tool_message: bool,
    block: &mut Value,
    f: &mut impl FnMut(usize, SlotKind, &mut String) -> bool,
) {
    let Some(map) = block.as_object_mut() else {
        return;
    };
    if map.get("type").and_then(Value::as_str) == Some("tool_result") {
        match map.get_mut("content") {
            Some(Value::String(text)) => {
                f(index, SlotKind::ToolResult, text);
            }
            Some(Value::Array(inner)) => {
                for item in inner.iter_mut() {
                    if let Some(text) = item.get_mut("text").and_then(text_mut) {
                        f(index, SlotKind::ToolResult, text);
                    }
                }
            }
            _ => {}
        }
        return;
    }
    if is_tool_message && let Some(text) = map.get_mut("text").and_then(text_mut) {
        // openai tool 消息的 block 数组形态。
        f(index, SlotKind::ToolResult, text);
        return;
    }
    if let Some(text) = map.get_mut("text").and_then(text_mut) {
        f(index, SlotKind::Message, text);
    }
}

// ---------------------------------------------------------------------------
// 引擎一:whitespace-lite
// ---------------------------------------------------------------------------

/// 连续空白折叠(≥2 个空白字符 → 单空格);代码围栏(``` 包围段)内
/// 不动 —— 折叠会改变代码语义。最短 64 字符才值得处理。
pub struct WhitespaceLite;

const WHITESPACE_MIN_CHARS: usize = 64;

impl CompressionEngine for WhitespaceLite {
    fn id(&self) -> &'static str {
        "whitespace-lite"
    }

    fn stack_priority(&self) -> u32 {
        10
    }

    fn targets(&self) -> EngineTargets {
        EngineTargets {
            messages: true,
            tool_results: true,
        }
    }

    fn apply(&self, body: &mut Value, ctx: &CompressionCtx) -> EngineStats {
        let mut chars_before = 0usize;
        let mut chars_after = 0usize;
        let mut changed_any = false;
        let mut touched = false;
        {
            let mut f = |index: usize, kind: SlotKind, text: &mut String| -> bool {
                let _ = index;
                if kind == SlotKind::System || text.chars().count() < WHITESPACE_MIN_CHARS {
                    return false;
                }
                touched = true;
                let before = text.chars().count();
                let compressed = collapse_whitespace_outside_fences(text);
                if compressed == *text {
                    return false;
                }
                chars_before += before;
                chars_after += compressed.chars().count();
                *text = compressed;
                changed_any = true;
                true
            };
            for_each_text_slot(body, ctx, &mut f);
        }
        if !changed_any {
            return EngineStats::unchanged(
                self.id(),
                Some(if touched { "no-gain" } else { "no-op" }),
            );
        }
        EngineStats {
            engine_id: self.id(),
            changed: true,
            skip_reason: None,
            chars_before,
            chars_after,
        }
    }
}

/// 围栏外折叠连续空白(围栏段原样保留);无围栏整段折叠。
fn collapse_whitespace_outside_fences(text: &str) -> String {
    static RUN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let run = RUN.get_or_init(|| regex::Regex::new(r"\s{2,}").expect("内置正则必然可编译"));
    if !text.contains("```") {
        return run.replace_all(text, " ").into_owned();
    }
    // 按 ``` 分段:偶数段(围栏外)折叠,奇数段(围栏内)原样。
    let mut out = String::with_capacity(text.len());
    for (index, segment) in text.split("```").enumerate() {
        if index > 0 {
            out.push_str("```");
        }
        if index % 2 == 0 {
            out.push_str(&run.replace_all(segment, " "));
        } else {
            out.push_str(segment);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 引擎二:table-columnar
// ---------------------------------------------------------------------------

/// 同构 JSON 数组 → 列式紧凑块。触发条件(全部满足):
/// 槽位文本(去空白后)整体是一个 JSON 数组、≥3 行、每行是非空对象、
/// 各行键集合完全一致、值全为标量(string/number/bool/null)。
/// 输出形态(键以 `{"k":null}` 模板行保留,数字/值逐行保留 —— 保真门
/// 的存在性集合因此不受损):
///
/// ```text
/// [JSON table: 3 cols × 12 rows]
/// {"name":null,"age":null,"city":null}
/// alice|30|NYC
/// ...
/// [12 rows]
/// ```
pub struct TableColumnar;

const TABLE_MIN_ROWS: usize = 3;

impl CompressionEngine for TableColumnar {
    fn id(&self) -> &'static str {
        "table-columnar"
    }

    fn stack_priority(&self) -> u32 {
        20
    }

    fn targets(&self) -> EngineTargets {
        EngineTargets {
            messages: true,
            tool_results: true,
        }
    }

    fn apply(&self, body: &mut Value, ctx: &CompressionCtx) -> EngineStats {
        let mut chars_before = 0usize;
        let mut chars_after = 0usize;
        let mut changed_any = false;
        let mut touched = false;
        let mut parsed_ok = false;
        {
            let mut f = |index: usize, kind: SlotKind, text: &mut String| -> bool {
                let _ = index;
                if kind == SlotKind::System || text.chars().count() < WHITESPACE_MIN_CHARS {
                    return false;
                }
                touched = true;
                let Some((rows, keys)) = parse_homogeneous_table(text) else {
                    return false;
                };
                parsed_ok = true;
                let compressed = render_columnar(&keys, &rows);
                if compressed.len() >= text.len() {
                    return false;
                }
                chars_before += text.chars().count();
                chars_after += compressed.chars().count();
                *text = compressed;
                changed_any = true;
                true
            };
            for_each_text_slot(body, ctx, &mut f);
        }
        if !changed_any {
            // 有槽位但全部非同构 JSON → parse-failed;解析成功但无收益
            // (如行数太少摊不薄表头)→ no-gain;没碰到任何槽位 → no-op。
            let reason = if !touched {
                "no-op"
            } else if parsed_ok {
                "no-gain"
            } else {
                "parse-failed"
            };
            return EngineStats::unchanged(self.id(), Some(reason));
        }
        EngineStats {
            engine_id: self.id(),
            changed: true,
            skip_reason: None,
            chars_before,
            chars_after,
        }
    }
}

type TableRow = Vec<Value>;

/// 同构表解析:失败 → None(skip 原因归 parse-failed / no-gain)。
fn parse_homogeneous_table(text: &str) -> Option<(Vec<TableRow>, Vec<String>)> {
    let parsed: Value = serde_json::from_str(text.trim()).ok()?;
    let items = parsed.as_array()?;
    if items.len() < TABLE_MIN_ROWS {
        return None;
    }
    let mut keys: Vec<String> = Vec::new();
    let mut rows: Vec<TableRow> = Vec::with_capacity(items.len());
    for (row_index, item) in items.iter().enumerate() {
        let map = item.as_object()?;
        if map.is_empty() {
            return None;
        }
        if row_index == 0 {
            keys = map.keys().cloned().collect();
        } else if map.len() != keys.len()
            || !keys.iter().all(|key| map.contains_key(key))
        {
            return None;
        }
        let row: Vec<Value> = keys
            .iter()
            .map(|key| {
                let value = &map[key];
                match value {
                    Value::String(_) | Value::Number(_) | Value::Bool(_) | Value::Null => {
                        value.clone()
                    }
                    _ => Value::Null, // 非标值按 null 归一(同构性兜底)
                }
            })
            .collect();
        rows.push(row);
    }
    Some((rows, keys))
}

/// 列式渲染(值内 `|`/换行/反斜杠转义,保单行结构)。
fn render_columnar(keys: &[String], rows: &[TableRow]) -> String {
    let escape = |value: &Value| -> String {
        match value {
            Value::String(text) => text
                .replace('\\', "\\\\")
                .replace('|', "\\|")
                .replace('\n', "\\n")
                .replace('\r', "\\r"),
            Value::Null => String::new(),
            other => other.to_string(),
        }
    };
    let template: Vec<String> = keys
        .iter()
        .map(|key| format!("{}:null", serde_json::to_string(key).unwrap_or_default()))
        .collect();
    let mut out = format!(
        "[JSON table: {} cols × {} rows]\n{{{}}}\n",
        keys.len(),
        rows.len(),
        template.join(",")
    );
    for row in rows {
        let cells: Vec<String> = row.iter().map(escape).collect();
        out.push_str(&cells.join("|"));
        out.push('\n');
    }
    out.push_str(&format!("[{} rows]", rows.len()));
    out
}

// ---------------------------------------------------------------------------
// 引擎三:tool-output-trim
// ---------------------------------------------------------------------------

/// 超长工具输出裁剪:仅 ToolResult 槽位;超过 [`TRIM_TRIGGER_CHARS`] 时
/// 保头 [`TRIM_KEEP_CHARS`] + 尾 [`TRIM_KEEP_CHARS`] + 省略桩。中间段
/// 含硬性不可丢集合(数字/URL/key/围栏)时由保真门回退(管线负责)。
pub struct ToolOutputTrim;

const TRIM_TRIGGER_CHARS: usize = 6_000;
const TRIM_KEEP_CHARS: usize = 1_600;

impl CompressionEngine for ToolOutputTrim {
    fn id(&self) -> &'static str {
        "tool-output-trim"
    }

    fn stack_priority(&self) -> u32 {
        30
    }

    fn targets(&self) -> EngineTargets {
        EngineTargets {
            messages: false,
            tool_results: true,
        }
    }

    fn apply(&self, body: &mut Value, ctx: &CompressionCtx) -> EngineStats {
        let mut chars_before = 0usize;
        let mut chars_after = 0usize;
        let mut changed_any = false;
        let mut touched = false;
        {
            let mut f = |index: usize, kind: SlotKind, text: &mut String| -> bool {
                let _ = index;
                if kind != SlotKind::ToolResult {
                    return false;
                }
                let len = text.chars().count();
                if len <= TRIM_TRIGGER_CHARS {
                    return false;
                }
                touched = true;
                // 字符边界切分(chars_indices 保证不劈开多字节字符)。
                let head_end = text
                    .char_indices()
                    .nth(TRIM_KEEP_CHARS)
                    .map(|(byte, _)| byte)
                    .unwrap_or(text.len());
                let tail_start = text
                    .char_indices()
                    .nth(len - TRIM_KEEP_CHARS)
                    .map(|(byte, _)| byte)
                    .unwrap_or(text.len());
                if head_end >= tail_start {
                    return false;
                }
                let elided = len - TRIM_KEEP_CHARS * 2;
                let compressed = format!(
                    "{}\n[...省略 {elided} 字符...]\n{}",
                    &text[..head_end],
                    &text[tail_start..]
                );
                if compressed.len() >= text.len() {
                    return false;
                }
                chars_before += len;
                chars_after += compressed.chars().count();
                *text = compressed;
                changed_any = true;
                true
            };
            for_each_text_slot(body, ctx, &mut f);
        }
        if !changed_any {
            return EngineStats::unchanged(
                self.id(),
                Some(if touched { "no-gain" } else { "not-a-tool-result" }),
            );
        }
        EngineStats {
            engine_id: self.id(),
            changed: true,
            skip_reason: None,
            chars_before,
            chars_after,
        }
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compression::fidelity;
    use serde_json::json;

    fn no_protection() -> CompressionCtx {
        CompressionCtx::default()
    }

    #[test]
    fn whitespace_lite_collapses_runs_but_not_fences() {
        let mut body = json!({
            "messages": [
                {"role": "user", "content": "prose   with    runs\n\n\nand  trailing  words  to  exceed  the  minimum  length  bar"},
                {"role": "user", "content": "```\ncode   stays    here\n```  with  enough  prose  around  the  fence  to  pass  the  bar"},
            ],
        });
        let stats = WhitespaceLite.apply(&mut body, &no_protection());
        assert!(stats.changed);
        assert_eq!(
            body["messages"][0]["content"],
            json!("prose with runs and trailing words to exceed the minimum length bar"),
            "围栏外折叠为单空格(换行同样并入)"
        );
        assert_eq!(
            body["messages"][1]["content"],
            json!("```\ncode   stays    here\n``` with enough prose around the fence to pass the bar"),
            "围栏内原样、围栏外照常折叠"
        );
        // 短文本不处理。
        let mut small = json!({"messages": [{"role": "user", "content": "a  b"}]});
        let stats = WhitespaceLite.apply(&mut small, &no_protection());
        assert_eq!(stats.skip_reason, Some("no-op"));
    }

    #[test]
    fn whitespace_lite_protects_system_and_prefix_when_ctx_says_so() {
        let mut body = json!({
            "system": [{"type": "text", "text": "sys   text  that  is  long  enough  to  be  considered  by  the  engine"}],
            "messages": [
                {"role": "user", "content": "pinned   text  that  is  long  enough  to  be  considered  here"},
                {"role": "user", "content": "free    text  with  enough  words  to  comfortably  pass  the  minimum  length  bar  here"},
            ],
        });
        let ctx = CompressionCtx {
            protect_system: true,
            protect_messages_up_to: Some(0),
        };
        WhitespaceLite.apply(&mut body, &ctx);
        assert_eq!(
            body["system"][0]["text"],
            json!("sys   text  that  is  long  enough  to  be  considered  by  the  engine")
        );
        assert_eq!(
            body["messages"][0]["content"],
            json!("pinned   text  that  is  long  enough  to  be  considered  here")
        );
        assert_eq!(
            body["messages"][1]["content"],
            json!("free text with enough words to comfortably pass the minimum length bar here")
        );
    }

    #[test]
    fn whitespace_lite_reports_no_gain_when_already_collapsed() {
        let mut body = json!({"messages": [{"role": "user", "content": "already single spaced text that is long enough to be considered by the engine here"}]});
        let stats = WhitespaceLite.apply(&mut body, &no_protection());
        assert!(!stats.changed);
        assert_eq!(stats.skip_reason, Some("no-gain"));
    }

    #[test]
    fn table_columnar_compacts_homogeneous_arrays() {
        let rows = format!(
            "[{}{}]",
            r#"{"name":"alice","age":30}"#,
            (1..=11)
                .map(|i| format!(r#",{{"name":"user{i}","age":{}}}"#, 20 + i))
                .collect::<String>()
        );
        let mut body = json!({
            "messages": [
                {"role": "user", "content": [
                    {"type": "tool_result", "content": rows},
                ]},
            ],
        });
        let stats = TableColumnar.apply(&mut body, &no_protection());
        assert!(stats.changed);
        let text = body["messages"][0]["content"][0]["content"].as_str().unwrap();
        assert!(text.starts_with("[JSON table: 2 cols × 12 rows]"));
        assert!(text.contains("{\"name\":null,\"age\":null}"));
        assert!(text.contains("alice|30"));
        assert!(text.ends_with("[12 rows]"), "审计标记");
        // 保真门放行(key 表头保留、数字逐行保留)。
        let before = json!({"messages": [{"content": rows}]});
        assert!(fidelity::vitals_preserved(&before, &body));
        // 压缩确实更短(键去重)。
        assert!(stats.chars_after < stats.chars_before);
    }

    #[test]
    fn table_columnar_skips_heterogeneous_or_small() {
        // 行数不足(文本超最短阈值,触发真实解析路径)。
        let mut body = json!({"messages": [{"content":
            r#"[{"a":1,"pad":"xxxxxxxxxxxx"},{"a":2,"pad":"xxxxxxxxxxxx"}]"#}]});
        let stats = TableColumnar.apply(&mut body, &no_protection());
        assert!(!stats.changed);
        // 键集合不一致。
        let mut body = json!({"messages": [{"content":
            r#"[{"a":1,"pad":"xxxxxxxxxxxx"},{"a":2,"b":3,"pad":"xxxxxxxxxxxx"},{"a":4,"pad":"xxxxxxxxxxxx"}]"#}]});
        let stats = TableColumnar.apply(&mut body, &no_protection());
        assert!(!stats.changed);
        // 非 JSON 文本 → parse-failed。
        let mut body = json!({"messages": [{"content": "not json at all, just long text. ".repeat(10)}]});
        let stats = TableColumnar.apply(&mut body, &no_protection());
        assert!(!stats.changed);
        assert_eq!(stats.skip_reason, Some("parse-failed"));
    }

    #[test]
    fn tool_output_trim_keeps_head_and_tail() {
        let head_filler = "x".repeat(3_400);
        let tail_filler = "y".repeat(3_600);
        let mut body = json!({
            "messages": [
                {"role": "user", "content": [
                    {"type": "tool_result", "content": format!("HEAD{head_filler}MIDDLE{tail_filler}TAIL")},
                ]},
            ],
        });
        let stats = ToolOutputTrim.apply(&mut body, &no_protection());
        assert!(stats.changed);
        let text = body["messages"][0]["content"][0]["content"].as_str().unwrap();
        assert!(text.starts_with("HEAD"));
        assert!(text.ends_with("TAIL"));
        // 总长 4+3400+6+3600+4=7014,省略 7014-3200=3814。
        assert!(text.contains("[...省略 3814 字符...]"), "省略桩: {}", &text[..text.len().min(80)]);
        assert!(!text.contains("MIDDLE"), "中间段被裁");
        assert!(stats.chars_after < stats.chars_before);
    }

    #[test]
    fn tool_output_trim_ignores_plain_messages_and_short_outputs() {
        // 普通消息(非 tool_result)不裁。
        let filler = "x".repeat(7_000);
        let mut body = json!({"messages": [{"role": "user", "content": format!("HEAD{filler}TAIL")}]});
        let stats = ToolOutputTrim.apply(&mut body, &no_protection());
        assert!(!stats.changed);
        assert_eq!(stats.skip_reason, Some("not-a-tool-result"));
        // 短工具输出不裁。
        let mut body =
            json!({"messages": [{"content": [{"type": "tool_result", "content": "short"}]}]});
        let stats = ToolOutputTrim.apply(&mut body, &no_protection());
        assert!(!stats.changed);
    }

    #[test]
    fn trim_middle_fence_breaks_fidelity_gate() {
        // 围栏落在被裁的中间段:围栏计数 2→0 → 管线保真门回退。
        let filler = "text ".repeat(2_000);
        let mut body = json!({
            "messages": [{"content": [
                {"type": "tool_result", "content": format!("{filler}```\ncode\n```{filler}")},
            ]}],
        });
        let report = crate::compression::compress_request_body(
            &mut body,
            crate::routing::contracts::RouterCompressionTier::Full,
            0,
        )
        .unwrap();
        let trim = report
            .engines
            .iter()
            .find(|s| s.engine_id == "tool-output-trim")
            .unwrap();
        assert_eq!(trim.skip_reason, Some("fidelity"), "围栏配对被破坏 → 回退");
        assert!(serde_json::to_string(&body).unwrap().contains("```"));
    }

    #[test]
    fn trim_with_fences_in_kept_regions_passes_gate() {
        // 围栏完整落在头/尾保留段:计数不减 → 正常应用。
        let filler = "text ".repeat(2_000);
        let mut body = json!({
            "messages": [{"content": [
                {"type": "tool_result", "content": format!("```\nearly\n```{filler}```\nlate\n```")},
            ]}],
        });
        let report = crate::compression::compress_request_body(
            &mut body,
            crate::routing::contracts::RouterCompressionTier::Full,
            0,
        )
        .unwrap();
        let trim = report
            .engines
            .iter()
            .find(|s| s.engine_id == "tool-output-trim")
            .unwrap();
        assert!(trim.changed, "围栏在保留段内 → 放行");
        let text = body["messages"][0]["content"][0]["content"].as_str().unwrap();
        assert_eq!(text.matches("```").count(), 4, "两对围栏全保留");
    }
}
