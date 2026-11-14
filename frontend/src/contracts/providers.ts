import type { ApiKeyLimitConfig, ProviderProxyOverride } from "./config";

export const BUILTIN_FUSION_TOOL_SERVER_NAME = "wr-fusion-builtins";
export const BUILTIN_FUSION_VISION_TOOL_NAME = "vision_understand";
export const BUILTIN_FUSION_WEB_SEARCH_TOOL_NAME = "web_search";

export type GatewayProviderProtocol =
  | "openai_responses"
  | "openai_chat_completions"
  | "anthropic_messages";

/** K1 (方案 20260905_02):凭据池选号策略。缺省语义 = "first",与后端
 *  Rust 宽容解析(`ProviderCredentialStrategy::parse` 非法值回退 first)对齐。
 *  2026-09-06 UI 收敛为两模式:固定密钥(写 "first",单启用约束由前端保证)
 *  与负载均衡(写 "balanced" = 优先级分层 + 层内加权);pinned/weighted/sticky
 *  保留为配置层兼容值,后端仍解析,UI 不再写入。 */
export type ProviderCredentialStrategy = "first" | "pinned" | "weighted" | "sticky" | "balanced";

/** 20261014_01:Base URL 模式。"base" = 基础 URL,请求时按所选 API 格式
 *  自动拼接端点路径;"full" = 完整 URL,原样调用,任何一层不追加/剥除/
 *  改写路径。缺省/缺字段 = "base"(存量供应商零迁移、行为不变);后端
 *  `UrlMode::parse` 宽容解析,非法值回落 base。 */
export type ProviderUrlMode = "base" | "full";

/** 20261016_03:客户端指纹模式。"passthrough" = 透传真实客户端身份(按
 *  上游线格式白名单);"preset" = 内置预设伪装(`presetId` 指向内置指纹表)。
 *  缺省/删除键 = off(存量行为零变化);后端 `ClientFingerprintOverride`
 *  宽松解析,非法结构归 off。 */
export type ProviderClientFingerprintOverride = {
  mode: "passthrough" | "preset";
  /** 仅 mode="preset" 时有意义;未知 id 由后端解析归 off、保存门禁拒绝。 */
  presetId?: string;
};

/** 内置客户端指纹预设(前端下拉数据源)。与后端
 *  `wr-domain/src/protocol/presets/*.json` 同源维护:增删预设时两侧同步;
 *  权威始终在后端(未知 id 归 off,保存门禁拒绝不兼容组合)。 */
export type ClientFingerprintPresetInfo = {
  compatibleProtocols: Array<"openai" | "openai_responses" | "anthropic">;
  id: string;
  label: string;
};

export const BUILTIN_CLIENT_FINGERPRINT_PRESETS: ClientFingerprintPresetInfo[] = [
  {
    compatibleProtocols: ["anthropic", "openai", "openai_responses"],
    id: "zcode",
    label: "ZCode"
  },
  {
    compatibleProtocols: ["anthropic"],
    id: "claude-code",
    label: "Claude Code"
  },
  {
    compatibleProtocols: ["openai_responses"],
    id: "codex",
    label: "Codex CLI"
  },
  {
    compatibleProtocols: ["anthropic", "openai"],
    id: "opencode",
    label: "OpenCode"
  }
];

export type GatewayProviderConfig = {
  /** 凭据池账号数组。后端密钥通道(secret_store 写入/注水)只处理 ``accounts``。 */
  accounts?: ProviderCredentialConfig[];
  api_base_url?: string;
  /** 20261014_01 §3.1:Base URL 模式(缺省 "base")。仅前端消费切换与
   *  保存链路;后端 Provider.url_mode 解析后由 `protocol::endpoint` 消费。
   *  等于默认值时前端摘键不落库(与 modelInputTypes 同口径)。 */
  urlMode?: ProviderUrlMode;
  /** ``null`` is the explicit "clear the stored key" marker sent to the
   * backend (empty string / absent leave the stored secret untouched). */
  api_key?: string | null;
  billing?: unknown;
  capabilities?: GatewayProviderCapability[];
  /** K1:凭据池选号策略(缺省 "first");仅 pinned 消费 ``pinnedAccountId``。 */
  credentialStrategy?: ProviderCredentialStrategy;
  /** 2026-09-06 两模式记忆:负载均衡模式下的启用密钥 id 快照。切到固定密钥
   *  模式时写入(启用集合会被单启用约束覆盖),切回负载均衡时恢复到
   *  ``accounts[].enabled``。仅前端消费;后端 ProviderRecord raw dict
   *  原样保留未知键,持久化无感。 */
  balancedEnabledAccountIds?: string[];
  /** 20261016_03:供应商级客户端指纹(出站身份层)。删除键/缺省 = off;
   *  显式出现在提交字面量(固定键集合),off 摘键不落库。 */
  clientFingerprint?: ProviderClientFingerprintOverride;
  /** K1:pinned 策略严格固定的账号 id(对应 ``ProviderAccount.id``)。 */
  pinnedAccountId?: string;
  enabled?: boolean;
  extraBody?: unknown;
  extraHeaders?: unknown;
  /** 20261016_03:自定义出站请求头(后端 `Provider.headers` 直接消费,
   *  三级优先级的显式层 —— 可覆盖指纹预设的单个键,如 UA 跟版本)。 */
  headers?: Record<string, string>;
  icon?: string;
  id?: string;
  modelContextWindow?: Record<string, number>;
  modelDescriptions?: Record<string, string>;
  modelDisplayNames?: Record<string, string>;
  /** 模型输入类型(2026-09-10):子集 of ["text","image","video","pdf"];缺省
   *  = ["text"]。仅前端消费(如 image → 列表「视觉」徽标),后端 raw dict
   *  原样保留未知键;等于默认值时前端直接摘键不落库。 */
  modelInputTypes?: Record<string, ProviderModelInputType[]>;
  modelMaxOutputTokens?: Record<string, number>;
  modelMetadata?: Record<string, ProviderModelMetadata>;
  models: string[];
  name: string;
  /** Backend-facing protocol family ("openai" | "anthropic"); the gateway
   *  reads this key (defaults to "openai") while ``type`` stays the
   *  fine-grained frontend value. Written together so the selected API
   *  format actually takes effect upstream. */
  protocol?: string;
  /** 20260906_01 §4.1c:供应商级出站代理覆盖。缺省/`inherit` = 跟随全局
   *  默认(设置页的默认策略);`direct` = 强制直连;`preset` = 走 `presetId`
   *  指向的命名预设(按 id 引用,改预设即生效)。 */
  proxy?: ProviderProxyOverride;
  provider?: string;
  transformer?: unknown;
  type?: GatewayProviderProtocol | string;
};

export type ProviderReasoningLevel = {
  description: string;
  effort: string;
};

/** 模型输入类型(2026-09-10):「文本」恒选(表单里锁定),勾选「图片」即
 *  视为视觉模型。 */
export type ProviderModelInputType = "text" | "image" | "video" | "pdf";

export type ProviderModelMetadata = {
  additionalSpeedTiers?: unknown[];
  defaultReasoningLevel?: string | null;
  defaultReasoningSummary?: string;
  serviceTiers?: unknown[];
  supportedReasoningLevels?: ProviderReasoningLevel[];
  supportsReasoningSummaries?: boolean;
};

export type ProviderCredentialConfig = {
  api_key?: string;
  enabled?: boolean;
  id?: string;
  label?: string;
  name?: string;
  limits?: ApiKeyLimitConfig;
  priority?: number;
  weight?: number;
};

export type ProviderAccountAuthMode = "provider-api-key" | "provider-api-key-raw" | "none";
export type ProviderAccountConnectorSource = "standard" | "http-json" | "plugin" | "local-estimate" | "merged" | "unsupported";
export type ProviderAccountStatus = "ok" | "warning" | "critical" | "error" | "unsupported";
export type ProviderAccountMeterKind = "balance" | "subscription" | "quota" | "time_window" | "tokens" | "requests";
export type ProviderAccountMeterUnit = "USD" | "CNY" | "hours" | "minutes" | "tokens" | "requests" | string;
export type ProviderAccountMeterWindow = "5h" | "daily" | "weekly" | "monthly" | string;
export type ProviderAccountHttpJsonParser = "grok-subscription" | "kimi-code-usages" | "new-api-key-usage" | "new-api-user-self";

export type ProviderAccountConfig = {
  connectors?: ProviderAccountConnectorConfig[];
  enabled?: boolean;
  refreshIntervalMs?: number;
};

export type ProviderAccountConnectorConfig =
  | ProviderAccountStandardConnectorConfig
  | ProviderAccountHttpJsonConnectorConfig
  | ProviderAccountPluginConnectorConfig
  | ProviderAccountLocalEstimateConnectorConfig;

export type ProviderAccountConnectorBaseConfig = {
  id?: string;
  type: ProviderAccountConnectorSource;
};

export type ProviderAccountStandardConnectorConfig = ProviderAccountConnectorBaseConfig & {
  auth?: ProviderAccountAuthMode;
  endpoint?: string;
  endpoints?: string[];
  headers?: Record<string, string>;
  type: "standard";
};

export type ProviderAccountHttpJsonConnectorConfig = ProviderAccountConnectorBaseConfig & {
  auth?: ProviderAccountAuthMode;
  body?: unknown;
  endpoint: string;
  headers?: Record<string, string>;
  mapping: ProviderAccountMappingConfig;
  method?: "GET" | "POST";
  parser?: ProviderAccountHttpJsonParser;
  type: "http-json";
};

export type ProviderAccountPluginConnectorConfig = ProviderAccountConnectorBaseConfig & {
  connectorId: string;
  options?: unknown;
  pluginId: string;
  type: "plugin";
};

export type ProviderAccountLocalEstimateConnectorConfig = ProviderAccountConnectorBaseConfig & {
  type: "local-estimate";
  windows: ProviderAccountLocalWindowConfig[];
};

export type ProviderAccountLocalWindowConfig = {
  id: string;
  label: string;
  limit: number;
  unit: "hours" | "tokens" | "requests";
  window: ProviderAccountMeterWindow;
};

export type ProviderAccountMappingConfig = {
  meters: ProviderAccountMappedMeterConfig[];
  message?: string;
  status?: string;
};

export type ProviderAccountMappedNumberExpression = number | string | Array<number | string>;
export type ProviderAccountMappedStringExpression = string | string[];

export type ProviderAccountMappedMeterConfig = {
  id: string;
  kind?: ProviderAccountMeterKind;
  label: string;
  limit?: ProviderAccountMappedNumberExpression;
  remaining?: ProviderAccountMappedNumberExpression;
  resetAt?: ProviderAccountMappedStringExpression;
  unit?: ProviderAccountMeterUnit;
  used?: ProviderAccountMappedNumberExpression;
  window?: ProviderAccountMeterWindow;
};

export type ProviderAccountMeterDetail = {
  description?: string;
  effectiveAt?: string;
  expiresAt?: string;
  id?: string;
  label?: string;
  redeemable?: boolean;
  status?: string;
};

export type ProviderAccountMeter = {
  details?: ProviderAccountMeterDetail[];
  id: string;
  kind: ProviderAccountMeterKind;
  label: string;
  limit?: number;
  remaining?: number;
  resetAt?: string;
  source?: ProviderAccountConnectorSource;
  unit: ProviderAccountMeterUnit;
  used?: number;
  window?: ProviderAccountMeterWindow;
};

export type ProviderAccountConnectorError = {
  connectorId?: string;
  message: string;
  source: ProviderAccountConnectorSource;
};

export type ProviderAccountSnapshot = {
  credentialId?: string;
  credentialLabel?: string;
  errors?: ProviderAccountConnectorError[];
  message?: string;
  meters: ProviderAccountMeter[];
  nextRefreshAt?: string;
  provider: string;
  source: ProviderAccountConnectorSource;
  status: ProviderAccountStatus;
  updatedAt: string;
};

export type ProviderAccountSnapshotRequestOptions = {
  forceRefresh?: boolean;
};

export type GatewayProviderCapability = {
  baseUrl: string;
  endpoint?: string;
  source?: "detected";
  type: GatewayProviderProtocol;
};

export type GatewayProviderDetectedProvider = "new-api";

export type GatewayProviderProbeRequest = {
  apiKey?: string;
  baseUrl: string;
  forceRefresh?: boolean;
  mode?: "connectivity" | "models" | "protocols";
  models?: string[];
  /** 20260906_01 §4.6:本次探测按该代理走(载荷无 providerId,草稿阶段也要
   *  能测,故显式传);缺省 = 回落全局默认。 */
  proxy?: ProviderProxyOverride;
  protocols?: GatewayProviderProtocol[];
  skipModelDiscovery?: boolean;
};

export type GatewayProviderProbeCandidate = {
  baseUrl: string;
  declaredProtocols?: GatewayProviderProtocol[];
  label?: string;
  protocols: GatewayProviderProtocol[];
  source: "custom";
  /** 20261014_01 §3.5:完整 URL 模式随候选透传,实探对地址原样发探测体。 */
  urlMode?: ProviderUrlMode;
};

export type GatewayProviderProbeCandidatesRequest = {
  apiKey?: string;
  candidates: GatewayProviderProbeCandidate[];
  forceRefresh?: boolean;
  mode?: "connectivity" | "models" | "protocols";
  models?: string[];
  proxy?: ProviderProxyOverride;
  protocols?: GatewayProviderProtocol[];
};

/** 20261017_01:添加模型弹窗「获取模型」——按协议拉上游全量模型列表。
 *  代理语义同探针(载荷无 providerId,显式传;缺省回落全局默认)。 */
export type GatewayProviderModelsFetchRequest = {
  apiKey?: string;
  baseUrl: string;
  protocol: GatewayProviderProtocol;
  proxy?: ProviderProxyOverride;
  urlMode?: ProviderUrlMode;
};

export type GatewayProviderModelsFetchResult = {
  durationMs?: number;
  message?: string;
  models: string[];
  ok: boolean;
  /** 本次获取实际走的出口(`direct` / `preset:<名称>@<host:port>`;
   *  凭据已掩码)。 */
  proxy?: string;
  status?: number | null;
};

export type ProviderIconDetectionRequest = {
  baseUrl: string;
  force?: boolean;
  sourceUrls?: string[];
};

export type ProviderIconDetectionResult = {
  cachedFile?: string;
  icon?: string;
  sourceUrl?: string;
};

export type GatewayProviderProbeProtocolResult = {
  baseUrl?: string;
  detectedProvider?: GatewayProviderDetectedProvider;
  /** Round-trip time of the probe request, in milliseconds. */
  durationMs?: number;
  endpoint: string;
  message: string;
  protocol: GatewayProviderProtocol;
  /** 20260906_01 §4.6:本次实探走的出口(`direct` / `system` /
   *  `preset:<名称>@<host:port>`;凭据已掩码)。 */
  proxy?: string;
  status?: number;
  supported: boolean;
  /** Token counts reported by the upstream for this probe, when present. */
  usage?: { completion_tokens?: number; prompt_tokens?: number } | null;
};

export type GatewayProviderProbeResult = {
  account?: ProviderAccountConfig;
  capabilities?: GatewayProviderCapability[];
  detectedProvider?: GatewayProviderDetectedProvider;
  detectedProtocol?: GatewayProviderProtocol;
  modelDisplayNames?: Record<string, string>;
  modelMetadata?: Record<string, ProviderModelMetadata>;
  modelSource?: "anthropic" | "gemini" | "openai";
  models: string[];
  normalizedBaseUrl: string;
  protocols: GatewayProviderProbeProtocolResult[];
};

export type GatewayProviderProbeCandidateResult = {
  candidate: GatewayProviderProbeCandidate;
  probe: GatewayProviderProbeResult;
};

export type GatewayProviderConnectivityCheckModelResult = {
  message: string;
  model: string;
  protocols: GatewayProviderProbeProtocolResult[];
  supported: boolean;
};

export type GatewayProviderConnectivityCheckRequest = {
  apiKey?: string;
  /** 20261016_03:本次实探按该客户端指纹走(preset 生效;passthrough 无
   *  入站上下文降级)。缺省 = off。 */
  clientFingerprint?: ProviderClientFingerprintOverride;
  candidates: GatewayProviderProbeCandidate[];
  forceRefresh?: boolean;
  models: string[];
  proxy?: ProviderProxyOverride;
  protocols?: GatewayProviderProtocol[];
};

export type GatewayProviderConnectivityCheckReport = {
  failed: GatewayProviderConnectivityCheckModelResult[];
  passed: GatewayProviderConnectivityCheckModelResult[];
  probe?: GatewayProviderProbeResult;
  results: GatewayProviderConnectivityCheckModelResult[];
};

/** 「管理与测速」请求：对一组地址测网络往返延迟（不带 Key、不计费）。 */
export type GatewayEndpointSpeedTestRequest = {
  /** 20260906_01 §4.6:按该供应商级代理测速;缺省 = 回落全局默认。 */
  proxy?: ProviderProxyOverride;
  timeoutSecs?: number;
  urls: string[];
};

/** 单个地址的测速结果；`latency` 为 null 表示失败（见 `error`）。 */
export type GatewayEndpointSpeedTestResult = {
  error?: string | null;
  latency: number | null;
  /** 本次测速实际走的出口(`direct` / `system` / `preset:<名称>@<host:port>`;
   *  凭据已掩码)。未发请求(地址非法)时缺省。 */
  proxy?: string;
  status?: number | null;
  url: string;
};

// --- K5/K7 (方案 20260905_02):凭据池状态 RPC -------------------------------

/** 单账号健康态。``state`` 数据源为凭据池内存态 + 配置快照(只读);冷却
 *  不持久化,重启清零。``cooldownReason`` 仅 cooling 时存在(如 "HTTP 429")。 */
export type ProviderCredentialAccountStat = {
  accountId: string;
  cooldownReason?: string;
  cooldownRemainingMs: number;
  lastStatusCode?: number;
  state: "ok" | "cooling" | "disabled";
};

export type ProviderCredentialProviderStat = {
  accounts: ProviderCredentialAccountStat[];
  providerId: string;
};

export type ProviderCredentialStats = {
  providers: ProviderCredentialProviderStat[];
};

/** `getProviderCredentialStats` 入参;缺省返回全部配置了凭据池的供应商。 */
export type ProviderCredentialStatsRequest = {
  providerId?: string;
};

export type ClearCredentialCooldownRequest = {
  accountId: string;
  providerId: string;
};

export type ClearCredentialCooldownResult = {
  accountId: string;
  cleared: boolean;
  providerId: string;
};
