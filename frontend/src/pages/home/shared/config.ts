import type {
  AppConfig,
  GatewayProviderConfig,
  ProviderProxyOverride,
  ProxyPreset,
} from "@/contracts/app";
import {
  fallbackConfig,
} from "./fallbacks";


import { normalizeApiKeys } from "./api-keys";
import { normalizeThemePreference } from "./common";
import { normalizeModelGroups } from "./model-groups";
import { normalizeRouterConfig } from "./router-fallback";

export function normalizeConfig(config: AppConfig): AppConfig {
  const router = normalizeRouterConfig(config.Router);
  const apiKeys = normalizeApiKeys(config.APIKEYS, config.APIKEY);

  return {
    ...fallbackConfig,
    ...config,
    APIKEY: apiKeys[0]?.key ?? "",
    APIKEYS: apiKeys,
    Providers: Array.isArray(config.Providers)
      ? config.Providers
      : Array.isArray((config as Record<string, unknown>).providers)
        ? (config as Record<string, unknown>).providers as GatewayProviderConfig[]
        : [],
    Router: router,
    gateway: {
      ...fallbackConfig.gateway,
      ...(config.gateway || {})
    },
    launchAtLogin: Boolean(config.launchAtLogin),
    // 聚合模型(20260905_04):与后端 parse_model_groups 同口径归一,
    // 旧配置(无 modelGroups)→ 空数组,零迁移。
    modelGroups: normalizeModelGroups(config.modelGroups),
    observability: normalizeObservabilityConfig(config.observability),
    proxy: normalizeProxyConfig(config.proxy),
    savings: normalizeSavingsConfig(config.savings),
    theme: normalizeThemePreference(config.theme),
  };
}

/** 成本节省全局配置归一(20261101_02 T3-B):与后端 SavingsConfig 同口径
 *  宽松解析 —— 非 dict/键缺失/坏值回退默认(off/关),不阻塞配置加载。 */
export function normalizeSavingsConfig(config: Partial<AppConfig["savings"]> | undefined): AppConfig["savings"] {
  const fallback = fallbackConfig.savings;
  const compressionMode = config?.compression?.mode === "auto" || config?.compression?.mode === "full"
    ? config.compression.mode
    : fallback.compression.mode;
  const intOr = (value: unknown, or: number, minimum: number): number => {
    const parsed = Number(value);
    return Number.isFinite(parsed) ? Math.max(minimum, Math.floor(parsed)) : or;
  };
  return {
    compression: {
      mode: compressionMode,
      thresholdTokens: intOr(config?.compression?.thresholdTokens, fallback.compression.thresholdTokens, 0)
    },
    prefixPin: {
      enabled: Boolean(config?.prefixPin?.enabled)
    },
    responseCache: {
      ...fallback.responseCache,
      ...(config?.responseCache || {}),
      enabled: Boolean(config?.responseCache?.enabled)
    },
    sticky: {
      enabled: Boolean(config?.sticky?.enabled),
      ttlSecs: intOr(config?.sticky?.ttlSecs, fallback.sticky.ttlSecs, 0),
      cacheColdSecs: intOr(config?.sticky?.cacheColdSecs, fallback.sticky.cacheColdSecs, 0)
    }
  };
}

export function normalizeObservabilityConfig(config: Partial<AppConfig["observability"]> | undefined): AppConfig["observability"] {
  return {
    ...fallbackConfig.observability,
    ...(config || {}),
    bodyRetentionDays: config && config.bodyRetentionDays !== undefined && Number.isFinite(Number(config.bodyRetentionDays))
      ? Math.max(0, Math.floor(Number(config.bodyRetentionDays)))
      : fallbackConfig.observability.bodyRetentionDays,
    callLogs: config && config.callLogs !== undefined
      ? Boolean(config.callLogs)
      : fallbackConfig.observability.callLogs,
    captureBodies: config && config.captureBodies !== undefined
      ? Boolean(config.captureBodies)
      : fallbackConfig.observability.captureBodies,
  };
}

export function normalizeProxyConfig(config: Partial<AppConfig["proxy"]> | undefined): AppConfig["proxy"] {
  // 20260906_01:预设列表先归一,upstream 的 presetId 要用它校验引用是否存在。
  const presets = normalizeProxyPresets(config?.presets);
  return {
    ...fallbackConfig.proxy,
    ...(config || {}),
    presets,
    targets: Array.isArray(config?.targets) ? config.targets : fallbackConfig.proxy.targets,
    testTargets: normalizeProxyTestTargets(config?.testTargets),
    upstream: normalizeProxyUpstreamConfig(config?.upstream, presets)
  };
}

/** 命名代理预设归一(方案 20260906_01 §4.1a):与后端 `parse_proxy_presets`
 *  同口径 —— id 非空、id 重复首个胜出、非法项丢弃、name 缺省回退 id。
 *  端口只取整不钳制:范围/scheme/自指属于**校验**职责(见
 *  `proxyPresetIssue`),归一化只做形态收敛,免得静默改掉用户输入。 */
export function normalizeProxyPresets(presets: Partial<ProxyPreset>[] | undefined): ProxyPreset[] {
  if (!Array.isArray(presets)) {
    return [];
  }
  const seen = new Set<string>();
  const normalized: ProxyPreset[] = [];
  for (const preset of presets) {
    const id = typeof preset?.id === "string" ? preset.id.trim() : "";
    if (!id || seen.has(id)) {
      continue;
    }
    seen.add(id);
    const name = typeof preset?.name === "string" ? preset.name.trim() : "";
    const port = Number.isFinite(Number(preset?.port)) ? Math.trunc(Number(preset.port)) : 0;
    normalized.push({
      id,
      name: name || id,
      password: typeof preset?.password === "string" ? preset.password : "",
      port,
      server: typeof preset?.server === "string" ? preset.server.trim() : "",
      username: typeof preset?.username === "string" ? preset.username.trim() : ""
    });
  }
  return normalized;
}

/** 全局默认策略归一:`presetId` 指向不存在的预设时清空 —— UI 回到「未选择」
 *  而不是显示幽灵项(运行时后端同样 warn + 直连,决策 #6)。
 *  2026-11-07:`system` / `custom` 模式退役 —— 白名单不再收录,读到旧值
 *  回落默认 `none`;后端不再迁移(旧值按未知模式直连)。 */
export function normalizeProxyUpstreamConfig(
  config: Partial<AppConfig["proxy"]["upstream"]> | undefined,
  presets: ProxyPreset[]
): AppConfig["proxy"]["upstream"] {
  const mode = config?.mode === "none" || config?.mode === "preset"
    ? config.mode
    : fallbackConfig.proxy.upstream.mode;
  const presetId = typeof config?.presetId === "string" ? config.presetId.trim() : "";
  return {
    mode,
    presetId: mode === "preset" && !presets.some((preset) => preset.id === presetId) ? "" : presetId
  };
}

/** 自定义代理检测目标归一(2026-09-13):http/https、去重保序、最多 12 条;
 *  后端 config.proxy 原样透传,这里只做形态收敛。 */
export function normalizeProxyTestTargets(value: unknown): string[] {
  if (!Array.isArray(value)) {
    return [];
  }
  const seen = new Set<string>();
  const out: string[] = [];
  for (const item of value) {
    if (typeof item !== "string") {
      continue;
    }
    const url = item.trim();
    if (!/^https?:\/\//.test(url) || seen.has(url)) {
      continue;
    }
    seen.add(url);
    out.push(url);
    if (out.length >= 12) {
      break;
    }
  }
  return out;
}

/** 供应商级代理覆盖归一(方案 20260906_01 §4.1c):与后端
 *  `ProviderProxyOverride::from_value` 同口径 —— `direct` 保留,`preset`
 *  必须命中现有预设,其余(含 `inherit`/非法结构/预设已删)→ undefined
 *  = 跟随全局默认。 */
export function normalizeProviderProxy(
  proxy: Partial<ProviderProxyOverride> | undefined,
  presets: ProxyPreset[]
): ProviderProxyOverride | undefined {
  if (proxy?.mode === "direct") {
    return { mode: "direct" };
  }
  if (proxy?.mode === "preset") {
    const presetId = typeof proxy.presetId === "string" ? proxy.presetId.trim() : "";
    return presets.some((preset) => preset.id === presetId) ? { mode: "preset", presetId } : undefined;
  }
  return undefined;
}
