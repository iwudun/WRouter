//! 转换策略与诊断(契约 20261012_01 §2.2 / §5 质量门 #2)。
//!
//! DiagnosticCode 首版枚举**冻结**(A5 质量门):新增码需同步更新
//! 内部设计记录《统一语义模型 阶段A 基线与契约》§2.3（不随开源仓库发布）。

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

/// 转换策略四记号(矩阵 §1 说明)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversionStrategy {
    /// 原生透传保真(不经过语义模型)。
    NativePassthrough,
    /// 语义转换。
    Semantic,
    /// 有损转换(必须带诊断)。
    Lossy,
    /// 明确拒绝。
    Unsupported,
}

/// 诊断码首版(冻结)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticCode {
    /// 字段被丢弃(store/previous_response_id 等无目标等价物)。
    DroppedField,
    /// 内容降级(非法 tool arguments → `{"raw": s}`、多模态转文本失败等)。
    DowngradedContent,
    /// 标识符被重新生成(Gemini functionCall 无 id → call_N)。
    RegeneratedId,
    /// 枚举值无法映射(未知 finish reason / tool choice 形态)。
    UnmappedEnum,
    /// 未识别字段/事件/块以 opaque 形态保留。
    OpaquePreserved,
    /// 多段内容被合并(多条 system 归并等)。
    MergedContent,
    /// 数值/参数按目标协议约束调整(如 budget_tokens 按 max_tokens 收敛)。
    AdjustedValue,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversionDiagnostic {
    pub code: DiagnosticCode,
    /// 源路径,如 `messages[2].content[0]`。
    pub path: String,
    pub detail: String,
    /// 被丢弃/降级内容的摘要(截断、脱敏后)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dropped: Option<Value>,
}

/// 一次转换的报告:策略 + 诊断清单 + 耗时。诊断用于 trace/debug 日志
/// 与测试断言,不向正常响应泄露内部信息(方案 §3.5);耗时/策略/诊断
/// 类别同时进入内存 metrics(E1:指标只含类别与耗时,不含任何
/// payload/secret)。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConversionReport {
    pub strategy: Option<ConversionStrategy>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<ConversionDiagnostic>,
    /// 请求面转换耗时(微秒;调用方测量后回填,日志与 metrics 共用)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_us: Option<u64>,
}

impl ConversionReport {
    pub fn semantic() -> Self {
        Self {
            strategy: Some(ConversionStrategy::Semantic),
            diagnostics: Vec::new(),
            elapsed_us: None,
        }
    }

    pub fn passthrough() -> Self {
        Self {
            strategy: Some(ConversionStrategy::NativePassthrough),
            diagnostics: Vec::new(),
            elapsed_us: None,
        }
    }

    pub fn unsupported() -> Self {
        Self {
            strategy: Some(ConversionStrategy::Unsupported),
            diagnostics: Vec::new(),
            elapsed_us: None,
        }
    }

    /// 有损转换:必须有至少一条诊断说明损失内容。
    pub fn lossy() -> Self {
        Self {
            strategy: Some(ConversionStrategy::Lossy),
            diagnostics: Vec::new(),
            elapsed_us: None,
        }
    }

    /// 含实际损失诊断时把策略提升为 Lossy(语义转换/未知策略上调用)。
    /// `OpaquePreserved` 是保留而非损失,不触发。
    pub fn mark_lossy_if_needed(&mut self) {
        let lossy = self.diagnostics.iter().any(|diagnostic| {
            matches!(
                diagnostic.code,
                DiagnosticCode::DroppedField
                    | DiagnosticCode::DowngradedContent
                    | DiagnosticCode::RegeneratedId
                    | DiagnosticCode::UnmappedEnum
                    | DiagnosticCode::MergedContent
            )
        });
        if lossy && matches!(self.strategy, None | Some(ConversionStrategy::Semantic)) {
            self.strategy = Some(ConversionStrategy::Lossy);
        }
    }

    pub fn push(
        &mut self,
        code: DiagnosticCode,
        path: impl Into<String>,
        detail: impl Into<String>,
    ) {
        self.diagnostics.push(ConversionDiagnostic {
            code,
            path: path.into(),
            detail: detail.into(),
            dropped: None,
        });
    }

    pub fn push_dropped(
        &mut self,
        code: DiagnosticCode,
        path: impl Into<String>,
        detail: impl Into<String>,
        dropped: Value,
    ) {
        self.diagnostics.push(ConversionDiagnostic {
            code,
            path: path.into(),
            detail: detail.into(),
            dropped: Some(truncate(dropped)),
        });
    }

    pub fn is_empty(&self) -> bool {
        self.diagnostics.is_empty()
    }

    pub fn has_code(&self, code: DiagnosticCode) -> bool {
        self.diagnostics.iter().any(|d| d.code == code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lossy_strategy_is_promoted_only_for_real_loss_codes() {
        let mut report = ConversionReport::semantic();
        report.push(DiagnosticCode::OpaquePreserved, "x", "kept");
        report.mark_lossy_if_needed();
        assert_eq!(report.strategy, Some(ConversionStrategy::Semantic));

        report.push(DiagnosticCode::DroppedField, "y", "dropped");
        report.mark_lossy_if_needed();
        assert_eq!(report.strategy, Some(ConversionStrategy::Lossy));
        assert_eq!(
            ConversionReport::lossy().strategy,
            Some(ConversionStrategy::Lossy)
        );
    }
}

/// dropped 摘要截断:超大值压到前 512 字节的 JSON 文本,避免诊断本身
/// 携带巨型负载(方案 §3.5 / 阶段 D 采集层再做完整脱敏)。
fn truncate(value: Value) -> Value {
    let text = value.to_string();
    if text.len() <= 512 {
        return value;
    }
    let mut cut = 512;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    Value::String(format!("{}…(truncated)", &text[..cut]))
}
