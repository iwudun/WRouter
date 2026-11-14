import type {
  AppConfig,
  ApiKeyConfig,
  ApiKeyLimitConfig,
} from "@/contracts/app";


import { isPlainRecord, stringValue, uniqueStrings } from "./common";
import type { AddApiKeyDraft, ApiKeyLimitDraftRow, ApiKeyLimitMetric, ApiKeyListItem, LimitWindowPreset, QuotaWindowPreset } from "./types";

export function normalizeApiKeys(values: unknown, legacyKey?: string): ApiKeyConfig[] {
  const items = Array.isArray(values) ? values : [];
  const seen = new Set<string>();
  const result: ApiKeyConfig[] = [];
  for (const [index, value] of [...items, legacyKey ?? ""].entries()) {
    const apiKey = normalizeApiKeyConfig(value, index);
    const trimmed = apiKey?.key.trim();
    if (!apiKey || !trimmed || seen.has(trimmed)) {
      continue;
    }
    seen.add(trimmed);
    result.push({
      createdAt: apiKey.createdAt,
      ...(apiKey.enabled === false ? { enabled: false } : {}),
      ...(apiKey.expiresAt ? { expiresAt: apiKey.expiresAt } : {}),
      id: apiKey.id,
      key: trimmed,
      ...(apiKey.limits ? { limits: apiKey.limits } : {}),
      ...((apiKey.models ?? []).length ? { models: apiKey.models } : {}),
      ...(apiKey.name ? { name: apiKey.name } : {})
    });
  }
  return result;
}

export function normalizeApiKeyConfig(value: unknown, index: number): ApiKeyConfig | undefined {
  if (typeof value === "string") {
    return value.trim()
      ? {
          createdAt: new Date(0).toISOString(),
          id: `key-${index + 1}`,
          key: value.trim(),
          name: `Routing Key ${index + 1}`
        }
      : undefined;
  }
  if (!isPlainRecord(value)) {
    return undefined;
  }
  const key = stringValue(value.key) || stringValue(value.value) || stringValue(value.APIKEY);
  if (!key) {
    return undefined;
  }
  const limits = normalizeApiKeyLimits(value.limits);
  const name = stringValue(value.name);
  const models = Array.isArray(value.models)
    ? uniqueStrings(value.models.map((item) => String(item).trim()).filter(Boolean))
    : [];
  return {
    createdAt: stringValue(value.createdAt) || new Date(0).toISOString(),
    ...(value.enabled === false ? { enabled: false } : {}),
    ...(stringValue(value.expiresAt) ? { expiresAt: stringValue(value.expiresAt) } : {}),
    id: stringValue(value.id) || `key-${index + 1}`,
    key,
    ...(limits ? { limits } : {}),
    ...(models.length ? { models } : {}),
    ...(name ? { name } : {})
  };
}

export function normalizeApiKeyLimits(value: unknown): ApiKeyLimitConfig | undefined {
  if (!isPlainRecord(value)) {
    return undefined;
  }
  const limits: ApiKeyLimitConfig = {};
  for (const key of ["ipd", "iph", "ipm", "maxRequests", "maxTokens", "quotaWindowMs", "rpd", "rph", "rpm", "tpd", "tph", "tpm", "windowMs"] as const) {
    const limit = positiveInteger(value[key]);
    if (limit) {
      limits[key] = limit;
    }
  }
  return Object.keys(limits).length ? limits : undefined;
}

export function createApiKeyList(config: AppConfig): ApiKeyListItem[] {
  return normalizeApiKeys(config.APIKEYS, config.APIKEY).map((key, index) => ({
    createdAt: key.createdAt,
    enabled: key.enabled !== false,
    expiresAt: key.expiresAt,
    index,
    key,
    keyValue: key.key,
    limits: key.limits,
    masked: maskApiKey(key.key),
    models: key.models ?? [],
    name: key.name?.trim() || `Routing Key ${index + 1}`
  }));
}

export function createApiKeyDraft(): AddApiKeyDraft {
  return {
    enabled: true,
    expirationPreset: "never",
    expiresAt: toDatetimeLocalValue(addDays(new Date(), 30)),
    limitRows: [],
    models: [],
    name: "",
    quotaTokens: "",
    quotaUnlimited: true,
    quotaWindow: "day"
  };
}

export function createApiKeyEditDraft(apiKey: ApiKeyConfig): AddApiKeyDraft {
  const quotaWindowMs = apiKey.limits?.quotaWindowMs;
  const quotaWindowPreset = quotaWindowPresetFromMs(quotaWindowMs);
  return {
    enabled: apiKey.enabled !== false,
    expirationPreset: apiKey.expiresAt ? "custom" : "never",
    expiresAt: datetimeLocalValueFromIso(apiKey.expiresAt),
    limitRows: apiKeyLimitRowsFromConfig(apiKey.limits),
    models: apiKey.models ?? [],
    name: apiKey.name ?? "",
    quotaTokens: apiKey.limits?.maxTokens ? String(apiKey.limits.maxTokens) : "",
    quotaUnlimited: !apiKey.limits?.maxTokens,
    // 不在已知档位内的窗口 → "custom" + 原值,保存时原样回写(20261105 C-9)
    quotaWindow: quotaWindowPreset ?? (quotaWindowMs ? "custom" : "day"),
    quotaWindowCustomMs: quotaWindowPreset ? undefined : quotaWindowMs
  };
}

export function apiKeyMatchesQuery(apiKey: ApiKeyListItem, query: string): boolean {
  if (!query) {
    return true;
  }

  return [
    apiKey.name,
    apiKey.keyValue,
    apiKey.masked,
    apiKey.key.id,
    formatApiKeyExpiration(apiKey),
    formatApiKeyQuota(apiKey.limits)
  ].some((value) => value.toLowerCase().includes(query));
}

export function createGeneratedApiKey(draft: AddApiKeyDraft): ApiKeyConfig {
  const key = generateApiKeyValue();
  const limits = apiKeyLimitsFromDraft(draft);
  const expiresAt = expiresAtFromApiKeyDraft(draft);
  return {
    createdAt: new Date().toISOString(),
    ...(draft.enabled === false ? { enabled: false } : {}),
    ...(expiresAt ? { expiresAt } : {}),
    id: generateApiKeyId(),
    key,
    ...(limits ? { limits } : {}),
    ...(draft.models.length ? { models: [...draft.models] } : {}),
    name: draft.name.trim()
  };
}

export function updateApiKeyEditableConfig(apiKey: ApiKeyConfig, draft: AddApiKeyDraft): ApiKeyConfig {
  // The dialog only edits rate limits + the token quota. Legacy fields the UI
  // no longer exposes (image limits, custom-window maxRequests) are carried
  // over so saving an edit never silently drops them.
  const limits = apiKeyLimitsFromDraft(draft);
  const expiresAt = expiresAtFromApiKeyDraft(draft);
  const preserved: ApiKeyLimitConfig = {};
  for (const field of ["ipd", "iph", "ipm", "maxRequests", "windowMs"] as const) {
    const value = apiKey.limits?.[field];
    if (value) {
      preserved[field] = value;
    }
  }
  const mergedLimits = limits || Object.keys(preserved).length ? { ...preserved, ...limits } : undefined;
  return {
    createdAt: apiKey.createdAt,
    ...(draft.enabled === false ? { enabled: false } : {}),
    id: apiKey.id,
    key: apiKey.key,
    ...(draft.models.length ? { models: [...draft.models] } : {}),
    ...(draft.name.trim() ? { name: draft.name.trim() } : {}),
    ...(expiresAt ? { expiresAt } : {}),
    ...(mergedLimits ? { limits: mergedLimits } : {})
  };
}

export function apiKeyLimitsFromDraft(draft: AddApiKeyDraft): ApiKeyLimitConfig | undefined {
  const limits: ApiKeyLimitConfig = {};
  for (const row of draft.limitRows) {
    const value = positiveInteger(row.value);
    if (!value) {
      continue;
    }
    const field = apiKeyLimitField(row.metric, row.window);
    if (field) {
      limits[field] = value;
    }
  }
  const quotaTokens = draft.quotaUnlimited ? undefined : positiveInteger(draft.quotaTokens);
  if (quotaTokens) {
    limits.maxTokens = quotaTokens;
    // 原窗口值不在已知档位 → 原样回写;绝不静默降级成 day(20261105 C-9)
    limits.quotaWindowMs = draft.quotaWindow === "custom"
      ? draft.quotaWindowCustomMs ?? QUOTA_WINDOW_MS.day
      : quotaWindowMsFromPreset(draft.quotaWindow);
  }
  return Object.keys(limits).length ? limits : undefined;
}

export function apiKeyLimitRowsFromConfig(limits: ApiKeyLimitConfig | undefined): ApiKeyLimitDraftRow[] {
  if (!limits) {
    return [];
  }
  return [
    createApiKeyLimitDraftRow("requests", "minute", limits.rpm),
    createApiKeyLimitDraftRow("requests", "hour", limits.rph),
    createApiKeyLimitDraftRow("requests", "day", limits.rpd),
    createApiKeyLimitDraftRow("tokens", "minute", limits.tpm),
    createApiKeyLimitDraftRow("tokens", "hour", limits.tph),
    createApiKeyLimitDraftRow("tokens", "day", limits.tpd)
  ].filter((row): row is ApiKeyLimitDraftRow => Boolean(row));
}

export function createApiKeyLimitDraftRow(
  metric: ApiKeyLimitMetric = "requests",
  window: LimitWindowPreset = "minute",
  value?: number | string
): ApiKeyLimitDraftRow | undefined {
  const normalized = value === undefined || value === "" ? "" : String(value);
  if (value !== undefined && value !== "" && !positiveInteger(value)) {
    return undefined;
  }
  return {
    id: `limit_${randomBase64Url(6)}`,
    metric,
    value: normalized,
    window
  };
}

export function apiKeyLimitField(metric: ApiKeyLimitMetric, window: LimitWindowPreset): keyof ApiKeyLimitConfig | undefined {
  if (metric === "requests") {
    if (window === "minute") return "rpm";
    if (window === "hour") return "rph";
    return "rpd";
  }
  if (metric === "tokens") {
    if (window === "minute") return "tpm";
    if (window === "hour") return "tph";
    return "tpd";
  }
  return undefined;
}

export function expiresAtFromApiKeyDraft(draft: AddApiKeyDraft): string | undefined {
  if (draft.expirationPreset === "never") {
    return undefined;
  }
  if (draft.expirationPreset === "custom") {
    const date = new Date(draft.expiresAt);
    return Number.isFinite(date.getTime()) ? date.toISOString() : undefined;
  }
  const days = draft.expirationPreset === "7d" ? 7 : draft.expirationPreset === "90d" ? 90 : 30;
  return addDays(new Date(), days).toISOString();
}

export function formatApiKeyExpiration(apiKey: ApiKeyListItem): string {
  if (!apiKey.expiresAt) {
    return "Never";
  }
  const date = new Date(apiKey.expiresAt);
  if (!Number.isFinite(date.getTime())) {
    return "Invalid";
  }
  return date.toLocaleDateString(undefined, { day: "2-digit", month: "short", year: "numeric" });
}

export type ApiKeyExpirationStatus = "expired" | "expiring" | "none" | "valid";

export const API_KEY_EXPIRING_SOON_MS = 7 * 86_400_000;

export function apiKeyExpirationStatus(expiresAt: string | undefined, now = Date.now()): ApiKeyExpirationStatus {
  if (!expiresAt) {
    return "none";
  }
  const time = new Date(expiresAt).getTime();
  if (!Number.isFinite(time)) {
    return "none";
  }
  if (time <= now) {
    return "expired";
  }
  if (time <= now + API_KEY_EXPIRING_SOON_MS) {
    return "expiring";
  }
  return "valid";
}

export function formatApiKeyQuota(limits: ApiKeyLimitConfig | undefined): string {
  if (!limits?.maxTokens) {
    return "Unlimited";
  }
  return `${formatCompactTokenCount(limits.maxTokens)} tokens ${limitWindowLabel(limits.quotaWindowMs)}`;
}

function limitWindowLabel(quotaWindowMs: number | undefined): string {
  const preset = quotaWindowPresetFromMs(quotaWindowMs);
  if (preset) return `per ${preset}`;
  const custom = formatQuotaWindowDuration(quotaWindowMs);
  return custom ? `per ${custom}` : "per day";
}

export function formatCompactTokenCount(value: number): string {
  if (value >= 1_000_000) {
    const millions = value / 1_000_000;
    return `${millions >= 10 ? Math.round(millions) : Math.round(millions * 10) / 10}M`;
  }
  if (value >= 1_000) {
    return `${Math.round(value / 1_000)}K`;
  }
  return String(value);
}

export type RelativeTimeUnit = "day" | "hour" | "minute" | "month" | "year";
export type RelativeTimeValue = { unit: RelativeTimeUnit; value: number } | "now";

export function relativeTimeValue(value: string | undefined, now = Date.now()): RelativeTimeValue {
  if (!value) {
    return "now";
  }
  const time = new Date(value).getTime();
  if (!Number.isFinite(time)) {
    return "now";
  }
  const seconds = Math.max(0, Math.round((now - time) / 1000));
  if (seconds < 60) {
    return "now";
  }
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) {
    return { unit: "minute", value: minutes };
  }
  const hours = Math.round(minutes / 60);
  if (hours < 24) {
    return { unit: "hour", value: hours };
  }
  const days = Math.round(hours / 24);
  if (days < 30) {
    return { unit: "day", value: days };
  }
  const months = Math.round(days / 30);
  if (months < 12) {
    return { unit: "month", value: months };
  }
  return { unit: "year", value: Math.round(months / 12) };
}

export function relativeTimeUnitLabel(unit: RelativeTimeUnit, value: number): string {
  const plural = value === 1 ? unit : `${unit}s`;
  return `${plural} ago`;
}

/** 配额统计窗口的已知档位 ↔ 后端 `quotaWindowMs` 的毫秒值 */
export const QUOTA_WINDOW_MS: Record<QuotaWindowPreset, number> = {
  minute: 60_000,
  hour: 3_600_000,
  day: 86_400_000,
  week: 604_800_000,
  month: 2_592_000_000
};

/**
 * 已知档位 → preset。**窗口值不在已知档位内时返回 undefined(不回落到 day)**。
 *
 * 后端 `quotaWindowMs` 是任意毫秒数(`limits.rs` 直接采纳写入值),前端不是它的全集 ——
 * 若在此回落到 day,「手改配置写进去的周/月配额」会在 UI 里编辑保存时被静默改写成
 * 日配额(20261105 C-9)。不认识的档位必须原样保留,而不是猜一个。
 */
export function quotaWindowPresetFromMs(value: number | undefined): QuotaWindowPreset | undefined {
  if (value === QUOTA_WINDOW_MS.minute) return "minute";
  if (value === QUOTA_WINDOW_MS.hour) return "hour";
  if (value === QUOTA_WINDOW_MS.day) return "day";
  if (value === QUOTA_WINDOW_MS.week) return "week";
  if (value === QUOTA_WINDOW_MS.month) return "month";
  return undefined;
}

export function quotaWindowMsFromPreset(preset: QuotaWindowPreset): number {
  return QUOTA_WINDOW_MS[preset];
}

/** 自定义窗口的可读时长(7_200_000 → "2h");非整分/整小时则给毫秒 */
export function formatQuotaWindowDuration(ms: number | undefined): string {
  if (!ms || ms <= 0) return "";
  if (ms % 3_600_000 === 0) return `${ms / 3_600_000}h`;
  if (ms % 60_000 === 0) return `${ms / 60_000}m`;
  return `${ms}ms`;
}

export function positiveInteger(value: unknown): number | undefined {
  const number = Number(value);
  return Number.isFinite(number) && number > 0 ? Math.trunc(number) : undefined;
}

export async function copyTextToClipboard(value: string): Promise<void> {
  if (navigator.clipboard?.writeText) {
    try {
      await navigator.clipboard.writeText(value);
      return;
    } catch {
      // Fall back to a temporary textarea for desktop-shell/file contexts where clipboard permissions vary.
    }
  }
  const textarea = document.createElement("textarea");
  textarea.value = value;
  textarea.setAttribute("readonly", "true");
  textarea.style.left = "-9999px";
  textarea.style.position = "fixed";
  document.body.appendChild(textarea);
  textarea.select();
  document.execCommand("copy");
  textarea.remove();
}

function addDays(date: Date, days: number): Date {
  const next = new Date(date);
  next.setDate(next.getDate() + days);
  return next;
}

function toDatetimeLocalValue(date: Date): string {
  const offsetMs = date.getTimezoneOffset() * 60_000;
  return new Date(date.getTime() - offsetMs).toISOString().slice(0, 16);
}

function datetimeLocalValueFromIso(value: string | undefined): string {
  if (!value) {
    return toDatetimeLocalValue(addDays(new Date(), 30));
  }
  const date = new Date(value);
  return Number.isFinite(date.getTime()) ? toDatetimeLocalValue(date) : toDatetimeLocalValue(addDays(new Date(), 30));
}

function generateApiKeyId(): string {
  return `key_${randomBase64Url(9)}`;
}

function maskApiKey(value: string): string {
  const trimmed = value.trim();
  // Keep only a short head/tail hint — the head groups by issue batch, the
  // tail lets users eyeball-match a key in a support conversation without
  // exposing enough characters to brute-force the middle.
  if (trimmed.length <= 12) {
    return "****";
  }
  return `${trimmed.slice(0, 6)}${"*".repeat(6)}${trimmed.slice(-4)}`;
}

function generateApiKeyValue(): string {
  return `sk-${randomBase64Url(24)}`;
}

function randomBase64Url(byteLength: number): string {
  const bytes = new Uint8Array(24);
  const sizedBytes = byteLength === bytes.length ? bytes : new Uint8Array(byteLength);
  const target = sizedBytes;
  if (window.crypto?.getRandomValues) {
    window.crypto.getRandomValues(target);
  } else {
    for (let index = 0; index < target.length; index += 1) {
      target[index] = Math.floor(Math.random() * 256);
    }
  }
  const binary = Array.from(target, (byte) => String.fromCharCode(byte)).join("");
  return window.btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/g, "");
}
