import type {
  ApiKeyConfig,
  ApiKeyLimitConfig,
  GatewayProviderConnectivityCheckModelResult,
  GatewayProviderConnectivityCheckReport,
  GatewayProviderProbeCandidate,
  GatewayProviderProbeCandidateResult,
  GatewayProviderProtocol,
  ProviderCredentialConfig,
  ProviderCredentialStrategy,
  ProviderModelInputType,
  ProviderModelMetadata,
  ProviderProxyOverride,
  ProviderUrlMode
} from "@/contracts/app";
import {
  createEmptyUsageStats,
} from "./usage";


// 2026-09-10:「设置」从独立弹窗改为主内容区常规视图(与其它页同一导航)。
// 2026-11-01:「模型指纹」从设置分区升级为主导航独立视图。
// 20261101_02 T3:「路由规则」视图下线(规则域删除,全局 savings 迁设置页)。
// 20261101_03:「快速开始」接入指引页 —— 应用默认第一页。
export type ViewId = "quickstart" | "usage-stats" | "api-keys" | "call-logs" | "call-log-detail" | "providers" | "model-fingerprints" | "settings";
export type NavigationId = ViewId;
// R8: AppLanguagePreference / ResolvedLanguage live in @/lib/language;
// re-export here so the home barrel stays intact.
export type { AppLanguagePreference, ResolvedLanguage } from "@/lib/language";
export type ResolvedTheme = "light" | "dark";
export type SettingsPageId = "general" | "appearance" | "observability" | "about";

export const fallbackUsageStats = createEmptyUsageStats("7d");

export type AddProviderDraft = {
  apiKey: string;
  /** 20261016_03:「客户端指纹」表单档位(镜像 proxyMode/proxyPresetId 的
   *  两键模式):存**原始选择**,落库时由 `fingerprintOverrideFromSelection`
   *  收敛(off / preset 未选具体预设 → 不写 clientFingerprint 键)。 */
  clientFingerprintMode: "off" | "passthrough" | "preset";
  clientFingerprintPresetId: string;
  /** 多密钥凭据池(2026-10-11 统一:添加页与详情页同一形态)。空数组 =
   *  legacy 单密钥形态(仅存量迁移前的草稿会出现);提交时 accounts 非空
   *  即写池并把供应商级 api_key 显式置 null(触发后端三态 DELETE)。 */
  accounts: ProviderCredentialConfig[];
  baseUrl: string;
  /** K1 (方案 20260905_02):新供应商默认 "first"(方案 §3.7;default-config.ts
   *  无 provider 段,集中默认点在此草稿工厂)。 */
  credentialStrategy: ProviderCredentialStrategy;
  enabled: boolean;
  icon: string;
  modelContextWindow?: Record<string, number>;
  modelDescriptions?: Record<string, string>;
  modelDisplayNames?: Record<string, string>;
  modelInputTypes?: Record<string, ProviderModelInputType[]>;
  modelMaxOutputTokens?: Record<string, number>;
  modelMetadata?: Record<string, ProviderModelMetadata>;
  modelSearch: string;
  modelsText: string;
  name: string;
  /** 固定密钥模式的单启用记忆(与 GatewayProviderConfig.pinnedAccountId
   *  同义;草稿期由密钥列表开关/策略切换同步,提交时随 accounts 一起落库)。 */
  pinnedAccountId?: string;
  protocol: GatewayProviderProtocol;
  /** 20260906_01 §4.9b:「网络代理」表单档位。存**原始选择**(而非归一后的
   *  覆盖字段),这样「选了自定义代理但预设列表为空」的空态可表达 —— 落库时
   *  由 `proxyOverrideFromSelection` 收敛(inherit/空引用 → 不写 proxy 键)。 */
  proxyMode: ProviderProxyOverride["mode"];
  proxyPresetId: string;
  selectedModels: string[];
  /** 20261014_01:Base URL 模式(§3.2 TAB 选中态;新供应商默认 "base",
  *  编辑路径由 createProviderDraftFromProvider 播种 —— 不播种会在提交时
  *  被固定键集合的字面量抹掉,full 供应商静默退回 base)。 */
  urlMode: ProviderUrlMode;
};

export type ProviderProbeCandidate = GatewayProviderProbeCandidate;
export type ProviderProbeCandidateResult = GatewayProviderProbeCandidateResult;
export type ProviderConnectivityCheckModelResult = GatewayProviderConnectivityCheckModelResult;
export type ProviderConnectivityCheckReport = GatewayProviderConnectivityCheckReport;
// 20261014_01:barrel 消费方(详情面板/列表)经此处取 URL 模式类型。
export type { ProviderUrlMode } from "@/contracts/app";
// 20261016_03:客户端指纹类型与内置预设清单(barrel 消费方经此处取)。
export type {
  ClientFingerprintPresetInfo,
  ProviderClientFingerprintOverride
} from "@/contracts/app";
export { BUILTIN_CLIENT_FINGERPRINT_PRESETS } from "@/contracts/app";

export type AddApiKeyDraft = {
  enabled: boolean;
  expirationPreset: ApiKeyExpirationPreset;
  expiresAt: string;
  limitRows: ApiKeyLimitDraftRow[];
  models: string[];
  name: string;
  quotaTokens: string;
  quotaUnlimited: boolean;
  quotaWindow: QuotaWindowChoice;
  /** 原窗口值不在已知档位内时,原样保留的毫秒数(保存时回写,不静默降级) */
  quotaWindowCustomMs?: number;
};

export type ApiKeyLimitDraftRow = {
  id: string;
  metric: ApiKeyLimitMetric;
  value: string;
  window: LimitWindowPreset;
};

export type ApiKeyLimitMetric = "requests" | "tokens";
export type ApiKeyExpirationPreset = "7d" | "30d" | "90d" | "custom" | "never";
/** per-metric 速率行的窗口档位 —— 后端只有 rpm/rph/rpd/tpm/tph/tpd 六个固定字段,不可扩档 */
export type LimitWindowPreset = "day" | "hour" | "minute";
/** 配额统计窗口档位 —— 后端 `quotaWindowMs` 是任意毫秒数,故与 per-metric 行分离 */
export type QuotaWindowPreset = "day" | "hour" | "minute" | "week" | "month";
/** "custom" = 原窗口值不在已知档位内,保存时必须原样回写 */
export type QuotaWindowChoice = QuotaWindowPreset | "custom";

export type ApiKeyListItem = {
  createdAt?: string;
  enabled: boolean;
  expiresAt?: string;
  index: number;
  key: ApiKeyConfig;
  keyValue: string;
  limits?: ApiKeyLimitConfig;
  masked: string;
  models: string[];
  name: string;
};

export type KeyValueDraftRow = {
  id: string;
  key: string;
  value: string;
};

export type AppToast = {
  id: number;
  message: string;
};

export type ServerActionBusy = "" | "cert" | "proxy";
