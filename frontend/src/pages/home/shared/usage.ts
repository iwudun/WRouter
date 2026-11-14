import type {
  CallLogCredentialOption,
  CallLogListFilter,
  CallLogPage,
  UsageStatsCustomRange,
  UsageStatsRange,
  UsageStatsSnapshot,
  UsageTotals
} from "@/contracts/app";

// 自定义时间窗口的线格式:"YYYY-MM-DD..YYYY-MM-DD"(本地日历日,两端都含)。
const USAGE_CUSTOM_RANGE_PATTERN = /^(\d{4}-\d{2}-\d{2})\.\.(\d{4}-\d{2}-\d{2})$/;

/** 编码自定义时间窗口(与后端 range 串同格式)。 */
export function encodeUsageCustomRange(from: string, to: string): UsageStatsCustomRange {
  return `${from}..${to}`;
}

/** 解析自定义时间窗口;不是自定义窗口则返回 undefined。 */
export function parseUsageCustomRange(range: string): { from: string; to: string } | undefined {
  const matched = USAGE_CUSTOM_RANGE_PATTERN.exec(range);
  if (!matched) {
    return undefined;
  }
  return { from: matched[1], to: matched[2] };
}

/** 自定义窗口的展示名:"10/1 – 10/7"(同一天只显示一个;与图表桶标签同风格)。 */
export function formatUsageCustomRange(range: { from: string; to: string }): string {
  const short = (key: string) => {
    const parts = key.split("-");
    return `${Number(parts[1])}/${Number(parts[2])}`;
  };
  return range.from === range.to ? short(range.from) : `${short(range.from)} – ${short(range.to)}`;
}

/** 自定义窗口跨的天数(含首尾)。 */
export function usageCustomRangeDays(range: { from: string; to: string }): number {
  const from = parseUsageDayKey(range.from);
  const to = parseUsageDayKey(range.to);
  if (!from || !to) {
    return 1;
  }
  return Math.max(1, Math.round((to.getTime() - from.getTime()) / 86_400_000) + 1);
}

/** "YYYY-MM-DD" → 本地日历日(非 UTC 解析,避免跨时区串日)。 */
export function parseUsageDayKey(key: string): Date | undefined {
  const matched = /^(\d{4})-(\d{2})-(\d{2})$/.exec(key);
  if (!matched) {
    return undefined;
  }
  const date = new Date(Number(matched[1]), Number(matched[2]) - 1, Number(matched[3]));
  return Number.isNaN(date.getTime()) ? undefined : date;
}

function positiveInteger(value: unknown): number | undefined {
  const number = Number(value);
  return Number.isFinite(number) && number > 0 ? Math.trunc(number) : undefined;
}

export function createEmptyUsageStats(range: UsageStatsRange): UsageStatsSnapshot {
  return {
    clientModels: [],
    generatedAt: new Date().toISOString(),
    modelSeries: [],
    models: [],
    providerModels: [],
    range,
    recentRequests: [],
    series: createEmptyUsageSeries(range),
    totals: emptyUsageTotals()
  };
}

export function createEmptyCallLogPage(filter: CallLogListFilter = {}): CallLogPage {
  const pageSize = positiveInteger(filter.pageSize) ?? 25;
  return {
    generatedAt: new Date().toISOString(),
    items: [],
    options: {
      credentials: [],
      models: []
    },
    page: positiveInteger(filter.page) ?? 1,
    pageSize,
    statusCounts: { all: 0, canceled: 0, completed: 0, failed: 0, pending: 0, processing: 0 },
    total: 0,
    totalRows: 0,
    totalPages: 1,
    windowSize: 0
  };
}

export function createEmptyUsageSeries(range: UsageStatsRange) {
  const now = new Date();
  if (range === "today" || range === "24h") {
    const start = new Date(now);
    start.setMinutes(0, 0, 0);
    if (range === "today") {
      start.setHours(0);
    } else {
      start.setHours(start.getHours() - 23);
    }
    const count = range === "today" ? now.getHours() + 1 : 24;
    return Array.from({ length: count }, (_, index) => {
      const date = new Date(start);
      date.setHours(start.getHours() + index);
      return {
        ...emptyUsageTotals(),
        bucket: `${date.getFullYear()}-${date.getMonth() + 1}-${date.getDate()} ${date.getHours()}`,
        label: `${String(date.getHours()).padStart(2, "0")}:00`
      };
    });
  }

  // 日桶窗口:7 天 / 30 天 / 全部(占位 30 桶)/ 自定义(按窗口天数)。
  const custom = parseUsageCustomRange(range);
  const count = custom ? usageCustomRangeDays(custom) : range === "7d" ? 7 : 30;
  const start = new Date(now);
  start.setHours(0, 0, 0, 0);
  if (custom) {
    const from = parseUsageDayKey(custom.from);
    if (from) {
      start.setFullYear(from.getFullYear(), from.getMonth(), from.getDate());
    }
  } else {
    start.setDate(start.getDate() - (count - 1));
  }
  return Array.from({ length: count }, (_, index) => {
    const date = new Date(start);
    date.setDate(start.getDate() + index);
    return {
      ...emptyUsageTotals(),
      bucket: `${date.getFullYear()}-${date.getMonth() + 1}-${date.getDate()}`,
      label: `${date.getMonth() + 1}/${date.getDate()}`
    };
  });
}

export function emptyUsageTotals(): UsageTotals {
  return {
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
    successRate: 0,
    totalTokens: 0
  };
}

// R8: formatCompactNumber / formatUsdCost / formatPercent live in @/lib/format;
// re-export here so the home barrel stays intact.
export { formatCompactNumber, formatExactNumber, formatUsdCost, formatPercent } from "@/lib/format";
import { formatCompactNumber } from "@/lib/format";

export function formatAxisNumber(value: number): string {
  return formatCompactNumber(value);
}

export function logSelectOptions(label: string, values: string[], selected: string | undefined): Array<{ label: string; value: string }> {
  const merged = new Set(values);
  if (selected) {
    merged.add(selected);
  }
  return [
    { label, value: "" },
    ...Array.from(merged).map((value) => ({ label: value, value }))
  ];
}

// Credential filters are keyed by the stable api_key id while the options
// display names — unlike logSelectOptions (value === label), the option value
// must be the id so the backend credential filter (api_key_id equality)
// matches. A selected id that fell out of the current window keeps a synthetic
// option so the control does not silently display "all".
export function credentialSelectOptions(
  label: string,
  credentials: CallLogCredentialOption[],
  selectedId: string | undefined
): Array<{ label: string; value: string }> {
  const options = [
    { label, value: "" },
    ...credentials.map((credential) => ({ label: credential.name || credential.id, value: credential.id }))
  ];
  if (selectedId && !credentials.some((credential) => credential.id === selectedId)) {
    options.push({ label: selectedId, value: selectedId });
  }
  return options;
}
