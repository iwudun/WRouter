//! 出站执行计划与 attempt 上下文(P2-3)。
//!
//! 规划在**出站前**固定源 profile、目标协议、正文模式、保真等级与
//! 预期响应表示;收到响应头后只能通过
//! [`wr_domain::protocol::contract::select_response_executor`] 选择已规划的
//! 执行器。未知客户端协议、未知组合与未规划表示都显式失败,不回退透传。

use wr_domain::protocol::contract::{
    ContractViolation, ExchangePlan, RequestProfile, ResponseExecutor, plan_exchange,
    select_response_executor,
};
use wr_domain::provider::{Provider, ProviderProtocol};
use wr_domain::semantic::SourceProtocol;

/// 一次上游交换的出站计划(每个候选/目标构造一次)。
#[derive(Debug, Clone)]
pub(super) struct PreparedExchange {
    pub source: SourceProtocol,
    pub target: ProviderProtocol,
    pub plan: ExchangePlan,
}

impl PreparedExchange {
    pub(super) fn new(
        client_protocol: &str,
        stream: bool,
        provider: &Provider,
    ) -> Result<Self, ContractViolation> {
        let source = SourceProtocol::parse(client_protocol).ok_or_else(|| {
            ContractViolation::UnknownSourceProtocol {
                value: client_protocol.to_owned(),
            }
        })?;
        let profile = RequestProfile::for_source(source, stream)?;
        let plan = plan_exchange(
            profile,
            provider.protocol,
            provider.serves_openai_responses_natively(),
        )?;
        Ok(Self {
            source,
            target: provider.protocol,
            plan,
        })
    }

    pub(super) fn select_executor(
        &self,
        content_type: Option<&str>,
    ) -> Result<ResponseExecutor, ContractViolation> {
        select_response_executor(&self.plan, content_type)
    }
}

/// 一次 attempt 的身份与执行上下文。不包含原始凭据/密钥。
#[derive(Debug, Clone, Copy)]
pub(super) struct AttemptContext<'a> {
    pub request_id: &'a str,
    pub attempt: u32,
    pub provider: &'a Provider,
    pub account: Option<&'a str>,
    pub model: &'a str,
    pub exchange: &'a PreparedExchange,
}

impl<'a> AttemptContext<'a> {
    pub(super) fn new(
        request_id: &'a str,
        attempt: u32,
        provider: &'a Provider,
        account: Option<&'a str>,
        model: &'a str,
        exchange: &'a PreparedExchange,
    ) -> Self {
        Self {
            request_id,
            attempt,
            provider,
            account,
            model,
            exchange,
        }
    }

    /// 低基数诊断标签(不写请求体/凭据)。
    pub(super) fn trace_fields(&self) -> String {
        format!(
            "request_id={} attempt={} provider={} account={} model={} source={} operation={} target={} request_mode={} response_mode={} fidelity={}",
            self.request_id,
            self.attempt,
            self.provider.id,
            self.account.unwrap_or("-"),
            self.model,
            self.exchange.source.as_str(),
            self.exchange.plan.profile.operation.as_str(),
            self.exchange.target.as_str(),
            self.exchange.plan.request.mode.as_str(),
            self.exchange.plan.response.mode.as_str(),
            match self.exchange.plan.fidelity() {
                wr_domain::protocol::contract::FidelityLevel::ByteExact => "byte_exact",
                wr_domain::protocol::contract::FidelityLevel::StructureExact => "structure_exact",
                wr_domain::protocol::contract::FidelityLevel::SemanticEquivalent => {
                    "semantic_equivalent"
                }
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn provider(protocol: &str) -> Provider {
        let mut raw = serde_json::Map::new();
        raw.insert("id".to_owned(), json!("p"));
        raw.insert("api_base_url".to_owned(), json!("https://upstream.example/v1"));
        raw.insert("protocol".to_owned(), json!(protocol));
        Provider::from_raw(&raw).unwrap()
    }

    #[test]
    fn unknown_client_protocol_is_rejected_before_outbound() {
        let error = PreparedExchange::new("mystery", true, &provider("openai")).unwrap_err();
        assert!(matches!(
            error,
            ContractViolation::UnknownSourceProtocol { .. }
        ));
    }

    #[test]
    fn embeddings_stream_is_rejected_before_outbound() {
        let error =
            PreparedExchange::new("openai_embeddings", true, &provider("openai")).unwrap_err();
        assert!(matches!(error, ContractViolation::InvalidStreaming { .. }));
    }

    #[test]
    fn cross_protocol_and_native_plans_are_explicit() {
        let native = PreparedExchange::new("openai", true, &provider("openai")).unwrap();
        assert!(native.plan.is_byte_exact());
        assert_eq!(
            native.select_executor(Some("text/event-stream")).unwrap(),
            ResponseExecutor::Sse
        );

        let translated = PreparedExchange::new("anthropic", true, &provider("openai")).unwrap();
        assert!(!translated.plan.is_byte_exact());
        assert_eq!(
            translated
                .select_executor(Some("application/x-ndjson"))
                .unwrap(),
            ResponseExecutor::Ndjson
        );
        assert!(translated.select_executor(Some("text/plain")).is_err());
    }
}
