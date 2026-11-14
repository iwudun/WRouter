//! 指纹库构建(方案 20261030_01 R4)。
//!
//! 镜像 ModelTrace `bank_builder.py`,并落地四项增强:
//! - **X1 干扰基秩自适应**:ModelTrace 硬编码 `rank ≤ 2`
//!   (`bank_builder.py:87`),环境数变化时会欠拟合/过拟合;本实现按**累计
//!   能量 ≥ 95%** 选秩,上限 `min(4, 环境数)`。
//! - **X2 干扰基按供应商分组**:中转站会注入自己的 system prompt,造成
//!   **供应商级**分布偏移;全局干扰基消不掉它。WRouter 改为按 provider
//!   分组各算一组干扰基,检测时用该供应商的基(同供应商内偏移自动抵消)。
//! - **X3 阈值绑定校准**:判定阈值从**实测混淆矩阵**导出,不再用
//!   0.5/0.8/0.15/0.65 这类手写常量。
//! - **X7 重复收录检测**:同模型重复入库会让 top-1 概率塌到 0.5
//!   (实测 639/648 样本跌破 0.8),入库前必须查重。
//!
//! **全量重拟合语义**(方案 §3.5):新增/删除条目后全局库与各锚点都要重算,
//! 与 ModelTrace 一致;写入采用「先写临时条目 → 重算全库 → 原子替换」。
//!
//! **校准不得静默回退**:ModelTrace 在采集不完整时会复制上一版的
//! `calibration`(含 `cv_accuracy`)(`bank_builder.py:269-282`)。WRouter
//! 落库时**禁止**沿用旧准确率——采集不完整就不给校准档位,UI 不得显示
//! 旧值冒充当前值。

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::fingerprint::features::{
    DIMENSION, MINIMUM_VALID_NUMBERS, ORDERED_BLOCK_WEIGHT, count_numbers, hellinger_feature,
    ordered_block_feature, parse_numbers,
};
use crate::fingerprint::linalg::{
    adaptive_rank, project_out, right_singular_directions,
};
use crate::fingerprint::{
    CalibrationEntry, EvidenceThresholds, FingerprintBank, FingerprintModel, HellingerArtifacts,
    OrderedBlockArtifacts, ProbeSuite, RobustArtifacts,
};

/// 方案 X1:干扰基累计能量阈值。
pub const NUISANCE_ENERGY_THRESHOLD: f64 = 0.95;
/// 方案 X1:干扰基秩上限(同时受环境数约束)。
pub const NUISANCE_MAX_RANK: usize = 4;
/// 方案 X7:中心点余弦距离小于该值即判「疑似同一模型」。
///
/// 定值依据(2026-11-01 用 ModelTrace 真实库实测校准,非拍脑袋):
/// - **不同模型**(含同家族孪生版本):`unified_bank.json` 全部 153 对中心的
///   余弦距离最小 **0.349**(claude-opus-4-7 vs claude-opus-4-8);
/// - **同模型两次独立采集**(每环境 1 条 × 12 的 bootstrap 模拟,n=1080):
///   距离 min **0.133** / p50 0.339 —— 本就与本阈值无关,不应拦;
/// - **同源复制/准复制**(同批样本改名字甚至改个别数字):距离 ≈ 0~0.05。
///
/// 0.10 位于「准复制」与「独立采集/不同模型」之间:上界有 1.3~3.5 倍安全
/// 裕度(不误拒),下界能拦住会让 top-1 概率直接塌陷的紧重复。
pub const DUPLICATE_CENTER_DISTANCE: f64 = 0.10;
/// 校准记录的拟合温度上界(镜像 `bank_builder.py:217` 的 12.0)。
const BETA_MAX: f64 = 12.0;
/// 校准记录的拟合温度下界。
const BETA_MIN: f64 = 0.05;
/// 温度网格点数(镜像 `range(801)`)。
const BETA_GRID: usize = 801;

/// 建库用的样本(一条回答)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BankSample {
    /// 模型标签(归因目标)。
    pub source: String,
    /// 模型原始输出文本。
    pub text: String,
    /// 探针期望数字数(用于接受门槛)。
    #[serde(default)]
    pub expected_count: usize,
    /// 采集环境标识。
    #[serde(default)]
    pub condition_id: String,
    /// 干扰项分组的环境标识(缺省回落 `condition_id`)。
    ///
    /// ModelTrace 的原始语料带此字段(`row.get("nuisance_condition_id",
    /// row["condition_id"])`);WRouter 自采样本两值恒等,字段只为导入
    /// 外部语料时保真。
    #[serde(default)]
    pub nuisance_condition_id: Option<String>,
    /// 探针标识。
    #[serde(default)]
    pub challenge_id: String,
    /// 采集通道(provider)—— 方案 X2 的分组键。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// 模型家族。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    /// 家族展示名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family_name: Option<String>,
    /// 采集该样本所用的探针套件(方案 X9)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe_suite: Option<ProbeSuite>,
}

/// 建库错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BankError {
    /// 没有任何可用样本。
    NoSamples,
    /// 样本涉及的模型不足(闭集归因至少要 2 个候选,否则无法区分)。
    NotEnoughModels(usize),
    /// 某个模型没有可用样本。
    ModelWithoutSamples(String),
    /// 样本未覆盖任何环境。
    NoEnvironments,
}

impl std::fmt::Display for BankError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BankError::NoSamples => formatter.write_str("no usable samples"),
            BankError::NotEnoughModels(count) => {
                write!(formatter, "need at least 2 models for closed-set attribution, got {count}")
            }
            BankError::ModelWithoutSamples(model) => {
                write!(formatter, "model '{model}' has no usable samples")
            }
            BankError::NoEnvironments => formatter.write_str("samples cover no environment"),
        }
    }
}

impl std::error::Error for BankError {}

/// 行矩阵按列求均值(总体均值,镜像 numpy `mean(axis=0)`)。
fn mean_axis0(matrix: &[Vec<f64>]) -> Vec<f64> {
    let dimension = matrix.first().map(Vec::len).unwrap_or(0);
    if matrix.is_empty() {
        return vec![0.0; dimension];
    }
    let mut sums = vec![0.0f64; dimension];
    for row in matrix {
        for (slot, value) in sums.iter_mut().zip(row) {
            *slot += value;
        }
    }
    let count = matrix.len() as f64;
    sums.iter().map(|value| value / count).collect()
}

/// 行矩阵按列求总体标准差(镜像 numpy `std(axis=0)`,ddof=0)。
fn std_axis0(matrix: &[Vec<f64>]) -> Vec<f64> {
    if matrix.is_empty() {
        return Vec::new();
    }
    let mean = mean_axis0(matrix);
    let dimension = mean.len();
    let mut sums = vec![0.0f64; dimension];
    for row in matrix {
        for (index, value) in row.iter().enumerate() {
            sums[index] += (value - mean[index]).powi(2);
        }
    }
    let count = matrix.len() as f64;
    sums.iter().map(|value| (value / count).sqrt()).collect()
}

/// 逐列做 `(x - mean) / scale`。
fn standardize_columns(matrix: &[Vec<f64>], mean: &[f64], scale: &[f64]) -> Vec<Vec<f64>> {
    matrix
        .iter()
        .map(|row| {
            row.iter()
                .enumerate()
                .map(|(index, value)| (value - mean[index]) / scale[index])
                .collect()
        })
        .collect()
}

/// 取指定索引子集的行。
fn select_rows(matrix: &[Vec<f64>], indices: &[usize]) -> Vec<Vec<f64>> {
    indices.iter().map(|index| matrix[*index].clone()).collect()
}

/// 拟合稳健工件(镜像 `bank_builder.fit_robust_artifacts`)。
///
/// `rows` 是参与拟合的样本(已解析出数字);`model_ids` 是模型顺序。
fn fit_robust_artifacts(rows: &[&PreparedRow<'_>], model_ids: &[String]) -> RobustArtifacts {
    // 完整环境 = 该环境下出现过**全部**模型的环境(镜像 `complete_environments`)。
    let mut environments: Vec<String> = rows
        .iter()
        .map(|row| row.condition_id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    environments.sort();
    let complete: Vec<String> = environments
        .iter()
        .filter(|environment| {
            let sources: BTreeSet<&str> = rows
                .iter()
                .filter(|row| &row.condition_id == *environment)
                .map(|row| row.source)
                .collect();
            sources.len() == model_ids.len()
                && model_ids.iter().all(|model| sources.contains(model.as_str()))
        })
        .cloned()
        .collect();
    let robust_ready = !complete.is_empty();
    let selected: Vec<&PreparedRow<'_>> = if robust_ready {
        rows.iter()
            .filter(|row| complete.contains(&row.condition_id))
            .copied()
            .collect()
    } else {
        rows.to_vec()
    };

    // ---- 边缘分布 ----
    let features: Vec<Vec<f64>> = selected
        .iter()
        .map(|row| hellinger_feature(&row.counts))
        .collect();
    let feature_mean = mean_axis0(&features);
    let mut feature_scale = std_axis0(&features);
    for value in &mut feature_scale {
        if *value < 1e-12 {
            *value = 1.0;
        }
    }
    let mut standardized = standardize_columns(&features, &feature_mean, &feature_scale);

    // 干扰基:按环境的偏移均值 → 去均值 → 取主方向。
    let nuisance_environments: Vec<String> = selected
        .iter()
        .map(|row| row.nuisance_condition_id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut environment_means: Vec<Vec<f64>> = Vec::new();
    for environment in &nuisance_environments {
        let indices: Vec<usize> = selected
            .iter()
            .enumerate()
            .filter(|(_, row)| &row.nuisance_condition_id == environment)
            .map(|(index, _)| index)
            .collect();
        environment_means.push(mean_axis0(&select_rows(&standardized, &indices)));
    }
    let basis = compute_nuisance_basis(&environment_means);
    if !basis.is_empty() {
        for row in &mut standardized {
            *row = project_out(row, &basis);
        }
    }

    // ---- 模型中心点 ----
    let centroids = model_centroids(&standardized, &selected, model_ids);

    // ---- 有序块 ----
    let ordered_features: Vec<Vec<f64>> = selected
        .iter()
        .map(|row| ordered_block_feature(&row.numbers))
        .collect();
    let ordered_mean = mean_axis0(&ordered_features);
    let mut ordered_scale = std_axis0(&ordered_features);
    for value in &mut ordered_scale {
        if *value < 1e-12 {
            *value = 1.0;
        }
    }
    let mut ordered_standardized =
        standardize_columns(&ordered_features, &ordered_mean, &ordered_scale);
    let mut ordered_environment_means: Vec<Vec<f64>> = Vec::new();
    for environment in &nuisance_environments {
        let indices: Vec<usize> = selected
            .iter()
            .enumerate()
            .filter(|(_, row)| &row.nuisance_condition_id == environment)
            .map(|(index, _)| index)
            .collect();
        ordered_environment_means.push(mean_axis0(&select_rows(
            &ordered_standardized,
            &indices,
        )));
    }
    let ordered_basis = compute_nuisance_basis(&ordered_environment_means);
    if !ordered_basis.is_empty() {
        for row in &mut ordered_standardized {
            *row = project_out(row, &ordered_basis);
        }
    }
    let ordered_centroids = model_centroids(&ordered_standardized, &selected, model_ids);

    // 各环境 × 各模型的**未投影**中心点(模板项打分用)。
    let unprojected_ordered = standardize_columns(&ordered_features, &ordered_mean, &ordered_scale);
    let template_environments: Vec<Option<String>> = if robust_ready {
        complete.iter().cloned().map(Some).collect()
    } else {
        vec![None]
    };
    let mut environment_centroids: Vec<Vec<Vec<f64>>> = Vec::new();
    for environment in &template_environments {
        let mut per_model: Vec<Vec<f64>> = Vec::new();
        for model_id in model_ids {
            let indices: Vec<usize> = selected
                .iter()
                .enumerate()
                .filter(|(_, row)| {
                    let environment_matches = match environment {
                        None => true,
                        Some(name) => &row.condition_id == name,
                    };
                    environment_matches && row.source == *model_id
                })
                .map(|(index, _)| index)
                .collect();
            let mut centroid = mean_axis0(&select_rows(&unprojected_ordered, &indices));
            let length = crate::fingerprint::features::norm(&centroid);
            if length > 1e-12 {
                for value in &mut centroid {
                    *value /= length;
                }
            }
            per_model.push(centroid);
        }
        environment_centroids.push(per_model);
    }

    RobustArtifacts {
        model_order: model_ids.to_vec(),
        robust_ready,
        training_rows: selected.len(),
        complete_environments: complete,
        hellinger: HellingerArtifacts {
            feature_mean,
            feature_scale,
            nuisance_rank: basis.len(),
            nuisance_environments,
            nuisance_basis: basis,
            centroids,
        },
        ordered_blocks: OrderedBlockArtifacts {
            weight: ORDERED_BLOCK_WEIGHT,
            feature: "four position blocks x 16 value bins plus final-digit distribution".to_owned(),
            feature_mean: ordered_mean,
            feature_scale: ordered_scale,
            nuisance_rank: ordered_basis.len(),
            nuisance_basis: ordered_basis,
            centroids: ordered_centroids,
            environment_centroids,
        },
    }
}

/// 干扰基求解:环境偏移均值 → 去均值 → 右奇异主方向(方案 X1 自适应秩)。
fn compute_nuisance_basis(environment_means: &[Vec<f64>]) -> Vec<Vec<f64>> {
    if environment_means.len() < 2 {
        return Vec::new();
    }
    let mut offsets = environment_means.to_vec();
    // 减去环境均值的均值(镜像 `offsets -= offsets.mean(axis=0, keepdims=True)`)。
    let center = mean_axis0(&offsets);
    for row in &mut offsets {
        for (slot, value) in row.iter_mut().zip(&center) {
            *slot -= value;
        }
    }
    let directions = right_singular_directions(&offsets);
    let rank = adaptive_rank(
        &directions.singular_values,
        NUISANCE_MAX_RANK.min(environment_means.len()),
        NUISANCE_ENERGY_THRESHOLD,
    );
    directions.basis.into_iter().take(rank).collect()
}

/// 各模型中心点:投影后按模型求均值并归一。
fn model_centroids(
    standardized: &[Vec<f64>],
    rows: &[&PreparedRow<'_>],
    model_ids: &[String],
) -> Vec<Vec<f64>> {
    model_ids
        .iter()
        .map(|model_id| {
            let indices: Vec<usize> = rows
                .iter()
                .enumerate()
                .filter(|(_, row)| row.source == *model_id)
                .map(|(index, _)| index)
                .collect();
            let mut centroid = mean_axis0(&select_rows(standardized, &indices));
            let length = crate::fingerprint::features::norm(&centroid);
            if length > 1e-12 {
                for value in &mut centroid {
                    *value /= length;
                }
            }
            centroid
        })
        .collect()
}

/// 已解析的建库样本。
struct PreparedRow<'a> {
    source: &'a str,
    numbers: Vec<u32>,
    counts: Vec<u32>,
    condition_id: String,
    nuisance_condition_id: String,
    challenge_id: String,
    family: Option<String>,
    family_name: Option<String>,
    provider: Option<String>,
}

/// 解析样本:丢弃无数字的回答(镜像 `read_rows` 只保留 `strict_valid`)。
fn prepare_rows(samples: &[BankSample]) -> Vec<PreparedRow<'_>> {
    samples
        .iter()
        .filter_map(|sample| {
            let numbers = parse_numbers(&sample.text);
            if numbers.is_empty() {
                return None;
            }
            Some(PreparedRow {
                source: &sample.source,
                counts: count_numbers(&numbers),
                numbers,
                condition_id: sample.condition_id.clone(),
                nuisance_condition_id: sample
                    .nuisance_condition_id
                    .clone()
                    .unwrap_or_else(|| sample.condition_id.clone()),
                challenge_id: sample.challenge_id.clone(),
                family: sample.family.clone(),
                family_name: sample.family_name.clone(),
                provider: sample.provider.clone(),
            })
        })
        .collect()
}

/// 校准记录:(融合分数, 真实模型下标)。
type CalibrationRecord = (Vec<f64>, usize);

/// 生成分组交叉验证的校准记录(镜像 `bank_builder.calibration_records`)。
///
/// 按 **challenge 整组留出**:同一探针的全部回答同时留出,避免同探针重复
/// 采样泄漏(这是 ModelTrace 的核心防御,WRouter 保留)。
fn calibration_records(
    rows: &[&PreparedRow<'_>],
    model_ids: &[String],
    query_count: usize,
) -> Vec<CalibrationRecord> {
    let mut records: Vec<CalibrationRecord> = Vec::new();
    let conditions: BTreeSet<&str> = rows.iter().map(|row| row.condition_id.as_str()).collect();
    for condition in conditions {
        let mut challenge_ids: Vec<&str> = rows
            .iter()
            .filter(|row| row.condition_id == condition)
            .map(|row| row.challenge_id.as_str())
            .collect();
        challenge_ids.sort_unstable();
        challenge_ids.dedup();
        // 只保留「全部模型都有」的探针。
        challenge_ids.retain(|challenge_id| {
            model_ids.iter().all(|model_id| {
                rows.iter()
                    .any(|row| row.source == model_id && row.challenge_id == *challenge_id)
            })
        });
        if challenge_ids.len() < query_count {
            continue;
        }
        for held_ids in combinations(&challenge_ids, query_count) {
            let train: Vec<&PreparedRow<'_>> = rows
                .iter()
                .filter(|row| !held_ids.contains(&row.challenge_id.as_str()))
                .copied()
                .collect();
            let artifacts = fit_robust_artifacts(&train, model_ids);
            let temporary = FingerprintBank {
                schema: "temporary".to_owned(),
                built_at: String::new(),
                method: serde_json::Value::Null,
                recommended_queries: 3,
                minimum_valid_numbers: MINIMUM_VALID_NUMBERS,
                source_scope: String::new(),
                providers: Vec::new(),
                models: Vec::new(),
                robust: artifacts,
                groups: BTreeMap::new(),
                calibration: BTreeMap::new(),
                probe_suite: None,
            };
            for (truth_index, model_id) in model_ids.iter().enumerate() {
                let held: Vec<&PreparedRow<'_>> = rows
                    .iter()
                    .filter(|row| row.source == model_id && held_ids.contains(&row.challenge_id.as_str()))
                    .copied()
                    .collect();
                if held.len() != query_count {
                    continue;
                }
                let response_scores: Vec<Vec<f64>> = held
                    .iter()
                    .map(|row| {
                        crate::fingerprint::score::score_numbers(&row.numbers, &temporary).fused
                    })
                    .collect();
                let combined: Vec<f64> = (0..model_ids.len())
                    .map(|index| {
                        response_scores
                            .iter()
                            .map(|scores| scores[index])
                            .sum::<f64>()
                            / query_count as f64
                    })
                    .collect();
                records.push((combined, truth_index));
            }
        }
    }
    records
}

/// 组合枚举(镜像 `itertools.combinations`)。
fn combinations<T: Clone>(items: &[T], count: usize) -> Vec<Vec<T>> {
    if count == 0 || count > items.len() {
        return Vec::new();
    }
    let mut result: Vec<Vec<T>> = Vec::new();
    let mut indices: Vec<usize> = (0..count).collect();
    loop {
        result.push(indices.iter().map(|index| items[*index].clone()).collect());
        let mut position = count;
        loop {
            if position == 0 {
                return result;
            }
            position -= 1;
            if indices[position] != position + items.len() - count {
                break;
            }
            if position == 0 {
                return result;
            }
        }
        indices[position] += 1;
        for next in (position + 1)..count {
            indices[next] = indices[next - 1] + 1;
        }
    }
}

/// 网格搜 β(镜像 `bank_builder.fit_beta`)。
///
/// 上界 12.0(与 ModelTrace 一致)。已知 ModelTrace 自身的搜索上界会被触及
/// (实测 β=12.0),此处保持一致以免引入第二套温度语义。
fn fit_beta(records: &[CalibrationRecord]) -> (f64, f64) {
    let mut best_beta = 1.0f64;
    let mut best_loss = f64::INFINITY;
    if records.is_empty() {
        return (best_beta, best_loss);
    }
    let log_min = BETA_MIN.ln();
    let log_max = BETA_MAX.ln();
    for step in 0..BETA_GRID {
        let beta = (log_min + step as f64 * (log_max - log_min) / (BETA_GRID - 1) as f64).exp();
        let mut loss = 0.0f64;
        for (scores, truth) in records {
            let maximum = scores
                .iter()
                .map(|value| beta * value)
                .fold(f64::NEG_INFINITY, f64::max);
            let sum_exp: f64 = scores
                .iter()
                .map(|value| (beta * value - maximum).exp())
                .sum();
            loss += maximum + sum_exp.ln() - beta * scores[*truth];
        }
        loss /= records.len() as f64;
        if loss < best_loss {
            best_beta = beta;
            best_loss = loss;
        }
    }
    (best_beta, best_loss)
}

/// 由实测混淆矩阵导出证据强度阈值(方案 X3)。
///
/// 判定阈值**不再手写**(ModelTrace 插件用 0.5/0.8/0.15/0.65 常量,与库内
/// `calibration` 毫无关联)。此处由该档位的交叉验证准确率导出:
///
/// ```text
/// τ_strong   = 1 - cv_accuracy · 0.5      (保守:宁可漏报不可误报)
/// τ_moderate = 1 - cv_accuracy · 1.0
/// abs_margin = max(1 - cv_accuracy, 0.5)   (top1−top2 间距下限)
/// ```
pub fn derive_thresholds(cv_accuracy: f64) -> EvidenceThresholds {
    let accuracy = cv_accuracy.clamp(0.0, 1.0);
    EvidenceThresholds {
        strong: 1.0 - accuracy * 0.5,
        moderate: 1.0 - accuracy,
        margin: (1.0 - accuracy).max(0.5),
    }
}

/// 混淆矩阵:行 = 真实模型序,列 = 预测模型序。
fn confusion_matrix(records: &[CalibrationRecord], model_count: usize) -> Vec<Vec<u32>> {
    let mut matrix = vec![vec![0u32; model_count]; model_count];
    for (scores, truth) in records {
        let prediction = scores
            .iter()
            .enumerate()
            .max_by(|left, right| {
                left.1
                    .partial_cmp(right.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(index, _)| index)
            .unwrap_or(0);
        if *truth < model_count && prediction < model_count {
            matrix[*truth][prediction] += 1;
        }
    }
    matrix
}

/// 构建指纹库(镜像 `bank_builder.build_bank` + 方案 X2/X3/X7)。
///
/// `samples` 需覆盖**全部**目标模型;每个模型至少要有可解析的回答。
/// 按 provider 分组时额外产出分组工件(方案 X2)。
pub fn build_bank(samples: &[BankSample]) -> Result<FingerprintBank, BankError> {
    let prepared = prepare_rows(samples);
    let rows: Vec<&PreparedRow<'_>> = prepared.iter().collect();
    if rows.is_empty() {
        return Err(BankError::NoSamples);
    }
    let mut model_ids: Vec<String> = Vec::new();
    for sample in samples {
        if !model_ids.contains(&sample.source) {
            model_ids.push(sample.source.clone());
        }
    }
    if model_ids.len() < 2 {
        return Err(BankError::NotEnoughModels(model_ids.len()));
    }
    for model_id in &model_ids {
        if !rows.iter().any(|row| row.source == model_id) {
            return Err(BankError::ModelWithoutSamples(model_id.clone()));
        }
    }

    let robust = fit_robust_artifacts(&rows, &model_ids);

    // ---- 校准(分组交叉验证) ----
    let mut calibration = BTreeMap::new();
    for query_count in [1usize, 2, 3] {
        let records = calibration_records(&rows, &model_ids, query_count);
        let key = query_count.to_string();
        if records.is_empty() {
            // 采集不完整 → **不给**校准档位(不回退到旧值;ModelTrace 在此处
            // 会复制上一版 calibration,WRouter 明确禁止,见模块文档)。
            calibration.insert(
                key,
                CalibrationEntry {
                    beta: 1.0,
                    cv_accuracy: None,
                    cv_correct: 0,
                    cv_samples: 0,
                    cv_nll: None,
                    fallback: true,
                    confusion: None,
                    thresholds: None,
                },
            );
            continue;
        }
        let (beta, nll) = fit_beta(&records);
        let correct = records
            .iter()
            .filter(|(scores, truth)| {
                scores
                    .iter()
                    .enumerate()
                    .max_by(|left, right| {
                        left.1
                            .partial_cmp(right.1)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .map(|(index, _)| index)
                    == Some(*truth)
            })
            .count();
        let accuracy = correct as f64 / records.len() as f64;
        let confusion = confusion_matrix(&records, model_ids.len());
        calibration.insert(
            key,
            CalibrationEntry {
                beta,
                cv_accuracy: Some(accuracy),
                cv_correct: correct,
                cv_samples: records.len(),
                cv_nll: Some(nll),
                fallback: false,
                confusion: Some(confusion),
                thresholds: Some(derive_thresholds(accuracy)),
            },
        );
    }

    // ---- 模型条目 ----
    let mut models: Vec<FingerprintModel> = Vec::new();
    for model_id in &model_ids {
        let selected: Vec<&PreparedRow<'_>> = rows
            .iter()
            .filter(|row| row.source == model_id)
            .copied()
            .collect();
        let mut pooled = vec![0u32; DIMENSION];
        for row in &selected {
            for (index, value) in row.counts.iter().enumerate() {
                pooled[index] += value;
            }
        }
        let mut conditions: BTreeMap<String, usize> = BTreeMap::new();
        for row in &selected {
            *conditions.entry(row.condition_id.clone()).or_insert(0) += 1;
        }
        let family = selected.first().and_then(|row| row.family.clone());
        let family_name = selected.first().and_then(|row| row.family_name.clone());
        let provider = selected
            .iter()
            .find_map(|row| row.provider.clone())
            .or_else(|| {
                samples
                    .iter()
                    .find(|sample| &sample.source == model_id)
                    .and_then(|sample| sample.provider.clone())
            });
        let probe_suite = samples
            .iter()
            .find(|sample| &sample.source == model_id)
            .and_then(|sample| sample.probe_suite);
        models.push(FingerprintModel {
            id: model_id.clone(),
            display_name: model_id.clone(),
            family,
            family_name,
            response_count: selected.len(),
            valid_number_count: selected.iter().map(|row| row.numbers.len()).sum(),
            conditions,
            counts: pooled,
            provider,
            probe_suite,
        });
    }

    // ---- 方案 X2:按供应商分组各算一组干扰基 ----
    let mut groups: BTreeMap<String, RobustArtifacts> = BTreeMap::new();
    let mut providers: Vec<String> = Vec::new();
    for sample in samples {
        if let Some(provider) = &sample.provider
            && !providers.contains(provider)
        {
            providers.push(provider.clone());
        }
    }
    providers.sort();
    for provider in &providers {
        let group_rows: Vec<&PreparedRow<'_>> = rows
            .iter()
            .filter(|row| row.provider.as_deref() == Some(provider.as_str()))
            .copied()
            .collect();
        if group_rows.len() < 2 {
            continue;
        }
        let group_models: Vec<String> = {
            let mut ids: Vec<String> = group_rows.iter().map(|row| row.source.to_owned()).collect();
            ids.dedup();
            ids
        };
        if group_models.len() < 2 {
            continue;
        }
        groups.insert(
            provider.clone(),
            fit_robust_artifacts(&group_rows, &group_models),
        );
    }

    let recommended_queries = 3;
    Ok(FingerprintBank {
        schema: "robust-number-fingerprint-bank".to_owned(),
        built_at: chrono::Utc::now().to_rfc3339(),
        method: serde_json::json!({
            "name": "Ordered-block + nuisance-Hellinger",
            "range": [1, 355],
            "alpha": crate::fingerprint::features::ALPHA,
            "ordered_block_weight": ORDERED_BLOCK_WEIGHT,
            "parser": "longest digit run; alphabetic separators split runs; retain values 1..355",
            "response_score": "shared nuisance-Hellinger and ordered-block environment/nuisance fusion",
            "aggregation": "mean of response-wise fused class z-scores",
            "probability": "uniform-prior softmax with fusion-specific grouped cross-validation temperature scaling",
            "nuisance_rank_policy": format!(
                "adaptive: cumulative energy >= {NUISANCE_ENERGY_THRESHOLD}, rank <= min({NUISANCE_MAX_RANK}, environments)"
            ),
        }),
        recommended_queries,
        minimum_valid_numbers: MINIMUM_VALID_NUMBERS,
        source_scope: "Reference outputs enrolled for the listed model labels.".to_owned(),
        providers,
        models,
        robust,
        groups,
        calibration,
        probe_suite: None,
    })
}

/// 方案 X7:检测新增条目是否与既有条目「疑似同一模型」。
///
/// 判据:新条目的 Hellinger 中心与既有条目的中心距离过近(默认 < 0.02)。
/// 实测同模型重复入库会让 top-1 概率从 ~1.0 塌到 0.5(中位),
/// 639/648 样本跌破 0.8 —— 必须在入库前拦住。
///
/// `candidate` 是新条目拟合出的中心向量(已投影、归一);
/// `existing` 是库内各条目的中心向量。
pub fn find_duplicate(
    candidate: &[f64],
    existing: &[(String, Vec<f64>)],
    tolerance: f64,
) -> Option<String> {
    let candidate_norm = crate::fingerprint::features::norm(candidate);
    if candidate_norm <= 1e-12 {
        return None;
    }
    let mut best: Option<(String, f64)> = None;
    for (model_id, center) in existing {
        if center.len() != candidate.len() {
            continue;
        }
        let center_norm = crate::fingerprint::features::norm(center);
        if center_norm <= 1e-12 {
            continue;
        }
        // 中心距离 = 1 - 余弦相似度(两者均已归一)。
        let cosine = crate::fingerprint::features::dot(candidate, center) / (candidate_norm * center_norm);
        let distance = (1.0 - cosine).abs();
        if distance < tolerance
            && best.as_ref().is_none_or(|(_, current)| distance < *current)
        {
            best = Some((model_id.clone(), distance));
        }
    }
    best.map(|(model_id, _)| model_id)
}

/// 从库工件里取出各模型的 Hellinger 中心(供查重使用)。
pub fn model_centers(artifacts: &RobustArtifacts) -> Vec<(String, Vec<f64>)> {
    artifacts
        .model_order
        .iter()
        .cloned()
        .zip(artifacts.hellinger.centroids.iter().cloned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fingerprint::features::ORDERED_BLOCK_DIMENSION;

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
        let numbers: Vec<String> = (0..count)
            .map(|index| ((index as u32 * 7 + offset) % 355 + 1).to_string())
            .collect();
        numbers.join(", ")
    }

    #[test]
    fn combinations_matches_python_itertools() {
        let items = vec!["a", "b", "c", "d"];
        assert_eq!(
            combinations(&items, 1),
            vec![
                vec!["a"],
                vec!["b"],
                vec!["c"],
                vec!["d"]
            ]
        );
        assert_eq!(
            combinations(&items, 2),
            vec![
                vec!["a", "b"],
                vec!["a", "c"],
                vec!["a", "d"],
                vec!["b", "c"],
                vec!["b", "d"],
                vec!["c", "d"]
            ]
        );
        assert_eq!(combinations(&items, 3).len(), 4);
        assert_eq!(combinations(&items, 5).len(), 0);
    }

    #[test]
    fn rejects_single_model_banks() {
        let samples = vec![sample("m1", &synthetic(100, 0), "environment-01", "q-01")];
        assert!(matches!(
            build_bank(&samples),
            Err(BankError::NotEnoughModels(1))
        ));
    }

    #[test]
    fn rejects_empty_sample_sets() {
        assert!(matches!(build_bank(&[]), Err(BankError::NoSamples)));
    }

    #[test]
    fn builds_bank_with_calibration_and_thresholds() {
        let mut samples: Vec<BankSample> = Vec::new();
        for model in ["m1", "m2"] {
            for environment in 1..=3 {
                for challenge in 1..=3 {
                    samples.push(sample(
                        model,
                        &synthetic(300, if model == "m1" { 0 } else { 11 }),
                        &format!("environment-{environment:02}"),
                        // challenge_id 必须**全局唯一**(跨环境):分组交叉验证按
                        // challenge 整组留出,重名会把多个环境的回答一起留出。
                        &format!("e{environment:02}-q{challenge:02}"),
                    ));
                }
            }
        }
        let bank = build_bank(&samples).expect("bank should build");
        assert_eq!(bank.models.len(), 2);
        assert_eq!(bank.robust.model_order.len(), 2);
        assert!(bank.robust.hellinger.centroids[0].len() == DIMENSION);
        assert!(bank.robust.ordered_blocks.centroids[0].len() == ORDERED_BLOCK_DIMENSION);
        // 校准三档齐备,且阈值由交叉验证导出(非手写常量)。
        for key in ["1", "2", "3"] {
            let entry = bank.calibration.get(key).expect("calibration entry");
            assert!(!entry.fallback, "采集完整时不得回退");
            assert!(entry.cv_accuracy.is_some());
            assert!(entry.thresholds.is_some());
        }
    }

    #[test]
    fn derives_thresholds_from_accuracy() {
        let thresholds = derive_thresholds(1.0);
        assert!((thresholds.strong - 0.5).abs() < 1e-12);
        assert!((thresholds.moderate - 0.0).abs() < 1e-12);
        assert!((thresholds.margin - 0.5).abs() < 1e-12);
        let partial = derive_thresholds(0.9);
        assert!((partial.strong - 0.55).abs() < 1e-12);
        assert!((partial.moderate - 0.1).abs() < 1e-12);
        assert!((partial.margin - 0.5).abs() < 1e-12);
    }

    #[test]
    fn duplicate_detection_flags_identical_centers() {
        let existing = vec![("m1".to_owned(), vec![1.0, 0.0, 0.0])];
        assert_eq!(
            find_duplicate(&[1.0, 0.0, 0.0], &existing, DUPLICATE_CENTER_DISTANCE),
            Some("m1".to_owned())
        );
        assert_eq!(
            find_duplicate(&[0.0, 1.0, 0.0], &existing, DUPLICATE_CENTER_DISTANCE),
            None
        );
    }
}
