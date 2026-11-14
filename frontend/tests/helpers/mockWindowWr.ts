import type {
  RuntimeControl,
  UsageStatsSnapshot
} from "@/contracts/app";
import {
  fallbackConfig,
  fallbackGatewayStatus,
  fallbackInfo,
  fallbackUpdateStatus
} from "@/pages/home/shared/fallbacks";
import { vi, type Mock } from "vitest";

/**
 * 20261102_01 P6:服务控制默认形态 —— systemd 托管(可停进程)、网关运行中。
 * 用例可按需覆盖 managedBy / exitAction / gatewayEnabled。
 */
const fallbackRuntimeControl: RuntimeControl = {
  canExit: true,
  canPause: true,
  exitAction: "stopService",
  gatewayEnabled: true,
  gatewayEndpoint: "http://127.0.0.1:3460",
  gatewayState: "running",
  managedBy: "systemd",
  managementEndpoint: "http://127.0.0.1:3462",
  restartHint: "systemctl --user start wrouter.service"
};

/**
 * Minimal-but-valid UsageStatsSnapshot so the usage-stats view poller
 * (useUsageStatsViewState, 5s interval) never receives null.
 */
const fallbackUsageStats: UsageStatsSnapshot = {
  clientModels: [],
  generatedAt: new Date().toISOString(),
  modelSeries: [],
  models: [],
  providerModels: [],
  range: "30d",
  recentRequests: [],
  series: [],
  totals: {
    avgDurationMs: 0,
    avgFirstByteMs: 0,
    avgDecodeTps: 0,
    cacheRatio: 0,
    cacheTokens: 0,
    costUsd: 0,
    errorCount: 0,
    inputTokens: 0,
    outputTokens: 0,
    requestCount: 0,
    successRate: 1,
    totalTokens: 0
  }
};

/**
 * Installs a controllable `window.wr` mock on the global window and returns
 * `{ wr, spies }`.
 *
 * Coverage target: every method App.tsx touches during bootstrap
 * (App.tsx:374-417), plus the two hooks that fire on mount regardless of view
 * (useUpdateFlowState: getUpdateStatus/onUpdateStatusChanged;
 * useUsageStatsViewState: getUsageStats).
 *
 * Requirements enforced by design:
 * - Subscription methods MUST return an unsubscribe function — React
 *   StrictMode double-invokes effects (cleanup + re-run), so a missing
 *   return value crashes the app during mount.
 * - `runtime: "desktop"` keeps appRuntime() on the desktop branch, avoiding
 *   Web-fallback code paths that would grey out desktop-only controls.
 * - Every method is a vi.fn() resolving to a Promise — the app's bootstrap
 *   uses `.then(...)` without await, so all RPCs must resolve (never reject)
 *   unless the test intends to exercise a `.catch()` fallback.
 *
 * Use `overrides` to flip branches, e.g. a custom getConfig stub.
 */
export function mockWindowWr(overrides: Record<string, unknown> = {}) {
  const noopUnsub = () => {};
  const asyncStub = <T>(value: T) => vi.fn(() => Promise.resolve(value));

  const base: Record<string, unknown> = {
    runtime: "desktop",
    getAppInfo: asyncStub(fallbackInfo),
    getCallLogs: asyncStub({
      generatedAt: new Date().toISOString(),
      items: [],
      options: { credentials: [], models: [] },
      page: 1,
      pageSize: 25,
      statusCounts: { all: 0, error: 0, stream: 0, success: 0 },
      total: 0,
      totalRows: 0,
      totalPages: 1,
      windowSize: 2000
    }),
    getConfig: asyncStub(fallbackConfig),
    getGatewayStatus: asyncStub(fallbackGatewayStatus),
    // 20261102_01 P6:服务控制(暂停 / 恢复 / 退出服务)。
    getRuntimeControl: asyncStub(fallbackRuntimeControl),
    setGatewayEnabled: asyncStub({ enabled: true, state: "running" }),
    stopService: asyncStub({ restartHint: fallbackRuntimeControl.restartHint, stopping: true }),
    getUpdateStatus: asyncStub(fallbackUpdateStatus),
    getUsageStats: asyncStub(fallbackUsageStats),
    onOpenSettingsRequest: vi.fn(() => noopUnsub),
    onOpenUpdateRequest: vi.fn(() => noopUnsub),
    onUpdateStatusChanged: vi.fn(() => noopUnsub)
  };

  const wr = { ...base, ...overrides };
  const spies: Record<string, Mock> = {};
  for (const [key, value] of Object.entries(wr)) {
    if (typeof value === "function") spies[key] = value as Mock;
  }

  Object.defineProperty(window, "wr", {
    value: wr,
    writable: true,
    configurable: true
  });
  return { wr, spies };
}
