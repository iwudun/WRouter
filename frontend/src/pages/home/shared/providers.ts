import type {
  AppConfig,
  ClientFingerprintPresetInfo,
  GatewayProviderConfig,
  GatewayProviderModelsFetchResult,
  GatewayProviderProbeResult,
  GatewayProviderProtocol,
  ProviderClientFingerprintOverride,
  ProviderCredentialConfig,
  ProviderModelInputType,
  ProviderModelMetadata,
  ProviderProxyOverride,
} from "@/contracts/app";
import { providerUrlWithDefaultScheme } from "@/lib/url";
import { providerProtocolOptions } from "./options";

import { normalizeProviderModelSelector } from "./common";
import { isModelGroupEnabled, RESERVED_PROVIDER_SLUG, type ModelGroupConfig } from "./model-groups";
import { probeProxyPayload, proxyOverrideFromSelection, proxySelectionFromOverride } from "./proxy-presets";
import type { AddProviderDraft, ProviderProbeCandidate, ProviderProbeCandidateResult, ViewId } from "./types";

export function createRouteModelOptions(
  providers: GatewayProviderConfig[],
  modelGroups: ModelGroupConfig[] = []
): Array<{ label: string; value: string }> {
  const providerOptions = providers.flatMap((provider) => {
    if (!provider.name || !Array.isArray(provider.models)) {
      return [];
    }
    return provider.models
      .filter(Boolean)
      .map((model) => ({
        label: `${provider.name}/${providerModelDisplayName(provider, model)}`,
        value: `${provider.name}/${model}`
      }));
  });
  // 聚合模型以 `wrouter/<聚合名>` 前缀入列(命名空间见 isReservedProviderName),
  // 后端 resolve 有对应前缀分支,供回退链等目标下拉直接选用。
  const groupOptions = modelGroups
    .filter(isModelGroupEnabled)
    .map((group) => ({
      label: `${RESERVED_PROVIDER_SLUG}/${group.name}`,
      value: `${RESERVED_PROVIDER_SLUG}/${group.name}`
    }));
  return [...providerOptions, ...groupOptions];
}

export function routeTargetOptions(modelOptions: Array<{ label: string; value: string }>, value: string): Array<{ label: string; value: string }> {
  const normalizedValue = normalizeProviderModelSelector(value);
  const options = [{ label: "Unset", value: "" }, ...modelOptions];
  if (normalizedValue && !options.some((option) => option.value === normalizedValue)) {
    return [{ label: normalizedValue, value: normalizedValue }, ...options];
  }
  return options;
}

// i18n key for the request-URL field hint in base mode (English text doubles
// as the en copy; the zh translation lives in i18n.tsx under this exact key).
export const providerBaseUrlHintKey =
  "· Root URL (recommended): e.g. https://api.example.com/v1\n  The endpoint is appended automatically based on the selected API format\n· A full endpoint matching the selected format is called as-is\n· Endpoint on a custom path: switch to Full URL mode";

// i18n key for the full-URL-mode hint (20261014_01 §3.2): shown when the
// Base URL section TAB is on 「完整 URL」.
export const providerFullUrlHintKey =
  "This address is called exactly as entered: no path is appended, stripped or rewritten.\nThe request body still follows the selected API format.";

export function providerDisplayIcon(provider: GatewayProviderConfig): string {
  // Providers render the generic Box placeholder unless they carry an explicit
  // icon URL. Returning "" makes the icon component fall back to the Box.
  return provider.icon?.trim() ?? "";
}

export function createProviderDraft(providers: GatewayProviderConfig[]): AddProviderDraft {
  return {
    accounts: [],
    apiKey: "",
    baseUrl: "",
    credentialStrategy: "first",
    enabled: true,
    icon: "",
    modelContextWindow: undefined,
    modelDescriptions: undefined,
    modelDisplayNames: undefined,
    modelInputTypes: undefined,
    modelMaxOutputTokens: undefined,
    modelMetadata: undefined,
    modelSearch: "",
    modelsText: "",
    name: "",
    protocol: "openai_chat_completions",
    // 20260906_01:新供应商默认跟随全局代理策略。
    proxyMode: "inherit",
    proxyPresetId: "",
    selectedModels: [],
    // 20261016_03:新供应商默认关闭客户端指纹(存量行为零变化)。
    clientFingerprintMode: "off",
    clientFingerprintPresetId: "",
    // 20261014_01:新供应商默认基础 URL 模式(自动拼接端点,现状行为)。
    urlMode: "base"
  };
}

export function createProviderDraftFromProvider(provider: GatewayProviderConfig): AddProviderDraft {
  const baseUrl = providerBaseUrl(provider);
  const protocol = toProviderProtocol(provider.type) ?? toProviderProtocol(provider.provider) ?? "openai_chat_completions";
  // 20260906_01:读回该供应商的代理覆盖 —— 存量供应商的连通性检查/测速
  // 必须按它真实会走的出口探测(决策 #9)。
  const proxySelection = proxySelectionFromOverride(provider.proxy);
  // 2026-10-11 统一多密钥形态:草稿一律以凭据池为密钥来源。存量池原样
  // 播种(含明文,get_config 已注水);纯 legacy 单密钥供应商在此迁入首行
  // (对齐详情面板 migrateProviderLegacyKey 的「建行 + 清 legacy」语义,
  // 提交时 api_key 显式 null 会真正删掉 secrets 里的影子键)。
  const seededAccounts = providerCredentialAccounts(provider).map((account) => ({ ...account }));
  const legacyKey = providerApiKey(provider).trim();
  if (seededAccounts.length === 0 && legacyKey) {
    seededAccounts.push({
      ...createProviderAccount([]),
      api_key: legacyKey,
      enabled: true
    });
  }
  return {
    accounts: seededAccounts,
    apiKey: "",
    baseUrl,
    credentialStrategy: provider.credentialStrategy ?? "first",
    enabled: providerIsEnabled(provider),
    icon: provider.icon ?? "",
    modelContextWindow: modelContextWindowForModels(provider.modelContextWindow, provider.models),
    modelDescriptions: modelDescriptionsForModels(provider.modelDescriptions, provider.models),
    modelDisplayNames: modelDisplayNamesForModels(provider.modelDisplayNames, provider.models),
    modelInputTypes: modelInputTypesForModels(provider.modelInputTypes, provider.models),
    modelMaxOutputTokens: modelMaxOutputTokensForModels(provider.modelMaxOutputTokens, provider.models),
    modelMetadata: modelMetadataForModels(provider.modelMetadata, provider.models),
    modelSearch: "",
    modelsText: provider.models.join("\n"),
    name: provider.name,
    pinnedAccountId: provider.pinnedAccountId,
    protocol,
    proxyMode: proxySelection.mode,
    proxyPresetId: proxySelection.presetId,
    selectedModels: [],
    // 20261016_03:编辑路径必须播种客户端指纹 —— 提交对象是固定键集合的
    // 新字面量,漏播种会在保存一次后把已配置的指纹静默抹回 off。
    ...clientFingerprintSelectionFromOverride(provider.clientFingerprint),
    // 20261014_01:编辑路径必须播种 URL 模式 —— 提交对象是固定键集合的
    // 新字面量,漏播种会把 full 供应商静默退回 base(保存即抹掉的坑)。
    urlMode: provider.urlMode === "full" ? "full" : "base"
  };
}

// 20261016_03:客户端指纹(草稿两键 ↔ 落库覆盖字段;镜像 proxy 同名函数)。

/** 前端 API 格式 → 后端线格式口径(预设 compatibleProtocols 的取值域)。 */
export function providerWireProtocol(protocol: GatewayProviderProtocol): "anthropic" | "openai" | "openai_responses" {
  if (protocol === "anthropic_messages") {
    return "anthropic";
  }
  return protocol === "openai_responses" ? "openai_responses" : "openai";
}

/** 表单选择 → 落库的覆盖字段。`off`、或选了「内置预设」但还没选到具体
 *  预设(presetId 空)时返回 undefined = 不写 `clientFingerprint` 键,
 *  与后端 `ClientFingerprintOverride::from_value` 的 None 归一一致。 */
export function clientFingerprintOverrideFromSelection(
  mode: AddProviderDraft["clientFingerprintMode"],
  presetId: string
): ProviderClientFingerprintOverride | undefined {
  if (mode === "passthrough") {
    return { mode: "passthrough" };
  }
  const trimmed = presetId.trim();
  return mode === "preset" && trimmed ? { mode: "preset", presetId: trimmed } : undefined;
}

/** 覆盖字段 → 表单选择(编辑已存供应商时的初值)。 */
export function clientFingerprintSelectionFromOverride(
  override: ProviderClientFingerprintOverride | undefined
): { clientFingerprintMode: AddProviderDraft["clientFingerprintMode"]; clientFingerprintPresetId: string } {
  const mode = override?.mode === "passthrough"
    ? "passthrough"
    : override?.mode === "preset" ? "preset" : "off";
  return {
    clientFingerprintMode: mode,
    clientFingerprintPresetId: override?.mode === "preset" ? override.presetId ?? "" : ""
  };
}

/** 预设与当前 API 格式的兼容判定(下拉禁选依据;权威在后端保存门禁)。 */
export function clientFingerprintPresetCompatible(
  preset: ClientFingerprintPresetInfo,
  protocol: GatewayProviderProtocol
): boolean {
  return preset.compatibleProtocols.includes(providerWireProtocol(protocol));
}

export function toProviderProtocol(value: string | undefined): GatewayProviderProtocol | undefined {
  return providerProtocolOptions.some((option) => option.value === value) ? value as GatewayProviderProtocol : undefined;
}

export function shouldAutoProbeProviderBaseUrl(value: string): boolean {
  const trimmed = value.trim();
  if (!trimmed || /\s/.test(trimmed)) {
    return false;
  }

  try {
    const hasScheme = /^[a-z][a-z0-9+.-]*:\/\//i.test(trimmed);
    const url = new URL(hasScheme ? trimmed : providerUrlWithDefaultScheme(trimmed));
    return hasScheme || url.hostname === "localhost" || url.hostname.includes(".") || url.hostname.includes(":");
  } catch {
    return false;
  }
}

export function providerProbeCandidates(draft: AddProviderDraft): ProviderProbeCandidate[] {
  const protocols = providerProtocolOptions.map((option) => option.value);
  return [
    {
      baseUrl: draft.baseUrl.trim(),
      protocols,
      source: "custom",
      // 20261014_01 §3.5:完整 URL 模式随候选透传,实探对地址原样发
      // 探测体;base 模式不写键(与落库摘键口径一致)。
      urlMode: draft.urlMode === "full" ? "full" : undefined
    }
  ];
}

export function isProviderProbeCandidateReady(candidate: ProviderProbeCandidate): boolean {
  return shouldAutoProbeProviderBaseUrl(candidate.baseUrl);
}

export async function probeProviderCandidates(
  candidates: ProviderProbeCandidate[],
  apiKey: string,
  models: string[],
  options: {
    mode?: "connectivity" | "models" | "protocols";
    /** 20260906_01 决策 #9:探针载荷无 providerId,代理选择只能显式随载荷传;
     *  undefined = 后端回落全局默认。 */
    proxy?: ProviderProxyOverride;
    protocols?: GatewayProviderProtocol[];
  } = {}
): Promise<ProviderProbeCandidateResult | undefined> {
  const mode = options.mode ?? "protocols";
  return await window.wr?.probeProviderCandidates({
    apiKey: mode === "connectivity" || mode === "models" ? apiKey : undefined,
    candidates,
    mode,
    models: mode === "connectivity" ? models : [],
    proxy: options.proxy,
    protocols: options.protocols
  });
}

export function providerProbeHasSupportedProtocol(probe: GatewayProviderProbeResult | undefined, protocol?: GatewayProviderProtocol): boolean {
  return Boolean(probe?.protocols.some((item) => item.supported && (!protocol || item.protocol === protocol)));
}

export function mergeModelDisplayNames(
  ...groups: Array<Record<string, string> | undefined>
): Record<string, string> | undefined {
  const merged: Record<string, string> = {};
  for (const group of groups) {
    for (const [rawModel, rawDisplayName] of Object.entries(group ?? {})) {
      const model = rawModel.trim();
      const displayName = rawDisplayName.trim();
      if (!model || !displayName || model === displayName) {
        continue;
      }
      merged[model] = displayName;
    }
  }
  return Object.keys(merged).length > 0 ? merged : undefined;
}

export function mergeModelMetadata(
  ...groups: Array<Record<string, ProviderModelMetadata> | undefined>
): Record<string, ProviderModelMetadata> | undefined {
  const merged: Record<string, ProviderModelMetadata> = {};
  for (const group of groups) {
    for (const [rawModel, metadata] of Object.entries(group ?? {})) {
      const model = rawModel.trim();
      if (!model || !metadata || typeof metadata !== "object") {
        continue;
      }
      merged[model] = metadata;
    }
  }
  return Object.keys(merged).length > 0 ? merged : undefined;
}

export function modelMetadataForModels(
  value: Record<string, ProviderModelMetadata> | undefined,
  models: string[]
): Record<string, ProviderModelMetadata> | undefined {
  const modelIds = new Set(models);
  const entries = Object.entries(value ?? {})
    .map(([rawModel, metadata]) => [rawModel.trim(), metadata] as const)
    .filter(([model, metadata]) => model && modelIds.has(model) && metadata && typeof metadata === "object");
  return entries.length > 0 ? Object.fromEntries(entries) : undefined;
}

export function modelDisplayNamesForModels(
  value: Record<string, string> | undefined,
  models: string[]
): Record<string, string> | undefined {
  const modelIds = new Set(models);
  const entries = Object.entries(value ?? {})
    .map(([rawModel, rawDisplayName]) => [rawModel.trim(), rawDisplayName.trim()] as const)
    .filter(([model, displayName]) => model && displayName && model !== displayName && modelIds.has(model));
  return entries.length > 0 ? Object.fromEntries(entries) : undefined;
}

export function modelDescriptionsForModels(
  value: Record<string, string> | undefined,
  models: string[]
): Record<string, string> | undefined {
  const modelIds = new Set(models);
  const entries = Object.entries(value ?? {})
    .map(([rawModel, rawDescription]) => [rawModel.trim(), rawDescription.trim()] as const)
    .filter(([model, description]) => model && description && modelIds.has(model));
  return entries.length > 0 ? Object.fromEntries(entries) : undefined;
}

export function modelContextWindowForModels(
  value: Record<string, number> | undefined,
  models: string[]
): Record<string, number> | undefined {
  const modelIds = new Set(models);
  const entries = Object.entries(value ?? {})
    .map(([rawModel, rawValue]) => [rawModel.trim(), rawValue] as const)
    .filter(([model, window]) => model && Number.isFinite(window) && window > 0 && modelIds.has(model));
  return entries.length > 0 ? Object.fromEntries(entries) : undefined;
}

// 2026-09-10:模型输入类型记录。等于默认值(仅 text / 空)的条目直接摘键,
// 配置里只保留与默认不同的模型,避免每个模型都落一条噪声。
export const DEFAULT_MODEL_INPUT_TYPES: ProviderModelInputType[] = ["text"];

const MODEL_INPUT_TYPE_VALUES: readonly ProviderModelInputType[] = ["text", "image", "video", "pdf"];

// 归一:「文本」恒选(去重保序补回 text),与默认完全一致时返回 undefined
// (调用方据此摘键,配置里只留与默认不同的模型)。
export function normalizeModelInputTypes(
  value: string[] | undefined
): ProviderModelInputType[] | undefined {
  const allowed = new Set<string>(MODEL_INPUT_TYPE_VALUES);
  const picked = (value ?? []).filter((item): item is ProviderModelInputType => allowed.has(item));
  const types = Array.from(new Set(["text", ...picked])) as ProviderModelInputType[];
  const sameAsDefault = types.length === DEFAULT_MODEL_INPUT_TYPES.length && types.every((item, index) => item === DEFAULT_MODEL_INPUT_TYPES[index]);
  return sameAsDefault ? undefined : types;
}

export function modelInputTypesForModels(
  value: Record<string, ProviderModelInputType[]> | undefined,
  models: string[]
): Record<string, ProviderModelInputType[]> | undefined {
  const modelIds = new Set(models);
  const entries = Object.entries(value ?? {})
    .map(([rawModel, types]) => [rawModel.trim(), types] as const)
    .filter(([model, types]) => model && modelIds.has(model) && Array.isArray(types))
    .map(([model, types]) => [model, normalizeModelInputTypes(types)] as const)
    .filter((entry): entry is [string, ProviderModelInputType[]] => Boolean(entry[1]));
  return entries.length > 0 ? Object.fromEntries(entries) : undefined;
}

// 读模型的输入类型(缺省 = 仅文本);「视觉」= 勾了「图片」。
export function providerModelInputTypes(provider: GatewayProviderConfig, model: string): ProviderModelInputType[] {
  const types = provider.modelInputTypes?.[model];
  if (!types || types.length === 0) {
    return DEFAULT_MODEL_INPUT_TYPES;
  }
  return normalizeModelInputTypes(types) ?? DEFAULT_MODEL_INPUT_TYPES;
}

export function providerModelSupportsVision(provider: GatewayProviderConfig, model: string): boolean {
  return providerModelInputTypes(provider, model).includes("image");
}

export function modelMaxOutputTokensForModels(
  value: Record<string, number> | undefined,
  models: string[]
): Record<string, number> | undefined {
  const modelIds = new Set(models);
  const entries = Object.entries(value ?? {})
    .map(([rawModel, rawValue]) => [rawModel.trim(), rawValue] as const)
    .filter(([model, tokens]) => model && Number.isFinite(tokens) && tokens > 0 && modelIds.has(model));
  return entries.length > 0 ? Object.fromEntries(entries) : undefined;
}

// Formats a token count as a compact context label: 128000 -> "128K", 1M -> "1M".
export function formatContextWindow(tokens: number): string {
  if (!Number.isFinite(tokens) || tokens <= 0) {
    return "";
  }
  if (tokens >= 1_000_000) {
    const m = tokens / 1_000_000;
    return Number.isInteger(m) ? `${m}M` : `${m.toFixed(1)}M`;
  }
  if (tokens >= 1000) {
    const k = tokens / 1000;
    return Number.isInteger(k) ? `${k}K` : `${k.toFixed(1)}K`;
  }
  return String(tokens);
}

export function providerModelDisplayName(provider: GatewayProviderConfig, model: string): string {
  return provider.modelDisplayNames?.[model]?.trim() || model;
}

export function providerModelDisplayTitle(provider: GatewayProviderConfig, model: string): string {
  return providerModelDisplayName(provider, model);
}

export function mergeProviderModelLists(...groups: string[][]): string[] {
  const seen = new Set<string>();
  const models: string[] = [];
  for (const group of groups) {
    for (const model of group) {
      const trimmed = model.trim();
      if (!trimmed || seen.has(trimmed)) {
        continue;
      }
      seen.add(trimmed);
      models.push(trimmed);
    }
  }
  return models;
}

export function numberValue(value: string): number {
  const parsed = Number(value);
  return Number.isFinite(parsed) ? Math.trunc(parsed) : 0;
}

export function splitLines(value: string): string[] {
  return value
    .split("\n")
    .map((item) => item.trim())
    .filter(Boolean);
}

export function providerBaseUrl(provider: GatewayProviderConfig): string {
  return provider.api_base_url || "";
}

/** 读供应商级单密钥:只认最新配置键 `api_key`。 */
export function providerApiKey(
  provider: Pick<GatewayProviderConfig, "api_key"> | undefined
): string {
  return typeof provider?.api_key === "string" ? provider.api_key : "";
}

// ---------------------------------------------------------------------------
// K6/K7 (方案 20260905_02):凭据池账号辅助
// ---------------------------------------------------------------------------

/** 读供应商凭据池账号:只认最新配置键 ``accounts``。 */
export function providerCredentialAccounts(
  provider: Pick<GatewayProviderConfig, "accounts"> | undefined
): ProviderCredentialConfig[] {
  const accounts = provider?.accounts;
  return Array.isArray(accounts) ? accounts : [];
}

/** 后端 `ProviderAccount.id` 缺失时的兜底 slug(`cred_{index}`)镜像,保证
 *  pinned/stats 引用稳定:前端始终自行生成 id,不依赖后端兜底。 */
export function createProviderAccountId(usedIds: Iterable<string>): string {
  const used = new Set(usedIds);
  let candidate = "";
  do {
    candidate = `cred-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
  } while (used.has(candidate));
  return candidate;
}

/** 探测密钥解析的结构入参:完整 GatewayProviderConfig 与草稿形态都满足。 */
export type ProviderProbeKeySource = Pick<
  GatewayProviderConfig,
  "accounts" | "api_key" | "credentialStrategy" | "pinnedAccountId"
>;

/** 探测/检测用有效密钥(2026-10-11 根因修复:与网关选号同序)。
 *  凭据池非空时**池优先** —— 此前供应商级 legacy api_key 排第一,而读取侧
 *  (load_app_config)会把 secrets 里残留的 `provider:{id}:api_key` 注水回
 *  provider,导致「检测可用性」永远用影子旧键、无视用户固定/启用的账号。
 *  池内取序镜像 `credential_pool::pick_account_with`:
 *  - 固定模式(first/pinned):pinned 账号优先(仅启用集内找,找不到不硬选),
 *    其次 priority **降序**再按配置序;
 *  - 负载均衡(balanced/weighted/sticky):取最高优先级层的可用首把
 *    (网关层内为加权随机,探测取确定性代表);
 *  - 池为空或池内无明文:回落供应商级 api_key(纯 legacy 配置形态)。 */
export function providerProbeApiKey(provider: ProviderProbeKeySource | undefined): string {
  if (!provider) {
    return "";
  }
  const keyOf = (account: ProviderCredentialConfig): string =>
    (account.api_key || "").trim();
  const accounts = providerCredentialAccounts(provider).filter((account) => account.enabled !== false);
  const pickByPriorityDesc = (): string => {
    for (const account of [...accounts].sort((a, b) => (b.priority ?? 0) - (a.priority ?? 0))) {
      const key = keyOf(account);
      if (key) {
        return key;
      }
    }
    return "";
  };
  if (accounts.length > 0) {
    if (providerKeyMode(provider.credentialStrategy) === "fixed") {
      const pinnedId = provider.pinnedAccountId?.trim();
      if (pinnedId) {
        const pinned = accounts.find((account) => account.id === pinnedId);
        const key = pinned ? keyOf(pinned) : "";
        if (key) {
          return key;
        }
      }
    }
    const byPriority = pickByPriorityDesc();
    if (byPriority) {
      return byPriority;
    }
  }
  return providerApiKey(provider);
}

/** 草稿态探测密钥:添加/编辑面板的密钥列表还落在草稿里(未落库),用同一
 *  解析语义直接作用于草稿形态 —— 单输入期「draft.apiKey 优先」的旧规则
 *  与统一后的多密钥形态冲突,收敛到这一个入口。 */
export function providerProbeApiKeyFromDraft(draft: Pick<AddProviderDraft, "accounts" | "apiKey" | "credentialStrategy" | "pinnedAccountId">): string {
  const source: ProviderProbeKeySource = {
    accounts: draft.accounts,
    api_key: draft.apiKey,
    credentialStrategy: draft.credentialStrategy,
    pinnedAccountId: draft.pinnedAccountId
  };
  return providerProbeApiKey(source);
}

/** 20261017_01:添加模型弹窗「获取模型」的请求拼装。密钥/代理都走探测
 *  同款单入口 —— 测的就是将来的走法;调用侧 await,undefined = 桥缺失。 */
export async function fetchProviderModelsForDraft(draft: AddProviderDraft): Promise<GatewayProviderModelsFetchResult | undefined> {
  return await window.wr?.fetchProviderModels({
    apiKey: providerProbeApiKeyFromDraft(draft),
    baseUrl: draft.baseUrl.trim(),
    protocol: draft.protocol,
    // 20260906_01 决策 #9:代理选择显式随载荷传(inherit → 不带字段)。
    proxy: probeProxyPayload(proxyOverrideFromSelection(draft.proxyMode, draft.proxyPresetId)),
    urlMode: draft.urlMode === "full" ? "full" : undefined
  });
}

/** 同上,已保存供应商形态(详情页添加模型)。 */
export async function fetchProviderModelsForProvider(provider: GatewayProviderConfig): Promise<GatewayProviderModelsFetchResult | undefined> {
  return await window.wr?.fetchProviderModels({
    apiKey: providerProbeApiKey(provider),
    baseUrl: (provider.api_base_url ?? "").trim(),
    protocol: toProviderProtocol(provider.type) ?? toProviderProtocol(provider.provider) ?? "openai_chat_completions",
    proxy: probeProxyPayload(provider.proxy),
    urlMode: provider.urlMode === "full" ? "full" : undefined
  });
}

/** 新账号行默认值:enabled/priority/weight 与后端 `ProviderAccount::from_dict`
 *  缺省语义对齐(enabled=true、priority=0、weight=1)。 */
export function createProviderAccount(usedIds: Iterable<string>): ProviderCredentialConfig {
  return {
    enabled: true,
    id: createProviderAccountId(usedIds),
    priority: 0,
    weight: 1
  };
}

// ---- 2026-09-06 UI 两模式(固定密钥 / 负载均衡) ------------------------------

export type ProviderKeyMode = "fixed" | "balanced";

/** 配置策略 → UI 两模式映射:固定密钥 = first(缺省)与 pinned(旧配置,
 *  行为在单启用约束下等价);负载均衡 = balanced 与旧 weighted/sticky
 *  (UI 已不再写入,历史配置显示归入负载均衡)。 */
export function providerKeyMode(strategy: GatewayProviderConfig["credentialStrategy"]): ProviderKeyMode {
  return strategy === "weighted" || strategy === "sticky" || strategy === "balanced"
    ? "balanced"
    : "fixed";
}

/** 固定密钥模式的单启用约束:只保留 `keepId` 启用,其余一律停用。 */
export function enforceSingleEnabledKey(
  accounts: ProviderCredentialConfig[],
  keepId: string
): ProviderCredentialConfig[] {
  return accounts.map((account) => ({ ...account, enabled: account.id === keepId }));
}

/** 新密钥的默认名称:`{base} N`,N 从 `密钥数 + 1` 起找第一个未占用的号
 *  (名称唯一约束见 ProviderApiKeysSection 的查重)。 */
export function nextProviderAccountLabel(accounts: ProviderCredentialConfig[], base: string): string {
  const used = new Set(
    accounts.map((account) => (account.label ?? "").trim()).filter(Boolean)
  );
  let n = accounts.length + 1;
  while (used.has(`${base} ${n}`)) {
    n += 1;
  }
  return `${base} ${n}`;
}

/** 当前启用密钥的 id 集合(负载均衡模式记忆的采集形态)。 */
export function enabledAccountIds(accounts: ProviderCredentialConfig[]): string[] {
  return accounts
    .filter((account) => account.enabled !== false)
    .map((account) => account.id ?? "")
    .filter(Boolean);
}

/** 把记忆的启用 id 集合恢复到密钥数组(id 不在集合中的一律停用)。 */
export function applyEnabledIds(
  accounts: ProviderCredentialConfig[],
  ids: string[]
): ProviderCredentialConfig[] {
  const keep = new Set(ids);
  return accounts.map((account) => ({ ...account, enabled: keep.has(account.id ?? "") }));
}

/** 2026-09-06 历史数据兼容:为无名称的存量密钥补默认名「{base} N」——N 以
 *  该行位置为起点(1 起),撞已占用名则顺延。仅在有缺名行时产生新数组。 */
export function fillDefaultAccountLabels(
  accounts: ProviderCredentialConfig[],
  base: string
): { accounts: ProviderCredentialConfig[]; changed: boolean } {
  const used = new Set(
    accounts.map((account) => (account.label ?? "").trim()).filter(Boolean)
  );
  let changed = false;
  const next = accounts.map((account, index) => {
    if ((account.label ?? "").trim()) {
      return account;
    }
    let n = index + 1;
    while (used.has(`${base} ${n}`)) {
      n += 1;
    }
    used.add(`${base} ${n}`);
    changed = true;
    return { ...account, label: `${base} ${n}` };
  });
  return { accounts: changed ? next : accounts, changed };
}

/** K7:冷却剩余时长展示(mm:ss / h:mm:ss;无轮询,由徽章侧本地秒级递减)。 */
export function formatCooldownRemaining(ms: number): string {
  if (!Number.isFinite(ms) || ms <= 0) {
    return "0:00";
  }
  const totalSeconds = Math.ceil(ms / 1000);
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;
  const mm = hours > 0 ? String(minutes).padStart(2, "0") : String(minutes);
  const ss = String(seconds).padStart(2, "0");
  return hours > 0 ? `${hours}:${mm}:${ss}` : `${mm}:${ss}`;
}

/** Translate user-cleared keys into the explicit-clear marker before a save.
 *
 * The backend treats an absent/empty ``api_key`` as "leave the stored secret
 * untouched" and only deletes it on an explicit ``null``. A draft key that is
 * empty while the last-saved config still holds a non-empty one means the
 * user cleared the input, so those providers are sent as ``null``; everything
 * else round-trips unchanged. */
export function markClearedProviderApiKeys(config: AppConfig, saved: AppConfig): AppConfig {
  if (!Array.isArray(config.Providers) || !Array.isArray(saved.Providers)) {
    return config;
  }
  const savedKeyById = new Map<string, string>();
  for (const provider of saved.Providers) {
    const key = provider.id || provider.name;
    if (key) {
      savedKeyById.set(key, providerApiKey(provider));
    }
  }
  let changed = false;
  const providers = config.Providers.map((provider) => {
    const key = provider.id || provider.name;
    const savedKey = key ? savedKeyById.get(key) : "";
    // Convert only when the saved config holds a non-empty key and the draft
    // cleared it; anything else round-trips unchanged.
    if (!savedKey || providerApiKey(provider) || provider.api_key === null) {
      return provider;
    }
    changed = true;
    return { ...provider, api_key: null };
  });
  return changed ? { ...config, Providers: providers } : config;
}

export function providerIsEnabled(provider: GatewayProviderConfig): boolean {
  return provider.enabled !== false;
}

export function providerCapabilitiesSummary(provider: GatewayProviderConfig, translate: (value: string) => string = (value) => value): string {
  const capabilities = provider.capabilities ?? [];
  if (capabilities.length === 0) {
    return translatedProviderProtocolLabel(toProviderProtocol(provider.type) ?? toProviderProtocol(provider.provider) ?? "openai_chat_completions", translate);
  }
  return capabilities.map((capability) => translatedProviderProtocolLabel(capability.type, translate)).join(", ");
}

export function providerListItemKey(provider: GatewayProviderConfig, index: number): string {
  return provider.id || `${index}:${provider.name || "provider"}`;
}

/**
 * Stable ASCII slug for a provider name (used as the provider id). Pure
 * non-ASCII names (千问, 火山方舟 …) slugify to an empty string; a constant
 * "provider" fallback made every such provider share the same id — and
 * therefore the same backend secrets-table slot, so their API keys
 * clobbered each other on save. Fall back to a stable per-name id derived
 * from the Unicode code points instead.
 */
export function providerNameSlug(value: string): string {
  const trimmed = value.trim();
  const slug = trimmed
    .toLowerCase()
    .replace(/[^a-z0-9_.-]+/g, "-")
    .replace(/^-+|-+$/g, "");
  if (slug) {
    return slug;
  }
  const encoded = [...trimmed].map((ch) => ch.codePointAt(0)?.toString(36) ?? "0").join("");
  return encoded ? `provider-${encoded}` : "provider";
}

/** 供应商名预留检查(2026-09-15):"wrouter" 是聚合模型专属命名空间
 * (`wrouter/<聚合名>` 前缀调用 + /v1/models 的 owned_by),供应商的
 * 名称与 slug 生成的 id 都不得占用;slug 同拦是防 "wrouter-" 这类
 * 去尾杠后撞 id 的别名。 */
export function isReservedProviderName(name: string): boolean {
  return name.trim().toLowerCase() === RESERVED_PROVIDER_SLUG
    || providerNameSlug(name) === RESERVED_PROVIDER_SLUG;
}

/** Pick an id not already used by another provider (suffix -2, -3 …). */
export function uniqueProviderId(baseId: string, usedIds: Iterable<string>): string {
  const used = new Set(usedIds);
  let candidate = baseId;
  let suffix = 2;
  while (used.has(candidate)) {
    candidate = `${baseId}-${suffix}`;
    suffix += 1;
  }
  return candidate;
}

export function providerMatchesQuery(provider: GatewayProviderConfig, query: string): boolean {
  if (!query) {
    return true;
  }

  return [
    provider.name,
    providerBaseUrl(provider),
    providerCapabilitiesSummary(provider),
    ...(provider.capabilities ?? []).map((capability) => capability.baseUrl),
    ...provider.models,
    ...provider.models.map((model) => providerModelDisplayName(provider, model)),
    ...Object.values(provider.modelDescriptions ?? {})
  ]
    .filter(Boolean)
    .some((value) => value.toLowerCase().includes(query));
}

export function viewUsesInternalScroll(view: ViewId): boolean {
  // 2026-11-01:「模型指纹」独立视图工具行裸排 + 库内容内部滚动,与其它视图同款。
  // 20261101_03:「快速开始」页自管滚动。
  return view === "api-keys" || view === "call-log-detail" || view === "model-fingerprints" || view === "providers" || view === "settings" || view === "quickstart";
}

export function uniqueProviderName(providers: GatewayProviderConfig[], baseName = "provider"): string {
  const trimmedBaseName = baseName.trim();
  if (trimmedBaseName && trimmedBaseName !== "provider") {
    let candidate = trimmedBaseName;
    let index = 2;
    while (providers.some((provider) => providerNameEquals(provider.name, candidate))) {
      candidate = `${trimmedBaseName} ${index}`;
      index += 1;
    }
    return candidate;
  }

  let index = providers.length + 1;
  while (providers.some((provider) => providerNameEquals(provider.name, `provider-${index}`))) {
    index += 1;
  }
  return `provider-${index}`;
}

export function isProviderNameDuplicate(providers: GatewayProviderConfig[], name: string, ignoreIndex?: number): boolean {
  return providers.some((provider, index) => index !== ignoreIndex && providerNameEquals(provider.name, name));
}

export function providerNameEquals(left: string, right: string): boolean {
  return left.trim().toLowerCase() === right.trim().toLowerCase();
}

export function providerProtocolLabel(protocol: GatewayProviderProtocol | string): string {
  return providerProtocolOptions.find((option) => option.value === protocol)?.label ?? String(protocol);
}

export function translatedProviderProtocolLabel(protocol: GatewayProviderProtocol | string, translate: (value: string) => string): string {
  return translate(providerProtocolLabel(protocol));
}

export function translateProbeProtocolMessage(message: string | undefined, translate: (value: string) => string): string {
  const trimmed = message?.trim() ?? "";
  if (!trimmed) {
    return "";
  }
  if (isProbeAuthorizationMissingMessage(trimmed)) {
    return "";
  }

  const httpMatch = /^HTTP\s+(\d{3})(?::\s*(.*))?$/i.exec(trimmed);
  if (!httpMatch) {
    return translate(trimmed);
  }

  const status = httpMatch[1];
  const detail = httpMatch[2]?.trim();
  return detail ? `HTTP ${status}: ${translate(detail)}` : `HTTP ${status}`;
}

function isProbeAuthorizationMissingMessage(message: string): boolean {
  const normalized = message
    .trim()
    .replace(/[。.\s]+$/g, "")
    .toLowerCase();
  const match = /^http\s+401\s*:\s*(.*)$/i.exec(normalized);
  const detail = match?.[1] ?? normalized;
  return detail.includes("header中未收到authorization参数") && detail.includes("无法进行身份验证");
}