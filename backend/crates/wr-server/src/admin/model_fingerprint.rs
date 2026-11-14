//! 模型指纹检测管理面(方案 20261030_01 R12)。
//!
//! 四个管理 RPC:
//! - `detectModelFingerprint`:对指定供应商的模型发起指纹检测,给出归因结论;
//! - `listModelFingerprints`:列出用户自建库条目;
//! - `enrollModelFingerprint`:采集并入库(新增指纹);
//! - `deleteModelFingerprint`:删除用户自建条目。
//!
//! 2026-11-01 按职责拆分(此前 1700 行单文件混四职责):
//! - 探针执行(请求体构造 / HTTP 发送 / 缺口语义 / 供应商与套件装载)→
//!   [`super::model_fingerprint_probe`];
//! - 导入导出(ModelTrace 兼容文档 / 无损与重建两条导入路径)→
//!   [`super::model_fingerprint_transfer`];
//! - 本文件只留:用户库装载、四个库 RPC handler、X7 查重判定。
//!
//! **关键约束**:
//! - 探针请求**不经过网关转发管线**:不计入路由规则、不写调用日志、不进费用
//!   账本(决策点 4),与连通性探针同款 —— 否则会污染用户的路由统计;
//! - **一次检测只能对应一套探针**(方案 X9):跨套件比对由后端硬拒绝。
//!
//! **两层库**(方案 §3.2):
//! - L2 全局候选库:用户自建;
//! - L3 供应商锚点:用户在指定供应商下实测(通道偏移在同供应商内抵消)。

use std::time::Instant;

use serde_json::{Map, Value, json};

use wr_app::ports::FingerprintStore;
use wr_domain::fingerprint::bank::{BankSample, build_bank, find_duplicate, model_centers};
use wr_domain::fingerprint::{
    AnalysisSample, AttributionResult, FingerprintBank, ProbeSuite, ProbeTier, analyze_with_group,
};

use super::model_fingerprint_probe::{
    load_provider_context, provider_api_key, run_fingerprint_probes, specs_for,
};
use super::{AdminState, config};

// ---------------------------------------------------------------------------
// 用户库装载与建库
// ---------------------------------------------------------------------------

/// 从存储条目构造建库样本。
pub(super) fn entries_to_samples(entries: &[Value], provider_id: Option<&str>) -> Vec<BankSample> {
    let mut samples = Vec::new();
    for entry in entries {
        let model_name = entry
            .get("modelName")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let provider = entry
            .get("providerId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        // 只取指定供应商的条目(锚点库按供应商分组建)。
        if let Some(wanted) = provider_id
            && provider.as_deref() != Some(wanted)
        {
            continue;
        }
        let family = entry.get("family").and_then(Value::as_str).map(str::to_owned);
        let family_name = entry.get("familyName").and_then(Value::as_str).map(str::to_owned);
        let probe_suite = entry
            .get("probeSuite")
            .and_then(Value::as_str)
            .and_then(ProbeSuite::parse);
        let Some(items) = entry.get("samples").and_then(Value::as_array) else {
            continue;
        };
        for sample in items {
            samples.push(BankSample {
                source: model_name.clone(),
                text: sample
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                expected_count: sample
                    .get("expectedCount")
                    .and_then(Value::as_u64)
                    .unwrap_or_default() as usize,
                condition_id: sample
                    .get("conditionId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                nuisance_condition_id: sample
                    .get("nuisanceConditionId")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                challenge_id: sample
                    .get("challengeId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                provider: provider.clone(),
                family: family.clone(),
                family_name: family_name.clone(),
                probe_suite,
            });
        }
    }
    samples
}

/// 装载用户自建库(可选按供应商过滤)。
async fn load_user_library(
    state: &AdminState,
    provider_id: Option<&str>,
) -> Result<Option<FingerprintBank>, String> {
    let entries = state
        .store
        .all_entries()
        .await
        .map_err(|error| error.to_string())?;
    let samples = entries_to_samples(&entries, provider_id);
    let model_count = {
        let mut ids: Vec<&str> = samples.iter().map(|sample| sample.source.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        ids.len()
    };
    // 闭集归因至少要 2 个候选模型,否则库不可用。
    if model_count < 2 {
        return Ok(None);
    }
    match build_bank(&samples) {
        Ok(bank) => Ok(Some(bank)),
        Err(error) => Err(format!("build fingerprint bank failed: {error}")),
    }
}

/// 方案 X7:新条目与既有条目的中心距离过近 → 给出拒绝理由(无重复则 `None`)。
pub(super) fn duplicate_model_reason(bank: &FingerprintBank, model_name: &str) -> Option<String> {
    let centers = model_centers(&bank.robust);
    let new_center = centers
        .iter()
        .find(|(id, _)| id == model_name)
        .map(|(_, center)| center.clone())?;
    let others: Vec<(String, Vec<f64>)> = centers
        .iter()
        .filter(|(id, _)| id != model_name)
        .cloned()
        .collect();
    find_duplicate(
        &new_center,
        &others,
        wr_domain::fingerprint::bank::DUPLICATE_CENTER_DISTANCE,
    )
    .map(|duplicate| {
        format!(
            "疑似与既有条目 '{duplicate}' 是同一个模型(重复收录会让 top-1 概率塌陷)。请改用别名,或直接复用既有条目。"
        )
    })
}

// ---------------------------------------------------------------------------
// detectModelFingerprint
// ---------------------------------------------------------------------------

/// `detectModelFingerprint`。
pub async fn detect_model_fingerprint(
    state: &AdminState,
    args: &[Value],
) -> Result<Value, String> {
    let payload = args
        .first()
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let provider_id = payload
        .get("providerId")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or("detectModelFingerprint requires providerId")?;
    let model_names: Vec<String> = payload
        .get("modelNames")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    if model_names.is_empty() {
        return Err("detectModelFingerprint requires at least one model".to_owned());
    }
    let tier = payload
        .get("probeTier")
        .and_then(Value::as_str)
        .and_then(ProbeTier::parse)
        .unwrap_or(ProbeTier::Standard);
    let seed = payload
        .get("probeSeed")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("wrouter-{provider_id}"));
    let context = load_provider_context(state, &provider_id).await?;
    let api_key = provider_api_key(state, &provider_id).await?;
    let mut provider = context.provider.clone();
    provider.api_key = api_key;

    // 用户自建库(检测只走 L2/L3;内置参考库已下线)。
    let user_bank = load_user_library(state, None).await?;
    let suite = user_bank
        .as_ref()
        .and_then(|bank| bank.probe_suite)
        .unwrap_or(ProbeSuite::WrouterV1);
    let specs = specs_for(tier, &seed);

    let proxy_config = config::admin_proxy_config(state).await;
    let mut results: Vec<Value> = Vec::new();
    for model_name in &model_names {
        // 每个模型各自计时(起点放在循环内:多模型时第 N 个的耗时
        // 不应包含前面模型的探针时长)。
        let started = Instant::now();
        let outcomes = run_fingerprint_probes(
            &provider,
            &context.source_protocol,
            model_name,
            &specs,
            proxy_config.as_ref(),
        )
        .await;

        let mut samples: Vec<AnalysisSample> = Vec::new();
        let mut accepted = 0usize;
        let mut gaps = 0usize;
        let mut parse_failures = 0usize;
        let mut diagnostics: Vec<Value> = Vec::new();
        for run in &outcomes {
            if run.outcome.accepted {
                accepted += 1;
                samples.push(AnalysisSample::new(
                    run.outcome.text.clone(),
                    run.expected_count,
                ));
            } else {
                let reason = run
                    .outcome
                    .reason
                    .clone()
                    .unwrap_or_else(|| "unknown".to_owned());
                if reason.starts_with("parse:") {
                    parse_failures += 1;
                } else {
                    gaps += 1;
                }
            }
            diagnostics.push(json!({
                "challengeId": run.challenge_id,
                "conditionId": run.condition_id,
                "accepted": run.outcome.accepted,
                "reason": run.outcome.reason,
            }));
        }

        // 两层结论:L2 全局候选库 + L3 供应商锚点。
        let mut conclusions: Map<String, Value> = Map::new();
        let mut skipped: Vec<Value> = Vec::new();

        if let Some(bank) = user_bank.as_ref() {
            let attribution = analyze_with_group(&samples, bank, None);
            conclusions.insert("global".to_owned(), attribution_value(&attribution, "global"));
            // L3 供应商内对照:用该供应商的分组工件(通道偏移抵消)。
            if bank.groups.get(&provider_id).is_some() {
                let attribution = analyze_with_group(&samples, bank, Some(&provider_id));
                conclusions.insert("anchor".to_owned(), attribution_value(&attribution, "anchor"));
            }
        } else {
            skipped.push(json!({
                "library": "global",
                "reason": "用户自建库至少需要 2 个模型才能做闭集归因。",
            }));
        }

        results.push(json!({
            "model": model_name,
            "providerId": provider_id,
            "probeSuite": suite.as_str(),
            "probeTier": tier.as_str(),
            "probeSeed": seed,
            "conclusions": conclusions,
            "skipped": skipped,
            "diagnostics": {
                "probes": specs.len(),
                "accepted": accepted,
                "gaps": gaps,
                "parseFailures": parse_failures,
                "durationMs": (started.elapsed().as_secs_f64() * 1000.0 * 100.0).round() / 100.0,
            },
            "probeDiagnostics": diagnostics,
        }));
    }

    Ok(json!({
        "results": results,
        "probeSuite": suite.as_str(),
        "probeTier": tier.as_str(),
    }))
}

/// 归因结论 → JSON(附来源标记)。
fn attribution_value(attribution: &Result<AttributionResult, String>, source: &str) -> Value {
    match attribution {
        Ok(result) => {
            let mut value = serde_json::to_value(result).unwrap_or(Value::Null);
            if let Some(map) = value.as_object_mut() {
                map.insert("source".to_owned(), Value::String(source.to_owned()));
            }
            value
        }
        Err(message) => json!({"source": source, "error": message}),
    }
}

// ---------------------------------------------------------------------------
// listModelFingerprints
// ---------------------------------------------------------------------------

/// `listModelFingerprints`。
pub async fn list_model_fingerprints(state: &AdminState, args: &[Value]) -> Result<Value, String> {
    let payload = args
        .first()
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let kind = payload.get("kind").and_then(Value::as_str).map(str::to_owned);
    let provider_id = payload
        .get("providerId")
        .and_then(Value::as_str)
        .map(str::to_owned);

    let entries = state
        .store
        .list_entries(kind.as_deref(), provider_id.as_deref())
        .await
        .map_err(|error| error.to_string())?;

    // 用户自建库条目(去掉样本正文,列表页不需要)。
    let user_entries: Vec<Value> = entries
        .iter()
        .map(|entry| {
            let mut copy = entry.clone();
            if let Some(map) = copy.as_object_mut() {
                map.remove("samples");
            }
            copy
        })
        .collect();

    Ok(json!({
        "entries": user_entries,
    }))
}

// ---------------------------------------------------------------------------
// enrollModelFingerprint
// ---------------------------------------------------------------------------

/// `enrollModelFingerprint`:采集并入库。
///
/// 采集 → 建库 → **X7 重复检测** → 通过才落库。
pub async fn enroll_model_fingerprint(state: &AdminState, args: &[Value]) -> Result<Value, String> {
    let payload = args
        .first()
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let provider_id = payload
        .get("providerId")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or("enrollModelFingerprint requires providerId")?;
    let model_name = payload
        .get("modelName")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or("enrollModelFingerprint requires modelName")?;
    let family = payload.get("family").and_then(Value::as_str).map(str::to_owned);
    let family_name = payload.get("familyName").and_then(Value::as_str).map(str::to_owned);
    let label = payload
        .get("label")
        .and_then(Value::as_str)
        .unwrap_or(&model_name)
        .to_owned();
    let tier = payload
        .get("probeTier")
        .and_then(Value::as_str)
        .and_then(ProbeTier::parse)
        .unwrap_or(ProbeTier::Standard);
    let seed = payload
        .get("probeSeed")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("wrouter-{provider_id}-{model_name}"));
    let kind = payload
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("global")
        .to_owned();
    let anchor_provider = if kind == "anchor" {
        Some(provider_id.clone())
    } else {
        None
    };

    let context = load_provider_context(state, &provider_id).await?;
    let api_key = provider_api_key(state, &provider_id).await?;
    let mut provider = context.provider.clone();
    provider.api_key = api_key;

    let suite = ProbeSuite::WrouterV1;
    let specs = specs_for(tier, &seed);
    let proxy_config = config::admin_proxy_config(state).await;
    let outcomes = run_fingerprint_probes(
        &provider,
        &context.source_protocol,
        &model_name,
        &specs,
        proxy_config.as_ref(),
    )
    .await;

    let mut samples: Vec<Value> = Vec::new();
    let mut gaps = 0usize;
    for run in &outcomes {
        if run.outcome.accepted {
            samples.push(json!({
                "challengeId": run.challenge_id,
                "conditionId": run.condition_id,
                "expectedCount": run.expected_count,
                "text": run.outcome.text,
            }));
        } else {
            gaps += 1;
        }
    }
    if samples.is_empty() {
        return Err("采集失败:没有一条探针拿到可用回答(上游拒答或严重截断)。".to_owned());
    }

    // 组装建库样本(带上既有条目)→ 建库 → 查重。
    let existing = state
        .store
        .list_entries(None, None)
        .await
        .map_err(|error| error.to_string())?;
    let mut all_samples = entries_to_samples(&existing, anchor_provider.as_deref());
    let probe_seed = seed.clone();
    for sample in &samples {
        all_samples.push(BankSample {
            source: model_name.clone(),
            text: sample
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            expected_count: 0,
            condition_id: sample
                .get("conditionId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            nuisance_condition_id: None,
            challenge_id: sample
                .get("challengeId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            provider: Some(provider_id.clone()),
            family: family.clone(),
            family_name: family_name.clone(),
            probe_suite: Some(suite),
        });
    }
    let bank = build_bank(&all_samples)
        .map_err(|error| format!("build fingerprint bank failed: {error}"))?;

    // X7:重复收录检测 —— 新模型中心与既有条目中心过近 → 拒绝。
    if let Some(reason) = duplicate_model_reason(&bank, &model_name) {
        return Err(reason);
    }

    let entry_id = format!(
        "{}:{}:{}",
        kind,
        anchor_provider.as_deref().unwrap_or("-"),
        model_name
    );
    let entry = json!({
        "id": entry_id,
        "kind": kind,
        "providerId": anchor_provider,
        "modelName": model_name,
        "family": family,
        "familyName": family_name,
        "label": label,
        "probeSuite": suite.as_str(),
        "sampleCount": samples.len(),
        "probeSeed": probe_seed,
        "probeEnv": samples
            .iter()
            .filter_map(|sample| sample.get("conditionId").and_then(Value::as_str))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
            .join(","),
        "cvAccuracy": bank
            .calibration
            .get("1")
            .and_then(|entry| entry.cv_accuracy),
        "samples": samples,
    });
    state
        .store
        .upsert_entry(&entry)
        .await
        .map_err(|error| error.to_string())?;

    Ok(json!({
        "entry": {
            "id": entry_id,
            "kind": kind,
            "providerId": anchor_provider,
            "modelName": model_name,
            "family": family,
            "label": label,
            "probeSuite": suite.as_str(),
            "sampleCount": samples.len(),
            "cvAccuracy": bank.calibration.get("1").and_then(|entry| entry.cv_accuracy),
        },
        "gaps": gaps,
        "probeTier": tier.as_str(),
        "probeSeed": probe_seed,
    }))
}

// ---------------------------------------------------------------------------
// deleteModelFingerprint
// ---------------------------------------------------------------------------

/// `deleteModelFingerprint`:删除用户自建条目(内置库只读,不可删)。
pub async fn delete_model_fingerprint(state: &AdminState, args: &[Value]) -> Result<Value, String> {
    let payload = args
        .first()
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let id = payload
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or("deleteModelFingerprint requires id")?;
    let removed = state
        .store
        .delete_entry(&id)
        .await
        .map_err(|error| error.to_string())?;
    Ok(json!({"ok": removed, "id": id}))
}

// 库元信息已并入 `listModelFingerprints` 的 `builtin` 段(UI 单一数据源);
// 导入导出见 `model_fingerprint_transfer`。

#[cfg(test)]
mod tests {
    use super::*;
    use wr_domain::fingerprint::probe::SeededRng;

    /// 造一条数字文本(确定性伪随机)。
    fn numbers_text(seed: &str, count: usize) -> String {
        let mut rng = SeededRng::from_str_seed(seed);
        let mut values = Vec::with_capacity(count);
        for _ in 0..count {
            values.push(((rng.next_u64() % 355) + 1).to_string());
        }
        values.join(" ")
    }

    #[test]
    fn duplicate_model_is_rejected() {
        // m1 与 m2 是同一个模型(回答几乎一样),m3/m4 是另外两个模型。
        // 需要**多个模型 + 多个环境**:环境太少时干扰基会把模型间的差异
        // 一并投影掉,中心点退化(零范数)→ 查重无法判定。
        let mut samples = Vec::new();
        for (model, seed, mutate) in [
            ("m1", "a", false),
            ("m2", "a", true),
            ("m3", "b", false),
            ("m4", "c", false),
        ] {
            for index in 0..12 {
                let base = numbers_text(&format!("{seed}-{index}"), 300);
                let text = if mutate {
                    // 与 m1 同源,只改最后一个数字 → 中心点几乎重合。
                    let mut parts: Vec<String> = base.split(' ').map(str::to_owned).collect();
                    let last = parts.len() - 1;
                    parts[last] = ((parts[last].parse::<u32>().unwrap_or(1)) % 355 + 1).to_string();
                    parts.join(" ")
                } else {
                    base
                };
                samples.push(BankSample {
                    source: model.to_owned(),
                    text,
                    expected_count: 300,
                    condition_id: format!("env-{}", index + 1),
                    nuisance_condition_id: None,
                    challenge_id: format!("{model}-c{index}"),
                    provider: None,
                    family: None,
                    family_name: None,
                    probe_suite: Some(ProbeSuite::WrouterV1),
                });
            }
        }
        let bank = build_bank(&samples).expect("建库");
        let reason = duplicate_model_reason(&bank, "m1").expect("应判为重复");
        assert!(reason.contains("m2"), "理由里要指出与谁重复:{reason}");
        // m3 明显不同 → 不该被误判。
        assert!(
            duplicate_model_reason(&bank, "m3").is_none(),
            "m3 不该被判为重复"
        );
    }
}
