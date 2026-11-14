//! Inbound/Outbound 适配器契约、注册表与能力门禁(B2/B6;方案 §3.2)。
//!
//! 适配器只负责协议语义编解码;endpoint、认证 headers、vendor fixup
//! 与 HTTP 执行在传输层(`protocol::endpoint` / `protocol::upstream_headers`
//! / vendor hook)保持协议语义之外(20261012_01 §2.1 决策 #2)。
//!
//! 非流请求/响应适配器在本模块;流事件由 `OutboundStreamAdapter` +
//! `semantic::stream` 的解码器承担。

use serde_json::Value;

use crate::provider::ProviderProtocol;
use crate::semantic::diagnostic::ConversionReport;
use crate::semantic::error::SemanticError;
use crate::semantic::model::{RequestKind, SemanticRequest, SemanticResponse, SourceProtocol};

/// 出站编码上下文:仅承载影响编码形态的传输侧事实。
/// `native_responses`:openai 上游以 `style=openai_responses` 原生提供
/// Responses 端点(`serves_openai_responses_natively`)。
/// `kind`:请求种类(gemini 上游的 interactions 形态分流)。
#[derive(Debug, Clone, Copy, Default)]
pub struct OutboundContext {
    pub native_responses: bool,
    pub kind: RequestKind,
}

/// 适配器产物:值 + 转换报告(策略与诊断)。
#[derive(Debug, Clone)]
pub struct Decoded<T> {
    pub value: T,
    pub report: ConversionReport,
}

impl<T> Decoded<T> {
    pub fn new(value: T, report: ConversionReport) -> Self {
        Self { value, report }
    }
}

/// Inbound/Outbound 流 adapter 能力常量。
pub const STREAM_CHAT: AdapterCapabilities = AdapterCapabilities {
    request_kinds: &[RequestKind::Chat],
    stream_modes: &[StreamMode::Stream],
    supports_prefill_continuation: false,
};

/// 阶段五 C-3:Anthropic 上游的流式能力集 —— **支持 assistant prefill
/// 续写**(Anthropic 的 assistant 消息作为最后一条,模型就会接着写)。
///
/// 其它协议族没有官方续写语义(OpenAI Chat Completions / Gemini 都不
/// 提供):对它们发「续写请求」等于假续写 —— 模型会把 prefill 当成新
/// 的用户消息重新回答,回复会重复。
pub const ANTHROPIC_STREAM: AdapterCapabilities = AdapterCapabilities {
    request_kinds: &[RequestKind::Chat],
    stream_modes: &[StreamMode::Stream],
    supports_prefill_continuation: true,
};

/// 支持的请求种类 × 流模式(能力查询,方案 B2)。
#[derive(Debug, Clone, Copy)]
pub struct AdapterCapabilities {
    pub request_kinds: &'static [RequestKind],
    /// 支持的流模式(NonStream 为请求/响应适配器;Stream 表示流式适配器已接线)。
    pub stream_modes: &'static [StreamMode],
    /// 阶段五 C-3:是否支持「assistant prefill 续写」(上游 wire 语义)。
    pub supports_prefill_continuation: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamMode {
    NonStream,
    Stream,
}

impl AdapterCapabilities {
    pub fn supports(&self, kind: RequestKind, mode: StreamMode) -> bool {
        self.request_kinds.contains(&kind) && self.stream_modes.contains(&mode)
    }
}

pub(crate) const CHAT_NONSTREAM: AdapterCapabilities = AdapterCapabilities {
    request_kinds: &[RequestKind::Chat],
    stream_modes: &[StreamMode::NonStream],
    supports_prefill_continuation: false,
};

/// Upstream SSE payload adapter.
pub trait OutboundStreamAdapter: Sync {
    fn protocol(&self) -> ProviderProtocol;
    fn capabilities(&self) -> AdapterCapabilities;
}

pub trait InboundAdapter: Sync {
    fn protocol(&self) -> SourceProtocol;
    fn capabilities(&self) -> AdapterCapabilities;

    /// 客户端原始请求 → 语义请求。
    fn decode_request(&self, raw: &Value) -> Result<Decoded<SemanticRequest>, SemanticError>;

    /// 语义响应 → 客户端协议形态。
    fn encode_response(
        &self,
        response: &SemanticResponse,
    ) -> Result<Decoded<Value>, SemanticError>;

    /// 语义错误 → 客户端错误信封 `(http_status, body)`
    /// (B7;线级形态与旧 `format_error_for_protocol` 一致)。
    fn encode_error(
        &self,
        error: &SemanticError,
        attempts: Option<&[String]>,
    ) -> (u16, Value);
}

/// 上流出站适配器:语义模型 → 上游协议形态。
pub trait OutboundAdapter: Sync {
    fn protocol(&self) -> ProviderProtocol;
    fn capabilities(&self) -> AdapterCapabilities;

    /// 语义请求 → 上游请求体(每次候选重新编码;vendor fixup 在此
    /// 之后由传输层 hook 施加,幂等)。
    fn encode_request(
        &self,
        request: &SemanticRequest,
        ctx: &OutboundContext,
    ) -> Result<Decoded<Value>, SemanticError>;

    /// 上游非流响应 JSON → 语义响应。
    fn decode_response(
        &self,
        raw: &Value,
        ctx: &OutboundContext,
    ) -> Result<Decoded<SemanticResponse>, SemanticError>;
}

// ---------------------------------------------------------------------------
// 注册表
// ---------------------------------------------------------------------------

/// 按客户端协议取 Inbound 适配器。embeddings 无语义适配
/// (透传或门禁拒绝,矩阵 §1.2/§1.3);gemini_stream 与
/// gemini_interactions 均由对应 Inbound 适配器承载。
pub fn inbound(protocol: SourceProtocol) -> Option<&'static dyn InboundAdapter> {
    match protocol {
        SourceProtocol::OpenAi => Some(&super::openai_chat::Inbound),
        SourceProtocol::OpenAiResponses => Some(&super::openai_responses::Inbound),
        SourceProtocol::Anthropic => Some(&super::anthropic::Inbound),
        SourceProtocol::Gemini => Some(&super::gemini::Inbound),
        SourceProtocol::GeminiStream => Some(&super::gemini::Inbound),
        SourceProtocol::GeminiInteractions => Some(&super::gemini::InteractionsInbound),
        SourceProtocol::OpenAiEmbeddings => None,
    }
}

/// 按上游协议取 Outbound 适配器(三种上游恒有实现)。
pub fn outbound(protocol: ProviderProtocol) -> &'static dyn OutboundAdapter {
    match protocol {
        ProviderProtocol::OpenAi => &super::openai_chat::Outbound,
        ProviderProtocol::Anthropic => &super::anthropic::Outbound,
        ProviderProtocol::Gemini => &super::gemini::Outbound,
    }
}

// ---------------------------------------------------------------------------
// 矩阵门禁(§1.6 缺口 #2/#3/#4 的显式化)
// ---------------------------------------------------------------------------

/// 客户端协议 × 上游协议组合的显式不支持(升格 wrouter_error 信封)。
///
/// 覆盖矩阵目标策略(20261012_01 §1.6):
/// - embeddings × anthropic/gemini → unsupported(#3/#4;旧行为为
///   静默透传/空 contents 畸变);
/// - gemini_interactions × 非 gemini 上游 → unsupported(#2;旧行为为
///   clone 透传 + `/interactions` URL);
/// - gemini_stream 面由流式管线约束,此处仅约束非流请求面。
pub fn ensure_combination_supported(
    source: SourceProtocol,
    target: ProviderProtocol,
) -> Result<(), SemanticError> {
    if source == SourceProtocol::OpenAiEmbeddings
        && matches!(target, ProviderProtocol::Anthropic | ProviderProtocol::Gemini)
    {
        return Err(SemanticError::unsupported(format!(
            "WRouter 不支持 openai_embeddings 入口转发到 {} 上游(协议转换矩阵 unsupported 格)",
            target.as_str()
        )));
    }
    if source == SourceProtocol::GeminiInteractions && target != ProviderProtocol::Gemini {
        return Err(SemanticError::unsupported(format!(
            "WRouter 不支持 gemini_interactions 入口转发到 {} 上游(协议转换矩阵 unsupported 格)",
            target.as_str()
        )));
    }
    Ok(())
}
