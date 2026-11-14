//! 模型指纹检测(方案 20261030_01 阶段一 · 组 1)。
//!
//! **术语硬约束**:本模块的「模型指纹」= 用模型输出统计特征判定后端真实
//! 模型身份;项目既有的「客户端指纹」(方案 20261016_03)= 出站 HTTP 头/TLS
//! 身份伪装。两者无关,本模块全程**不复用** `clientFingerprint` 一词。
//!
//! 分层(方案 §3.1):
//! - [`features`] —— 数字解析 + 边缘分布/有序块特征(镜像 ModelTrace);
//! - [`linalg`] —— 对称矩阵 Jacobi 特征分解 + 正交投影(自实现,无新依赖);
//! - [`probe`] —— 探针套件(`wrouter-v1` 随机化);
//! - [`bank`] —— 库构建(中心点拟合、干扰基、分组交叉验证、温度标定);
//! - [`score`] —— 打分(融合、z-score、softmax、证据强度分级、显式弃权)。
//!
//! 纯度:本模块**无 IO、无 async**,过 `scripts/check-rust.sh` 的
//! `wr-domain` 纯度守护(禁 `tokio|axum|reqwest|rusqlite|hyper|tower`)。

pub mod bank;
pub mod features;
pub mod linalg;
pub mod probe;
pub mod score;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub use bank::{build_bank, BankError, BankSample};
pub use features::{
    ALPHA, DIMENSION, MINIMUM_VALID_NUMBERS, ORDERED_BLOCK_DIMENSION, ORDERED_BLOCK_WEIGHT,
    VALUE_MAX, VALUE_MIN,
};
pub use probe::{ProbeSpec, ProbeTier, wrouter_v1_suite};
pub use score::{
    AnalysisSample, analyze, analyze_with_group, score_numbers, score_numbers_with_group,
    ScoreComponents,
};

/// 探针套件标识(方案 X9:跨套件禁止比对)。
///
/// 当前只有 `wrouter-v1`(扩展随机化池,X5,用于用户自建库)。内置参考库与
/// `modeltrace-v1` 已下线;历史数据中的 `modeltrace-v1` 解析为 `None`,
/// 按「只支持最新格式」处理。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ProbeSuite {
    /// WRouter 自有随机化套件。
    #[serde(rename = "wrouter-v1")]
    WrouterV1,
}

impl ProbeSuite {
    /// 稳定的字符串标识(落库字段 `probe_suite`)。
    pub fn as_str(self) -> &'static str {
        match self {
            ProbeSuite::WrouterV1 => "wrouter-v1",
        }
    }

    /// 解析字符串标识。
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "wrouter-v1" => Some(ProbeSuite::WrouterV1),
            _ => None,
        }
    }
}

impl std::fmt::Display for ProbeSuite {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 库条目(一个被归因的模型)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FingerprintModel {
    /// 模型标识(标签)。
    pub id: String,
    /// 展示名。
    pub display_name: String,
    /// 模型家族(如 `gpt` / `claude`)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    /// 家族展示名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family_name: Option<String>,
    /// 采集到的回答数。
    #[serde(default)]
    pub response_count: usize,
    /// 解析出的有效数字总数。
    #[serde(default)]
    pub valid_number_count: usize,
    /// 各采集环境的回答数。
    #[serde(default)]
    pub conditions: BTreeMap<String, usize>,
    /// 汇总计数向量(355 维);用于 `profile_similarity`。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub counts: Vec<u32>,
    /// 采集通道(provider)溯源。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// 采集该条目所用的探针套件(方案 X9)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe_suite: Option<ProbeSuite>,
}

/// Hellinger 边缘分布工件。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HellingerArtifacts {
    /// 355 维特征均值。
    pub feature_mean: Vec<f64>,
    /// 355 维特征尺度。
    pub feature_scale: Vec<f64>,
    /// 干扰基秩。
    #[serde(default)]
    pub nuisance_rank: usize,
    /// 干扰基对应的环境列表。
    #[serde(default)]
    pub nuisance_environments: Vec<String>,
    /// 干扰基(行向量,单位正交)。
    #[serde(default)]
    pub nuisance_basis: Vec<Vec<f64>>,
    /// 各模型中心点(已投影掉干扰基并归一)。
    pub centroids: Vec<Vec<f64>>,
}

/// 有序块工件。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderedBlockArtifacts {
    /// 融合权重。
    pub weight: f64,
    /// 特征说明(可读性字段)。
    #[serde(default)]
    pub feature: String,
    /// 74 维特征均值。
    pub feature_mean: Vec<f64>,
    /// 74 维特征尺度。
    pub feature_scale: Vec<f64>,
    /// 干扰基秩。
    #[serde(default)]
    pub nuisance_rank: usize,
    /// 干扰基(行向量,单位正交)。
    #[serde(default)]
    pub nuisance_basis: Vec<Vec<f64>>,
    /// 各模型中心点(已投影并归一)。
    pub centroids: Vec<Vec<f64>>,
    /// 各环境 × 各模型中心点(未投影版本,用于模板项打分)。
    pub environment_centroids: Vec<Vec<Vec<f64>>>,
}

/// 稳健工件集合(一个「库组」)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RobustArtifacts {
    /// 模型顺序(与 `centroids` 行序一致)。
    pub model_order: Vec<String>,
    /// 是否具备完整环境覆盖。
    #[serde(default)]
    pub robust_ready: bool,
    /// 参与拟合的回答数。
    #[serde(default)]
    pub training_rows: usize,
    /// 完整覆盖的环境列表。
    #[serde(default)]
    pub complete_environments: Vec<String>,
    /// 边缘分布工件。
    pub hellinger: HellingerArtifacts,
    /// 有序块工件。
    pub ordered_blocks: OrderedBlockArtifacts,
}

/// 证据强度阈值(方案 X3:由混淆矩阵导出,不是手写常量)。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct EvidenceThresholds {
    /// 强证据:top1 概率下限。
    pub strong: f64,
    /// 中等证据:top1 概率下限。
    pub moderate: f64,
    /// top1−top2 间距下限(弃权判据)。
    pub margin: f64,
}

/// 校准档位(按探针条数分档)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationEntry {
    /// softmax 温度系数 β。
    pub beta: f64,
    /// 分组交叉验证准确率(**真实值**;采集不完整时不得沿用旧值)。
    #[serde(default)]
    pub cv_accuracy: Option<f64>,
    /// 交叉验证正确数。
    #[serde(default)]
    pub cv_correct: usize,
    /// 交叉验证样本数。
    #[serde(default)]
    pub cv_samples: usize,
    /// 交叉验证负对数似然。
    #[serde(default)]
    pub cv_nll: Option<f64>,
    /// 是否回退到旧值(ModelTrace 的 fallback 语义;WRouter 落库时禁止回退)。
    #[serde(default)]
    pub fallback: bool,
    /// 实测混淆矩阵(行 = 真实模型序,列 = 预测模型序)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confusion: Option<Vec<Vec<u32>>>,
    /// 证据强度阈值(由 `confusion` 导出)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thresholds: Option<EvidenceThresholds>,
}

/// 校准表(键 = 探针条数 `"1"` / `"2"` / `"3"`)。
pub type Calibration = BTreeMap<String, CalibrationEntry>;

/// 指纹库。
///
/// **两层**(方案 §3.2):
/// - L2 全局候选库:用户自建,`probe_suite = wrouter-v1`;
/// - L3 供应商锚点:用户在指定 provider 下实测,通道偏移在同供应商内抵消。
///
/// `robust` 是**全局组**;`groups` 是**分组组**(方案 X2:按供应商分组各算
/// 一组干扰基),键为分组标识(用户库用 providerId)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FingerprintBank {
    /// schema 标识。
    pub schema: String,
    /// 构建时间(ISO-8601)。
    #[serde(default)]
    pub built_at: String,
    /// 方法说明(可读性字段)。
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub method: serde_json::Value,
    /// 推荐探针条数。
    #[serde(default)]
    pub recommended_queries: usize,
    /// 单条回答接受所需的最少数字数。
    #[serde(default)]
    pub minimum_valid_numbers: usize,
    /// 数据来源说明。
    #[serde(default)]
    pub source_scope: String,
    /// 采集通道列表(溯源)。
    #[serde(default)]
    pub providers: Vec<String>,
    /// 库条目。
    pub models: Vec<FingerprintModel>,
    /// 全局组工件。
    pub robust: RobustArtifacts,
    /// 分组组工件(方案 X2);内置库为空。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub groups: BTreeMap<String, RobustArtifacts>,
    /// 校准表。
    #[serde(default)]
    pub calibration: Calibration,
    /// 整个库使用的探针套件(方案 X9)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe_suite: Option<ProbeSuite>,
}

impl FingerprintBank {
    /// 模型标识列表(与 `robust.model_order` 一致)。
    pub fn model_ids(&self) -> &[String] {
        &self.robust.model_order
    }

    /// 按探针条数取校准档位;超过 3 条时沿用 q=3(与 ModelTrace 的
    /// `min(len(valid), 3)` 行为一致)。
    pub fn calibration_for(&self, query_count: usize) -> Option<&CalibrationEntry> {
        let key = query_count.min(3).to_string();
        self.calibration.get(&key)
    }

    /// 取样本所属分组组(方案 X2);无分组时回落全局组。
    pub fn artifacts_for(&self, group: Option<&str>) -> &RobustArtifacts {
        if let Some(key) = group
            && let Some(found) = self.groups.get(key)
        {
            return found;
        }
        &self.robust
    }

    /// 库覆盖到的探针套件集合(跨套件比对由调用方硬拒绝)。
    pub fn probe_suites(&self) -> Vec<ProbeSuite> {
        let mut suites: Vec<ProbeSuite> = Vec::new();
        if let Some(suite) = self.probe_suite {
            suites.push(suite);
        }
        for model in &self.models {
            if let Some(suite) = model.probe_suite
                && !suites.contains(&suite)
            {
                suites.push(suite);
            }
        }
        suites
    }
}

/// 证据强度分级(方案 X3)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EvidenceStrength {
    /// 高置信命中。
    Strong,
    /// 较可能。
    Moderate,
    /// 倾向性结论,仅供参考。
    Weak,
    /// 证据不足,**不显示具体模型名**(方案 X4)。
    Insufficient,
}

impl EvidenceStrength {
    /// 稳定的字符串标识(前端 i18n 键的入参)。
    pub fn as_str(self) -> &'static str {
        match self {
            EvidenceStrength::Strong => "strong",
            EvidenceStrength::Moderate => "moderate",
            EvidenceStrength::Weak => "weak",
            EvidenceStrength::Insufficient => "insufficient",
        }
    }
}

/// 单条探针的诊断信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeDiagnostic {
    /// 探针序号。
    pub index: usize,
    /// 解析出的数字个数。
    pub parsed_numbers: usize,
    /// 接受所需的最少数字数。
    pub minimum_numbers: usize,
    /// 是否被接受(计入样本)。
    pub accepted: bool,
    /// 失败原因(拒答 / 截断 / 解析为空)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// 归因候选(一个模型)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttributionCandidate {
    /// 模型标识。
    pub model: String,
    /// 展示名。
    pub display_name: String,
    /// 归因概率。
    pub probability: f64,
    /// 融合后的 z-score。
    pub score: f64,
    /// 干扰项 z-score。
    pub nuisance_score: f64,
    /// 与该模型汇总分布的 JS 相似度。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_similarity: Option<f64>,
    /// 家族标识。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    /// 家族展示名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family_name: Option<String>,
    /// 家族内条件概率。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditional_probability: Option<f64>,
}

/// 归因结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttributionResult {
    /// 归因到的模型;`None` = 显式弃权(方案 X4,证据不足)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prediction: Option<String>,
    /// 归因到的模型展示名;弃权时为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prediction_name: Option<String>,
    /// top-1 概率。
    pub probability: f64,
    /// 证据强度。
    pub strength: EvidenceStrength,
    /// 采纳的有效探针数。
    pub used_outputs: usize,
    /// 候选列表(按概率降序)。
    pub results: Vec<AttributionCandidate>,
    /// 逐条诊断。
    pub diagnostics: Vec<ProbeDiagnostic>,
    /// 实际使用的校准档位。
    pub calibration_queries: String,
    /// 温度系数 β。
    pub beta: f64,
    /// 交叉验证准确率(该档位的真实值)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cv_accuracy: Option<f64>,
    /// top1 与 top2 的概率间距。
    pub margin: f64,
    /// 命中的家族(弃权时为 `None`)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family_prediction: Option<String>,
    /// 家族概率分布。
    #[serde(default)]
    pub family_probabilities: Vec<FamilyProbability>,
}

/// 家族级概率。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FamilyProbability {
    /// 家族标识。
    pub family: String,
    /// 家族展示名。
    pub display_name: String,
    /// 家族概率。
    pub probability: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_suite_round_trips() {
        let suite = ProbeSuite::WrouterV1;
        let text = serde_json::to_string(&suite).unwrap();
        assert_eq!(text, "\"wrouter-v1\"");
        assert_eq!(ProbeSuite::parse(suite.as_str()), Some(suite));
        assert_eq!(ProbeSuite::parse("modeltrace-v1"), None);
        assert_eq!(ProbeSuite::parse("unknown"), None);
    }

    #[test]
    fn evidence_strength_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&EvidenceStrength::Insufficient).unwrap(),
            "\"insufficient\""
        );
    }

    #[test]
    fn bank_round_trips_through_json() {
        // 用 JSON 字面量直接反序列化(与真实内置库/导出文件的读取路径同构),
        // 同时覆盖「缺省字段回落」:分组组/校准表缺省、套件字符串解析。
        let text = r#"{
            "schema": "robust-number-fingerprint-bank",
            "built_at": "2026-10-30T00:00:00Z",
            "recommended_queries": 3,
            "minimum_valid_numbers": 80,
            "providers": ["codex"],
            "models": [{
                "id": "m1",
                "display_name": "m1",
                "response_count": 1,
                "valid_number_count": 10,
                "counts": [],
                "provider": "codex",
                "probe_suite": "wrouter-v1"
            }],
            "robust": {
                "model_order": ["m1"],
                "robust_ready": true,
                "training_rows": 1,
                "hellinger": {
                    "feature_mean": [0.0],
                    "feature_scale": [1.0],
                    "centroids": [[0.0]]
                },
                "ordered_blocks": {
                    "weight": 0.25,
                    "feature_mean": [0.0],
                    "feature_scale": [1.0],
                    "centroids": [[0.0]],
                    "environment_centroids": []
                }
            },
            "probe_suite": "wrouter-v1"
        }"#;
        let parsed: FingerprintBank = serde_json::from_str(text).expect("内置库同构 JSON 应可解析");
        assert_eq!(parsed.models.len(), 1);
        assert_eq!(parsed.models[0].probe_suite, Some(ProbeSuite::WrouterV1));
        assert_eq!(parsed.probe_suite, Some(ProbeSuite::WrouterV1));
        assert_eq!(parsed.probe_suites(), vec![ProbeSuite::WrouterV1]);
        assert_eq!(parsed.calibration_for(5).map(|entry| entry.beta), None);
        assert_eq!(parsed.artifacts_for(None).model_order, vec!["m1".to_owned()]);
        let serialized = serde_json::to_string(&parsed).unwrap();
        let reparsed: FingerprintBank = serde_json::from_str(&serialized).unwrap();
        assert_eq!(reparsed.models[0].id, "m1");
    }
}
