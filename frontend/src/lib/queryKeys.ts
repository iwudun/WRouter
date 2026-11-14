// F2a.7: central queryKey registry. Every server-state key is defined here so
// the F2b event→invalidation bridge can target caches by name without string
// literals scattered across hooks.
import type { CallLogListFilter, UsageStatsFilter, UsageStatsRange } from "@/contracts/app";

export const queryKeys = {
  appInfo: ["app", "info"] as const,
  config: ["config"] as const,
  gatewayStatus: ["status", "gateway"] as const,
  /** 20261102_01 P6:服务控制(后端自报的托管方式 / 可用动作 / 网关启停态)。 */
  runtimeControl: ["status", "runtime-control"] as const,
  /** Manual "force refresh" variant — separate key so the polled cache keeps its identity. */
  /** K7 (方案 20260905_02):凭据池健康态,按 providerId 分键。打开详情面板
   *  时拉取 + 手动刷新,不做常驻轮询(方案 §3.6)。 */
  providerCredentialStats: (providerId: string) => ["providers", "credential-stats", providerId] as const,
  callLogs: (filter: CallLogListFilter) => ["callLogs", filter] as const,
  usageStats: (range: UsageStatsRange, filter: UsageStatsFilter) => ["usage", range, filter] as const,
  savingsStats: (range: UsageStatsRange) => ["savings", range] as const,
  /** Activity calendar always reads the fixed last-30-days window (independent of range). */
  usageActivity: (filter: UsageStatsFilter) => ["usage", "activity-30d", filter] as const,
  requestLogDetail: (requestId: string) => ["requestLogDetail", requestId] as const,
  requestLogTrace: (requestId: string) => ["requestLogTrace", requestId] as const,
  routeDiagnostics: ["routing", "diagnostics"] as const
} as const;
