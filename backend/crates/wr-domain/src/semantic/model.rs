//! 统一语义模型内核类型(方案 20261011_01 §3.1 / 契约 20261012_01 §2)。
//!
//! 纯类型 + 少量取值助手:无 I/O、无 HTTP、无认证 —— provider/凭据/
//! 端点/headers 一律不进内核(20261012_01 §2.1 决策 #2)。协议特有字段
//! 经 [`Extensions`] 或 [`OpaqueField`] 带来源保留,未知 ≠ 缺失 ≠ 0
//! (计数全 Option)。
//!
//! Serde 表示:本模块类型**不作为 wire 格式**,编解码只经协议适配器;
//! `Serialize/Deserialize` 仅供测试快照与诊断日志(契约 §2.3)。

use std::collections::BTreeMap;

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

use crate::semantic::diagnostic::ConversionReport;

// ---------------------------------------------------------------------------
// 请求侧
// ---------------------------------------------------------------------------

/// 请求种类(方案决策 #8:渐进扩展,embeddings/interactions 不伪装 chat)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestKind {
    #[default]
    Chat,
    Embeddings,
    Interactions,
}

impl RequestKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Embeddings => "embeddings",
            Self::Interactions => "interactions",
        }
    }
}

/// 客户端入口协议(语义侧类型;字符串形态与
/// [`crate::provider::client_protocol`] 常量一一对应)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceProtocol {
    OpenAi,
    OpenAiResponses,
    OpenAiEmbeddings,
    Anthropic,
    Gemini,
    GeminiStream,
    GeminiInteractions,
}

impl SourceProtocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::OpenAiResponses => "openai_responses",
            Self::OpenAiEmbeddings => "openai_embeddings",
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
            Self::GeminiStream => "gemini_stream",
            Self::GeminiInteractions => "gemini_interactions",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "openai" => Self::OpenAi,
            "openai_responses" => Self::OpenAiResponses,
            "openai_embeddings" => Self::OpenAiEmbeddings,
            "anthropic" => Self::Anthropic,
            "gemini" => Self::Gemini,
            "gemini_stream" => Self::GeminiStream,
            "gemini_interactions" => Self::GeminiInteractions,
            _ => return None,
        })
    }

    pub fn is_gemini_family(self) -> bool {
        matches!(
            self,
            Self::Gemini | Self::GeminiStream | Self::GeminiInteractions
        )
    }

    /// 请求种类推导(矩阵 §1.1:5 个对话入口 / embeddings / interactions)。
    pub fn request_kind(self) -> RequestKind {
        match self {
            Self::OpenAiEmbeddings => RequestKind::Embeddings,
            Self::GeminiInteractions => RequestKind::Interactions,
            _ => RequestKind::Chat,
        }
    }
}

/// 消息角色。Developer 是 OpenAI 专属通道;跨协议编码时降级 system
/// 并记诊断(契约 §3.1)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    Developer,
    User,
    Assistant,
    Tool,
}

/// 流式偏好:payload 内是否显式携带 `stream` 键(线级保真需要区分
/// 「缺省」与「显式 false」)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamPreference {
    #[default]
    NotSpecified,
    Explicit(bool),
}

/// 协议扩展命名空间(契约 §2.3):按来源协议分块,键命名
/// `provider/protocol/version/key` 中的来源必填;同名键多来源不合并、
/// 全部保留。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Extensions {
    #[serde(default)]
    blocks: Vec<ExtensionBlock>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionBlock {
    /// 来源协议/提供方命名空间(如 "openai"、"anthropic"、"gemini")。
    pub source: String,
    pub fields: BTreeMap<String, Value>,
}

impl Extensions {
    pub fn insert(&mut self, source: &str, key: &str, value: Value) {
        if let Some(block) = self.blocks.iter_mut().find(|b| b.source == source) {
            block.fields.insert(key.to_owned(), value);
            return;
        }
        let mut fields = BTreeMap::new();
        fields.insert(key.to_owned(), value);
        self.blocks.push(ExtensionBlock {
            source: source.to_owned(),
            fields,
        });
    }

    pub fn get(&self, source: &str, key: &str) -> Option<&Value> {
        self.blocks
            .iter()
            .find(|b| b.source == source)
            .and_then(|b| b.fields.get(key))
    }

    pub fn blocks(&self) -> &[ExtensionBlock] {
        &self.blocks
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.iter().all(|b| b.fields.is_empty())
    }
}

/// 未识别字段的带路径保留(方案决策 #2:raw 与 semantic 并存)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpaqueField {
    /// 源路径,如顶层键名或 `generationConfig.unknownKey`。
    pub path: String,
    pub value: Value,
}

/// 统一语义请求。不可变:候选重试 = 同一实例 × 每候选
/// [`crate::semantic::adapter::OutboundContext`] 重新编码。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticRequest {
    pub kind: RequestKind,
    pub source: SourceProtocol,
    pub model: String,
    pub generation: GenerationOptions,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
    pub tool_choice: Option<ToolChoice>,
    pub stream: StreamPreference,
    pub extensions: Extensions,
    pub opaque: Vec<OpaqueField>,
}

impl SemanticRequest {
    pub fn new(source: SourceProtocol, model: impl Into<String>) -> Self {
        Self {
            kind: source.request_kind(),
            source,
            model: model.into(),
            generation: GenerationOptions::default(),
            messages: Vec::new(),
            tools: Vec::new(),
            tool_choice: None,
            stream: StreamPreference::NotSpecified,
            extensions: Extensions::default(),
            opaque: Vec::new(),
        }
    }

    pub fn report(&self) -> ConversionReport {
        ConversionReport::semantic()
    }
}

/// 生成参数(全 Option;未出现的协议原键保留于 `opaque`)。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GenerationOptions {
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_tokens: Option<i64>,
    pub stop_sequences: Vec<String>,
    pub parallel_tool_calls: Option<bool>,
    /// 推理努力(语义枚举;原字符串进 `opaque` 供家族透传)。
    pub reasoning_effort: Option<ReasoningEffort>,
    /// thinking/thinkingConfig 结构(anthropic `thinking.budget_tokens`、
    /// gemini `thinkingConfig`)。
    pub thinking: Option<ThinkingOptions>,
    pub extensions: Extensions,
    pub opaque: Vec<OpaqueField>,
}

/// 推理努力语义枚举(契约 §3.2)。厂商私值(xhigh 等)落 `Opaque`
/// 原样保留,由 vendor fixup 在出站 payload 上改写(独立 hook,幂等)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    Minimal,
    Low,
    Medium,
    High,
    Max,
    Off,
    Opaque(String),
}

impl ReasoningEffort {
    /// openai 系 `reasoning_effort` 字符串 → 语义值。大小写不敏感
    /// (客户端可能发 "High");vendor 私有值保留**原样**进 `Opaque`
    /// 供家族透传保真。
    pub fn parse(raw: &str) -> Self {
        match raw.to_lowercase().as_str() {
            "minimal" => Self::Minimal,
            "low" => Self::Low,
            "medium" => Self::Medium,
            "high" => Self::High,
            "max" => Self::Max,
            "none" => Self::Off,
            _ => Self::Opaque(raw.to_owned()),
        }
    }

    pub fn as_raw(&self) -> String {
        match self {
            Self::Minimal => "minimal".to_owned(),
            Self::Low => "low".to_owned(),
            Self::Medium => "medium".to_owned(),
            Self::High => "high".to_owned(),
            Self::Max => "max".to_owned(),
            Self::Off => "none".to_owned(),
            Self::Opaque(raw) => raw.clone(),
        }
    }

    /// effort → 思考预算档位推导(anthropic `budget_tokens` / gemini
    /// `thinkingBudget` 出站共用;阈值与 `gateway::budget_to_effort` 互逆)。
    /// `Off` 与 vendor 私有值返回 None,由调用方决定省略或记诊断。
    pub fn budget_hint(&self) -> Option<i64> {
        match self {
            Self::Minimal => Some(1_024),
            Self::Low => Some(5_000),
            Self::Medium => Some(15_000),
            Self::High => Some(30_000),
            // Max 与 vendor 私有 "xhigh"/"ultra"(Codex 扩展档)同档
            // (>30000);落进未识别分支会把最深思考静默关掉。
            Self::Max => Some(60_000),
            Self::Opaque(raw) if raw == "xhigh" || raw == "ultra" => Some(60_000),
            Self::Off | Self::Opaque(_) => None,
        }
    }
}

/// 思考配置(gemini thinkingConfig / anthropic thinking 的公共子集;
/// type: enabled/disabled 等协议特有键进扩展)。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ThinkingOptions {
    pub budget_tokens: Option<i64>,
    pub include_thoughts: Option<bool>,
}

/// 单条消息。`content` 空 Vec 保留「显式空 content」语义。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    #[serde(default)]
    pub content: Vec<ContentPart>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningBlock>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_result: Option<ToolResult>,
    /// openai tool 消息 name 等协议本地辅助标识。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 协议本地消息 id(Responses item id 等),不参与内核关联(契约 §2.4)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Extensions::is_empty")]
    pub extensions: Extensions,
}

impl Message {
    pub fn new(role: Role) -> Self {
        Self {
            role,
            content: Vec::new(),
            reasoning: None,
            tool_calls: Vec::new(),
            tool_result: None,
            name: None,
            id: None,
            extensions: Extensions::default(),
        }
    }

    /// 首 个文本 part 的拼接视图(文本聚合语义与旧实现 text join 对齐)。
    pub fn joined_text(&self) -> String {
        let mut out = String::new();
        for part in &self.content {
            if let ContentPart::Text { text } = part {
                out.push_str(text);
            }
        }
        out
    }
}

/// 内容块。多模态统一走显式变体;无法归一的块原样进 `Opaque`
/// (不塞近似通用字段,方案决策 #2)。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "part", rename_all = "snake_case")]
pub enum ContentPart {
    Text {
        text: String,
    },
    /// 引用形态图片(URL / file id)。
    ImageRef {
        url: String,
    },
    ImageInline {
        #[serde(skip_serializing_if = "Option::is_none")]
        media_type: Option<String>,
        data_base64: String,
    },
    Audio {
        #[serde(skip_serializing_if = "Option::is_none")]
        media_type: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        data_base64: Option<String>,
    },
    File {
        #[serde(skip_serializing_if = "Option::is_none")]
        file_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        filename: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        data_base64: Option<String>,
    },
    Refusal {
        text: String,
    },
    /// 未知/带协议特有字段的块原样保留(源协议的 JSON 块)。
    Opaque {
        #[serde(skip_serializing_if = "Option::is_none")]
        kind_hint: Option<String>,
        value: Value,
    },
}

/// 推理/思考块。signature(thoughtSignature / thinking signature)进
/// 扩展字段而非丢弃(契约 §3.2 修复点)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasoningBlock {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub opaque: Vec<Value>,
}

/// 工具定义。parameters_schema 原样保存;Gemini schema 清洗是
/// Outbound 编码步骤(可重入、幂等),不改内核(契约 §3.3)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub parameters_schema: Value,
    #[serde(default, skip_serializing_if = "Extensions::is_empty")]
    pub extensions: Extensions,
}

/// 工具调用标识:单次请求会话内保留原值;目标协议强制无 id 时
/// (Gemini functionCall)允许生成并记 `RegeneratedId`(契约 §2.4)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallId {
    pub local: String,
    pub origin: IdOrigin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdOrigin {
    /// 客户端/上游原值。
    Source,
    /// 转换层生成(必须伴随 RegeneratedId 诊断)。
    Regenerated,
}

/// 工具调用参数双形态(契约 §2.3:openai 是 JSON 字符串、
/// anthropic/gemini 是对象;保留入口原形态避免双重转义)。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolArguments {
    Raw(String),
    Parsed(Value),
}

impl ToolArguments {
    /// 编码视角:对象形态;非法 Raw 返回 `{"raw": s}` 兜底并由调用方
    /// 记 `DowngradedContent` 诊断(旧实现的静默行为的显式化)。
    pub fn as_object(&self) -> (Value, bool) {
        match self {
            Self::Parsed(value) => (value.clone(), false),
            Self::Raw(raw) => match serde_json::from_str::<Value>(raw) {
                Ok(value) => (value, false),
                Err(_) => (serde_json::json!({"raw": raw}), true),
            },
        }
    }

    /// openai `arguments` 字符串形态(Parsed → dumps)。
    pub fn as_raw_string(&self) -> String {
        match self {
            Self::Raw(raw) => raw.clone(),
            Self::Parsed(value) => crate::json_py::py_json_dumps(value),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: ToolCallId,
    pub name: String,
    pub arguments: ToolArguments,
    #[serde(default, skip_serializing_if = "Extensions::is_empty")]
    pub extensions: Extensions,
}

/// 工具结果。Gemini functionResponse 依 name 关联(`resolved_by_name`),
/// 协议缺失关联 id 时 `call_ref: None`(记诊断,契约 §2.4)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_ref: Option<ToolCallId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_by_name: Option<String>,
    /// 结构化原样(协议结果负载不做语义拆解)。
    pub content: Value,
    pub is_error: bool,
    #[serde(default, skip_serializing_if = "Extensions::is_empty")]
    pub extensions: Extensions,
}

/// 工具选择(契约 §3.3:全协议公共枚举;无法表达的形态落 Opaque)。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoice {
    Auto,
    None,
    Required,
    Named { name: String },
    Opaque(Value),
}

// ---------------------------------------------------------------------------
// 响应侧
// ---------------------------------------------------------------------------

/// 统一语义响应。多候选保留(Gemini/OpenAI n>1);旧转换实现仅取首个
/// 属实现限制,非模型限制(契约 §2.2)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub candidates: Vec<ResponseCandidate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish: Option<FinishReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub incomplete: Option<IncompleteReason>,
    #[serde(default, skip_serializing_if = "Extensions::is_empty")]
    pub extensions: Extensions,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub opaque: Vec<OpaqueField>,
}

impl SemanticResponse {
    pub fn new() -> Self {
        Self {
            id: None,
            model: None,
            candidates: Vec::new(),
            usage: None,
            finish: None,
            refusal: None,
            incomplete: None,
            extensions: Extensions::default(),
            opaque: Vec::new(),
        }
    }

    /// 首个候选(旧实现语义;矩阵记录的「仅取首个」实现限制)。
    pub fn first_candidate(&self) -> Option<&ResponseCandidate> {
        self.candidates.first()
    }

    /// 全候选文本聚合(跨候选,镜像旧实现 text parts concat 行为)。
    pub fn joined_text(&self) -> String {
        let mut out = String::new();
        for candidate in &self.candidates {
            for part in &candidate.content {
                if let ContentPart::Text { text } = part {
                    out.push_str(text);
                }
            }
        }
        out
    }
}

impl Default for SemanticResponse {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseCandidate {
    pub index: usize,
    #[serde(default)]
    pub content: Vec<ContentPart>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningBlock>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish: Option<FinishReason>,
    #[serde(default, skip_serializing_if = "Extensions::is_empty")]
    pub extensions: Extensions,
}

impl ResponseCandidate {
    pub fn new(index: usize) -> Self {
        Self {
            index,
            content: Vec::new(),
            reasoning: None,
            tool_calls: Vec::new(),
            finish: None,
            extensions: Extensions::default(),
        }
    }
}

/// 结束原因语义枚举(契约 §3.6 冻结映射表;未知原值 Opaque + 诊断)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    Length,
    ToolCall,
    ContentFilter,
    Refusal,
    IncompleteOther,
    /// 未识别原值原样保留(必须伴随 UnmappedEnum 诊断)。
    Opaque(String),
}

/// Responses incomplete 原因(契约 §3.4)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IncompleteReason {
    MaxOutputTokens,
    ContentFilter,
    Other(String),
}

impl IncompleteReason {
    pub fn as_responses_str(&self) -> &str {
        match self {
            Self::MaxOutputTokens => "max_output_tokens",
            Self::ContentFilter => "content_filter",
            Self::Other(raw) => raw.as_str(),
        }
    }

    /// 旧 `map_finish_reason_to_responses` 的字符串 → 枚举反向。
    pub fn parse_responses(raw: &str) -> Option<Self> {
        match raw {
            "max_output_tokens" => Some(Self::MaxOutputTokens),
            "content_filter" => Some(Self::ContentFilter),
            other if other.is_empty() => None,
            other => Some(Self::Other(other.to_owned())),
        }
    }
}

/// 用量:全 Option + 来源标注(方案决策 #5:未知 ≠ 缺失 ≠ 0)。
/// **计费口径不变更**:日志计费仍按现有 0 缺省计算,仅语义层区分
/// Reported 缺失与真实 0(契约 §3.7)。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<i64>,
}

impl Usage {
    pub fn reported() -> Self {
        Self {
            input: None,
            output: None,
            total: None,
            reasoning: None,
            cache_read: None,
            cache_write: None,
        }
    }

    /// 编码/计量口径:缺失按 0(与旧实现的 int_or 0 线级行为一致)。
    pub fn or_zero(value: Option<i64>) -> i64 {
        value.unwrap_or(0)
    }

    /// 语义用量 → 归一化计量用量(唯一转换入口)。
    ///
    /// 未知字段按 0;`total` 缺失或为 0 时回退 `input + output`,与
    /// [`crate::usage::normalize_usage`] 的「0 即缺失」口径一致。
    /// `total` 显式非零时保留上游原值。
    pub fn to_domain(self) -> crate::usage::Usage {
        let input = self.input.unwrap_or(0);
        let output = self.output.unwrap_or(0);
        let total = match self.total {
            Some(total) if total != 0 => total,
            _ => input + output,
        };
        crate::usage::Usage {
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: self.cache_read.unwrap_or(0),
            cache_write_tokens: self.cache_write.unwrap_or(0),
            total_tokens: total,
            reasoning_tokens: self.reasoning.unwrap_or(0),
        }
    }
}

// ---------------------------------------------------------------------------
// 流事件词汇(阶段 C 细化生命周期,此处按契约 §2.2 冻结)
// ---------------------------------------------------------------------------

/// 语义流事件词汇表。阶段 B 仅冻结形态;生命周期约束(事件顺序、
/// terminal 规则、usage 更新策略)在阶段 C 落地。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum SemanticEvent {
    MessageStart {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    PartStart {
        candidate: usize,
        index: usize,
        part: ContentPart,
    },
    PartDelta {
        candidate: usize,
        index: usize,
        text: String,
    },
    /// 拒绝文本增量(Responses `response.refusal.delta`)。拒绝不是普通
    /// 正文:单独成事件,聚合时写入 `ContentPart::Refusal`。
    RefusalDelta {
        candidate: usize,
        index: usize,
        text: String,
    },
    ReasoningStart {
        candidate: usize,
        index: usize,
    },
    ReasoningDelta {
        candidate: usize,
        index: usize,
        text: String,
    },
    /// 思考签名增量(Anthropic `signature_delta`、Gemini
    /// `thoughtSignature`)。单独成事件而非塞进 `ReasoningDelta`,因为
    /// 签名不是用户可见文本,聚合时写入 `ReasoningBlock::signature`
    /// (契约 §3.2:签名必须保留)。
    ReasoningSignature {
        candidate: usize,
        index: usize,
        signature: String,
    },
    /// 内容块结束(Anthropic `content_block_stop`、Responses
    /// `output_item.done`)。语义为空,存在是为了让聚合器知道该块
    /// 不会再增长,并为 trace 提供结构边界。
    PartEnd {
        candidate: usize,
        index: usize,
    },
    ToolCallStart {
        candidate: usize,
        index: usize,
        call: ToolCall,
    },
    ToolArgumentsDelta {
        candidate: usize,
        index: usize,
        fragment: String,
    },
    ToolResult {
        candidate: usize,
        result: ToolResult,
    },
    Usage(Usage),
    Finish {
        candidate: usize,
        reason: FinishReason,
    },
    StreamEnd {
        incomplete: bool,
    },
    Error(crate::semantic::error::SemanticError),
    /// 未识别事件原样保留(方案决策:不吞事件)。
    Opaque {
        raw: Value,
    },
}

#[cfg(test)]
mod tests {
    use super::Usage;

    fn usage(input: Option<i64>, output: Option<i64>, total: Option<i64>) -> Usage {
        Usage {
            input,
            output,
            total,
            ..Usage::reported()
        }
    }

    #[test]
    fn to_domain_derives_total_when_missing() {
        let domain = usage(Some(7), Some(3), None).to_domain();
        assert_eq!(domain.input_tokens, 7);
        assert_eq!(domain.output_tokens, 3);
        assert_eq!(domain.total_tokens, 10);
    }

    #[test]
    fn to_domain_keeps_explicit_total() {
        let domain = usage(Some(7), Some(3), Some(42)).to_domain();
        assert_eq!(domain.total_tokens, 42);
    }

    #[test]
    fn to_domain_treats_zero_total_as_missing() {
        let domain = usage(Some(7), Some(3), Some(0)).to_domain();
        assert_eq!(domain.total_tokens, 10);
    }

    #[test]
    fn to_domain_zeroes_fully_absent_usage() {
        let domain = usage(None, None, None).to_domain();
        assert_eq!(domain, crate::usage::Usage::empty());
    }

    #[test]
    fn to_domain_preserves_cache_and_reasoning() {
        let semantic = Usage {
            input: Some(1),
            output: Some(2),
            cache_read: Some(3),
            cache_write: Some(4),
            reasoning: Some(2),
            ..Usage::reported()
        };
        let domain = semantic.to_domain();
        assert_eq!(domain.cache_read_tokens, 3);
        assert_eq!(domain.cache_write_tokens, 4);
        assert_eq!(domain.reasoning_tokens, 2);
        assert_eq!(domain.total_tokens, 3);
    }
}
