//! 转换指标接线(自原转发共享模块纯搬移):只记类别与耗时,不含正文/凭据。

use super::*;

/// E1:请求面转换的策略/耗时/诊断类别进 metrics;耗时回填转换报告
/// (随 `retry_attempts[].conversion` 落库)。指标只含类别与耗时,
/// 不含任何请求 payload/secret。
pub(super) fn record_conversion_metrics(
    deps: &GatewayDeps,
    report: &Option<wr_domain::semantic::ConversionReport>,
    protocol: &str,
    provider: &Provider,
    elapsed_us: u64,
) {
    let Some(report) = report else {
        return;
    };
    let strategy = match report.strategy {
        Some(wr_domain::semantic::ConversionStrategy::NativePassthrough) => "native_passthrough",
        Some(wr_domain::semantic::ConversionStrategy::Semantic) => "semantic",
        Some(wr_domain::semantic::ConversionStrategy::Lossy) => "lossy",
        Some(wr_domain::semantic::ConversionStrategy::Unsupported) => "unsupported",
        None => "unknown",
    };
    deps.metrics
        .record_conversion(strategy, protocol, provider.protocol.as_str(), elapsed_us);
    for diagnostic in &report.diagnostics {
        let code = match diagnostic.code {
            wr_domain::semantic::DiagnosticCode::DroppedField => "dropped_field",
            wr_domain::semantic::DiagnosticCode::DowngradedContent => "downgraded_content",
            wr_domain::semantic::DiagnosticCode::RegeneratedId => "regenerated_id",
            wr_domain::semantic::DiagnosticCode::UnmappedEnum => "unmapped_enum",
            wr_domain::semantic::DiagnosticCode::OpaquePreserved => "opaque_preserved",
            wr_domain::semantic::DiagnosticCode::MergedContent => "merged_content",
            wr_domain::semantic::DiagnosticCode::AdjustedValue => "adjusted_value",
        };
        deps.metrics.record_conversion_diagnostic(code);
    }
}

/// P5-3:响应面转换的策略/诊断进指标(仅类别,无正文/凭据)。
pub(super) fn record_response_conversion_metrics(
    deps: &GatewayDeps,
    report: &Option<wr_domain::semantic::ConversionReport>,
    protocol: &str,
    provider: &Provider,
) {
    let Some(report) = report else {
        return;
    };
    let strategy = match report.strategy {
        Some(wr_domain::semantic::ConversionStrategy::NativePassthrough) => "native_passthrough",
        Some(wr_domain::semantic::ConversionStrategy::Semantic) => "semantic",
        Some(wr_domain::semantic::ConversionStrategy::Lossy) => "lossy",
        Some(wr_domain::semantic::ConversionStrategy::Unsupported) => "unsupported",
        None => "unknown",
    };
    deps.metrics
        .record_response_conversion(strategy, protocol, provider.protocol.as_str());
    for diagnostic in &report.diagnostics {
        let code = match diagnostic.code {
            wr_domain::semantic::DiagnosticCode::DroppedField => "dropped_field",
            wr_domain::semantic::DiagnosticCode::DowngradedContent => "downgraded_content",
            wr_domain::semantic::DiagnosticCode::RegeneratedId => "regenerated_id",
            wr_domain::semantic::DiagnosticCode::UnmappedEnum => "unmapped_enum",
            wr_domain::semantic::DiagnosticCode::OpaquePreserved => "opaque_preserved",
            wr_domain::semantic::DiagnosticCode::MergedContent => "merged_content",
            wr_domain::semantic::DiagnosticCode::AdjustedValue => "adjusted_value",
        };
        deps.metrics.record_conversion_diagnostic(code);
    }
}
