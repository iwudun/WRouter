//! 模型指纹探针执行(方案 20261030_01 R12;2026-11-01 自 `model_fingerprint.rs`
//! 按职责拆出 —— 该文件此前 1700 行混了探针执行 / 库装载 / RPC / 导入导出)。
//!
//! 职责:把一组 [`ProbeSpec`] 变成上游真实回答 —— 请求体按源协议组装、HTTP
//! 发送、响应解析与**缺口语义**(拒答/截断计缺口不计样本);外加探针用的
//! 供应商上下文装载与套件裁剪。
//!
//! **关键约束**(与 RPC 层共同遵守):
//! - 探针请求**不经过网关转发管线**:不计入路由规则、不写调用日志、不进费用
//!   账本(决策点 4),与连通性探针同款 —— 否则会污染用户的路由统计;
//! - 探针 `max_tokens` 放大到 [`PROBE_FINGERPRINT_MAX_TOKENS`]:300+ 个整数
//!   约需 1200–2000 token,256 会截断;
//! - 拒答/截断(`finish_reason` 为 `length`/`content_filter`/`refusal`)**计为
//!   检测缺口**,不计入样本,并在结果里显式报告缺口数。

use serde_json::{Map, Value, json};

use wr_app::ports::SecretStore;
use wr_domain::fingerprint::{ProbeSpec, ProbeTier, wrouter_v1_suite};
use wr_domain::provider::{Provider, provider_base_url};
use wr_infra::sqlite_config::SqliteConfigSource;

use super::connectivity_probe::{probe_client, probe_proxy_decision, response_output_text};
use super::{AdminState, config};

/// 指纹探针的输出 token 上限。
///
/// 与连通性探针的 `PROBE_MAX_TOKENS`(256)分开:一次探针要 292–332 个整数,
/// 约需 1200–2000 token,256 会把回答截断 → 样本无效。
pub const PROBE_FINGERPRINT_MAX_TOKENS: i64 = 4096;

/// 单次检测允许的最大探针条数(完整档 36 条)。
const MAX_PROBE_COUNT: usize = 36;

// ---------------------------------------------------------------------------
// 请求体构造(按源协议形态)
// ---------------------------------------------------------------------------

/// 构造指纹探针的请求体(源协议形态)。
///
/// - openai chat:system 前缀作为一条 `system` 消息;user 前缀并入 user 消息;
/// - anthropic:`system` 是**顶层字段**(不是消息);
/// - openai_responses:`instructions` 承载 system 前缀,`input` 承载 user 内容。
pub fn fingerprint_probe_body(source_protocol: &str, model: &str, spec: &ProbeSpec) -> Value {
    // user 消息的最终文本由 `ProbeSpec::user_content()` 统一组装。
    let user_content = spec.user_content();
    match source_protocol {
        "anthropic" => {
            let mut body = json!({
                "model": model,
                "max_tokens": PROBE_FINGERPRINT_MAX_TOKENS,
                "messages": [{"role": "user", "content": user_content}],
            });
            if !spec.system.is_empty()
                && let Some(map) = body.as_object_mut()
            {
                map.insert("system".to_owned(), Value::String(spec.system.clone()));
            }
            body
        }
        "openai_responses" => {
            let mut body = json!({
                "model": model,
                "input": user_content,
                "max_output_tokens": PROBE_FINGERPRINT_MAX_TOKENS,
                "stream": false,
            });
            if !spec.system.is_empty()
                && let Some(map) = body.as_object_mut()
            {
                map.insert("instructions".to_owned(), Value::String(spec.system.clone()));
            }
            body
        }
        _ => {
            let mut messages: Vec<Value> = Vec::new();
            if !spec.system.is_empty() {
                messages.push(json!({"role": "system", "content": spec.system}));
            }
            messages.push(json!({"role": "user", "content": user_content}));
            json!({
                "model": model,
                "messages": messages,
                "max_tokens": PROBE_FINGERPRINT_MAX_TOKENS,
            })
        }
    }
}

/// 从响应里取结束原因(用于把截断/拒答计为**缺口**)。
pub fn response_finish_reason(source_protocol: &str, data: &Value) -> Option<String> {
    if source_protocol == "anthropic" {
        return data.get("stop_reason").and_then(Value::as_str).map(str::to_owned);
    }
    if source_protocol == "openai_responses" {
        return data
            .get("status")
            .and_then(Value::as_str)
            .filter(|status| *status != "completed")
            .map(str::to_owned);
    }
    data.get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("finish_reason"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// 结束原因是否算「缺口」(截断/拒答/内容过滤)。
pub fn finish_reason_is_gap(reason: &str) -> bool {
    matches!(
        reason,
        "length" | "content_filter" | "refusal" | "max_tokens" | "incomplete"
    )
}

// ---------------------------------------------------------------------------
// 探针执行
// ---------------------------------------------------------------------------

/// 一条探针的完整结果(带回规格信息)。
pub struct ProbeRun {
    /// 探针标识。
    pub challenge_id: String,
    /// 采集环境。
    pub condition_id: String,
    /// 期望数字数。
    pub expected_count: usize,
    /// 执行结果。
    pub outcome: ProbeOutcome,
}

/// 一条探针的执行结果。
pub struct ProbeOutcome {
    /// 文本(拒答/失败时为空)。
    pub text: String,
    /// 是否计入样本。
    pub accepted: bool,
    /// 缺口原因。
    pub reason: Option<String>,
}

/// 对一个模型跑一组探针。
pub async fn run_fingerprint_probes(
    provider: &Provider,
    source_protocol: &str,
    model: &str,
    specs: &[ProbeSpec],
    proxy_config: Option<&Map<String, Value>>,
) -> Vec<ProbeRun> {
    let mut outcomes = Vec::with_capacity(specs.len());
    for spec in specs {
        let outcome = run_one_probe(provider, source_protocol, model, spec, proxy_config).await;
        outcomes.push(ProbeRun {
            challenge_id: spec.challenge_id.clone(),
            condition_id: spec.condition.clone(),
            expected_count: spec.expected_count,
            outcome,
        });
    }
    outcomes
}

/// 跑一条探针。
async fn run_one_probe(
    provider: &Provider,
    source_protocol: &str,
    model: &str,
    spec: &ProbeSpec,
    proxy_config: Option<&Map<String, Value>>,
) -> ProbeOutcome {
    let url = wr_domain::protocol::endpoint_with_query(
        source_protocol,
        provider,
        model,
        false,
        wr_domain::protocol::client_fingerprint::resolve_fingerprint_query(provider, None).as_deref(),
    );
    let body = match wr_domain::protocol::upstream_payload(
        source_protocol,
        &fingerprint_probe_body(source_protocol, model, spec),
        provider,
    ) {
        Ok(body) => body,
        Err(failure) => {
            return ProbeOutcome {
                text: String::new(),
                accepted: false,
                reason: Some(format!("payload:{}", failure.message())),
            };
        }
    };
    let headers = wr_domain::protocol::upstream_headers_with_fingerprint(provider, None);
    let client = probe_client(&probe_proxy_decision(proxy_config, &provider.base_url));
    let mut request = client.post(&url);
    for (key, value) in &headers {
        if let Some(text) = value.as_str() {
            request = request.header(key.as_str(), text);
        }
    }
    let body_bytes = serde_json::to_vec(&body).unwrap_or_else(|_| b"{}".to_vec());
    request = request.body(body_bytes);

    let response = match request.send().await {
        Ok(response) => response,
        Err(error) => {
            return ProbeOutcome {
                text: String::new(),
                accepted: false,
                reason: Some(format!("unreachable:{}", wr_infra::upstream::reqwest_error_message(&error))),
            };
        }
    };
    let status = i64::from(response.status().as_u16());
    // HTTP 错误先于 JSON 解析判定:4xx/5xx 一律是缺口(含上游返回非 JSON
    // 错误页的情形),不能被「解析失败」吞掉诊断分类。
    if status >= 400 {
        return ProbeOutcome {
            text: String::new(),
            accepted: false,
            reason: Some(format!("http:{status}")),
        };
    }
    let raw = response.text().await.unwrap_or_default();
    let parsed: Result<Value, _> = serde_json::from_str(&raw);
    let data = match parsed {
        Ok(data) => data,
        Err(error) => {
            return ProbeOutcome {
                text: String::new(),
                accepted: false,
                reason: Some(format!("parse:{}", error)),
            };
        }
    };
    if let Some(reason) = response_finish_reason(source_protocol, &data)
        && finish_reason_is_gap(&reason)
    {
        return ProbeOutcome {
            text: String::new(),
            accepted: false,
            reason: Some(reason),
        };
    }
    let text = response_output_text(source_protocol, &data);
    if text.trim().is_empty() {
        return ProbeOutcome {
            text,
            accepted: false,
            reason: Some("empty".to_owned()),
        };
    }
    ProbeOutcome {
        text,
        accepted: true,
        reason: None,
    }
}

// ---------------------------------------------------------------------------
// 供应商装载
// ---------------------------------------------------------------------------

/// 供应商的探针上下文。
pub(super) struct ProviderContext {
    pub provider: Provider,
    /// 源协议(openai / anthropic / openai_responses)。
    pub source_protocol: String,
}

/// 取供应商实体与源协议。
///
/// **gemini 暂不支持**(方案 §6 风险):`frontend_protocol_family` 只映射
/// openai / anthropic 两族,指纹探针沿用同款口径 —— 不静默失败,直接报错。
pub(super) async fn load_provider_context(
    state: &AdminState,
    provider_id: &str,
) -> Result<ProviderContext, String> {
    let source = SqliteConfigSource::new(&state.deps.settings.data_dir);
    let mut config = source
        .load_app_config()
        .map_err(|error| format!("load config failed: {error}"))?;
    config::hydrate_account_secrets(state, &mut config).await;
    let record = config
        .providers
        .iter()
        .find(|provider| provider.raw.get("id").and_then(Value::as_str) == Some(provider_id))
        .ok_or_else(|| format!("provider '{provider_id}' not found"))?;
    let raw = record.raw.clone();
    let provider = Provider::from_raw(&raw).map_err(|error| format!("provider invalid: {error}"))?;
    if provider.base_url.trim().is_empty() {
        return Err(format!("provider '{provider_id}' has no base URL"));
    }
    let source_protocol = match provider.effective_wire_protocol() {
        "anthropic" => "anthropic".to_owned(),
        "openai_responses" => "openai_responses".to_owned(),
        "gemini" => {
            return Err("Gemini 协议暂不支持指纹检测(仅支持 OpenAI / Anthropic 系)".to_owned());
        }
        _ => "openai".to_owned(),
    };
    let _ = provider_base_url(&raw);
    Ok(ProviderContext {
        provider,
        source_protocol,
    })
}

/// 取供应商密钥(与转发同入口)。
pub(super) async fn provider_api_key(state: &AdminState, provider_id: &str) -> Result<String, String> {
    let secret = state.store.provider_secret(provider_id)
        .await
        .map_err(|error| error.to_string())?;
    Ok(secret)
}

// ---------------------------------------------------------------------------
// 探针套件选择
// ---------------------------------------------------------------------------

/// 按档位取探针(用户自建库固定 `wrouter-v1` 套件)。
pub(super) fn specs_for(tier: ProbeTier, seed: &str) -> Vec<ProbeSpec> {
    let count = tier.probe_count().min(MAX_PROBE_COUNT);
    wrouter_v1_suite(seed, count)
}
