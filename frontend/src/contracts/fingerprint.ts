// 模型指纹检测与指纹库管理(方案 20261030_01 阶段二 · F1)的 IPC 契约。
//
// **术语硬约束**:本文件的「模型指纹」= 用模型输出统计特征判定后端真实
// 模型身份;项目既有的「客户端指纹」(方案 20261016_03)= 出站 HTTP 头/TLS
// 身份伪装。两者无关,本文件全程**不复用** `clientFingerprint` 一词。
//
// **字段命名的两套约定**(与后端 serde 保持一致,不要统一):
// - RPC **请求/响应包装层**(handler 自己拼的 json!)= camelCase;
// - `AttributionResult` / `AttributionCandidate` / `ProbeDiagnostic` 这几个
//   **域类型**直接由 `wr-domain` 的 serde 序列化 = snakeCase。

/** 探针套件标识(方案 X9:跨套件禁止比对,后端硬拒绝)。内置参考库已下线。 */
export type FingerprintProbeSuite = "wrouter-v1";

/** 探针档位:快速 3 条 / 标准 12 条(默认)/ 完整 36 条。 */
export type FingerprintProbeTier = "quick" | "standard" | "full";

/** 证据强度四档(方案 X3)。`insufficient` 档**不显示具体模型名**(X4 弃权)。 */
export type FingerprintEvidenceStrength = "strong" | "moderate" | "weak" | "insufficient";

/** 归因候选(一个模型)。 */
export type FingerprintAttributionCandidate = {
  conditional_probability?: number;
  display_name: string;
  family?: string;
  family_name?: string;
  model: string;
  nuisance_score: number;
  probability: number;
  profile_similarity?: number;
  score: number;
};

/** 单条探针的诊断信息。 */
export type FingerprintProbeDiagnostic = {
  accepted: boolean;
  index: number;
  minimum_numbers: number;
  parsed_numbers: number;
  reason?: string;
};

/** 家族级概率。 */
export type FingerprintFamilyProbability = {
  display_name: string;
  family: string;
  probability: number;
};

/** 一段归因结论(后端 `attribution_value` 产出;失败时只有 `error`)。 */
export type FingerprintAttribution = {
  /** 归因到的模型;`insufficient` 档为 undefined(显式弃权)。 */
  prediction?: string;
  /** 归因到的模型展示名;弃权时为 undefined。 */
  prediction_name?: string;
  probability: number;
  strength: FingerprintEvidenceStrength;
  used_outputs: number;
  results: FingerprintAttributionCandidate[];
  diagnostics: FingerprintProbeDiagnostic[];
  calibration_queries: string;
  beta: number;
  cv_accuracy?: number;
  margin: number;
  family_prediction?: string;
  family_probabilities: FingerprintFamilyProbability[];
  /** 结论来源:`global` / `anchor`。 */
  source?: string;
  /** 归因失败时的原因。 */
  error?: string;
};

/** 检测诊断(整体)。 */
export type FingerprintDetectionDiagnostics = {
  probes: number;
  accepted: number;
  gaps: number;
  parseFailures: number;
  durationMs: number;
};

/** 单条探针的执行结果。 */
export type FingerprintProbeRunDiagnostic = {
  accepted: boolean;
  challengeId: string;
  conditionId: string;
  reason?: string;
};

/** 被跳过的库(跨套件 / 不可用)。 */
export type FingerprintSkippedLibrary = {
  library: string;
  reason: string;
};

/** 单个模型的检测结果。 */
export type FingerprintDetectionResult = {
  model: string;
  providerId: string;
  probeSuite: FingerprintProbeSuite;
  probeTier: FingerprintProbeTier;
  probeSeed: string;
  /** 两段结论:全局候选库 / 供应商锚点。 */
  conclusions: {
    global?: FingerprintAttribution;
    anchor?: FingerprintAttribution;
  };
  skipped: FingerprintSkippedLibrary[];
  diagnostics: FingerprintDetectionDiagnostics;
  probeDiagnostics: FingerprintProbeRunDiagnostic[];
};

/** `detectModelFingerprint` 入参。 */
export type FingerprintDetectRequest = {
  providerId: string;
  modelNames: string[];
  probeTier?: FingerprintProbeTier;
  probeSeed?: string;
};

/** `detectModelFingerprint` 出参。 */
export type FingerprintDetectResponse = {
  results: FingerprintDetectionResult[];
  probeSuite: FingerprintProbeSuite;
  probeTier: FingerprintProbeTier;
};

/** 用户自建库条目(列表页不带样本正文)。 */
export type FingerprintLibraryEntry = {
  id: string;
  kind: "global" | "anchor";
  providerId?: string;
  modelName: string;
  family?: string;
  familyName?: string;
  label: string;
  probeSuite: FingerprintProbeSuite;
  sampleCount: number;
  probeSeed?: string;
  /** 采集覆盖的环境(逗号分隔)。 */
  probeEnv?: string;
  /** 单探针交叉验证准确率(**真实值**;不可用时为 null)。 */
  cvAccuracy?: number | null;
};

/** `listModelFingerprints` 入参。 */
export type FingerprintListRequest = {
  kind?: "global" | "anchor";
  providerId?: string;
};

/** `listModelFingerprints` 出参。 */
export type FingerprintListResponse = {
  entries: FingerprintLibraryEntry[];
};

/** `enrollModelFingerprint` 入参。 */
export type FingerprintEnrollRequest = {
  providerId: string;
  modelName: string;
  family?: string;
  familyName?: string;
  label?: string;
  /** `global` = 全局候选库;`anchor` = 该供应商下的锚点。 */
  kind?: "global" | "anchor";
  probeTier?: FingerprintProbeTier;
  probeSeed?: string;
};

/** `enrollModelFingerprint` 出参。 */
export type FingerprintEnrollResponse = {
  entry: FingerprintLibraryEntry;
  /** 未采到有效回答的探针条数。 */
  gaps: number;
  probeTier: FingerprintProbeTier;
  probeSeed: string;
};

/** `deleteModelFingerprint` 入参。 */
export type FingerprintDeleteRequest = {
  id: string;
};

/** `deleteModelFingerprint` 出参。 */
export type FingerprintDeleteResponse = {
  ok: boolean;
  id: string;
};

/** `exportModelFingerprints` 入参。 */
export type FingerprintExportRequest = {
  /** 预留:按类别导出。 */
  kind?: "global" | "anchor";
};

/** `exportModelFingerprints` 出参。 */
export type FingerprintExportResponse = {
  ok: boolean;
  /** 导出文件正文(ModelTrace 兼容的 `robust-number-fingerprint-bank`)。 */
  json: string;
  modelCount: number;
  sampleCount: number;
  /** 是否带上了 ModelTrace 兼容的拟合产物段。 */
  bankIncluded: boolean;
  probeSuite: FingerprintProbeSuite;
  /** 降级原因(如库不足 2 个模型)。 */
  reason?: string | null;
};

/** `importModelFingerprint` 入参。 */
export type FingerprintImportRequest = {
  /** 导出文件正文。 */
  json: string;
};

/** 单条导入结果。 */
export type FingerprintImportItem = {
  model: string;
  id?: string;
  sampleCount?: number;
  reason?: string;
};

/** `importModelFingerprint` 出参。 */
export type FingerprintImportResponse = {
  ok: boolean;
  imported: FingerprintImportItem[];
  skipped: FingerprintImportItem[];
  probeSuite: FingerprintProbeSuite;
};
