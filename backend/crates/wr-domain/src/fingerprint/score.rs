//! 指纹打分与归因(方案 20261030_01 R5)。
//!
//! 数值语义严格镜像 ModelTrace `fingerprint.py`:
//! - 边缘分布项:`(feature - mean) / scale` → 投影掉干扰基 → 归一 → 与各模型
//!   中心点做内积 → 标准化;
//! - 有序块项:模板项(各环境得分取最大)与干扰项各占 0.5,融合后再标准化;
//! - 两项按 `0.75 / 0.25` 融合;
//! - 多条回答的分数取**均值**,再按 `softmax(β × score)` 出概率;
//! - β 取校准档位 `min(len(valid), 3)` 对应的温度。
//!
//! 方案增强:
//! - **X3 阈值绑定校准**:证据强度由该档位的交叉验证准确率导出的阈值判定,
//!   不用手写常量;输出四档 `strong/moderate/weak/insufficient`。
//! - **X4 显式弃权**:证据不足时**不给具体模型名**(闭集归因会把库外模型
//!   硬塞给最相似候选,这是不可接受的误导)。
//! - **X2 供应商分组**:打分时可用该供应商的分组工件(通道偏移在同供应商内
//!   自动抵消)。

use crate::fingerprint::features::{
    DIMENSION, MINIMUM_VALID_NUMBERS, count_numbers, hellinger_feature, js_similarity,
    normalize, ordered_block_feature, parse_numbers, softmax, standardize,
};
use crate::fingerprint::linalg::project_out;
use crate::fingerprint::{
    AttributionCandidate, AttributionResult, CalibrationEntry, EvidenceStrength, FamilyProbability,
    FingerprintBank, ProbeDiagnostic, RobustArtifacts,
};

/// 打分分量。
#[derive(Debug, Clone)]
pub struct ScoreComponents {
    /// 融合后的分数(每模型一个)。
    pub fused: Vec<f64>,
    /// 边缘分布项的干扰分数。
    pub nuisance: Vec<f64>,
    /// 有序块项的分数。
    pub ordered: Vec<f64>,
    /// 仅边缘分布项的分数。
    pub marginal_fused: Vec<f64>,
}

/// 待归因的一条探针输出。
#[derive(Debug, Clone)]
pub struct AnalysisSample {
    /// 模型输出文本。
    pub text: String,
    /// 探针期望数字数(0 = 未知)。
    pub expected_count: usize,
    /// 外部已知的问题(拒答 / 截断 / 解析为空);有值则强制计为缺口。
    pub reason: Option<String>,
}

impl AnalysisSample {
    /// 构造一条正常样本。
    pub fn new(text: impl Into<String>, expected_count: usize) -> Self {
        Self {
            text: text.into(),
            expected_count,
            reason: None,
        }
    }
}

/// 边缘分布项打分(镜像 `fingerprint.robust_score_counts`)。
///
/// Python 侧对 nuisance **标准化两次**;`standardize` 是幂等的(标准化向量
/// 再标准化仍是自身,仅差浮点噪声),本实现保留同样的两次调用以与 Python
/// 逐位对齐。
fn score_counts(counts: &[u32], artifacts: &RobustArtifacts) -> (Vec<f64>, Vec<f64>) {
    let artifact = &artifacts.hellinger;
    let feature = hellinger_feature(counts);
    let mut projected: Vec<f64> = feature
        .iter()
        .enumerate()
        .map(|(index, value)| {
            (value - artifact.feature_mean[index]) / artifact.feature_scale[index]
        })
        .collect();
    if !artifact.nuisance_basis.is_empty() {
        projected = project_out(&projected, &artifact.nuisance_basis);
    }
    let projected = normalize(&projected);
    let scores: Vec<f64> = artifact
        .centroids
        .iter()
        .map(|centroid| crate::fingerprint::features::dot(&projected, centroid))
        .collect();
    let nuisance = standardize(&scores);
    let fused = standardize(&nuisance);
    (fused, nuisance)
}

/// 有序块项打分(镜像 `fingerprint.ordered_block_scores`)。
fn ordered_block_scores(numbers: &[u32], artifacts: &RobustArtifacts) -> Vec<f64> {
    let artifact = &artifacts.ordered_blocks;
    let feature = ordered_block_feature(numbers);
    let standardized_feature: Vec<f64> = feature
        .iter()
        .enumerate()
        .map(|(index, value)| {
            (value - artifact.feature_mean[index]) / artifact.feature_scale[index]
        })
        .collect();
    let unit = normalize(&standardized_feature);

    // 模板项:各环境得分取最大(镜像 `np.max(environment_scores, axis=0)`)。
    let model_count = artifact.centroids.len();
    let mut template = vec![f64::NEG_INFINITY; model_count];
    for centroids in &artifact.environment_centroids {
        for (model_index, centroid) in centroids.iter().enumerate() {
            if model_index >= model_count {
                break;
            }
            let score = crate::fingerprint::features::dot(&unit, centroid);
            if score > template[model_index] {
                template[model_index] = score;
            }
        }
    }
    if template.iter().all(|value| value.is_infinite()) {
        template = vec![0.0; model_count];
    }
    let template = standardize(&template);

    // 干扰项:投影掉干扰基后与中心点做内积。
    let mut projected = standardized_feature;
    if !artifact.nuisance_basis.is_empty() {
        projected = project_out(&projected, &artifact.nuisance_basis);
    }
    let projected = normalize(&projected);
    let nuisance: Vec<f64> = artifact
        .centroids
        .iter()
        .map(|centroid| crate::fingerprint::features::dot(&projected, centroid))
        .collect();
    let nuisance = standardize(&nuisance);

    let combined: Vec<f64> = template
        .iter()
        .zip(&nuisance)
        .map(|(left, right)| 0.5 * left + 0.5 * right)
        .collect();
    standardize(&combined)
}

/// 对一组数字打分(镜像 `fingerprint.robust_score_numbers`)。
///
/// `group` 指定分组工件(方案 X2);`None` 用全局工件。
pub fn score_numbers_with_group(
    numbers: &[u32],
    bank: &FingerprintBank,
    group: Option<&str>,
) -> ScoreComponents {
    let artifacts = bank.artifacts_for(group);
    let counts = count_numbers(numbers);
    let (marginal_fused, nuisance) = score_counts(&counts, artifacts);
    let weight = artifacts.ordered_blocks.weight;
    if weight == 0.0 {
        return ScoreComponents {
            fused: marginal_fused.clone(),
            nuisance,
            ordered: marginal_fused.clone(),
            marginal_fused,
        };
    }
    let ordered = ordered_block_scores(numbers, artifacts);
    let fused: Vec<f64> = marginal_fused
        .iter()
        .zip(&ordered)
        .map(|(marginal, ordered)| (1.0 - weight) * marginal + weight * ordered)
        .collect();
    ScoreComponents {
        fused,
        nuisance,
        ordered,
        marginal_fused,
    }
}

/// 对一组数字打分(用全局工件)。
pub fn score_numbers(numbers: &[u32], bank: &FingerprintBank) -> ScoreComponents {
    score_numbers_with_group(numbers, bank, None)
}

/// 归因(镜像 `fingerprint.analyze_outputs` + 方案 X3/X4)。
///
/// `group` 指定分组工件(方案 X2)。
///
/// 失败模式:没有任何可用回答时返回 `Err`(对应 ModelTrace 的
/// `ValueError("没有可用回答:…")`)。
pub fn analyze_with_group(
    samples: &[AnalysisSample],
    bank: &FingerprintBank,
    group: Option<&str>,
) -> Result<AttributionResult, String> {
    let artifacts = bank.artifacts_for(group);
    let model_ids = artifacts.model_order.clone();
    if model_ids.is_empty() {
        return Err("fingerprint bank contains no model".to_owned());
    }

    let minimum_valid = bank.minimum_valid_numbers.max(MINIMUM_VALID_NUMBERS);
    let mut valid: Vec<(Vec<u32>, Vec<f64>, Vec<f64>)> = Vec::new();
    let mut diagnostics: Vec<ProbeDiagnostic> = Vec::new();
    for (index, sample) in samples.iter().enumerate() {
        let numbers = parse_numbers(&sample.text);
        let minimum = if sample.expected_count > 0 {
            minimum_valid.max((sample.expected_count as f64 * 0.55).ceil() as usize)
        } else {
            minimum_valid
        };
        let mut reason = sample.reason.clone();
        if reason.is_none() && numbers.is_empty() {
            reason = Some("empty".to_owned());
        }
        let accepted = reason.is_none() && numbers.len() >= minimum;
        diagnostics.push(ProbeDiagnostic {
            index,
            parsed_numbers: numbers.len(),
            minimum_numbers: minimum,
            accepted,
            reason: if accepted {
                None
            } else {
                Some(reason.unwrap_or_else(|| "truncated".to_owned()))
            },
        });
        if !accepted {
            continue;
        }
        let counts = count_numbers(&numbers);
        let components = score_numbers_with_group(&numbers, bank, group);
        valid.push((counts, components.fused, components.nuisance));
    }

    if valid.is_empty() {
        return Err(
            "没有可用回答:请确认上游返回了完整数字序列(拒答或严重截断的回答不计入)。"
                .to_owned(),
        );
    }

    let combined_scores: Vec<f64> = (0..model_ids.len())
        .map(|index| {
            valid
                .iter()
                .map(|(_, scores, _)| scores[index])
                .sum::<f64>()
                / valid.len() as f64
        })
        .collect();
    let combined_nuisance: Vec<f64> = (0..model_ids.len())
        .map(|index| {
            valid
                .iter()
                .map(|(_, _, nuisance)| nuisance[index])
                .sum::<f64>()
                / valid.len() as f64
        })
        .collect();

    let calibration_queries = valid.len().min(3).to_string();
    let calibration: Option<&CalibrationEntry> = bank.calibration.get(&calibration_queries);
    let beta = calibration.map(|entry| entry.beta).unwrap_or(1.0);
    let scaled: Vec<f64> = combined_scores.iter().map(|value| beta * value).collect();
    let probabilities = softmax(&scaled);

    let mut pooled = vec![0u32; DIMENSION];
    for (counts, _, _) in &valid {
        for (index, value) in counts.iter().enumerate() {
            pooled[index] += value;
        }
    }

    let mut results: Vec<AttributionCandidate> = model_ids
        .iter()
        .enumerate()
        .map(|(index, model_id)| {
            let entry = bank.models.iter().find(|model| &model.id == model_id);
            let profile_similarity = entry
                .filter(|model| model.counts.len() == DIMENSION)
                .map(|model| js_similarity(&pooled, &model.counts));
            AttributionCandidate {
                model: model_id.clone(),
                display_name: entry
                    .map(|model| model.display_name.clone())
                    .unwrap_or_else(|| model_id.clone()),
                probability: probabilities[index],
                score: combined_scores[index],
                nuisance_score: combined_nuisance[index],
                profile_similarity,
                family: entry.and_then(|model| model.family.clone()),
                family_name: entry.and_then(|model| model.family_name.clone()),
                conditional_probability: None,
            }
        })
        .collect();
    results.sort_by(|left, right| {
        right
            .probability
            .partial_cmp(&left.probability)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // ---- 家族级概率(镜像 `analyze_global_outputs`) ----
    let mut family_order: Vec<String> = Vec::new();
    for candidate in &results {
        let family = candidate.family.clone().unwrap_or_else(|| "models".to_owned());
        if !family_order.contains(&family) {
            family_order.push(family);
        }
    }
    let mut family_probabilities: Vec<FamilyProbability> = Vec::new();
    for family in &family_order {
        let probability: f64 = results
            .iter()
            .filter(|candidate| {
                candidate.family.clone().unwrap_or_else(|| "models".to_owned()) == *family
            })
            .map(|candidate| candidate.probability)
            .sum();
        let display_name = results
            .iter()
            .find(|candidate| {
                candidate.family.clone().unwrap_or_else(|| "models".to_owned()) == *family
            })
            .and_then(|candidate| candidate.family_name.clone())
            .unwrap_or_else(|| family.clone());
        family_probabilities.push(FamilyProbability {
            family: family.clone(),
            display_name,
            probability,
        });
    }
    for candidate in &mut results {
        let family = candidate.family.clone().unwrap_or_else(|| "models".to_owned());
        let total = family_probabilities
            .iter()
            .find(|entry| entry.family == family)
            .map(|entry| entry.probability)
            .unwrap_or(0.0);
        candidate.conditional_probability = if total > 0.0 {
            Some(candidate.probability / total)
        } else {
            None
        };
    }
    let family_prediction = family_probabilities
        .iter()
        .max_by(|left, right| {
            left.probability
                .partial_cmp(&right.probability)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|entry| entry.family.clone());

    // ---- 证据强度(方案 X3)与弃权(方案 X4) ----
    let top_probability = results.first().map(|item| item.probability).unwrap_or(0.0);
    let runner_up = results.get(1).map(|item| item.probability).unwrap_or(0.0);
    let margin = (top_probability - runner_up).max(0.0);
    let thresholds = calibration.and_then(|entry| entry.thresholds);
    let strength = match thresholds {
        None => EvidenceStrength::Weak,
        Some(thresholds) => {
            if margin < thresholds.margin {
                EvidenceStrength::Insufficient
            } else if top_probability >= thresholds.strong {
                EvidenceStrength::Strong
            } else if top_probability >= thresholds.moderate {
                EvidenceStrength::Moderate
            } else {
                EvidenceStrength::Weak
            }
        }
    };
    // 证据不足 → **不给出具体模型名**(方案 X4)。
    let abstained = strength == EvidenceStrength::Insufficient;

    Ok(AttributionResult {
        prediction: if abstained {
            None
        } else {
            results.first().map(|item| item.model.clone())
        },
        prediction_name: if abstained {
            None
        } else {
            results.first().map(|item| item.display_name.clone())
        },
        probability: top_probability,
        strength,
        used_outputs: valid.len(),
        results,
        diagnostics,
        calibration_queries,
        beta,
        cv_accuracy: calibration.and_then(|entry| entry.cv_accuracy),
        margin,
        family_prediction: if abstained { None } else { family_prediction },
        family_probabilities,
    })
}

/// 归因(用全局工件)。
pub fn analyze(samples: &[AnalysisSample], bank: &FingerprintBank) -> Result<AttributionResult, String> {
    analyze_with_group(samples, bank, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fingerprint::bank::{BankSample, build_bank};
    use crate::fingerprint::ProbeSuite;
    use std::collections::BTreeMap;

    fn sample(source: &str, text: &str, condition: &str, challenge: &str) -> BankSample {
        BankSample {
            source: source.to_owned(),
            text: text.to_owned(),
            expected_count: 300,
            condition_id: condition.to_owned(),
            nuisance_condition_id: None,
            challenge_id: challenge.to_owned(),
            provider: None,
            family: Some("gpt".to_owned()),
            family_name: Some("GPT".to_owned()),
            probe_suite: Some(ProbeSuite::WrouterV1),
        }
    }

    fn synthetic(count: usize, offset: u32) -> String {
        (0..count)
            .map(|index| ((index as u32 * 7 + offset) % 355 + 1).to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn build_test_bank() -> FingerprintBank {
        let mut samples: Vec<BankSample> = Vec::new();
        for model in ["m1", "m2", "m3"] {
            for environment in 1..=3 {
                for challenge in 1..=3 {
                    let offset = if model == "m1" { 0 } else if model == "m2" { 11 } else { 23 };
                    samples.push(sample(
                        model,
                        &synthetic(300, offset),
                        &format!("environment-{environment:02}"),
                        // challenge_id 全局唯一(跨环境),否则分组交叉验证不成立。
                        &format!("e{environment:02}-q{challenge:02}"),
                    ));
                }
            }
        }
        build_bank(&samples).expect("bank should build")
    }

    #[test]
    fn analyze_rejects_empty_input() {
        let bank = build_test_bank();
        assert!(analyze(&[], &bank).is_err());
    }

    #[test]
    fn analyze_reports_gaps_for_truncated_answers() {
        let bank = build_test_bank();
        let samples = vec![
            AnalysisSample::new(synthetic(300, 0), 300),
            AnalysisSample {
                text: "我拒绝回答".to_owned(),
                expected_count: 300,
                reason: Some("refusal".to_owned()),
            },
        ];
        let result = analyze(&samples, &bank).expect("one valid sample suffices");
        assert_eq!(result.used_outputs, 1);
        assert_eq!(result.diagnostics.len(), 2);
        assert!(result.diagnostics[0].accepted);
        assert!(!result.diagnostics[1].accepted);
        assert_eq!(result.diagnostics[1].reason.as_deref(), Some("refusal"));
    }

    #[test]
    fn analyze_attributes_to_the_matching_model() {
        let bank = build_test_bank();
        let samples = vec![AnalysisSample::new(synthetic(300, 23), 300)];
        let result = analyze(&samples, &bank).expect("analysis");
        assert_eq!(result.prediction.as_deref(), Some("m3"));
        assert!(result.probability > 0.5);
        assert_eq!(result.calibration_queries, "1");
    }

    #[test]
    fn probabilities_sum_to_one() {
        let bank = build_test_bank();
        let samples = vec![AnalysisSample::new(synthetic(300, 0), 300)];
        let result = analyze(&samples, &bank).expect("analysis");
        let total: f64 = result.results.iter().map(|item| item.probability).sum();
        assert!((total - 1.0).abs() < 1e-9);
    }

    #[test]
    fn insufficient_evidence_withholds_model_name() {
        let bank = build_test_bank();
        // 极短的样本 → 低于接受门槛 → 没有有效回答。
        let samples = vec![AnalysisSample::new("1, 2, 3", 300)];
        assert!(analyze(&samples, &bank).is_err());
    }

    #[test]
    fn family_probabilities_are_reported() {
        let bank = build_test_bank();
        let samples = vec![AnalysisSample::new(synthetic(300, 0), 300)];
        let result = analyze(&samples, &bank).expect("analysis");
        assert!(!result.family_probabilities.is_empty());
        assert!(result.family_probabilities.iter().all(|entry| entry.probability >= 0.0));
    }

    #[test]
    fn calibration_lookup_falls_back_to_three_queries() {
        let bank = build_test_bank();
        let samples: Vec<AnalysisSample> = (0..4)
            .map(|index| AnalysisSample::new(synthetic(300, index as u32), 300))
            .collect();
        let result = analyze(&samples, &bank).expect("analysis");
        assert_eq!(result.calibration_queries, "3");
    }

    #[test]
    fn bank_without_calibration_still_scores() {
        let mut bank = build_test_bank();
        bank.calibration = BTreeMap::new();
        let samples = vec![AnalysisSample::new(synthetic(300, 0), 300)];
        let result = analyze(&samples, &bank).expect("analysis");
        // 无校准 → 无阈值 → 证据强度为 weak,但仍给结论。
        assert_eq!(result.strength, EvidenceStrength::Weak);
        assert!(result.prediction.is_some());
    }
}
