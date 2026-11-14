//! finish reason / stop reason 单一映射表(契约 20261012_01 §3.6 冻结)。
//!
//! 全部转换经 [`FinishReason`] 语义枚举中转;未识别原值 → `Opaque(raw)`
//! 且必须伴随 `UnmappedEnum` 诊断(由调用方 push)。修复点:旧实现
//! gemini→anthropic 的 MAX_TOKENS 落 end_turn、openai→anthropic 非 stop
//! 原样透传英文值、anthropic→openai 的 end_turn 原样透传 —— 均收敛到
//! 本表(有意变更,B 阶段实施记录与 golden 基线同步)。

use crate::semantic::diagnostic::{ConversionReport, DiagnosticCode};
use crate::semantic::model::FinishReason;

/// openai 系 `finish_reason` → 语义(stop/length/tool_calls/content_filter)。
pub fn from_openai(raw: Option<&str>, report: &mut ConversionReport, path: &str) -> FinishReason {
    let Some(raw) = raw.filter(|s| !s.is_empty()) else {
        return FinishReason::Stop;
    };
    match raw {
        "stop" => FinishReason::Stop,
        "length" => FinishReason::Length,
        "tool_calls" | "function_call" => FinishReason::ToolCall,
        "content_filter" => FinishReason::ContentFilter,
        other => {
            report.push(
                DiagnosticCode::UnmappedEnum,
                path,
                format!("未知 openai finish_reason {other:?} → Opaque"),
            );
            FinishReason::Opaque(other.to_owned())
        }
    }
}

/// Anthropic `stop_reason` → 语义。
pub fn from_anthropic(
    raw: Option<&str>,
    report: &mut ConversionReport,
    path: &str,
) -> FinishReason {
    let Some(raw) = raw.filter(|s| !s.is_empty()) else {
        return FinishReason::Stop;
    };
    match raw {
        "end_turn" | "stop_sequence" => FinishReason::Stop,
        "max_tokens" => FinishReason::Length,
        "tool_use" => FinishReason::ToolCall,
        "refusal" => FinishReason::Refusal,
        other => {
            report.push(
                DiagnosticCode::UnmappedEnum,
                path,
                format!("未知 anthropic stop_reason {other:?} → Opaque"),
            );
            FinishReason::Opaque(other.to_owned())
        }
    }
}

/// Gemini `finishReason` → 语义。旧实现的未知值小写透传行为由
/// `to_openai_chat` 对 Opaque 的小写化输出保持线级一致。
pub fn from_gemini(raw: Option<&str>, report: &mut ConversionReport, path: &str) -> FinishReason {
    let Some(raw) = raw.filter(|s| !s.is_empty()) else {
        return FinishReason::Stop;
    };
    match raw {
        "STOP" => FinishReason::Stop,
        "MAX_TOKENS" => FinishReason::Length,
        "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII"
        | "DANGEROUS_CONTENT" => FinishReason::ContentFilter,
        other => {
            report.push(
                DiagnosticCode::UnmappedEnum,
                path,
                format!("未知 gemini finishReason {other:?} → Opaque"),
            );
            FinishReason::Opaque(other.to_owned())
        }
    }
}

/// 语义 → openai chat `finish_reason`。
pub fn to_openai_chat(reason: &FinishReason) -> String {
    match reason {
        FinishReason::Stop => "stop".to_owned(),
        FinishReason::Length => "length".to_owned(),
        FinishReason::ToolCall => "tool_calls".to_owned(),
        FinishReason::ContentFilter => "content_filter".to_owned(),
        FinishReason::Refusal => "refusal".to_owned(),
        FinishReason::IncompleteOther => "stop".to_owned(),
        FinishReason::Opaque(raw) => raw.to_ascii_lowercase(),
    }
}

/// 语义 → Anthropic `stop_reason`。
pub fn to_anthropic(reason: &FinishReason) -> String {
    match reason {
        FinishReason::Stop | FinishReason::IncompleteOther => "end_turn".to_owned(),
        FinishReason::Length => "max_tokens".to_owned(),
        FinishReason::ToolCall => "tool_use".to_owned(),
        FinishReason::Refusal => "refusal".to_owned(),
        FinishReason::ContentFilter => "refusal".to_owned(),
        FinishReason::Opaque(raw) => raw.clone(),
    }
}

/// 语义 → Gemini `finishReason`。
pub fn to_gemini(reason: &FinishReason) -> String {
    match reason {
        FinishReason::Stop | FinishReason::ToolCall | FinishReason::IncompleteOther => {
            "STOP".to_owned()
        }
        FinishReason::Length => "MAX_TOKENS".to_owned(),
        FinishReason::ContentFilter | FinishReason::Refusal => "SAFETY".to_owned(),
        FinishReason::Opaque(raw) => raw.clone(),
    }
}

/// 旧 `map_finish_reason_to_responses` 的语义版:openai 系原值 →
/// Responses incomplete/reason 字符串(tool 存在性由调用方单独判定)。
pub fn to_responses(reason: &FinishReason) -> String {
    match reason {
        FinishReason::Stop | FinishReason::IncompleteOther | FinishReason::Refusal => {
            "stop".to_owned()
        }
        FinishReason::Length => "max_output_tokens".to_owned(),
        FinishReason::ContentFilter => "content_filter".to_owned(),
        FinishReason::ToolCall => "tool_use".to_owned(),
        FinishReason::Opaque(raw) => {
            let lowered = raw.to_ascii_lowercase();
            if lowered.contains("max") || lowered.contains("length") {
                "max_output_tokens".to_owned()
            } else if lowered.contains("safety") || lowered.contains("filter") || lowered.contains("recitation")
            {
                "content_filter".to_owned()
            } else if lowered.contains("tool") {
                "tool_use".to_owned()
            } else {
                "stop".to_owned()
            }
        }
    }
}
