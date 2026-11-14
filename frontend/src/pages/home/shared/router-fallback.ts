// 20261101_02 T3:路由规则域下线后,从原 shared/routing.ts 收缩出的
// 全局回退(fallback)归一助手 —— Router 块仅存 fallback,规则腿删除。
import { ROUTER_FALLBACK_MAX_RETRY_COUNT } from "@/contracts/app";
import type {
  RouterConfig,
  RouterFallbackConfig,
  RouterFallbackMode
} from "@/contracts/app";
import { fallbackConfig } from "./fallbacks";
import { isPlainRecord, normalizeProviderModelSelector, stringValue, uniqueStrings } from "./common";
import { clampNumber } from "./components/services";
import { routerFallbackModeOptions } from "./options";

export function normalizeRouterConfig(value: Partial<RouterConfig> | undefined): RouterConfig {
  const router = {
    ...fallbackConfig.Router,
    ...(value || {})
  } as RouterConfig & { default?: unknown };
  const { default: _legacyDefault, ...routerWithoutLegacyDefault } = router;
  return {
    ...routerWithoutLegacyDefault,
    fallback: normalizeRouterFallbackConfig((value as Record<string, unknown> | undefined)?.fallback)
  };
}

export function normalizeRouterFallbackConfig(value: Partial<RouterFallbackConfig> | unknown): RouterFallbackConfig {
  const record = isPlainRecord(value) ? value : {};
  const mode = parseRouterFallbackMode(record.mode) ?? fallbackConfig.Router.fallback.mode;
  const retryCount = clampNumber(Number(record.retryCount), 0, ROUTER_FALLBACK_MAX_RETRY_COUNT);
  const models = Array.isArray(record.models)
    ? uniqueStrings(
      record.models
        .map((model) => normalizeProviderModelSelector(stringValue(model)))
        .filter(Boolean)
    )
    : [];

  return {
    mode,
    models,
    retryCount: Number.isFinite(retryCount) ? retryCount : fallbackConfig.Router.fallback.retryCount
  };
}

export function parseRouterFallbackMode(value: unknown): RouterFallbackMode | undefined {
  if (typeof value !== "string") {
    return undefined;
  }
  const normalized = value.trim().toLowerCase();
  return routerFallbackModeOptions.some((option) => option.value === normalized)
    ? normalized as RouterFallbackMode
    : undefined;
}
