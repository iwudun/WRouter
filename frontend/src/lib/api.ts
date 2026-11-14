// F2a.6: 统一请求层（单一 fetcher 收口）。
//
// 迁移前：各 hook/组件直接调用 `window.wr.<method>`，window.wr 由桌面壳注入脚本
// （桌面）或 web-client-bridge（纯浏览器）注入，两条通道形状一致但
// 调用点分散。迁移后：全部查询/变更经本模块的 `api` 包装走 window.wr
// （bridge 优先且唯一），错误统一为 Error，捕获层只用 formatError。
import type {
  AppDataExportResult,
  AppConfig,
  AppSaveConfigOptions,
  CallLogListFilter,
  CallLogPage,
  GatewayEndpointSpeedTestRequest,
  GatewayEndpointSpeedTestResult,
  GatewayProviderConnectivityCheckRequest,
  GatewayProviderConnectivityCheckReport,
  GatewayProviderProbeCandidatesRequest,
  GatewayProviderProbeCandidateResult,
  GatewayProviderModelsFetchRequest,
  GatewayProviderModelsFetchResult,
  ClearCredentialCooldownRequest,
  ClearCredentialCooldownResult,
  FingerprintDeleteRequest,
  FingerprintDeleteResponse,
  FingerprintDetectRequest,
  FingerprintDetectResponse,
  FingerprintEnrollRequest,
  FingerprintEnrollResponse,
  FingerprintExportRequest,
  FingerprintExportResponse,
  FingerprintImportRequest,
  FingerprintImportResponse,
  FingerprintListRequest,
  FingerprintListResponse,
  ProviderCredentialStats,
  ProviderCredentialStatsRequest,
  ProviderAccountSnapshot,
  ProviderAccountSnapshotRequestOptions,
  ProxyPresetTestRequest,
  ProxyPresetTestResult,
  RequestLogDetailRequest,
  RequestLogDetail,
  RequestLogTracePage,
  RuntimeControl,
  SavingsStatsSnapshot,
  SetGatewayEnabledResult,
  StopServiceResult,
  UsageStatsFilter,
  UsageStatsRange,
  UsageStatsSnapshot
} from "@/contracts/app";

export function hasBridge(): boolean {
  return typeof window !== "undefined" && Boolean(window.wr);
}

async function call<T>(method: string, ...args: unknown[]): Promise<T> {
  const fn = window.wr?.[method as keyof NonNullable<Window["wr"]>] as
    | ((...fnArgs: unknown[]) => Promise<T>)
    | undefined;
  if (typeof fn !== "function") {
    throw new Error(`This action requires the WRouter backend (missing bridge method: ${method}).`);
  }
  return fn(...args);
}

// ---------------------------------------------------------------------------
// 统一 API 面（queryFn / mutationFn 的唯一数据通道）
// ---------------------------------------------------------------------------

export const api = {
  getAppInfo: () => call<import("@/contracts/app").AppInfo>("getAppInfo"),
  getConfig: () => call<AppConfig>("getConfig"),
  saveConfig: (config: AppConfig, options?: AppSaveConfigOptions) =>
    call<AppConfig>("saveConfig", config, options),
  saveApiKeys: (apiKeys: import("@/contracts/app").ApiKeyConfig[]) =>
    call<AppConfig>("saveApiKeys", apiKeys),

  getGatewayStatus: () => call<import("@/contracts/app").GatewayStatus>("getGatewayStatus"),

  // 20261102_01 P6:服务控制(暂停 / 恢复 / 退出服务)——通用能力,
  // 所有运行时走同一条管理 RPC 面。
  getRuntimeControl: () => call<RuntimeControl>("getRuntimeControl"),
  setGatewayEnabled: (enabled: boolean) => call<SetGatewayEnabledResult>("setGatewayEnabled", enabled),
  stopService: () => call<StopServiceResult>("stopService"),

  getProviderAccountSnapshots: (provider?: string, options?: ProviderAccountSnapshotRequestOptions) =>
    call<ProviderAccountSnapshot[]>("getProviderAccountSnapshots", provider, options),
  checkProviderConnectivity: (request: GatewayProviderConnectivityCheckRequest) =>
    call<GatewayProviderConnectivityCheckReport>("checkProviderConnectivity", request),
  probeProviderCandidates: (request: GatewayProviderProbeCandidatesRequest) =>
    call<GatewayProviderProbeCandidateResult | undefined>("probeProviderCandidates", request),
  // 20261017_01:添加模型弹窗「获取模型」——按协议拉上游模型列表。
  fetchProviderModels: (request: GatewayProviderModelsFetchRequest) =>
    call<GatewayProviderModelsFetchResult | undefined>("fetchProviderModels", request),
  // K5/K7 (方案 20260905_02):凭据池健康态查询与手动解冷却。
  getProviderCredentialStats: (request?: ProviderCredentialStatsRequest) =>
    call<ProviderCredentialStats>("getProviderCredentialStats", request),
  clearCredentialCooldown: (request: ClearCredentialCooldownRequest) =>
    call<ClearCredentialCooldownResult>("clearCredentialCooldown", request),
  testApiEndpoints: (request: GatewayEndpointSpeedTestRequest) =>
    call<GatewayEndpointSpeedTestResult[]>("testApiEndpoints", request),
  // 20260906_01 B5:用给定预设(含未保存的编辑值)探测固定目标。
  testProxyPreset: (request: ProxyPresetTestRequest) =>
    call<ProxyPresetTestResult>("testProxyPreset", request),

  // 20261030_01:模型指纹检测与指纹库管理(6 个管理 RPC)。
  detectModelFingerprint: (request: FingerprintDetectRequest) =>
    call<FingerprintDetectResponse>("detectModelFingerprint", request),
  listModelFingerprints: (request: FingerprintListRequest) =>
    call<FingerprintListResponse>("listModelFingerprints", request),
  enrollModelFingerprint: (request: FingerprintEnrollRequest) =>
    call<FingerprintEnrollResponse>("enrollModelFingerprint", request),
  deleteModelFingerprint: (request: FingerprintDeleteRequest) =>
    call<FingerprintDeleteResponse>("deleteModelFingerprint", request),
  exportModelFingerprints: (request: FingerprintExportRequest) =>
    call<FingerprintExportResponse>("exportModelFingerprints", request),
  importModelFingerprint: (request: FingerprintImportRequest) =>
    call<FingerprintImportResponse>("importModelFingerprint", request),

  getCallLogs: (filter?: CallLogListFilter) => call<CallLogPage>("getCallLogs", filter),
  getRequestLogDetail: (request: RequestLogDetailRequest) =>
    call<RequestLogDetail | undefined>("getRequestLogDetail", request),
  getRequestLogTrace: (request: { request_id: string; offset?: number; limit?: number; kind?: string }) =>
    call<RequestLogTracePage>("getRequestLogTrace", request),

  getUsageStats: (range?: UsageStatsRange, filter?: UsageStatsFilter) =>
    call<UsageStatsSnapshot>("getUsageStats", range, filter),

  getSavingsStats: (range?: UsageStatsRange) => call<SavingsStatsSnapshot>("getSavingsStats", range),
  getApiKeyLastUsed: () => call<Record<string, string>>("getApiKeyLastUsed"),
  exportData: () => call<AppDataExportResult>("exportData"),

  // 20261101_03:开发者工具切换(桌面 IPC,「开发」入口;Web 桥无此方法)。
  toggleDevTools: () => call<void>("toggleDevTools")
};
