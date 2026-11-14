//! 上游流能力适配器注册表。

use crate::provider::ProviderProtocol;
use crate::semantic::adapter::OutboundStreamAdapter;

pub struct OpenAiStreamAdapter;
pub struct AnthropicStreamAdapter;
pub struct GeminiStreamAdapter;

impl OutboundStreamAdapter for OpenAiStreamAdapter {
    fn protocol(&self) -> ProviderProtocol {
        ProviderProtocol::OpenAi
    }
    fn capabilities(&self) -> crate::semantic::adapter::AdapterCapabilities {
        crate::semantic::adapter::STREAM_CHAT
    }
}
impl OutboundStreamAdapter for AnthropicStreamAdapter {
    fn protocol(&self) -> ProviderProtocol {
        ProviderProtocol::Anthropic
    }
    fn capabilities(&self) -> crate::semantic::adapter::AdapterCapabilities {
        // 阶段五 C-3:Anthropic 上游支持 assistant prefill 续写。
        crate::semantic::adapter::ANTHROPIC_STREAM
    }
}
impl OutboundStreamAdapter for GeminiStreamAdapter {
    fn protocol(&self) -> ProviderProtocol {
        ProviderProtocol::Gemini
    }
    fn capabilities(&self) -> crate::semantic::adapter::AdapterCapabilities {
        crate::semantic::adapter::STREAM_CHAT
    }
}

pub fn outbound_stream(protocol: ProviderProtocol) -> &'static dyn OutboundStreamAdapter {
    match protocol {
        ProviderProtocol::OpenAi => &OpenAiStreamAdapter,
        ProviderProtocol::Anthropic => &AnthropicStreamAdapter,
        ProviderProtocol::Gemini => &GeminiStreamAdapter,
    }
}

/// 阶段五 C-3:该上游协议是否支持「assistant prefill 续写」(能力位查询)。
///
/// 网关据此决定断流后能不能续写:能力位为 false 的上游发续写请求等于
/// **假续写**(模型把 prefill 当新消息重新回答)→ 不发。
pub fn supports_prefill_continuation(protocol: ProviderProtocol) -> bool {
    outbound_stream(protocol)
        .capabilities()
        .supports_prefill_continuation
}
