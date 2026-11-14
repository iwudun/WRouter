//! 流式响应缓存命中重放(自原流式管线模块纯搬移)。

use super::delivery::{Delivery, EmissionBatch};
use super::*;

/// 精确键命中时逐帧重放缓存响应并落调用日志。返回 `true` 表示命中已处理,
/// 管线应直接结束;`false` 表示未命中,继续请求上游。
#[allow(clippy::too_many_arguments)]
pub(super) async fn replay_cached_response(
    deps: &Arc<GatewayDeps>,
    snapshot: &Arc<ConfigSnapshot>,
    protocol: &str,
    payload: &Value,
    model: &str,
    request_id: &str,
    identity: &GatewayIdentity,
    headers: &[(String, String)],
    delivery: &mut Delivery<'_>,
    started: Instant,
    attempts: &AttemptTrail,
    savings_extra: &Map<String, Value>,
    cache_key: Option<&str>,
) -> Result<bool, StreamPipelineError> {
    if let Some(cache_key) = cache_key
        && let Some(hit) =
            response_cache_lookup(deps, cache_key, &snapshot.config.savings.response_cache).await
    {
        let usage = wr_domain::usage::normalize_usage(hit.payload.get("usage"));
        let cost_saved = hit.cost_usd;
        record_cache_hit_savings(deps, request_id, &usage);
        let mut hit_extra = savings_extra.clone();
        hit_extra.insert("cacheSource".to_owned(), json!("hit"));
        hit_extra.insert("costSavedUsd".to_owned(), json!(cost_saved));
        let frames = match wr_domain::semantic::synthesis::synthesize_client_response_frames(
            protocol,
            &hit.payload,
        ) {
            Ok(frames) => frames,
            Err(error) => {
                // 缓存条目内部损坏/旧形状:不能把整段 JSON 塞进一个 data 帧
                // 冒充合法流;按 miss 继续请求上游(本请求会覆盖该条目)。
                tracing::warn!(
                    target: "wrouter.gateway.cache",
                    protocol = %protocol,
                    error = %error.message,
                    "cache_replay_invalid; falling through to upstream"
                );
                Vec::new()
            }
        };
        if !frames.is_empty() {
            let age_ms = session_affinity::now_ms().saturating_sub(hit.created_at_ms);
            delivery.set_headers(vec![
                ("x-wr-cache".to_owned(), "hit".to_owned()),
                ("x-wr-cache-age-ms".to_owned(), age_ms.to_string()),
                ("x-wr-cost-saved".to_owned(), format!("{cost_saved:.6}")),
            ]);
            for frame in frames {
                if delivery
                    .emit(EmissionBatch::from_frames(vec![frame], true))
                    .await
                    .is_err()
                {
                    return Err(StreamPipelineError { client_gone: true });
                }
            }
            deps.record_log_extra(
                &snapshot,
                request_id,
                &RequestLogEvent {
                    protocol: protocol.to_owned(),
                    model: model.to_owned(),
                    status: 200,
                    stream: true,
                    duration_ms: elapsed_ms(started),
                    provider_attempts: trail_provider_ids(&attempts),
                    retry_attempts: trail_values(&attempts),
                    end_reason: "cache_hit",
                    provider: None,
                    account: None,
                    api_key_id: Some(identity.key_id.clone()),
                    first_byte_ms: None,
                    error: None,
                },
                RequestLogExtra::default()
                    .with_identity(&identity.key_name)
                    .with_preview(RequestLogRecorder::call_logs_enabled(&snapshot), payload)
                    .with_usage(usage, 0.0)
                    .with_request_meta(
                        Some(&client_label(headers)),
                        Some("POST"),
                        Some(protocol),
                        Some(&iso_now()),
                    )
                    .with_extra(hit_extra),
            );
            return Ok(true);
        }
    }
    Ok(false)
}
