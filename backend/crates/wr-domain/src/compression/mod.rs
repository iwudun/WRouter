//! token 压缩管线(20260905_03 §4.3 S4/S5;Rust 新增,无 Python 镜像)。
//!
//! 架构:
//! - [`CompressionEngine`] trait:`{id, targets, stack_priority, apply}`;
//!   管线按 `stack_priority` 升序叠加,每引擎产出 [`EngineStats`]
//!   (含 skip_reason,全链路透传到调用日志);
//! - **缓存感知门(前置,必做)**:请求带 `cache_control` 或目标供应商
//!   具备前缀缓存能力时,system 段与「最后断点(含)之前的 messages」
//!   强制不压 —— 否则上游前缀缓存全 miss,压缩省的被缓存损失吐回去
//!   (方案 §2「压缩×缓存协同」第一前置);
//! - **保真门**([`fidelity`]):每引擎 apply 后校验硬性不可丢集合
//!   (数字/URL/文本内 JSON key/代码围栏),失败回退该引擎改动并记
//!   `skip: fidelity`;校验器自身出错 fail-open(放行压缩,不阻塞)。
//!
//! 首期 3 引擎(全部无损,量大管饱):`whitespace-lite`、
//! `table-columnar`、`tool-output-trim`(见 [`engines`])。
//!
//! 明确不做(方案 §4.3 边界):LLM 摘要压缩、ONNX 语义剪枝、上下文
//! 图像化、可逆 CCR 取回工具。

pub mod engines;
pub mod fidelity;

use serde_json::{Map, Value};

use crate::routing::contracts::RouterCompressionTier;

/// 引擎作用目标(trait 契约;管线据此过滤引擎可触及的槽位)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineTargets {
    pub messages: bool,
    pub tool_results: bool,
}

/// 压缩上下文(缓存感知门的产物,引擎与管线共用)。
#[derive(Debug, Clone, Copy, Default)]
pub struct CompressionCtx {
    /// system 段强制不压。当前全部内建协议(openai/anthropic/gemini 系)
    /// 都具备前缀缓存能力(显式 cache_control 或隐式前缀缓存),system
    /// 压缩的缓存损失恒大于收益 —— 管线恒传 true(见模块注释)。
    pub protect_system: bool,
    /// 断点前缀保护:`messages[..=index]` 不压(最后断点所在条含);
    /// 无断点 → None(无保护)。
    pub protect_messages_up_to: Option<usize>,
}

/// 单引擎执行统计(skip_reason 全链路透传到调用日志)。
#[derive(Debug, Clone, PartialEq)]
pub struct EngineStats {
    pub engine_id: &'static str,
    pub changed: bool,
    /// None = 正常应用;Some = 跳过原因("fidelity"/"no-gain"/
    /// "parse-failed"/"below-min-length"/"not-a-tool-result"…)。
    pub skip_reason: Option<&'static str>,
    /// 引擎触及槽位的字符量(槽位级,与报告级 chars_before/after 区分)。
    pub chars_before: usize,
    pub chars_after: usize,
}

impl EngineStats {
    fn unchanged(engine_id: &'static str, skip_reason: Option<&'static str>) -> Self {
        Self {
            engine_id,
            changed: false,
            skip_reason,
            chars_before: 0,
            chars_after: 0,
        }
    }
}

/// 压缩引擎 trait(方案 §4.3)。
pub trait CompressionEngine: Send + Sync {
    fn id(&self) -> &'static str;
    /// 小者先跑(管线按此升序)。
    fn stack_priority(&self) -> u32;
    fn targets(&self) -> EngineTargets;
    /// 原地修改 body 并返回统计;引擎自行决定对单个槽位是否生效,
    /// 永不 panic(畸形结构静默跳过);未改动时必须给出 skip_reason。
    fn apply(&self, body: &mut Value, ctx: &CompressionCtx) -> EngineStats;
}

/// 管线报告(压缩启用时的调用日志明细来源)。
#[derive(Debug, Clone, PartialEq)]
pub struct CompressionReport {
    /// 整条管线级跳过原因(目前仅 auto 档低于阈值:"below-threshold");
    /// 引擎级跳过在 `engines[].skipReason`。
    pub skipped_reason: Option<&'static str>,
    pub engines: Vec<EngineStats>,
    pub chars_before: usize,
    pub chars_after: usize,
}

impl CompressionReport {
    /// 估算节省 token(字符差 / 4,与触发估算同口径;真实 usage 回填
    /// 校准由节省度量侧负责,方案 §4.6)。
    pub fn tokens_saved_estimate(&self) -> i64 {
        ((self.chars_before.saturating_sub(self.chars_after)) / 4) as i64
    }

    /// 调用日志 extra 形态(camelCase;`engines` 含 skip 明细)。
    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        map.insert(
            "skippedReason".to_owned(),
            match self.skipped_reason {
                Some(reason) => Value::String(reason.to_owned()),
                None => Value::Null,
            },
        );
        map.insert(
            "engines".to_owned(),
            Value::Array(
                self.engines
                    .iter()
                    .map(|engine| {
                        let mut item = Map::new();
                        item.insert("id".to_owned(), Value::String(engine.engine_id.to_owned()));
                        item.insert("changed".to_owned(), Value::Bool(engine.changed));
                        item.insert(
                            "skipReason".to_owned(),
                            match engine.skip_reason {
                                Some(reason) => Value::String(reason.to_owned()),
                                None => Value::Null,
                            },
                        );
                        item.insert("charsBefore".to_owned(), json_number(engine.chars_before));
                        item.insert("charsAfter".to_owned(), json_number(engine.chars_after));
                        Value::Object(item)
                    })
                    .collect(),
            ),
        );
        map.insert("charsBefore".to_owned(), json_number(self.chars_before));
        map.insert("charsAfter".to_owned(), json_number(self.chars_after));
        map.insert(
            "tokensSavedEstimate".to_owned(),
            json_number(self.tokens_saved_estimate().max(0) as usize),
        );
        Value::Object(map)
    }
}

fn json_number(value: usize) -> Value {
    Value::Number(serde_json::Number::from(value))
}

/// 请求体 token 估算(JSON 文本长度 / 4;CJK 偏差大但只影响触发时机,
/// 统计口径由真实 usage 回填 —— 方案 §8)。
pub fn estimate_tokens(body: &Value) -> i64 {
    (serde_json::to_string(body).map(|text| text.len()).unwrap_or(0) / 4) as i64
}

/// 管线入口:tier 由调用方按「全局 → 规则覆盖」解析后传入
/// (auto+阈值 / full;off 时调用方不应调用,但此处也守一层)。
/// 返回 None = 未进管线(off);Some = 进了管线(应用或跳过,均带报告)。
pub fn compress_request_body(
    body: &mut Value,
    tier: RouterCompressionTier,
    threshold_tokens: i64,
) -> Option<CompressionReport> {
    if tier == RouterCompressionTier::Off || !body.is_object() {
        return None;
    }
    let chars_before = serde_json::to_string(body).map(|text| text.len()).unwrap_or(0);
    // auto 档:估算 token 低于阈值 → 不压(方案 §4.3 触发语义)。
    if tier == RouterCompressionTier::Auto && estimate_tokens(body) <= threshold_tokens {
        return Some(CompressionReport {
            skipped_reason: Some("below-threshold"),
            engines: Vec::new(),
            chars_before,
            chars_after: chars_before,
        });
    }
    // 缓存感知门:system 恒保护(全部内建协议均有前缀缓存能力);有
    // cache_control 断点时,断点前缀(含断点条)不压。
    let ctx = CompressionCtx {
        protect_system: true,
        protect_messages_up_to: crate::cache_prefix::last_breakpoint_message_index(body),
    };
    let mut engines: Vec<Box<dyn CompressionEngine>> = vec![
        Box::new(engines::WhitespaceLite),
        Box::new(engines::TableColumnar),
        Box::new(engines::ToolOutputTrim),
    ];
    engines.sort_by_key(|engine| engine.stack_priority());
    let mut stats = Vec::with_capacity(engines.len());
    for engine in &engines {
        // 保真门以「引擎前快照」为基准:apply 后校验失败 → 整体回退该
        // 引擎改动(克隆成本 ≈ 单请求毫秒级,方案 §8 性能结论内)。
        let snapshot = body.clone();
        let engine_stats = engine.apply(body, &ctx);
        if engine_stats.changed && !fidelity::vitals_preserved(&snapshot, body) {
            *body = snapshot;
            stats.push(EngineStats {
                skip_reason: Some("fidelity"),
                changed: false,
                ..engine_stats
            });
            continue;
        }
        stats.push(engine_stats);
    }
    let chars_after = serde_json::to_string(body).map(|text| text.len()).unwrap_or(0);
    Some(CompressionReport {
        skipped_reason: None,
        engines: stats,
        chars_before,
        chars_after,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn off_tier_and_non_object_return_none() {
        let mut body = json!({
            "messages": [{"role": "user", "content": "a  b  c  d"}],
        });
        assert!(compress_request_body(&mut body, RouterCompressionTier::Off, 0).is_none());
        // 非 dict body 守卫。
        let mut scalar = json!(42);
        assert!(compress_request_body(&mut scalar, RouterCompressionTier::Full, 0).is_none());
    }

    #[test]
    fn auto_tier_skips_below_threshold() {
        let mut body = json!({"messages": [{"role": "user", "content": "hi"}]});
        let report = compress_request_body(&mut body, RouterCompressionTier::Auto, 32_000)
            .expect("auto 档应返回报告");
        assert_eq!(report.skipped_reason, Some("below-threshold"));
        assert!(report.engines.is_empty());
        assert_eq!(report.chars_before, report.chars_after);
        assert_eq!(report.tokens_saved_estimate(), 0);
    }

    #[test]
    fn full_tier_runs_engines_in_priority_order() {
        // 双空格文本:whitespace-lite 可压(其余引擎各司其职后跳过)。
        let mut body = json!({
            "messages": [{"role": "user", "content": "hello  world.  ".repeat(3000)}],
        });
        let report =
            compress_request_body(&mut body, RouterCompressionTier::Full, 32_000).unwrap();
        assert_eq!(report.skipped_reason, None);
        // 引擎按 stack_priority 升序:whitespace-lite(10) → table-columnar(20)
        // → tool-output-trim(30)。
        let ids: Vec<&str> = report.engines.iter().map(|s| s.engine_id).collect();
        assert_eq!(
            ids,
            vec!["whitespace-lite", "table-columnar", "tool-output-trim"]
        );
        // 长空白文本被 whitespace-lite 压缩。
        assert!(report.chars_after < report.chars_before);
        assert!(report.tokens_saved_estimate() > 0);
        assert_eq!(
            body["messages"][0]["content"],
            json!("hello world. ".repeat(3000)),
            "连续空白折叠为单空格"
        );
        // 序列化形态可读(camelCase + 引擎明细)。
        let value = report.to_value();
        assert!(value["tokensSavedEstimate"].is_number());
        assert_eq!(value["engines"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn cache_aware_gate_protects_system_and_breakpoint_prefix() {
        // system + 断点前缀各带长空白文本:保护段不被压,断点之后的照压。
        let mut body = json!({
            "system": [{"type": "text", "text": "sys  text  here  a  b  c"}],
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "prefix  with  spaces", "cache_control": {"type": "ephemeral"}},
                ]},
                {"role": "user", "content": "tail  with  spaces  that  should  be  collapsed  because  it  passes  the  length  bar"},
            ],
        });
        compress_request_body(&mut body, RouterCompressionTier::Full, 0).unwrap();
        assert_eq!(
            body["system"][0]["text"],
            json!("sys  text  here  a  b  c"),
            "system 恒不压(缓存感知门)"
        );
        assert_eq!(
            body["messages"][0]["content"][0]["text"],
            json!("prefix  with  spaces"),
            "断点(含)之前的消息不压"
        );
        assert_eq!(
            body["messages"][1]["content"],
            json!("tail with spaces that should be collapsed because it passes the length bar"),
            "断点之后的消息正常压缩"
        );
    }

    #[test]
    fn fidelity_gate_reverts_engine_changes() {
        // 唯一数字落在 tool-output-trim 的裁剪中间段:保真门必须回退。
        let filler = "filler word ".repeat(1000);
        let mut body = json!({
            "messages": [
                {"role": "user", "content": [
                    {"type": "tool_result", "content": format!("head {filler}magic 12345 {filler}tail")},
                ]},
            ],
        });
        let report = compress_request_body(&mut body, RouterCompressionTier::Full, 0).unwrap();
        let trim = report
            .engines
            .iter()
            .find(|s| s.engine_id == "tool-output-trim")
            .unwrap();
        assert_eq!(trim.skip_reason, Some("fidelity"), "中间段数字丢失 → 回退");
        assert!(!trim.changed);
        assert!(
            serde_json::to_string(&body).unwrap().contains("magic 12345"),
            "回退后原文保留"
        );
    }
}
