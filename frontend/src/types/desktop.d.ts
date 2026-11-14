export {};

import type {
  AppConfig,
  AppDataExportResult,
  AppInfo,
  AppSaveConfigOptions,
  AppUpdateStatus,
  ApiKeyConfig,
  CallLogListFilter,
  CallLogPage,
  GatewayEndpointSpeedTestRequest,
  GatewayEndpointSpeedTestResult,
  GatewayProviderConnectivityCheckReport,
  GatewayProviderConnectivityCheckRequest,
  GatewayProviderProbeCandidateResult,
  GatewayProviderProbeCandidatesRequest,
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
  GatewayStatus,
  ProviderAccountSnapshotRequestOptions,
  ProviderAccountSnapshot,
  ProxyPresetTestRequest,
  ProxyPresetTestResult,
  RequestLogDetailRequest,
  RequestLogDetail,
  RequestLogTracePage,
  RouteDiagnostic,
  RuntimeControl,
  SavingsStatsSnapshot,
  SetGatewayEnabledResult,
  StopServiceResult,
  UsageStatsFilter,
  UsageStatsRange,
  UsageStatsSnapshot
} from "@/contracts/app";

declare global {
  interface Window {
    wr?: {
      /**
       * 运行时标志（A1）：由桌面壳(Tauri 注入脚本)注入 "desktop"，Web 桥注入 "web"。
       * 用于区分「桌面已实装 / Web 501」与「全 501」的能力，见 src/lib/runtime.ts。
       */
      runtime?: "desktop" | "web";
      /**
       * 20261101_03:桌面 shell 是否为未打包 dev 构建 —— 控制侧栏顶部
       * 「开发」徽章的可见性(20261102_01 P7-2:取代原先左下角的「开发」按钮)。
       * Web 桥不提供该字段（徽章不渲染）。
       */
      isDev?: boolean;
      /**
       * F2b.1/F2b.3: SSE 事件流连接信息。桌面壳可返回注入的
       * --wrouter-api/--wrouter-token；Web 桥不提供该字段（SSE 客户端
       * 回落同源 + sessionStorage token）。
       */
      backendConnection?: () => { apiBase: string; authToken: string };
      /**
       * 20261101_03:切换开发者工具（侧栏左下角「开发」入口）。
       * 仅桌面壳提供（Tauri 命令），Web 桥不实现；打包版壳侧会直接拒绝。
       */
      toggleDevTools?: () => Promise<void>;
      checkProviderConnectivity: (request: GatewayProviderConnectivityCheckRequest) => Promise<GatewayProviderConnectivityCheckReport>;
      exportData: () => Promise<AppDataExportResult>;
      getAppInfo: () => Promise<AppInfo>;
      getCallLogs: (filter?: CallLogListFilter) => Promise<CallLogPage>;
      getConfig: () => Promise<AppConfig>;
      getGatewayStatus: () => Promise<GatewayStatus>;
      /** 20261102_01 P6:服务控制 —— 后端自报托管方式与可执行的控制动作。 */
      getRuntimeControl: () => Promise<RuntimeControl>;
      /** 20261102_01 P6:暂停 / 恢复网关面(管理面不受影响)。 */
      setGatewayEnabled: (enabled: boolean) => Promise<SetGatewayEnabledResult>;
      /** 20261102_01 P6:优雅停机整进程(仅独立进程 / systemd 托管可用)。 */
      stopService: () => Promise<StopServiceResult>;
      getProviderAccountSnapshots: (provider?: string, options?: ProviderAccountSnapshotRequestOptions) => Promise<ProviderAccountSnapshot[]>;
      getProviderCredentialStats: (request?: ProviderCredentialStatsRequest) => Promise<ProviderCredentialStats>;
      clearCredentialCooldown: (request: ClearCredentialCooldownRequest) => Promise<ClearCredentialCooldownResult>;
      getApiKeyLastUsed: () => Promise<Record<string, string>>;
      getRequestLogDetail: (request: RequestLogDetailRequest) => Promise<RequestLogDetail | undefined>;
      getRequestLogTrace: (request: { request_id: string; offset?: number; limit?: number; kind?: string }) => Promise<RequestLogTracePage>;
      getSavingsStats: (range?: UsageStatsRange) => Promise<SavingsStatsSnapshot>;
      getUpdateStatus: () => Promise<AppUpdateStatus>;
      getUsageStats: (range?: UsageStatsRange, filter?: UsageStatsFilter) => Promise<UsageStatsSnapshot>;
      openExternal: (url: string) => Promise<void>;
      probeProviderCandidates: (request: GatewayProviderProbeCandidatesRequest) => Promise<GatewayProviderProbeCandidateResult | undefined>;
      /** 20261017_01:添加模型弹窗「获取模型」——按协议拉上游模型列表。 */
      fetchProviderModels: (request: GatewayProviderModelsFetchRequest) => Promise<GatewayProviderModelsFetchResult | undefined>;
      /** 20261030_01:模型指纹检测 —— 对指定供应商的模型发起指纹检测。 */
      detectModelFingerprint: (request: FingerprintDetectRequest) => Promise<FingerprintDetectResponse>;
      /** 20261030_01:列出用户自建库条目。 */
      listModelFingerprints: (request: FingerprintListRequest) => Promise<FingerprintListResponse>;
      /** 20261030_01:采集并入库(新增指纹)。 */
      enrollModelFingerprint: (request: FingerprintEnrollRequest) => Promise<FingerprintEnrollResponse>;
      /** 20261030_01:删除用户自建条目(内置库只读)。 */
      deleteModelFingerprint: (request: FingerprintDeleteRequest) => Promise<FingerprintDeleteResponse>;
      /** 20261030_01:导出用户自建库。 */
      exportModelFingerprints: (request: FingerprintExportRequest) => Promise<FingerprintExportResponse>;
      /** 20261030_01:导入指纹库文件。 */
      importModelFingerprint: (request: FingerprintImportRequest) => Promise<FingerprintImportResponse>;
      saveApiKeys: (apiKeys: ApiKeyConfig[]) => Promise<AppConfig>;
      saveConfig: (config: AppConfig, options?: AppSaveConfigOptions) => Promise<AppConfig>;
      setAppLanguage?: (language: "en" | "zh") => Promise<void>;
      testApiEndpoints: (request: GatewayEndpointSpeedTestRequest) => Promise<GatewayEndpointSpeedTestResult[]>;
      /** 20260906_01 B5:用给定预设(可含未保存的编辑值)探测固定目标。 */
      testProxyPreset: (request: ProxyPresetTestRequest) => Promise<ProxyPresetTestResult>;
      updateCheck: () => Promise<AppUpdateStatus>;
      updateDownload: () => Promise<AppUpdateStatus>;
      updateInstall: () => Promise<void>;
      onOpenSettingsRequest: (callback: () => void) => () => void;
      onOpenUpdateRequest: (callback: () => void) => () => void;
      onUpdateStatusChanged: (callback: (status: AppUpdateStatus) => void) => () => void;
    };
  }
}
