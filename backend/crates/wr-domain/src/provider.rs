//! Provider 领域实体(镜像 Python `domain/provider/entities.py`,R2 子集)。
//!
//! 从配置的归一化 provider dict(`wr-infra::config::ProviderRecord.raw`)
//! 构造;字段解析逐条对齐 Python `Provider.from_dict`,包括错误路径:
//! Python 里解析失败抛 ValueError/TypeError → providers() 整体失败 →
//! 请求 502 wrouter_error;Rust 镜像为 `from_raw` 返回 Err,由调用方
//! 升格为同一响应。
//!
//! provider 协议仅接受 `openai | anthropic | gemini`(镜像
//! `domain/provider/value_objects.py::ProtocolKind`;客户端入口协议
//! 另有四种,见 [`client_protocol`])。

use serde_json::{Map, Value};

use crate::python_semantics::{py_bool, py_float, py_int, py_str};

/// provider 可 speak 的上游协议(镜像 ProtocolKind,仅三种)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderProtocol {
    OpenAi,
    Anthropic,
    Gemini,
}

impl ProviderProtocol {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.to_ascii_lowercase().as_str() {
            "openai" => Ok(Self::OpenAi),
            "anthropic" => Ok(Self::Anthropic),
            "gemini" => Ok(Self::Gemini),
            other => Err(format!("Unsupported provider protocol: {other}")),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
        }
    }

    /// 该协议族是否会上报 prompt cache 的命中/写入量(阶段五 C-1
    /// 「粘性值不值」判据的前提)。
    ///
    /// - **Anthropic**:官方回报 `cache_read_input_tokens` /
    ///   `cache_creation_input_tokens` → 可作判据(但需请求带
    ///   `cache_control` 断点,否则恒为 0)。
    /// - **OpenAI / Gemini**:只报 cache_read、不报 cache_write,且
    ///   中转站常整段不回报 → 「cache_read == 0」在这种上游**恒为真**,
    ///   拿它当判据等于「永远解绑」,会把正常粘性全部打死。
    ///
    /// 因此判据只在 Anthropic 族生效(方案 §6.1 的收窄选项之一)。
    pub fn reports_cache_usage(self) -> bool {
        matches!(self, Self::Anthropic)
    }
}

/// 客户端入口协议常量(镜像 Python `domain/protocol/value_objects.py`)。
pub mod client_protocol {
    pub const OPENAI: &str = "openai";
    pub const OPENAI_RESPONSES: &str = "openai_responses";
    pub const OPENAI_EMBEDDINGS: &str = "openai_embeddings";
    pub const ANTHROPIC: &str = "anthropic";
    pub const GEMINI: &str = "gemini";
    pub const GEMINI_STREAM: &str = "gemini_stream";
    pub const GEMINI_INTERACTIONS: &str = "gemini_interactions";

    pub fn is_gemini(protocol: &str) -> bool {
        matches!(protocol, GEMINI | GEMINI_STREAM | GEMINI_INTERACTIONS)
    }
}

/// 标量 str() 的容错包装:数组/对象在 Python 里会给出 repr,R2 不支持
/// 该形态,报镜像错误。
fn py_str_scalar(v: &Value, field: &str) -> Result<String, String> {
    py_str(v).ok_or_else(|| format!("provider {field} must be a scalar value"))
}

/// 凭据选择策略(方案 20260905_02 §3.1;Rust 新增能力,无 Python 镜像)。
/// UI(2026-09-06 用户调整)只暴露两模式:固定密钥(= First,单启用约束由
/// 前端保证)与负载均衡(= Balanced);Pinned/Weighted/Sticky 保留为配置层
/// 兼容值,不再由前端写入。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProviderCredentialStrategy {
    /// 默认:候选按 `(priority DESC, 配置顺序)` 排序取第一个可用;冷却自动跳下一把。
    #[default]
    First,
    /// 严格固定 `pinned_account_id`:失败/冷却不偷换(用户意志最高)。
    Pinned,
    /// 加权随机:候选按 `weight.max(1)` 累计减权抽取;冷却中的先剔除。
    Weighted,
    /// 会话粘性:加权 rendezvous 哈希;无会话键的请求落 weighted 行为。
    Sticky,
    /// 负载均衡(2026-09-06 UI 两模式):优先级分层取最高可用层,层内按
    /// `weight.max(1)` 加权随机 —— First 的主备层级 × Weighted 的层内分摊。
    Balanced,
}

impl ProviderCredentialStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::First => "first",
            Self::Pinned => "pinned",
            Self::Weighted => "weighted",
            Self::Sticky => "sticky",
            Self::Balanced => "balanced",
        }
    }

    /// **宽容解析**:非法值回退 `First` 而不报 Err —— 单个策略字段配置
    /// 错误不应让 provider 整体解析失败(前端契约中该字段 optional,
    /// Rust 侧缺省即 First)。
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "pinned" => Self::Pinned,
            "weighted" => Self::Weighted,
            "sticky" => Self::Sticky,
            "balanced" => Self::Balanced,
            _ => Self::First,
        }
    }
}

/// Base URL 模式(方案 20261014_01;Rust 新增能力,无 Python 镜像)。
/// Base = 基础 URL,请求时按上游形态自动拼接端点路径(现状行为);
/// Full = 完整 URL,原样调用,任何一层不追加/剥除/重写路径。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UrlMode {
    /// 基础 URL(缺省):`endpoint()` 按已知后缀启发式补全。
    #[default]
    Base,
    /// 完整 URL:`endpoint()` 原样返回用户填写的地址(Gemini 家族除外,
    /// 其端点必须拼模型名与 action,full 语义无法表达,宽容回落 Base)。
    Full,
}

impl UrlMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::Full => "full",
        }
    }

    pub fn is_full(self) -> bool {
        self == Self::Full
    }

    /// **宽容解析**:`full` 之外的一切值(含大小写变体)回落 `Base` 而
    /// 不报 Err —— 与 `ProviderCredentialStrategy::parse` 口径一致,单个
    /// 字段配置错误不应让 provider 整体解析失败。
    pub fn parse(value: &str) -> Self {
        if value.trim().eq_ignore_ascii_case("full") {
            Self::Full
        } else {
            Self::Base
        }
    }
}

/// 供应商级出站代理模式(方案 20260906_01 §4.1c;三态映射
/// `environment | disabled | url`,见决策 #3)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProviderProxyMode {
    /// 跟随全局默认(`config.proxy.upstream`);与字段缺失同义。
    #[default]
    Inherit,
    /// 强制直连,压过一切全局代理配置。
    Direct,
    /// 走 `preset_id` 指向的命名代理预设。
    Preset,
}

impl ProviderProxyMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inherit => "inherit",
            Self::Direct => "direct",
            Self::Preset => "preset",
        }
    }

    /// **宽容解析**:未知值回退 `Inherit`(决策 #7 —— 代理选择配置错误
    /// 不该让整个 provider 加载失败,与 protocol 的严格校验相反)。
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "direct" => Self::Direct,
            "preset" => Self::Preset,
            _ => Self::Inherit,
        }
    }
}

/// 供应商级代理覆盖(方案 20260906_01 B1)。
///
/// `None` 与 `mode == "inherit"` 语义等价,解析侧统一归一为 `None`,
/// 使「沿用全局」只有一种表示(对齐 `failure_threshold` 的 None 惯例)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderProxyOverride {
    pub mode: ProviderProxyMode,
    /// `mode == Preset` 时必填;其余模式恒为 `None`。
    pub preset_id: Option<String>,
}

impl ProviderProxyOverride {
    /// 宽松解析(决策 #7):非对象、未知 mode、`preset` 缺 presetId、
    /// 显式 `inherit` 一律归一为 `None`,不返回 Err。
    pub fn from_value(value: Option<&Value>) -> Option<Self> {
        let map = value?.as_object()?;
        let mode = map
            .get("mode")
            .and_then(Value::as_str)
            .map(ProviderProxyMode::parse)
            .unwrap_or_default();
        match mode {
            ProviderProxyMode::Inherit => None,
            ProviderProxyMode::Direct => Some(Self {
                mode,
                preset_id: None,
            }),
            ProviderProxyMode::Preset => {
                let preset_id = ["presetId", "preset_id"]
                    .iter()
                    .find_map(|key| map.get(*key).and_then(Value::as_str))
                    .map(str::trim)
                    .filter(|text| !text.is_empty())?
                    .to_owned();
                Some(Self {
                    mode,
                    preset_id: Some(preset_id),
                })
            }
        }
    }

    /// 出站形态(REST /api/v1/providers 的 `proxy` 键;camelCase 与前端
    /// 合同一致)。
    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        map.insert(
            "mode".to_owned(),
            Value::String(self.mode.as_str().to_owned()),
        );
        if let Some(preset_id) = &self.preset_id {
            map.insert("presetId".to_owned(), Value::String(preset_id.clone()));
        }
        Value::Object(map)
    }
}

/// 客户端指纹模式(方案 20261016_03;Rust 新增能力,无 Python 镜像)。
/// `off`/缺省 = `None`(镜像 proxy 的 inherit 惯例,「关闭」只有一种表示);
/// 解析侧只认两个有效值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientFingerprintMode {
    /// 透传真实客户端身份:按上游线格式白名单从入站快照复制。
    Passthrough,
    /// 内置预设伪装:`preset_id` 指向 `protocol::client_fingerprint` 的
    /// 内置指纹表。
    Preset,
}

/// 供应商级客户端指纹覆盖(方案 20261016_03 §3.1)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientFingerprintOverride {
    pub mode: ClientFingerprintMode,
    /// `mode == Preset` 时必填;其余模式恒为 `None`。
    pub preset_id: Option<String>,
}

impl ClientFingerprintOverride {
    /// 宽松解析(镜像 ProviderProxyOverride:决策 #7 风格):非对象、
    /// 未知 mode、`preset` 缺 presetId、显式 `off` 一律归一为 `None`
    /// (= off),不返回 Err。
    pub fn from_value(value: Option<&Value>) -> Option<Self> {
        let map = value?.as_object()?;
        let mode = match map.get("mode").and_then(Value::as_str) {
            Some(text) => text.trim().to_ascii_lowercase(),
            None => return None,
        };
        match mode.as_str() {
            "passthrough" => Some(Self {
                mode: ClientFingerprintMode::Passthrough,
                preset_id: None,
            }),
            "preset" => {
                let preset_id = ["presetId", "preset_id"]
                    .iter()
                    .find_map(|key| map.get(*key).and_then(Value::as_str))
                    .map(str::trim)
                    .filter(|text| !text.is_empty())?
                    .to_owned();
                Some(Self {
                    mode: ClientFingerprintMode::Preset,
                    preset_id: Some(preset_id),
                })
            }
            // off / 未知值 → 关闭。
            _ => None,
        }
    }

    /// 出站形态(REST /api/v1/providers 的 `clientFingerprint` 键;
    /// camelCase 与前端合同一致)。
    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        let mode = match self.mode {
            ClientFingerprintMode::Passthrough => "passthrough",
            ClientFingerprintMode::Preset => "preset",
        };
        map.insert("mode".to_owned(), Value::String(mode.to_owned()));
        if let Some(preset_id) = &self.preset_id {
            map.insert("presetId".to_owned(), Value::String(preset_id.clone()));
        }
        Value::Object(map)
    }
}

/// raw dict 同义键的统一读取入口:按给定键序取第一个非空字符串。
///
/// provider 的 `api_base_url` / `api_key` 只传单一权威键(2026-11-07 W3-7
/// 起旧别名不再读取);其他字段仍可按需传入 camel/snake 变体。空串/
/// 非字符串视为假值跳过。
pub fn raw_alias_str(raw: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| {
            raw.get(*key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        })
}

/// provider 上游基地址(只认最新键 `api_base_url`)。
pub fn provider_base_url(raw: &Map<String, Value>) -> Option<String> {
    raw_alias_str(raw, &["api_base_url"])
}

/// provider 上游密钥(只认最新键 `api_key`)。
pub fn provider_api_key(raw: &Map<String, Value>) -> Option<String> {
    raw_alias_str(raw, &["api_key"])
}

/// 网关转发所需的 provider 实体(R2 子集)。
#[derive(Debug, Clone, PartialEq)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub protocol: ProviderProtocol,
    pub enabled: bool,
    pub models: Vec<String>,
    pub headers: Map<String, Value>,
    pub timeout_seconds: f64,
    /// 非流式「等响应头」超时(可选,秒):非流式上游要把整个响应生成完
    /// 才回响应头,该阶段 ≈ 完整生成时长;复用逐读的 `timeout_seconds`
    /// (默认 120s)会把长生成整段掐断。缺省用 upstream 侧默认(600s),
    /// 显式设置(>0)则覆盖;流式路径不消费此字段。
    pub non_stream_timeout_seconds: Option<f64>,
    pub max_retries: u32,
    pub max_in_flight: Option<i64>,
    pub protocol_style: Option<String>,
    /// 20261014_01:Base URL 模式(缺省 Base;宽容解析见 [`UrlMode::parse`])。
    /// 仅 `protocol::endpoint` 消费。
    pub url_mode: UrlMode,
    pub api_key: String,
    /// 每百万 token 单价(R4.6 成本入账读 `input_per_million` /
    /// `output_per_million`)。Python 是 `dict[str, float]`。
    pub billing: Map<String, Value>,
    /// P2 每 provider 可靠性覆盖:`None` 表示沿用全局 `reliability.*` 默认。
    pub failure_threshold: Option<i64>,
    pub cooldown_ms: Option<i64>,
    /// A5 多账号凭据池;空 = 单 legacy `api_key` 形态(向后兼容)。
    pub accounts: Vec<ProviderAccount>,
    /// 20260905_02 K1:凭据选择策略(缺省 first;宽容解析,非法值回退)。
    pub credential_strategy: ProviderCredentialStrategy,
    /// 20260905_02 K1:pinned 策略的固定账号 id(仅 Pinned 消费)。
    pub pinned_account_id: Option<String>,
    /// 20260905_03 S7:每模型上下文窗口(`modelContextWindow`:
    /// {模型名: token 数};仅上下文预检消费。宽容解析只收数字值,
    /// 缺窗口 = 不设限 —— 预检宁缺勿错)。
    pub context_windows: Map<String, Value>,
    /// 20260906_01 B1:供应商级出站代理覆盖;`None` = 沿用全局
    /// `config.proxy.upstream`(与 `failure_threshold` 的 None 惯例一致)。
    pub proxy: Option<ProviderProxyOverride>,
    /// 20261016_03:供应商级客户端指纹(出站身份层);`None` = off,
    /// 存量行为零变化。注入逻辑在
    /// [`crate::protocol::client_fingerprint`]。
    pub client_fingerprint: Option<ClientFingerprintOverride>,
}

impl Provider {
    /// 从归一化 provider dict 构造。无效 id / base_url / 协议 / 字段
    /// 类型返回 Err(镜像 Python from_dict 的 ValueError 路径)。
    pub fn from_raw(raw: &Map<String, Value>) -> Result<Self, String> {
        let id = raw
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned();
        if id.is_empty() || id.contains('/') || id.contains('\\') {
            return Err("Provider id must be a non-empty path-safe value".to_owned());
        }
        // 基地址/密钥只认最新配置键 `api_base_url` / `api_key`(W3-7);
        // 旧 `base_url`/`baseUrl`/`apiKey`/`apikey` 不再兜底。
        // 20261014_01:URL 模式必须在尾斜杠剥除**之前**解析 —— full 模式
        // 原样保留(尾斜杠/query 语义交给用户,部分网关 /path/ 与 /path
        // 是不同路由);base 模式维持剥除(append_endpoint 的拼接依赖无
        // 尾斜杠)。
        let url_mode = raw_alias_str(raw, &["url_mode", "urlMode"])
            .map(|value| UrlMode::parse(&value))
            .unwrap_or_default();
        let base_url_raw = provider_base_url(raw).unwrap_or_default();
        let base_url = if url_mode.is_full() {
            // raw_alias_str 已去首尾空白;此处不再做任何 URL 运算。
            base_url_raw
        } else {
            base_url_raw.trim_end_matches('/').to_owned()
        };
        if !base_url.starts_with("http://") && !base_url.starts_with("https://") {
            return Err("Provider base_url must start with http:// or https://".to_owned());
        }
        let protocol_value = raw
            .get("protocol")
            .and_then(Value::as_str)
            .unwrap_or("openai");
        let protocol = ProviderProtocol::parse(protocol_value)?;
        // Python: str(value.get("name") or provider_id)——假值回退到 id。
        let name = match raw.get("name") {
            Some(v) if py_bool(v) => py_str_scalar(v, "name")?,
            _ => id.clone(),
        };
        let api_key = provider_api_key(raw).unwrap_or_default();
        // Python: bool(value.get("enabled", True))。
        let enabled = raw.get("enabled").map(py_bool).unwrap_or(true);
        // Python: tuple(str(x) for x in value.get("models", []))——键存在
        // 但为 null 时 for 循环抛 TypeError,镜像为 Err。
        let models = match raw.get("models") {
            None => Vec::new(),
            Some(Value::Array(items)) => items
                .iter()
                .map(|v| py_str_scalar(v, "models item"))
                .collect::<Result<Vec<_>, _>>()?,
            Some(_) => return Err("provider models must be an array".to_owned()),
        };
        // Python: {str(k): str(v) for k, v in (value.get("headers") or {}).items()}。
        let headers = match raw.get("headers").filter(|v| py_bool(v)) {
            None => Map::new(),
            Some(Value::Object(map)) => map
                .iter()
                .map(|(k, v)| {
                    py_str_scalar(v, "headers value").map(|text| (k.clone(), Value::String(text)))
                })
                .collect::<Result<Map<_, _>, _>>()?,
            Some(_) => return Err("provider headers must be an object".to_owned()),
        };
        // Python: float(value.get("timeout_seconds") or value.get("timeoutSeconds") or 120)。
        let timeout_seconds = ["timeout_seconds", "timeoutSeconds"]
            .iter()
            .find_map(|key| raw.get(*key).filter(|v| py_bool(v)))
            .map(|v| py_float(v).ok_or_else(|| format!("invalid timeout_seconds: {v}")))
            .transpose()?
            .unwrap_or(120.0);
        // Rust 新增(无 Python 镜像):非流式等响应头超时,可选;非法/非正
        // 值宽容视为未设置(缺省语义见字段文档)。camelCase 别名镜像
        // timeout_seconds 的解析风格。
        let non_stream_timeout_seconds = ["non_stream_timeout_seconds", "nonStreamTimeoutSeconds"]
            .iter()
            .find_map(|key| raw.get(*key).and_then(py_float))
            .filter(|value| *value > 0.0);
        // Python: max(0, int(a if a is not None else value.get("maxRetries", 3)))。
        // 注意:键存在但为 null 时走 maxRetries;maxRetries 键存在但为
        // null 时 int(None) 抛 TypeError(镜像为 Err)。
        let retries_raw = match raw.get("max_retries") {
            Some(v) if !v.is_null() => v.clone(),
            _ => raw
                .get("maxRetries")
                .cloned()
                .unwrap_or_else(|| Value::from(3)),
        };
        let max_retries =
            py_int(&retries_raw).ok_or_else(|| "invalid max_retries value".to_owned())?;
        let max_retries = max_retries.max(0) as u32;
        // 20261015_01 R1:供应商级 `fallback_providers` 已移除 —— 兜底统一
        // 走聚合模型(聚合组顺序即候选序;加权/粘性与管理面 UI 一等公民)。
        // 存量配置里的该键按「解析对未知字段宽容」静默忽略,写回时自然洗净。
        // Python: _optional_int —— None 或 int() 失败时为 None。
        let optional_int = |snake: &str, camel: &str| -> Option<i64> {
            raw.get(snake).or_else(|| raw.get(camel)).and_then(py_int)
        };
        // G-8:type/style 是协议分流枚举,未知值必须在配置期拒绝,不能
        // 让 `openai` 家族带着未知线格式进入转发。
        let protocol_style = match raw.get("type") {
            None => None,
            Some(Value::String(value)) if value.trim().is_empty() => None,
            Some(Value::String(value)) => {
                let value = value.trim().to_ascii_lowercase();
                if !matches!(
                    value.as_str(),
                    "openai_responses"
                        | "openai_chat_completions"
                        | "anthropic_messages"
                        | "gemini_generate_content"
                        | "gemini_stream_generate_content"
                        | "gemini_interactions"
                ) {
                    return Err(format!("Unsupported provider type: {value}"));
                }
                Some(value)
            }
            Some(_) => return Err("provider type must be a string".to_owned()),
        };
        // Python: {str(k): float(v) for k, v in (value.get("billing") or {}).items()}
        // —— 非数值键值会抛 TypeError,这里镜像为 Err。
        let billing = match raw.get("billing").filter(|v| py_bool(v)) {
            None => Map::new(),
            Some(Value::Object(map)) => map
                .iter()
                .map(|(k, v)| {
                    py_float(v)
                        .map(|value| (k.clone(), Value::from(value)))
                        .ok_or_else(|| format!("provider billing.{k} must be a number"))
                })
                .collect::<Result<Map<_, _>, _>>()?,
            Some(_) => return Err("provider billing must be an object".to_owned()),
        };
        Ok(Self {
            id,
            name,
            base_url,
            protocol,
            enabled,
            models,
            headers,
            timeout_seconds,
            non_stream_timeout_seconds,
            max_retries,
            max_in_flight: optional_int("max_in_flight", "maxInFlight"),
            protocol_style,
            url_mode,
            api_key,
            billing,
            failure_threshold: optional_int("failure_threshold", "failureThreshold"),
            cooldown_ms: optional_int("cooldown_ms", "cooldownMs"),
            accounts: parse_accounts(raw.get("accounts").filter(|v| py_bool(v))),
            // 20260905_02 K1:策略与 pinned 账号(camelCase 别名镜像
            // base_url/api_key 的解析风格)。策略非法值宽容回退 first,
            // 不走 Err;pinned 非字符串/空值视为未指定。
            credential_strategy: ["credential_strategy", "credentialStrategy"]
                .iter()
                .find_map(|key| raw.get(*key).and_then(Value::as_str))
                .map(ProviderCredentialStrategy::parse)
                .unwrap_or_default(),
            pinned_account_id: ["pinned_account_id", "pinnedAccountId"]
                .iter()
                .find_map(|key| raw.get(*key).and_then(Value::as_str))
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            // 20260905_03 S7:每模型上下文窗口(camelCase/snake 别名;
            // 只收 dict 里的数字值,其余值丢弃 —— 缺窗口即不设限)。
            context_windows: ["modelContextWindow", "model_context_window"]
                .iter()
                .find_map(|key| raw.get(*key).and_then(Value::as_object))
                .map(|map| {
                    map.iter()
                        .filter(|(_, value)| value.is_number())
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect::<Map<String, Value>>()
                })
                .unwrap_or_default(),
            // 20260906_01 B1:代理覆盖宽松解析(非法结构 → None = inherit,
            // 不走 Err;告警由 wr-infra 决策侧发出,本 crate 保持零日志依赖)。
            proxy: ProviderProxyOverride::from_value(raw.get("proxy")),
            // 20261016_03:客户端指纹宽松解析(非法结构 → None = off)。
            client_fingerprint: ClientFingerprintOverride::from_value(
                raw.get("clientFingerprint").or_else(|| raw.get("client_fingerprint")),
            ),
        })
    }

    /// openai 上游是否原生提供 Responses 端点(A2/A3 分流,R3 使用)。
    pub fn serves_openai_responses_natively(&self) -> bool {
        self.protocol == ProviderProtocol::OpenAi
            && self
                .protocol_style
                .as_deref()
                .map(str::to_ascii_lowercase)
                .as_deref()
                == Some("openai_responses")
    }

    /// 供应商的**有效上游线格式**(协议家族 + 线格式合并;与读取侧
    /// wr-infra `effective_provider_protocol` 同一口径):openai 家族按
    /// `type` 细分 Responses / Chat Completions,其余家族原样。网关在
    /// 请求落库时把该值写进 payload 键 `provider_protocol`,调用日志的
    /// 转换链路据此取**请求时刻**的事实;读取侧再按当前配置现算会随
    /// 配置漂移(改供应商 type,历史行链路全部跟着变,2026-10-14)。
    pub fn effective_wire_protocol(&self) -> &'static str {
        let style = self
            .protocol_style
            .as_deref()
            .map(|value| value.trim().to_ascii_lowercase());
        match style.as_deref() {
            Some("openai_responses") => "openai_responses",
            Some("openai_chat_completions") => "openai",
            _ => self.protocol.as_str(),
        }
    }

    /// 实体序列化(镜像 Python `Provider.to_dict(include_secret)`,
    /// R5 管理面 REST /api/v1/providers 的响应形状来源)。
    ///
    /// 键序照 Python dict 字面量:id/name/base_url/api_key/protocol/enabled/
    /// models/headers/timeout_seconds/max_retries/billing/
    /// failure_threshold/cooldown_ms/max_in_flight,再按需 accounts。
    /// `include_secret=false` 时非空 api_key 掩码为 `"***"`(空串保持空串)。
    /// Python 的 model_metadata/account/icon 块 Rust 子集未承载,不产出。
    pub fn to_dict(&self, include_secret: bool) -> Value {
        let masked = if include_secret || self.api_key.is_empty() {
            self.api_key.clone()
        } else {
            "***".to_owned()
        };
        let mut m = Map::new();
        m.insert("id".to_owned(), Value::String(self.id.clone()));
        m.insert("name".to_owned(), Value::String(self.name.clone()));
        m.insert("base_url".to_owned(), Value::String(self.base_url.clone()));
        m.insert("api_key".to_owned(), Value::String(masked));
        m.insert(
            "protocol".to_owned(),
            Value::String(self.protocol.as_str().to_owned()),
        );
        m.insert("enabled".to_owned(), Value::Bool(self.enabled));
        m.insert(
            "models".to_owned(),
            Value::Array(
                self.models
                    .iter()
                    .cloned()
                    .map(Value::String)
                    .collect::<Vec<_>>(),
            ),
        );
        m.insert("headers".to_owned(), Value::Object(self.headers.clone()));
        m.insert(
            "timeout_seconds".to_owned(),
            Value::from(self.timeout_seconds),
        );
        // Rust 新增键:仅在显式设置时出站(None = 沿用 upstream 默认,
        // 与 proxy 覆盖的省略惯例一致)。
        if let Some(value) = self.non_stream_timeout_seconds {
            m.insert("non_stream_timeout_seconds".to_owned(), Value::from(value));
        }
        m.insert("max_retries".to_owned(), Value::from(self.max_retries));
        m.insert("billing".to_owned(), Value::Object(self.billing.clone()));
        m.insert(
            "failure_threshold".to_owned(),
            self.failure_threshold
                .map(Value::from)
                .unwrap_or(Value::Null),
        );
        m.insert(
            "cooldown_ms".to_owned(),
            self.cooldown_ms.map(Value::from).unwrap_or(Value::Null),
        );
        m.insert(
            "max_in_flight".to_owned(),
            self.max_in_flight.map(Value::from).unwrap_or(Value::Null),
        );
        if !self.accounts.is_empty() {
            m.insert(
                "accounts".to_owned(),
                Value::Array(
                    self.accounts
                        .iter()
                        .map(|a| a.to_dict(include_secret))
                        .collect::<Vec<_>>(),
                ),
            );
        }
        // 20260906_01 B1:代理覆盖仅在显式设置时出站(None = inherit 省略)。
        if let Some(proxy) = &self.proxy {
            m.insert("proxy".to_owned(), proxy.to_value());
        }
        // 20261016_03:客户端指纹仅在显式设置时出站(None = off 省略)。
        if let Some(fingerprint) = &self.client_fingerprint {
            m.insert("clientFingerprint".to_owned(), fingerprint.to_value());
        }
        Value::Object(m)
    }
}

/// A5 凭据池条目(镜像 `domain/provider/accounts.py::ProviderAccount`)。
///
/// `id` 是 `::cred:` 选择器用的凭据 slug;配置里没写 id 时由
/// `name`/`label`/`cred_{index}` 兜底生成(Python `_parse_accounts`)。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProviderAccount {
    pub id: String,
    pub label: String,
    pub api_key: String,
    pub enabled: bool,
    pub priority: i64,
    pub weight: i64,
    pub account_config: Map<String, Value>,
}

impl ProviderAccount {
    /// `from_dict`:非对象或缺 id → Err(镜像 Python 的 ValueError)。
    pub fn from_dict(value: &Value) -> Result<Self, String> {
        let Some(raw) = value.as_object() else {
            return Err("ProviderAccount.from_dict expects a dict".to_owned());
        };
        let text = |keys: &[&str]| -> String {
            keys.iter()
                .find_map(|key| raw.get(*key).filter(|v| py_bool(v)))
                .map(|v| py_str(v).unwrap_or_default())
                .unwrap_or_default()
                .trim()
                .to_owned()
        };
        let id = text(&["id", "credentialId"]);
        if id.is_empty() {
            return Err("ProviderAccount.id is required".to_owned());
        }
        let number = |keys: &[&str], default: i64| -> i64 {
            keys.iter()
                .find_map(|key| raw.get(*key).filter(|v| py_bool(v)))
                .and_then(py_int)
                .unwrap_or(default)
        };
        let account_config = match raw
            .get("account_config")
            .or_else(|| raw.get("accountConfig"))
            .filter(|v| py_bool(v))
        {
            Some(Value::Object(map)) => map.clone(),
            _ => Map::new(),
        };
        Ok(Self {
            id,
            label: text(&["label", "credentialLabel"]),
            api_key: text(&["api_key"]),
            enabled: raw.get("enabled").map(py_bool).unwrap_or(true),
            priority: number(&["priority"], 0),
            weight: number(&["weight"], 1),
            account_config,
        })
    }

    /// 列表/详情出站形态:`include_secret=false` 时密钥掩成 `***`
    /// (无密钥仍是空串)—— C8 脱敏的实体侧保证。
    pub fn to_dict(&self, include_secret: bool) -> Value {
        let mut map = Map::new();
        map.insert("id".to_owned(), Value::String(self.id.clone()));
        map.insert("label".to_owned(), Value::String(self.label.clone()));
        let masked = if include_secret || self.api_key.is_empty() {
            self.api_key.clone()
        } else {
            "***".to_owned()
        };
        map.insert("api_key".to_owned(), Value::String(masked));
        map.insert("enabled".to_owned(), Value::Bool(self.enabled));
        map.insert("priority".to_owned(), Value::from(self.priority));
        map.insert("weight".to_owned(), Value::from(self.weight));
        if !self.account_config.is_empty() {
            map.insert(
                "account_config".to_owned(),
                Value::Object(self.account_config.clone()),
            );
        }
        Value::Object(map)
    }
}

/// Python `Provider._parse_accounts`:只吃 list/tuple,非对象项跳过,
/// 缺 id 的裸凭据 dict 补一个稳定 slug,解析失败的项静默跳过。
fn parse_accounts(raw: Option<&Value>) -> Vec<ProviderAccount> {
    let Some(Value::Array(items)) = raw else {
        return Vec::new();
    };
    let mut accounts = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let Some(map) = item.as_object() else {
            continue;
        };
        let has_id = ["id", "credentialId"]
            .iter()
            .any(|key| map.get(*key).is_some_and(py_bool));
        let normalized = if has_id {
            item.clone()
        } else {
            let fallback = ["name", "label"]
                .iter()
                .find_map(|key| map.get(*key).filter(|v| py_bool(v)))
                .map(|v| py_str(v).unwrap_or_default())
                .filter(|text| !text.trim().is_empty())
                .unwrap_or_else(|| format!("cred_{index}"));
            let mut patched = map.clone();
            patched.insert("id".to_owned(), Value::String(fallback.trim().to_owned()));
            Value::Object(patched)
        };
        if let Ok(account) = ProviderAccount::from_dict(&normalized) {
            accounts.push(account);
        }
    }
    accounts
}

#[cfg(test)]
mod tests;
