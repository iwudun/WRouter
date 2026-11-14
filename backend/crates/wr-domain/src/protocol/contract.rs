//! 协议执行契约(方案 20261106_01 §2、§5;P1-2)。
//!
//! 本模块把「正文处理模式」「表示转换」「保真等级」「能力/损失」变成
//! 可执行类型,并给出唯一规划入口 [`plan_exchange`]。
//!
//! P1 只固定决策,不接入生产链路:
//! - 同协议原生格规划为 `Raw` + `ByteExact`;
//! - 跨协议规划为 `Translate` + `SemanticEquivalent`;
//! - `GeminiInteractions -> Gemini` 请求面是已注册的专项 `Patch`,
//!   响应面仍按 `Translate` 规划(P4 再独立核实其完整契约);
//! - 既有的禁止组合(embeddings × anthropic/gemini、interactions ×
//!   非 gemini)在这里仍然是硬拒绝,不能被规划器降级为透传。
//!
//! 能力矩阵是**目标契约**:`Unsupported` 明确列出当前尚未闭环的能力
//! (对应 G-01…G-06 等),P3/P4 修复后按实际证据升级,不能因为当前
//! 代码能编译就标 `Equivalent`。

use std::fmt;

use crate::provider::ProviderProtocol;
use crate::semantic::SourceProtocol;
use crate::semantic::model::RequestKind;
use crate::wire::record::Representation;

/// 三种保真等级(方案 §2.1)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FidelityLevel {
    /// 未修改实体正文原字节保留。
    ByteExact,
    /// JSON 值/顺序/存在性/身份引用一致;允许声明过的重序列化。
    StructureExact,
    /// 异协议表达不同但声明支持的行为等价。
    SemanticEquivalent,
}

impl FidelityLevel {
    /// 规划组合时取较弱一侧(保真承诺不叠加)。枚举顺序按强度递增,
    /// 因此较弱者就是较大值。
    pub fn minimum(self, other: Self) -> Self {
        self.max(other)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ByteExact => "byte_exact",
            Self::StructureExact => "structure_exact",
            Self::SemanticEquivalent => "semantic_equivalent",
        }
    }
}

/// 正文处理模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyMode {
    Raw,
    Patch,
    Translate,
    Unsupported,
}

impl BodyMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::Patch => "patch",
            Self::Translate => "translate",
            Self::Unsupported => "unsupported",
        }
    }

    pub fn fidelity(self) -> FidelityLevel {
        match self {
            Self::Raw => FidelityLevel::ByteExact,
            Self::Patch => FidelityLevel::StructureExact,
            Self::Translate | Self::Unsupported => FidelityLevel::SemanticEquivalent,
        }
    }
}

/// 网关实际处理的协议操作(不是 provider 家族名)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Operation {
    ChatCompletions,
    Responses,
    Embeddings,
    Messages,
    GeminiGenerateContent,
    GeminiStreamGenerateContent,
    GeminiInteractions,
}

impl Operation {
    pub fn for_source(source: SourceProtocol, stream: bool) -> Result<Self, ContractViolation> {
        let operation = match source {
            SourceProtocol::OpenAi => Self::ChatCompletions,
            SourceProtocol::OpenAiResponses => Self::Responses,
            SourceProtocol::OpenAiEmbeddings => Self::Embeddings,
            SourceProtocol::Anthropic => Self::Messages,
            SourceProtocol::Gemini => {
                if stream {
                    Self::GeminiStreamGenerateContent
                } else {
                    Self::GeminiGenerateContent
                }
            }
            SourceProtocol::GeminiStream => Self::GeminiStreamGenerateContent,
            SourceProtocol::GeminiInteractions => Self::GeminiInteractions,
        };
        if source == SourceProtocol::GeminiStream && !stream {
            return Err(ContractViolation::InvalidStreaming { operation, stream });
        }
        // Interactions 没有流式线格式契约:放行会把 `candidates[]` 冒充成
        // interactions 表示,必须在规划层拒绝。
        if source == SourceProtocol::GeminiInteractions && stream {
            return Err(ContractViolation::InvalidStreaming { operation, stream });
        }
        Ok(operation)
    }

    pub fn request_kind(self) -> RequestKind {
        match self {
            Self::Embeddings => RequestKind::Embeddings,
            Self::GeminiInteractions => RequestKind::Interactions,
            _ => RequestKind::Chat,
        }
    }

    /// 操作是否必须走流式端点。
    pub fn requires_stream(self) -> Option<bool> {
        match self {
            Self::GeminiStreamGenerateContent => Some(true),
            Self::Embeddings | Self::GeminiGenerateContent => Some(false),
            Self::ChatCompletions | Self::Responses | Self::Messages | Self::GeminiInteractions => {
                None
            }
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ChatCompletions => "chat_completions",
            Self::Responses => "responses",
            Self::Embeddings => "embeddings",
            Self::Messages => "messages",
            Self::GeminiGenerateContent => "gemini_generate_content",
            Self::GeminiStreamGenerateContent => "gemini_stream_generate_content",
            Self::GeminiInteractions => "gemini_interactions",
        }
    }

    /// 端点路径形态(由 `protocol::endpoint` 负责实际拼接;这里只固定
    /// 操作与路径的对应关系,避免规划器与 URL 构造各说各话)。
    pub fn api_path(self) -> &'static str {
        match self {
            Self::ChatCompletions => "/chat/completions",
            Self::Responses => "/responses",
            Self::Embeddings => "/embeddings",
            Self::Messages => "/messages",
            Self::GeminiGenerateContent => "models/{model}:generateContent",
            Self::GeminiStreamGenerateContent => "models/{model}:streamGenerateContent",
            Self::GeminiInteractions => "/interactions",
        }
    }
}

/// 出站前固定的请求 profile。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestProfile {
    pub source: SourceProtocol,
    pub operation: Operation,
    pub stream: bool,
}

impl RequestProfile {
    pub fn new(
        source: SourceProtocol,
        operation: Operation,
        stream: bool,
    ) -> Result<Self, ContractViolation> {
        let expected = Operation::for_source(source, stream)?;
        if expected != operation {
            return Err(ContractViolation::OperationMismatch {
                source,
                expected,
                actual: operation,
            });
        }
        if let Some(requires_stream) = operation.requires_stream()
            && requires_stream != stream
        {
            return Err(ContractViolation::InvalidStreaming { operation, stream });
        }
        Ok(Self {
            source,
            operation,
            stream,
        })
    }

    pub fn for_source(source: SourceProtocol, stream: bool) -> Result<Self, ContractViolation> {
        let operation = Operation::for_source(source, stream)?;
        Self::new(source, operation, stream)
    }
}

/// 响应侧预期表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseRepresentation {
    Json,
    Sse,
    Ndjson,
    JsonOrSse,
}

impl ResponseRepresentation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Sse => "sse",
            Self::Ndjson => "ndjson",
            Self::JsonOrSse => "json_or_sse",
        }
    }

    pub fn accepts(self, actual: Representation) -> bool {
        match self {
            Self::Json => actual == Representation::Json,
            Self::Sse => actual == Representation::Sse,
            Self::Ndjson => actual == Representation::Ndjson,
            Self::JsonOrSse => matches!(actual, Representation::Json | Representation::Sse),
        }
    }
}

/// 请求/响应各自的正文模式与保真目标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlannedBody {
    pub mode: BodyMode,
    pub fidelity: FidelityLevel,
}

impl PlannedBody {
    pub fn raw() -> Self {
        Self {
            mode: BodyMode::Raw,
            fidelity: FidelityLevel::ByteExact,
        }
    }

    pub fn patch() -> Self {
        Self {
            mode: BodyMode::Patch,
            fidelity: FidelityLevel::StructureExact,
        }
    }

    pub fn translate() -> Self {
        Self {
            mode: BodyMode::Translate,
            fidelity: FidelityLevel::SemanticEquivalent,
        }
    }
}

/// 协议能力(方案 §2.3 的必覆盖清单)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Capability {
    Text,
    Refusal,
    Reasoning,
    ReasoningSignature,
    MultiCandidate,
    MultiItemPart,
    ToolDefinitions,
    ToolChoice,
    ParallelToolCalls,
    ToolArgumentStreaming,
    ToolResults,
    BuiltinTools,
    StructuredOutput,
    MultimodalInput,
    MultimodalOutput,
    Annotations,
    Usage,
    TerminalState,
    Errors,
    StatefulReferences,
}

impl Capability {
    pub const ALL: &'static [Self] = &[
        Self::Text,
        Self::Refusal,
        Self::Reasoning,
        Self::ReasoningSignature,
        Self::MultiCandidate,
        Self::MultiItemPart,
        Self::ToolDefinitions,
        Self::ToolChoice,
        Self::ParallelToolCalls,
        Self::ToolArgumentStreaming,
        Self::ToolResults,
        Self::BuiltinTools,
        Self::StructuredOutput,
        Self::MultimodalInput,
        Self::MultimodalOutput,
        Self::Annotations,
        Self::Usage,
        Self::TerminalState,
        Self::Errors,
        Self::StatefulReferences,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Refusal => "refusal",
            Self::Reasoning => "reasoning",
            Self::ReasoningSignature => "reasoning_signature",
            Self::MultiCandidate => "multi_candidate",
            Self::MultiItemPart => "multi_item_part",
            Self::ToolDefinitions => "tool_definitions",
            Self::ToolChoice => "tool_choice",
            Self::ParallelToolCalls => "parallel_tool_calls",
            Self::ToolArgumentStreaming => "tool_argument_streaming",
            Self::ToolResults => "tool_results",
            Self::BuiltinTools => "builtin_tools",
            Self::StructuredOutput => "structured_output",
            Self::MultimodalInput => "multimodal_input",
            Self::MultimodalOutput => "multimodal_output",
            Self::Annotations => "annotations",
            Self::Usage => "usage",
            Self::TerminalState => "terminal_state",
            Self::Errors => "errors",
            Self::StatefulReferences => "stateful_references",
        }
    }
}

/// 每项能力只能落在四种结果之一(方案 §2.3)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilitySupportKind {
    /// 原样保留。
    Native,
    /// 异协议等价表达。
    Equivalent,
    /// 已声明降级(必须能被调用方感知)。
    Degraded,
    /// 明确不支持。
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilitySupport {
    pub capability: Capability,
    pub support: CapabilitySupportKind,
    /// 不支持/降级的原因与规则锚点。
    pub note: Option<&'static str>,
}

impl CapabilitySupport {
    fn equivalent(capability: Capability) -> Self {
        Self {
            capability,
            support: CapabilitySupportKind::Equivalent,
            note: None,
        }
    }

    fn degraded(capability: Capability, note: &'static str) -> Self {
        Self {
            capability,
            support: CapabilitySupportKind::Degraded,
            note: Some(note),
        }
    }

    fn unsupported(capability: Capability, note: &'static str) -> Self {
        Self {
            capability,
            support: CapabilitySupportKind::Unsupported,
            note: Some(note),
        }
    }
}

/// 一次交换的损失契约。Raw 原生格为空表示无可声明损失。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LossContract {
    capabilities: Vec<CapabilitySupport>,
    notes: Vec<&'static str>,
}

impl LossContract {
    pub fn lossless() -> Self {
        Self {
            capabilities: Vec::new(),
            notes: Vec::new(),
        }
    }

    pub fn translated(operation: Operation) -> Self {
        let capabilities = translation_capabilities(operation);
        Self {
            capabilities,
            notes: vec![
                "跨协议只对声明为 Equivalent 的能力负责;Unsupported 能力必须在出站前或响应处理中显式拒绝/报错,不得静默丢弃",
                "能力支持等级随 P3/P4 证据升级,不得只凭代码存在宣称等价",
            ],
        }
    }

    pub fn capabilities(&self) -> &[CapabilitySupport] {
        &self.capabilities
    }

    pub fn notes(&self) -> &[&'static str] {
        &self.notes
    }

    pub fn is_lossless(&self) -> bool {
        self.capabilities
            .iter()
            .all(|entry| entry.support == CapabilitySupportKind::Native)
    }

    pub fn unsupported(&self) -> impl Iterator<Item = &CapabilitySupport> {
        self.capabilities
            .iter()
            .filter(|entry| entry.support == CapabilitySupportKind::Unsupported)
    }

    pub fn support_for(&self, capability: Capability) -> Option<&CapabilitySupport> {
        self.capabilities
            .iter()
            .find(|entry| entry.capability == capability)
    }
}

/// 目标翻译能力矩阵。P1 的保守声明:现有适配器能表达且已有回归的记
/// `Equivalent`,已知缺口与未验证能力一律 `Unsupported`,由 P3/P4 升级。
fn translation_capabilities(operation: Operation) -> Vec<CapabilitySupport> {
    if operation == Operation::Embeddings {
        return Vec::new();
    }
    vec![
        CapabilitySupport::equivalent(Capability::Text),
        CapabilitySupport::equivalent(Capability::Refusal),
        CapabilitySupport::equivalent(Capability::Reasoning),
        CapabilitySupport::unsupported(
            Capability::ReasoningSignature,
            "签名/加密推理没有跨协议迁移契约;同协议原生路径保留,禁止伪造迁移",
        ),
        CapabilitySupport::equivalent(Capability::MultiCandidate),
        CapabilitySupport::equivalent(Capability::MultiItemPart),
        CapabilitySupport::equivalent(Capability::ToolDefinitions),
        CapabilitySupport::equivalent(Capability::ToolChoice),
        CapabilitySupport::equivalent(Capability::ParallelToolCalls),
        CapabilitySupport::equivalent(Capability::ToolArgumentStreaming),
        CapabilitySupport::equivalent(Capability::ToolResults),
        CapabilitySupport::unsupported(
            Capability::BuiltinTools,
            "内置工具没有统一 IR,跨协议不得猜测",
        ),
        CapabilitySupport::equivalent(Capability::StructuredOutput),
        CapabilitySupport::unsupported(
            Capability::MultimodalInput,
            "Responses/Gemini/Chat 的多模态输入未逐一建立等价回归",
        ),
        CapabilitySupport::unsupported(
            Capability::MultimodalOutput,
            "多模态输出没有跨协议等价表达",
        ),
        CapabilitySupport::degraded(
            Capability::Annotations,
            "Responses 保留 annotations;其他目标无法表达时显式降级",
        ),
        CapabilitySupport::equivalent(Capability::Usage),
        CapabilitySupport::equivalent(Capability::TerminalState),
        CapabilitySupport::equivalent(Capability::Errors),
        CapabilitySupport::unsupported(
            Capability::StatefulReferences,
            "资源 ID/previous_response_id/签名不能跨供应商或账号伪造迁移",
        ),
    ]
}

/// 一次交换的完整规划。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExchangePlan {
    pub profile: RequestProfile,
    pub target: ProviderProtocol,
    pub request: PlannedBody,
    pub response: PlannedBody,
    pub response_representation: ResponseRepresentation,
    pub loss: LossContract,
}

impl ExchangePlan {
    /// 整体保真取请求/响应较弱一侧。
    pub fn fidelity(&self) -> FidelityLevel {
        self.request.fidelity.minimum(self.response.fidelity)
    }

    pub fn is_byte_exact(&self) -> bool {
        self.request.fidelity == FidelityLevel::ByteExact
            && self.response.fidelity == FidelityLevel::ByteExact
    }
}

/// 出站前唯一规划入口。未知/不支持组合返回错误,不回退透传。
pub fn plan_exchange(
    profile: RequestProfile,
    target: ProviderProtocol,
    native_responses: bool,
) -> Result<ExchangePlan, ContractViolation> {
    let source = profile.source;
    match (source, target) {
        (
            SourceProtocol::OpenAiEmbeddings,
            ProviderProtocol::Anthropic | ProviderProtocol::Gemini,
        ) => {
            return Err(ContractViolation::UnsupportedCombination {
                source,
                target,
                reason: "embeddings 仅支持 OpenAI 上游;到 Anthropic/Gemini 无操作契约",
            });
        }
        (
            SourceProtocol::GeminiInteractions,
            ProviderProtocol::OpenAi | ProviderProtocol::Anthropic,
        ) => {
            return Err(ContractViolation::UnsupportedCombination {
                source,
                target,
                reason: "Gemini Interactions 仅支持 Gemini 上游;非 Gemini 无完整契约",
            });
        }
        _ => {}
    }

    let native = match (source, target) {
        // OpenAI 上游的线格式由 `type` 决定:chat 客户端打到 responses-style
        // 上游必须 Translate,不能按同一协议家族误判为 Raw。
        (SourceProtocol::OpenAi, ProviderProtocol::OpenAi) => !native_responses,
        (SourceProtocol::OpenAiResponses, ProviderProtocol::OpenAi) => native_responses,
        (SourceProtocol::Anthropic, ProviderProtocol::Anthropic) => true,
        (SourceProtocol::Gemini | SourceProtocol::GeminiStream, ProviderProtocol::Gemini) => true,
        (SourceProtocol::OpenAiEmbeddings, ProviderProtocol::OpenAi) => true,
        _ => false,
    };

    let response_representation = expected_representation(profile.operation, profile.stream);

    if native {
        return Ok(ExchangePlan {
            profile,
            target,
            request: PlannedBody::raw(),
            response: PlannedBody::raw(),
            response_representation,
            loss: LossContract::lossless(),
        });
    }

    if source == SourceProtocol::GeminiInteractions && target == ProviderProtocol::Gemini {
        return Ok(ExchangePlan {
            profile,
            target,
            request: PlannedBody::patch(),
            response: PlannedBody::translate(),
            response_representation,
            loss: LossContract::translated(profile.operation),
        });
    }

    Ok(ExchangePlan {
        profile,
        target,
        request: PlannedBody::translate(),
        response: PlannedBody::translate(),
        response_representation,
        loss: LossContract::translated(profile.operation),
    })
}

/// 便捷入口:由客户端协议与 stream 推导操作。
pub fn plan_for_source(
    source: SourceProtocol,
    target: ProviderProtocol,
    stream: bool,
    native_responses: bool,
) -> Result<ExchangePlan, ContractViolation> {
    let profile = RequestProfile::for_source(source, stream)?;
    plan_exchange(profile, target, native_responses)
}

fn expected_representation(operation: Operation, stream: bool) -> ResponseRepresentation {
    match operation {
        Operation::Embeddings | Operation::GeminiGenerateContent => ResponseRepresentation::Json,
        Operation::ChatCompletions | Operation::Responses | Operation::Messages => {
            if stream {
                ResponseRepresentation::Sse
            } else {
                ResponseRepresentation::Json
            }
        }
        Operation::GeminiStreamGenerateContent => ResponseRepresentation::JsonOrSse,
        Operation::GeminiInteractions => ResponseRepresentation::Json,
    }
}

/// 按**精确媒体类型**解析响应表示;不做子串匹配。
pub fn resolve_response_representation(
    expected: ResponseRepresentation,
    content_type: Option<&str>,
) -> Result<Representation, ContractViolation> {
    let Some(content_type) = content_type else {
        return Err(ContractViolation::MissingContentType);
    };
    let media_type = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if media_type.is_empty() {
        return Err(ContractViolation::MissingContentType);
    }
    let actual = match media_type.as_str() {
        "text/event-stream" => Representation::Sse,
        "application/json" => Representation::Json,
        "application/x-ndjson" | "application/ndjson" => Representation::Ndjson,
        _ => {
            return Err(ContractViolation::UnknownContentType {
                value: content_type.to_owned(),
            });
        }
    };
    if !expected.accepts(actual) {
        return Err(ContractViolation::UnexpectedRepresentation {
            expected,
            actual,
            content_type: content_type.to_owned(),
        });
    }
    Ok(actual)
}

/// 已规划响应执行器(收到响应头后只在这里选择,不临时开透传分支)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseExecutor {
    /// 上游原生 SSE(或已声明的 NDJSON→SSE 转换)。
    Sse,
    /// 上游 NDJSON 逐行事件(独立分帧,不按 SSE `data:` 解释)。
    Ndjson,
    /// 上游单个 JSON → 客户端流帧(已注册的表示合成,A6)。
    JsonSynthesis,
    /// 非流 JSON 原样交给非流解码。
    Json,
}

impl ResponseExecutor {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sse => "sse",
            Self::Ndjson => "ndjson",
            Self::JsonSynthesis => "json_synthesis",
            Self::Json => "json",
        }
    }
}

/// 响应头到达后选择执行器。表示转换必须在规划内:
/// - 流式操作接受 JSON→流的已注册合成(A6);
/// - 流式操作接受 NDJSON→语义流(独立逐行分帧);
/// - 其余媒体类型/方向错配一律显式失败,不默认为 JSON。
pub fn select_response_executor(
    plan: &ExchangePlan,
    content_type: Option<&str>,
) -> Result<ResponseExecutor, ContractViolation> {
    let Some(content_type) = content_type else {
        return Err(ContractViolation::MissingContentType);
    };
    let media_type = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if media_type.is_empty() {
        return Err(ContractViolation::MissingContentType);
    }
    let actual = match media_type.as_str() {
        "text/event-stream" => Representation::Sse,
        "application/json" => Representation::Json,
        "application/x-ndjson" | "application/ndjson" => Representation::Ndjson,
        _ => {
            return Err(ContractViolation::UnknownContentType {
                value: content_type.to_owned(),
            });
        }
    };
    let expected = plan.response_representation;
    match (plan.profile.stream, actual) {
        (true, Representation::Sse) if expected.accepts(Representation::Sse) => {
            Ok(ResponseExecutor::Sse)
        }
        (true, Representation::Ndjson)
            if matches!(
                expected,
                ResponseRepresentation::Sse
                    | ResponseRepresentation::JsonOrSse
                    | ResponseRepresentation::Ndjson
            ) =>
        {
            Ok(ResponseExecutor::Ndjson)
        }
        (true, Representation::Json)
            if matches!(
                expected,
                ResponseRepresentation::Sse | ResponseRepresentation::JsonOrSse
            ) =>
        {
            Ok(ResponseExecutor::JsonSynthesis)
        }
        (false, Representation::Json) if expected.accepts(Representation::Json) => {
            Ok(ResponseExecutor::Json)
        }
        _ => Err(ContractViolation::UnexpectedRepresentation {
            expected,
            actual,
            content_type: content_type.to_owned(),
        }),
    }
}

/// 规划拒绝原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContractViolation {
    UnknownSourceProtocol {
        value: String,
    },
    UnsupportedCombination {
        source: SourceProtocol,
        target: ProviderProtocol,
        reason: &'static str,
    },
    OperationMismatch {
        source: SourceProtocol,
        expected: Operation,
        actual: Operation,
    },
    InvalidStreaming {
        operation: Operation,
        stream: bool,
    },
    MissingContentType,
    UnknownContentType {
        value: String,
    },
    UnexpectedRepresentation {
        expected: ResponseRepresentation,
        actual: Representation,
        content_type: String,
    },
}

impl fmt::Display for ContractViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownSourceProtocol { value } => {
                write!(f, "unknown client protocol: {value}")
            }
            Self::UnsupportedCombination {
                source,
                target,
                reason,
            } => write!(
                f,
                "unsupported protocol combination {} -> {}: {reason}",
                source.as_str(),
                target.as_str()
            ),
            Self::OperationMismatch {
                source,
                expected,
                actual,
            } => write!(
                f,
                "operation mismatch for {}: expected {}, got {}",
                source.as_str(),
                expected.as_str(),
                actual.as_str()
            ),
            Self::InvalidStreaming { operation, stream } => write!(
                f,
                "operation {} cannot run with stream={stream}",
                operation.as_str()
            ),
            Self::MissingContentType => write!(f, "response Content-Type is missing"),
            Self::UnknownContentType { value } => {
                write!(f, "unsupported response Content-Type: {value}")
            }
            Self::UnexpectedRepresentation {
                expected,
                actual,
                content_type,
            } => write!(
                f,
                "response representation {} (Content-Type {content_type}) does not match planned {}",
                actual.as_str(),
                expected.as_str()
            ),
        }
    }
}

impl std::error::Error for ContractViolation {}

#[cfg(test)]
mod tests;
