//! 流式控制闸门与已提交后断联终态(自原流式管线模块纯搬移)。

use super::*;

/// 生成式续写的能力门(P2-5)。
///
/// 续写不是传输恢复:只在同协议、上游声明支持、无 thinking、无有状态
/// 引用且确有已提交正文时允许。已尝试过一次则不再续写。
pub(super) fn continuation_gate<'a>(
    protocol: &str,
    provider: &Provider,
    payload: &Value,
    already_attempted: bool,
) -> Result<(), &'a str> {
    if already_attempted {
        return Err("continuation_already_attempted");
    }
    if protocol != wr_domain::provider::client_protocol::ANTHROPIC
        || provider.protocol != wr_domain::provider::ProviderProtocol::Anthropic
    {
        return Err("continuation_protocol_mismatch");
    }
    if !wr_domain::semantic::stream::supports_prefill_continuation(provider.protocol) {
        return Err("continuation_not_supported_by_upstream");
    }
    if wr_domain::semantic::anthropic::has_thinking(payload) {
        return Err("continuation_with_thinking");
    }
    if wr_domain::gateway::has_stateful_reference(payload) {
        return Err("continuation_with_stateful_reference");
    }
    Ok(())
}

/// 已提交后下游发送失败(客户端断开)的统一 499 终态记录。
#[allow(clippy::too_many_arguments)]
pub(super) fn record_stream_client_gone(
    deps: &GatewayDeps,
    snapshot: &Arc<ConfigSnapshot>,
    request_id: &str,
    protocol: &str,
    model: &str,
    identity: &GatewayIdentity,
    provider: &Provider,
    account_id: &Option<String>,
    started: Instant,
    attempts: &AttemptTrail,
    payload: &Value,
    headers: &[(String, String)],
    first_byte_ms: Option<f64>,
    response_chunks: &[String],
    capture_enabled: bool,
    capture_budget: usize,
    logs_enabled: bool,
) {
    let mut gone_extra = serde_json::Map::new();
    gone_extra.insert("error".to_owned(), json!("client_disconnected"));
    deps.record_log_extra(
        snapshot,
        request_id,
        &RequestLogEvent {
            protocol: protocol.to_owned(),
            model: model.to_owned(),
            status: 499,
            stream: true,
            duration_ms: elapsed_ms(started),
            provider_attempts: trail_provider_ids(attempts),
            retry_attempts: trail_values(attempts),
            end_reason: "client_disconnected",
            provider: Some(provider.id.clone()),
            account: account_id.clone(),
            api_key_id: Some(identity.key_id.clone()),
            first_byte_ms,
            error: Some("client_disconnected".to_owned()),
        },
        RequestLogExtra::default()
            .with_identity(&identity.key_name)
            .with_preview(logs_enabled, payload)
            .with_extra(gone_extra)
            .with_capture(
                capture_enabled,
                capture_budget,
                Some(payload),
                Some(&response_chunks.concat()),
                None,
            )
            .with_request_headers(headers),
    );
}
