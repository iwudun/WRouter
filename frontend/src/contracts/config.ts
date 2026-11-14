import type { GatewayProviderConfig } from "./providers";

export type AppInfo = {
  appConfigDbFile: string;
  apiKeysDbFile: string;
  configDir: string;
  configFile: string;
  dataDir: string;
  gatewayConfigFile: string;
  launchAtLoginSupported: boolean;
  requestLogsDbFile: string;
  name: string;
  platform: string;
  usageDbFile: string;
  version: string;
};

export type AppDataExportResult = {
  canceled: boolean;
  exportedAt?: string;
  file?: string;
};

export type AppCaptureElementPngRequest = {
  borderRadius?: number;
  exportId?: string;
  fileName: string;
  output?: {
    height: number;
    width: number;
  };
  rect: {
    height: number;
    width: number;
    x: number;
    y: number;
  };
};

export type AppCaptureElementPngResult = {
  canceled: boolean;
  file?: string;
};

export type AppImageExportTargetRequest = {
  fileName: string;
};

export type AppImageExportTargetResult = {
  canceled: boolean;
  exportId?: string;
  file?: string;
};

export type AppRenderHtmlPngRequest = {
  borderRadius?: number;
  exportId?: string;
  fileName: string;
  html: string;
  output?: {
    height: number;
    width: number;
  };
  size: {
    height: number;
    width: number;
  };
};

export type AppRenderHtmlPngResult = {
  canceled: boolean;
  file?: string;
};

export type AppUpdateState =
  | "idle"
  | "checking"
  | "available"
  | "not-available"
  | "downloading"
  | "downloaded"
  | "installing"
  | "error";

export type AppUpdateDownloadProgress = {
  bytesPerSecond?: number;
  percent?: number;
  total?: number;
  transferred?: number;
};

export type AppUpdateStatus = {
  availableVersion?: string;
  canCheck: boolean;
  canDownload: boolean;
  canInstall: boolean;
  currentVersion: string;
  downloadedAt?: string;
  feedUrl?: string;
  lastCheckedAt?: string;
  lastError?: string;
  progress?: AppUpdateDownloadProgress;
  releaseDate?: string;
  releaseName?: string;
  releaseNotes?: string;
  state: AppUpdateState;
  supported: boolean;
};
// 压缩档位:off=禁用;auto=估算 token 超阈值才压;full=总是。
// (20261101_02 T3:路由规则域下线,压缩档位改为全局 savings.compression。)
export type RouterCompressionTier = "auto" | "full" | "off";

export type RouterFallbackMode = "off" | "retry" | "model-chain";

export const ROUTER_FALLBACK_MAX_RETRY_COUNT = 9999;

export type RouterFallbackConfig = {
  mode: RouterFallbackMode;
  models: string[];
  retryCount: number;
};

// 20261101_02 T3:路由规则域下线,Router 块仅存全局失败回退。
export type RouterConfig = {
  fallback: RouterFallbackConfig;
};

// ---------------------------------------------------------------------------
// 成本节省全局配置(20260905_03;20261101_02 T3-B 起为唯一配置入口,
// 规则级 savings 动作块已删)。键名与后端 SavingsConfig::to_dict 同形。
// ---------------------------------------------------------------------------

export type SavingsCompressionConfig = {
  mode: RouterCompressionTier;
  /** auto 档触发阈值(估算 token 数)。 */
  thresholdTokens: number;
};

export type SavingsResponseCacheConfig = {
  enabled: boolean;
  memoryMaxEntries: number;
  memoryMaxBytes: number;
  memoryTtlSecs: number;
  diskMaxEntries: number;
  diskTtlSecs: number;
};

export type SavingsStickyConfig = {
  /** 20261101_02 T3-B:会话粘性全局开关(规则域下线后的新契约)。 */
  enabled: boolean;
  ttlSecs: number;
  /**
   * 阶段五 C-1:粘住条目的「冷判定」窗口(秒)。绑定存活超过该窗口且
   * 一次 cache_read 都没有 → 判定该绑定不值,解绑落正常链。0 = 关闭。
   */
  cacheColdSecs: number;
};

export type SavingsPrefixPinConfig = {
  enabled: boolean;
};

export type SavingsRuntimeConfig = {
  compression: SavingsCompressionConfig;
  prefixPin: SavingsPrefixPinConfig;
  responseCache: SavingsResponseCacheConfig;
  sticky: SavingsStickyConfig;
};

export type GatewayRuntimeConfig = {
  enabled: boolean;
  generatedConfigFile: string;
  host: string;
  port: number;
};

export type ProxyMode = "gateway" | "transparent";

/** 出站代理默认策略(方案 20260906_01 §4.1b)。旧 `custom`(内联单代理)
 *  与 `system`(读环境变量)已于 2026-11-07 退役:后端不再迁移,读到旧值
 *  按未知模式直连。当前只保留 `none` 与命名预设 `preset`。 */
export type ProxyUpstreamMode = "none" | "preset";

/** 命名代理预设(方案 20260906_01 §4.1a):设置页统一管理的「自定义代理」,
 *  全局默认与供应商都**按 id 引用** —— 改预设即对所有引用方生效(决策 #2)。
 *  字段形态沿用旧 `upstream.custom`,复用同一套 URL 构造与端口补全逻辑。 */
export type ProxyPreset = {
  id: string;
  name: string;
  password: string;
  port: number;
  server: string;
  username: string;
};

export type ProxyUpstreamConfig = {
  mode: ProxyUpstreamMode;
  /** `mode === "preset"` 时指向 `presets[].id`;其余模式为空串。 */
  presetId: string;
};

/** RPC `testProxyPreset` 入参:被测预设本身(可以是设置页里尚未保存的
 *  编辑中值 —— 测试必须反映用户正在填的地址,而不是库里的旧值)。 */
export type ProxyPresetTestRequest = {
  preset: ProxyPreset;
  /** 自定义检测目标(http/https);缺省用后端内置列表。 */
  targets?: string[];
};

/** 单个固定目标的探测结果;`ok` = 拿到任何 HTTP 响应(代理转发成功,
 *  204/404 都算通),连不上代理才是 false。 */
export type ProxyPresetTestTargetResult = {
  error?: string | null;
  latencyMs?: number | null;
  ok: boolean;
  status?: number | null;
  url: string;
};

export type ProxyPresetTestResult = {
  ok: boolean;
  /** 掩码后的代理来源标签(`preset:<名称>@<host:port>`),供 UI 回显。 */
  proxy: string;
  targets: ProxyPresetTestTargetResult[];
};

/** 供应商级代理覆盖(方案 20260906_01 §4.1c;三态映射)。
 *  缺省/`inherit` = 跟随全局默认;后端把两者归一为同一语义。 */
export type ProviderProxyOverride = {
  mode: "direct" | "inherit" | "preset";
  presetId?: string;
};

export type ProxyRouteTarget = {
  host: string;
  paths?: string[];
};

export type ProxyRuntimeConfig = {
  browserMode: boolean;
  host: string;
  mode: ProxyMode;
  port: number;
  presets: ProxyPreset[];
  targets: ProxyRouteTarget[];
  /** 自定义代理检测目标(http/https,最多 12 条);设置页代理章节管理,
   *  后端 config.proxy 原样透传落盘(2026-09-13)。 */
  testTargets?: string[];
  upstream: ProxyUpstreamConfig;
};

export type ObservabilityConfig = {
  callLogs: boolean;
  captureBodies: boolean;
  bodyRetentionDays: number;
};

export type ApiKeyLimitConfig = {
  ipd?: number;
  iph?: number;
  ipm?: number;
  maxRequests?: number;
  maxTokens?: number;
  quotaWindowMs?: number;
  rpd?: number;
  rph?: number;
  rpm?: number;
  tpd?: number;
  tph?: number;
  tpm?: number;
  windowMs?: number;
};

export type ApiKeyConfig = {
  createdAt: string;
  /** Missing/undefined means enabled (legacy configs predate the flag). */
  enabled?: boolean;
  expiresAt?: string;
  id: string;
  key: string;
  limits?: ApiKeyLimitConfig;
  /** Allowed gateway model ids; missing/empty means all models. */
  models?: string[];
  name?: string;
};

export type ProxySystemStatus = {
  lastError?: string;
  state: "active" | "error" | "inactive" | "restored" | "unsupported";
  upstream?: string;
};

export type ProxyCertificateTrustState = "missing" | "trusted" | "unknown" | "unsupported" | "untrusted";

export type ProxyCertificateStatus = {
  caCertFile: string;
  caFingerprintSha256?: string;
  canInstall: boolean;
  message: string;
  platform: string;
  state: ProxyCertificateTrustState;
  trusted: boolean;
};

// 自定义模型聚合(方案 20260905_04 §3.1):一个对外聚合名关联多个
// (供应商, 模型) 目标,组内按策略负载均衡;聚合名可被客户端直接调用。
// 契约与代码统一 modelGroups/ModelGroup(产品/UI 文案用「聚合模型」)。
export type ModelGroupStrategy = "first" | "sticky" | "weighted";

export type ModelGroupTarget = {
  enabled?: boolean;
  model: string;
  priority?: number;
  providerId: string;
  weight?: number;
};

export type ModelGroupConfig = {
  enabled?: boolean;
  id: string;
  name: string;
  strategy?: ModelGroupStrategy;
  targets: ModelGroupTarget[];
};

export type AppConfig = {
  APIKEY: string;
  APIKEYS: ApiKeyConfig[];
  HOST: string;
  PORT: number;
  Providers: GatewayProviderConfig[];
  Router: RouterConfig;
  autoStart: boolean;
  gateway: GatewayRuntimeConfig;
  launchAtLogin: boolean;
  /** 聚合模型定义(缺失 = 空,旧配置零迁移)。 */
  modelGroups?: ModelGroupConfig[];
  observability: ObservabilityConfig;
  preferredProvider: string;
  proxy: ProxyRuntimeConfig;
  routerEndpoint: string;
  /** 成本节省全局配置(20260905_03;旧配置缺失 → 默认全关)。 */
  savings: SavingsRuntimeConfig;
  theme: "system" | "light" | "dark";
};

export type AppSaveConfigOptions = {
};

export type GatewayNetworkEndpoint = {
  address: string;
  interfaceName: string;
  endpoint: string;
};

export type GatewayStatus = {
  coreManagedExternally?: boolean;
  endpoint: string;
  generatedConfigFile: string;
  /** 20261102_01 P6:网关面(3460)是否处于启用态 —— false = 已暂停。 */
  gatewayEnabled?: boolean;
  lastError?: string;
  lastStartedAt?: string;
  networkEndpoints: GatewayNetworkEndpoint[];
  pid?: number;
  state: "stopped" | "starting" | "running" | "error";
};

// ---------------------------------------------------------------------------
// 服务控制(20261102_01 P6):暂停 / 恢复 / 退出服务
// ---------------------------------------------------------------------------
//
// 通用能力:所有运行时(Tauri 内嵌 / 独立进程 / systemd)走同一套管理 RPC,
// 前端不按运行时硬编码分支 —— 能力由后端自报(`getRuntimeControl`)。

/** 后端的托管方式:内嵌于桌面壳 / 独立进程 / systemd 托管。 */
export type RuntimeControlManagedBy = "embedded" | "process" | "systemd";

/** 退出动作:停掉后端进程 / 只关窗口(后端随之退出)。 */
export type RuntimeControlExitAction = "stopService" | "exitApp";

/** 网关面状态:运行中 / 已暂停 / 启动中 / 异常。 */
export type GatewayControlState = "running" | "stopped" | "starting" | "error";

export type RuntimeControl = {
  /** 是否允许从页面暂停/恢复网关面。 */
  canPause: boolean;
  /** 是否允许从页面退出(内嵌模式下为 false —— 页面停不掉同进程后端)。 */
  canExit: boolean;
  /** 网关面错误描述(正常时为 null)。 */
  gatewayError?: string | null;
  /** 网关面地址(如 http://127.0.0.1:3460)。 */
  gatewayEndpoint: string;
  /** 网关面当前状态。 */
  gatewayState: GatewayControlState;
  /** 网关面是否启用(false = 已暂停,客户端连不上)。 */
  gatewayEnabled: boolean;
  /** 后端托管方式。 */
  managedBy: RuntimeControlManagedBy;
  /** 管理面地址(如 http://127.0.0.1:3462)。 */
  managementEndpoint: string;
  /** 退出后如何重新拉起(如 systemctl --user start wrouter.service)。 */
  restartHint: string;
  /** 退出动作类型。 */
  exitAction: RuntimeControlExitAction;
  /**
   * 停机**已受理但进程尚未退出**(2026-11-03)。
   *
   * `true` 只表示「停机请求已被后端接受」,**不代表进程已经退出** ——
   * 页面据此显示「正在停止」,而不是把「已受理」当成「已退出」。
   */
  stopping?: boolean;
};

export type SetGatewayEnabledResult = {
  enabled: boolean;
  state: GatewayControlState;
};

export type StopServiceResult = {
  restartHint: string;
  stopping: boolean;
};

/**
 * 用量/节省统计的时间范围键。
 *
 * - 预设:`today`(今天)/ `7d` / `30d` / `all`(全部)
 * - 自定义:`YYYY-MM-DD..YYYY-MM-DD` —— 本地日历日,两端都含;线格式与
 *   后端一致(后端按该窗口取数,上界语义是「末日次日零点」)
 * - `24h`:旧版遗留(后端仍认),前端不再提供入口
 */
export type UsageStatsPresetRange = "today" | "7d" | "30d" | "all";
export type UsageStatsCustomRange = `${string}..${string}`;
export type UsageStatsRange = UsageStatsPresetRange | UsageStatsCustomRange | "24h";

export type UsageStatsFilter = {
  /**
   * 20261104_01 阶段三 · B-1a:调用方标识(原始 UA 串)精确过滤。
   * 后端按写入时刻的原始 UA 串比对(原始行与卷积行同口径)。
   */
  client?: string;
  credential?: string;
  includeProxy?: boolean;
  model?: string;
  provider?: string;
};

export type UsageTotals = {
  avgDurationMs: number;
  avgFirstByteMs: number;
  avgDecodeTps: number;
  cacheRatio: number;
  cacheTokens: number;
  costUsd: number;
  errorCount: number;
  inputTokens: number;
  outputTokens: number;
  /** 阶段四 C-8:首字延迟的 P50(毫秒;分桶上界,无样本时 0)。 */
  p50FirstByteMs?: number;
  /** 阶段四 C-8:首字延迟的 P95(毫秒;分桶上界,无样本时 0)。 */
  p95FirstByteMs?: number;
  /** 阶段四 C-8:解码速度的 P50(token/秒;分桶上界,无样本时 0)。 */
  p50DecodeTps?: number;
  /** 阶段四 C-8:解码速度的 P95(token/秒;分桶上界,无样本时 0)。 */
  p95DecodeTps?: number;
  requestCount: number;
  successRate: number;
  totalTokens: number;
};

export type UsageSeriesPoint = UsageTotals & {
  bucket: string;
  label: string;
};

export type UsageModelSeriesEntry = {
  model: string;
  points: UsageSeriesPoint[];
};

export type UsageComparisonRow = UsageTotals & {
  caption: string;
  client?: string;
  credential?: string;
  credentialId?: string;
  key: string;
  label: string;
  logicalModel?: string;
  maxShare: number;
  model?: string;
  provider?: string;
};

export type UsageStatsSnapshot = {
  clientModels: UsageComparisonRow[];
  generatedAt: string;
  modelSeries: UsageModelSeriesEntry[];
  models: UsageComparisonRow[];
  providerModels: UsageComparisonRow[];
  range: UsageStatsRange;
  recentRequests: UsageComparisonRow[];
  series: UsageSeriesPoint[];
  totals: UsageTotals;
};

// 成本节省快照(getSavingsStats,方案 20260905_03 §4.6)。金额均为估算
// (billing 单价折算),UI 需标注「估算」。
export type SavingsEngineStat = {
  applied: number;
  charsSaved: number;
  id: string;
};

export type SavingsSkipStat = {
  count: number;
  reason: string;
};

export type SavingsCompressionStats = {
  engines: SavingsEngineStat[];
  requests: number;
  skips: SavingsSkipStat[];
  tokensSaved: number;
};

export type SavingsResponseCacheStats = {
  hits: number;
  tokensSaved: number;
};

export type SavingsUpstreamCacheStats = {
  cacheReadTokens: number;
  estimatedUsdSaved: number;
};

export type SavingsStatsSnapshot = {
  compression: SavingsCompressionStats;
  period: UsageStatsRange | string;
  responseCache: SavingsResponseCacheStats;
  upstreamCache: SavingsUpstreamCacheStats;
};

