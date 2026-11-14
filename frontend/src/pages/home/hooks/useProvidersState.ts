// R7 split: providers domain — drafts, probes, submit/delete.
//
// providers 列表来自 draftConfig（配置域），其保存/删除的乐观更新与回滚
// 由 useConfigState 的 saveMutation 承载。请求层统一走 lib/api（F2a.6）。
import { useRef, useState } from "react";
import {
  AddProviderDraft, AppConfig, AppCopy, AppSaveConfigOptions, createProviderDraft,
  createProviderDraftFromProvider, fallbackConfig,
  GatewayProviderConfig, GatewayProviderProbeResult, GatewayProviderProtocol,
  isProviderNameDuplicate, isProviderProbeCandidateReady, isReservedProviderName,
  mergeProviderModelLists, modelContextWindowForModels, modelDescriptionsForModels, modelDisplayNamesForModels,
  modelInputTypesForModels, modelMaxOutputTokensForModels, modelMetadataForModels, normalizeModelInputTypes,
  normalizeProviderBaseUrl,
  ProviderConnectivityCheckReport,
  ProviderCredentialConfig, ProviderKeyMode,
  ProviderProxyOverride,
  clientFingerprintOverrideFromSelection,
  probeProxyPayload,
  providerNameSlug,
  providerProbeApiKey,
  providerProbeApiKeyFromDraft,
  providerProbeCandidates,
  proxyOverrideFromSelection,
  splitLines, toProviderProtocol, translateAppErrorMessage,
  uniqueProviderId
} from "../shared/index";
import { api, hasBridge } from "@/lib/api";
import type { ProviderClientFingerprintOverride, ProviderModelInputType, ProviderUrlMode } from "@/contracts/app";
import {
  applyProviderAccounts, applyProviderCredentialMode, applyProviderFixedKey,
  applyProviderLegacyKeyMigration,
} from "./providers-credential";


export type ProvidersStateOptions = {
  draftConfig: AppConfig;
  configLoaded: boolean;
  copy: AppCopy;
  showToast: (message: string) => void;
  t: (value: string) => string;
  formatError: (error: unknown) => string;
  buildConfigUpdate: (mutator: (config: AppConfig) => AppConfig) => AppConfig;
  setConfigDraft: (config: AppConfig) => AppConfig;
  persistConfig: (config: AppConfig, setError: (message: string) => void, options?: AppSaveConfigOptions) => Promise<boolean>;
  setActionError: (message: string) => void;
};

// Re-keys a per-model record when a model is renamed; returns the record
// untouched when the old key is absent.
function renameRecordKey<T>(record: Record<string, T> | undefined, from: string, to: string): Record<string, T> | undefined {
  if (!record || !(from in record)) {
    return record;
  }
  const next: Record<string, T> = {};
  for (const [key, value] of Object.entries(record)) {
    next[key === from ? to : key] = value;
  }
  return next;
}

/** Map the fine-grained frontend protocol to the coarse backend family the
 *  gateway understands (``provider.protocol``). */
function protocolFamily(protocol: GatewayProviderProtocol): "openai" | "anthropic" {
  return protocol === "anthropic_messages" ? "anthropic" : "openai";
}

export function useProvidersState({
  draftConfig, configLoaded, copy,
  showToast, t, formatError,
  buildConfigUpdate, setConfigDraft, persistConfig, setActionError,
}: ProvidersStateOptions) {
  // 供应商「添加」是一个唯一的内存临时条目:providerTemporary 表示草稿存在,
  // 面板可见性由列表 selection(provider-new)驱动,不再维护第二个布尔。
  const [providerTemporary, setProviderTemporary] = useState(false);
  const [providerDeleteIndex, setProviderDeleteIndex] = useState<number>();
  const [providerEditIndex, setProviderEditIndex] = useState<number>();
  const [providerCreatedIndex, setProviderCreatedIndex] = useState<number>();
  const [providerDraft, setProviderDraft] = useState<AddProviderDraft>(() => createProviderDraft(fallbackConfig.Providers));
  const [providerConnectivityProbe, setProviderConnectivityProbe] = useState<GatewayProviderProbeResult>();
  const [providerConnectivityLoading, setProviderConnectivityLoading] = useState(false);
  const [providerProbeError, setProviderProbeError] = useState("");

  const providerConnectivityRequestId = useRef(0);

  function openAddProviderDialog() {
    // The add entry is a single in-memory draft. Re-opening it must resume the
    // existing entry instead of resetting fields that the user already entered.
    if (providerTemporary) {
      return;
    }
    providerConnectivityRequestId.current += 1;
    setProviderEditIndex(undefined);
    setProviderTemporary(true);
    setProviderDraft(createProviderDraft(draftConfig.Providers));
    setProviderConnectivityProbe(undefined);
    setProviderProbeError("");
    setProviderConnectivityLoading(false);
  }

  function updateProviderDraft(patch: Partial<AddProviderDraft>, resetProbe = false) {
    setProviderDraft((current) => ({ ...current, ...patch }));
    setProviderProbeError("");
    const shouldResetConnectivityProbe =
      resetProbe ||
      patch.apiKey !== undefined ||
      // 2026-10-11 多密钥统一:密钥列表/策略/pinned 变化同样使旧探测失效。
      patch.accounts !== undefined ||
      patch.credentialStrategy !== undefined ||
      patch.pinnedAccountId !== undefined ||
      patch.baseUrl !== undefined ||
      patch.modelsText !== undefined ||
      patch.protocol !== undefined ||
      // 20261014_01 决策 5:URL 模式切换即作废旧探测结果(语义已变,
      // 旧探测不再代表将来的走法)。
      patch.urlMode !== undefined ||
      // 20261016_03:换了客户端指纹,既有探测结果同样不再代表将来的走法。
      patch.clientFingerprintMode !== undefined ||
      patch.clientFingerprintPresetId !== undefined ||
      // 20260906_01:换了出口(代理),既有探测结果不再代表将来的走法。
      patch.proxyMode !== undefined ||
      patch.proxyPresetId !== undefined ||
      patch.selectedModels !== undefined;
    if (shouldResetConnectivityProbe) {
      providerConnectivityRequestId.current += 1;
      setProviderConnectivityProbe(undefined);
      setProviderConnectivityLoading(false);
    }
  }

  async function checkProviderDraft(modelsToCheck?: string[]): Promise<ProviderConnectivityCheckReport> {
    // 2026-10-11 统一:草稿密钥列表(含编辑路径从既有供应商播种的池)与
    // 供应商级密钥按网关同序解析;旧「draft.apiKey 优先」规则与多密钥形态
    // 冲突,收敛到 providerProbeApiKeyFromDraft 单入口。
    return runProviderConnectivityCheck(
      providerDraft,
      modelsToCheck,
      providerProbeApiKeyFromDraft(providerDraft)
    );
  }

  async function runProviderConnectivityCheck(
    draft: AddProviderDraft,
    modelsToCheck?: string[],
    apiKeyOverride?: string,
    trackState = true
  ): Promise<ProviderConnectivityCheckReport> {
    const emptyReport: ProviderConnectivityCheckReport = { failed: [], passed: [], results: [] };
    const apiKey = (apiKeyOverride ?? draft.apiKey).trim();
    const models = mergeProviderModelLists(modelsToCheck ?? mergeProviderModelLists(draft.selectedModels, splitLines(draft.modelsText)));
    // 2026-10-14 修复:探测协议以表单当前所选 API 格式为唯一权威。此前编辑
    // 路径会优先用 capabilities 播种的旧探测格式( selectedProtocols),用户把
    // 格式换成 Anthropic Messages 后检测仍打旧格式的端点(如
    // .../anthropic/v1/chat/completions),检测可用性恒失败。
    const protocols = [draft.protocol];
    const candidates = providerProbeCandidates(draft)
      .map((candidate) => ({
        ...candidate,
        protocols: candidate.protocols.filter((protocol) => protocols.includes(protocol))
      }))
      .filter((candidate) => isProviderProbeCandidateReady(candidate) && candidate.protocols.length > 0);

    // trackState=false(详情面板路径):报告直接返回给调用弹窗,不写共享探测
    // 状态 —— 正式条目与临时条目的检测结果互不串写。
    if (trackState) {
      providerConnectivityRequestId.current += 1;
    }
    const requestId = providerConnectivityRequestId.current;
    if (trackState) {
      setProviderProbeError("");
    }
    if (!hasBridge()) {
      if (trackState) {
        setProviderProbeError(t("Request failed."));
      }
      return emptyReport;
    }
    if (candidates.length === 0) {
      if (trackState) {
        setProviderProbeError(t("No endpoint candidates available."));
      }
      return emptyReport;
    }
    if (models.length === 0) {
      if (trackState) {
        setProviderProbeError(t("Select or enter at least one model."));
      }
      return emptyReport;
    }

    if (trackState) {
      setProviderConnectivityLoading(true);
    }
    try {
      const report = await api.checkProviderConnectivity({
        apiKey,
        candidates,
        // 20261016_03:实探按草稿当前的客户端指纹走(preset 生效;passthrough
        // 在探测面无入站上下文,后端自然降级为仅注入认证;off → 不带字段)。
        clientFingerprint: clientFingerprintOverrideFromSelection(
          draft.clientFingerprintMode,
          draft.clientFingerprintPresetId
        ),
        forceRefresh: true,
        models,
        // 20260906_01 决策 #9:探针按草稿当前的代理选择走(inherit → 不带字段)。
        proxy: probeProxyPayload(proxyOverrideFromSelection(draft.proxyMode, draft.proxyPresetId)),
        protocols
      });
      if (trackState && providerConnectivityRequestId.current !== requestId) {
        return emptyReport;
      }

      if (trackState) {
        setProviderConnectivityProbe(report.probe);

        if (report.passed.length === 0) {
          setProviderProbeError(translateAppErrorMessage(copy, report.failed[0]?.message || "Request failed."));
        }
      }
      return report;
    } catch (error) {
      if (trackState && providerConnectivityRequestId.current === requestId) {
        setProviderConnectivityProbe(undefined);
        setProviderProbeError(formatError(error));
      }
      return emptyReport;
    } finally {
      if (trackState && providerConnectivityRequestId.current === requestId) {
        setProviderConnectivityLoading(false);
      }
    }
  }

  async function submitProviderDraft(): Promise<boolean> {
    if (providerConnectivityLoading) {
      return false;
    }

    const typedModels = splitLines(providerDraft.modelsText);
    const models = mergeProviderModelLists(providerDraft.selectedModels, typedModels);
    if (models.length === 0) {
      setProviderProbeError(t("Enter at least one model."));
      return false;
    }

    const providerName = providerDraft.name.trim();
    if (isProviderNameDuplicate(draftConfig.Providers, providerName, providerEditIndex)) {
      setProviderProbeError(t("Provider name already exists."));
      return false;
    }
    // "wrouter" 是聚合命名空间(2026-09-15),只拦新建:编辑路径的 id 沿用
    // 既有值不受名称影响,存量配置不会被卡死。
    if (providerEditIndex === undefined && isReservedProviderName(providerName)) {
      setProviderProbeError(t("This provider name is reserved."));
      return false;
    }

    const protocol = providerDraft.protocol;
    const baseUrl = providerDraft.baseUrl;
    const modelContextWindow = modelContextWindowForModels(providerDraft.modelContextWindow, models);
    const modelDescriptions = modelDescriptionsForModels(providerDraft.modelDescriptions, models);
    const modelDisplayNames = modelDisplayNamesForModels(providerDraft.modelDisplayNames, models);
    const modelInputTypes = modelInputTypesForModels(providerDraft.modelInputTypes, models);
    const modelMaxOutputTokens = modelMaxOutputTokensForModels(providerDraft.modelMaxOutputTokens, models);
    const modelMetadata = modelMetadataForModels(providerDraft.modelMetadata, models);

    const existingProvider = providerEditIndex !== undefined ? draftConfig.Providers[providerEditIndex] : undefined;
    let providerId = existingProvider?.id ?? providerNameSlug(providerName);
    if (!existingProvider) {
      // Guard against slug collisions (e.g. "My Provider" vs "my_provider"
      // both slug to "my-provider"): a duplicate id would share the backend
      // secrets slot with the existing provider and their API keys would
      // overwrite each other. Keep the first id, suffix later ones.
      providerId = uniqueProviderId(
        providerId,
        draftConfig.Providers.map((item) => item.id).filter((id): id is string => Boolean(id))
      );
    }
    // K6:凭据池账号随草稿编辑(2026-10-11 与详情页统一)。草稿播种时已把
    // 存量池/legacy 单密钥统一进 accounts(见 createProviderDraftFromProvider),
    // 因此落库只认草稿数组 —— 用户删光密钥行就是真删,不做隐式恢复。
    // 根因修复:池非空时供应商级 api_key 显式 null —— 走后端三态 DELETE,
    // 真正删掉 secrets 里残留的 `provider:{id}:api_key` 影子键(空串是
    // 「不动」,此前迁移只清 payload 显示值,读取侧又会注水复活)。
    const draftAccounts = providerDraft.accounts ?? [];
    const usePool = draftAccounts.length > 0;
    const provider: GatewayProviderConfig = {
      accounts: usePool ? draftAccounts.map((account) => ({ ...account })) : undefined,
      // 20261014_01 §3.3:full 模式跳过归一化,只 trim —— 地址按用户填写
      // 原样落库(尾斜杠/query 语义交给用户);base 模式维持归一化。
      api_base_url: providerDraft.urlMode === "full"
        ? baseUrl.trim()
        : normalizeProviderBaseUrl(baseUrl, protocol),
      api_key: usePool ? null : providerDraft.apiKey.trim(),
      // 编辑路径的 strategy 由 createProviderDraftFromProvider 从既有配置
      // 播种;新建供应商落方案 §3.7 默认 "first"。
      credentialStrategy: providerDraft.credentialStrategy,
      enabled: providerDraft.enabled ?? existingProvider?.enabled ?? true,
      icon: providerDraft.icon.trim() || undefined,
      id: providerId,
      modelContextWindow,
      modelDescriptions,
      modelDisplayNames,
      modelInputTypes,
      modelMaxOutputTokens,
      modelMetadata,
      models,
      name: providerName,
      pinnedAccountId: usePool
        ? providerDraft.pinnedAccountId || undefined
        : existingProvider?.pinnedAccountId,
      protocol: protocolFamily(protocol),
      // 20260906_01 §4.9b:网络代理选择(inherit / 未选到具体预设 → 不写键,
      // 与后端 ProviderProxyOverride::from_value 的 None 归一一致)。
      proxy: proxyOverrideFromSelection(providerDraft.proxyMode, providerDraft.proxyPresetId),
      type: protocol,
      // 20261016_03:客户端指纹(off / preset 未选到具体预设 → 摘键不落库;
      // 必须显式出现在此字面量,否则保存一次就把已配置指纹抹回 off)。
      clientFingerprint: clientFingerprintOverrideFromSelection(
        providerDraft.clientFingerprintMode,
        providerDraft.clientFingerprintPresetId
      ),
      // 20261014_01 §3.1:等于默认值摘键不落库;urlMode 必须显式出现在
      // 此字面量,否则保存一次就把 full 供应商抹回 base(编辑路径已由
      // createProviderDraftFromProvider 播种)。
      urlMode: providerDraft.urlMode === "full" ? "full" : undefined
    };
    const next = buildConfigUpdate((config) => {
      if (providerEditIndex === undefined) {
        config.Providers.push(provider);
      } else {
        config.Providers[providerEditIndex] = provider;
      }
      if (!config.preferredProvider) {
        config.preferredProvider = provider.name;
      }
      return config;
    });
    setConfigDraft(next);
    if (await persistConfig(next, setProviderProbeError)) {
      setProviderEditIndex(undefined);
      setProviderTemporary(false);
      if (providerEditIndex === undefined) {
        setProviderCreatedIndex(next.Providers.length - 1);
      }
      return true;
    }
    return false;
  }

  async function removeProvider(index: number): Promise<boolean> {
    const next = buildConfigUpdate((config) => {
      config.Providers.splice(index, 1);
      return config;
    });
    setConfigDraft(next);
    return persistConfig(next, setActionError);
  }

  function updateProviderModelDescription(providerIndex: number, model: string, description: string) {
    const next = buildConfigUpdate((config) => {
      const provider = config.Providers[providerIndex];
      const models = provider ? mergeProviderModelLists(provider.models) : [];
      if (!provider || !models.includes(model)) {
        return config;
      }
      const descriptions = { ...(provider.modelDescriptions ?? {}) };
      const trimmed = description.trim();
      if (trimmed) {
        descriptions[model] = trimmed;
      } else {
        delete descriptions[model];
      }
      provider.modelDescriptions = modelDescriptionsForModels(descriptions, models);
      return config;
    });
    setConfigDraft(next);
    void persistConfig(next, setActionError);
  }

  function updateModel(providerIndex: number, model: string, patch: { contextWindow?: number; inputTypes?: ProviderModelInputType[]; maxOutputTokens?: number; name?: string }) {
    const next = buildConfigUpdate((config) => {
      const provider = config.Providers[providerIndex];
      const models = provider ? mergeProviderModelLists(provider.models) : [];
      if (!provider || !models.includes(model)) {
        return config;
      }
      let key = model;
      const newName = patch.name?.trim();
      if (newName && newName !== model) {
        provider.models = provider.models.map((item) => (item === model ? newName : item));
        provider.modelDescriptions = renameRecordKey(provider.modelDescriptions, model, newName);
        provider.modelDisplayNames = renameRecordKey(provider.modelDisplayNames, model, newName);
        provider.modelContextWindow = renameRecordKey(provider.modelContextWindow, model, newName);
        provider.modelInputTypes = renameRecordKey(provider.modelInputTypes, model, newName);
        provider.modelMaxOutputTokens = renameRecordKey(provider.modelMaxOutputTokens, model, newName);
        provider.modelMetadata = renameRecordKey(provider.modelMetadata, model, newName);
        key = newName;
      }
      if (patch.contextWindow !== undefined) {
        const windows = { ...(provider.modelContextWindow ?? {}) };
        if (Number.isFinite(patch.contextWindow) && patch.contextWindow > 0) {
          windows[key] = patch.contextWindow;
        } else {
          delete windows[key];
        }
        provider.modelContextWindow = modelContextWindowForModels(windows, provider.models);
      }
      if (patch.maxOutputTokens !== undefined) {
        const limits = { ...(provider.modelMaxOutputTokens ?? {}) };
        if (Number.isFinite(patch.maxOutputTokens) && patch.maxOutputTokens > 0) {
          limits[key] = patch.maxOutputTokens;
        } else {
          delete limits[key];
        }
        provider.modelMaxOutputTokens = modelMaxOutputTokensForModels(limits, provider.models);
      }
      if (patch.inputTypes !== undefined) {
        // 与默认一致(仅文本)时归一为 undefined → 摘键,配置不留噪声。
        const inputTypes = { ...(provider.modelInputTypes ?? {}) };
        const normalized = normalizeModelInputTypes(patch.inputTypes);
        if (normalized) {
          inputTypes[key] = normalized;
        } else {
          delete inputTypes[key];
        }
        provider.modelInputTypes = modelInputTypesForModels(inputTypes, provider.models);
      }
      return config;
    });
    setConfigDraft(next);
    void persistConfig(next, setActionError);
  }

  function setProviderEnabled(providerIndex: number, enabled: boolean) {
    const next = buildConfigUpdate((config) => {
      const provider = config.Providers[providerIndex];
      if (!provider) {
        return config;
      }
      provider.enabled = enabled;
      return config;
    });
    setConfigDraft(next);
    void persistConfig(next, setActionError);
  }

  function setProviderBaseUrl(providerIndex: number, baseUrl: string) {
    const next = buildConfigUpdate((config) => {
      const provider = config.Providers[providerIndex];
      if (!provider) {
        return config;
      }
      const protocol = toProviderProtocol(provider.type);
      // 20261014_01:详情面板直改路径同样按 URL 模式分派 —— full 模式
      // 只 trim,不归一化(否则「原样」承诺在这里被打破)。
      provider.api_base_url = provider.urlMode === "full"
        ? baseUrl.trim()
        : normalizeProviderBaseUrl(baseUrl.trim(), protocol);
      return config;
    });
    setConfigDraft(next);
    void persistConfig(next, setActionError);
  }

  /** 20261014_01:详情面板 URL 模式切换(表单 TAB 的详情页对应物)。
   *  full 落 "full" 键;base 摘键(等于默认不落库)。已保存的地址不重写
   *  —— 切到 full 后再编辑输入框即按原样落库(见 setProviderBaseUrl)。 */
  function setProviderUrlMode(providerIndex: number, urlMode: ProviderUrlMode) {
    const next = buildConfigUpdate((config) => {
      const provider = config.Providers[providerIndex];
      if (!provider) {
        return config;
      }
      provider.urlMode = urlMode === "full" ? "full" : undefined;
      return config;
    });
    setConfigDraft(next);
    void persistConfig(next, setActionError);
  }

  /** 20261016_03:详情面板自定义请求头(落 `headers` 键,后端
   *  Provider.headers 直接消费;undefined = 摘键)。 */
  function setProviderHeaders(
    providerIndex: number,
    headers: Record<string, string> | undefined
  ) {
    const next = buildConfigUpdate((config) => {
      const provider = config.Providers[providerIndex];
      if (!provider) {
        return config;
      }
      provider.headers = headers;
      return config;
    });
    setConfigDraft(next);
    void persistConfig(next, setActionError);
  }

  /** 20261016_03:详情面板客户端指纹切换(表单下拉的详情页对应物)。
   *  off / preset 未选到具体预设 → 摘键(等于默认不落库);保存门禁在后端
   *  (presetId 未知 / 协议不兼容 → 400,由 persistConfig 错误通道呈现)。 */
  function setProviderClientFingerprint(
    providerIndex: number,
    fingerprint: ProviderClientFingerprintOverride | undefined
  ) {
    const next = buildConfigUpdate((config) => {
      const provider = config.Providers[providerIndex];
      if (!provider) {
        return config;
      }
      provider.clientFingerprint = fingerprint;
      return config;
    });
    setConfigDraft(next);
    void persistConfig(next, setActionError);
  }

  function setProviderProtocol(providerIndex: number, protocol: GatewayProviderProtocol) {
    const next = buildConfigUpdate((config) => {
      const provider = config.Providers[providerIndex];
      if (!provider) {
        return config;
      }
      provider.type = protocol;
      // The backend gateway reads ``protocol`` (openai/anthropic family),
      // while ``type`` keeps the fine-grained frontend value. Write both so
      // the selected API format actually takes effect upstream.
      provider.protocol = protocolFamily(protocol);
      return config;
    });
    setConfigDraft(next);
    void persistConfig(next, setActionError);
  }

  /** 20260906_01 §4.9b:供应商级网络代理。`undefined` = 跟随全局默认 ——
   *  直接删键(而不是写 `{mode:"inherit"}`),与后端
   *  `ProviderProxyOverride::from_value` 把 inherit 归一为 None 一致。 */
  function setProviderProxy(providerIndex: number, proxy: ProviderProxyOverride | undefined) {
    const next = buildConfigUpdate((config) => {
      const provider = config.Providers[providerIndex];
      if (!provider) {
        return config;
      }
      if (proxy) {
        provider.proxy = proxy;
      } else {
        delete provider.proxy;
      }
      return config;
    });
    setConfigDraft(next);
    void persistConfig(next, setActionError);
  }

  function setProviderName(providerIndex: number, name: string) {
    const next = buildConfigUpdate((config) => {
      const provider = config.Providers[providerIndex];
      if (!provider) {
        return config;
      }
      provider.name = name.trim();
      return config;
    });
    setConfigDraft(next);
    void persistConfig(next, setActionError);
  }

  function setProviderIcon(providerIndex: number, icon: string) {
    const next = buildConfigUpdate((config) => {
      const provider = config.Providers[providerIndex];
      if (!provider) {
        return config;
      }
      provider.icon = icon.trim() || undefined;
      return config;
    });
    setConfigDraft(next);
    void persistConfig(next, setActionError);
  }

  // K6 (方案 20260905_02):凭据池策略 / pinned 账号 / 账号数组编辑,走现有
  // 配置保存链路(buildConfigUpdate + persistConfig)。
  // 2026-09-06 两模式切换(固定密钥 / 负载均衡),各模式记忆自己的启用集合:
  //   固定侧记忆 = pinnedAccountId(单启用那把),负载均衡侧记忆 =
  //   balancedEnabledAccountIds(启用 id 快照)。切换时互换记忆并恢复对侧
  //   的开关状态——同侧往返恢复原状(方案 §3.6 第二轮调整 ④)。
  function setProviderCredentialMode(providerIndex: number, mode: ProviderKeyMode) {
    const next = buildConfigUpdate((config) => applyProviderCredentialMode(config, providerIndex, mode));
    setConfigDraft(next);
    void persistConfig(next, setActionError);
  }

  // 2026-09-06 第四轮:固定密钥模式的开关 = 单启用 + 固定侧记忆同步,**单次**
  // 配置写入。原先 accounts 与 pinnedAccountId 分两个 setter 连续调用,
  // 而 buildConfigUpdate 基于当前渲染闭包的 draftConfig 快照,第二次写会把
  // 第一次的改动覆盖回去 → 开关"点击失效,多点几次才生效"。
  function setProviderFixedKey(providerIndex: number, accountId: string) {
    const next = buildConfigUpdate((config) => applyProviderFixedKey(config, providerIndex, accountId));
    setConfigDraft(next);
    void persistConfig(next, setActionError);
  }

  // 2026-09-06 第五轮:详情面板统一渲染密钥列表。打开面板时把存量 legacy
  // 单密钥迁移为列表第一行(行为不变:同值密钥进池),**单次**配置写入完成
  // 「建行 + 清空 legacy」。2026-10-11 根因修复:api_key 写**显式 null**——
  // 空串在三态里是「不动 secrets」,旧写法清不掉 `provider:{id}:api_key`,
  // 读取侧(load_app_config)注水后影子键复活,「检测可用性」被它劫持
  // (池非空但供应商级仍有密钥的存量供应商一并清理)。
  function migrateProviderLegacyKey(providerIndex: number) {
    const next = buildConfigUpdate((config) => applyProviderLegacyKeyMigration(config, providerIndex));
    setConfigDraft(next);
    void persistConfig(next, setActionError);
  }

  function setProviderAccounts(providerIndex: number, accounts: ProviderCredentialConfig[]) {
    const next = buildConfigUpdate((config) => applyProviderAccounts(config, providerIndex, accounts));
    setConfigDraft(next);
    void persistConfig(next, setActionError);
  }

  async function checkProviderConnectivity(providerIndex: number, modelsToCheck?: string[]): Promise<ProviderConnectivityCheckReport> {
    const provider = draftConfig.Providers[providerIndex];
    if (!provider) {
      const emptyReport: ProviderConnectivityCheckReport = { failed: [], passed: [], results: [] };
      return emptyReport;
    }
    // 2026-09-13 回归修复:探测密钥取网关会实际使用的生效密钥 —— 纯凭据池
    // 供应商(供应商级 api_key 为空)回落 accounts 中的启用密钥,否则检测
    // 恒 401。显式传参而非塞进草稿,避免编辑保存时把账户密钥回写进
    // 供应商级 api_key。详情路径不写共享探测状态(临时/正式互不串写)。
    return runProviderConnectivityCheck(
      createProviderDraftFromProvider(provider),
      modelsToCheck,
      providerProbeApiKey(provider),
      false
    );
  }

  /** 侧栏供应商拖拽排序(2026-09-13):整体数组替换,随现有配置链路落库。 */
  function reorderProviders(next: GatewayProviderConfig[]) {
    const updated = buildConfigUpdate((config) => {
      config.Providers = next;
      return config;
    });
    setConfigDraft(updated);
    void persistConfig(updated, setActionError);
  }

  function closeProviderDraft() {
    providerConnectivityRequestId.current += 1;
    setProviderEditIndex(undefined);
    setProviderTemporary(false);
    setProviderDraft(createProviderDraft(draftConfig.Providers));
    setProviderConnectivityProbe(undefined);
    setProviderProbeError("");
    setProviderConnectivityLoading(false);
  }

  function addModel(providerIndex: number, name: string, fields: { contextWindow?: number; inputTypes?: ProviderModelInputType[]; maxOutputTokens?: number }) {
    const next = buildConfigUpdate((config) => {
      const provider = config.Providers[providerIndex];
      const trimmed = name.trim();
      if (!provider || !trimmed) {
        return config;
      }
      const models = mergeProviderModelLists([...provider.models, trimmed]);
      provider.models = models;
      if (fields.contextWindow !== undefined) {
        const windows = { ...(provider.modelContextWindow ?? {}) };
        windows[trimmed] = fields.contextWindow;
        provider.modelContextWindow = modelContextWindowForModels(windows, models);
      }
      if (fields.maxOutputTokens !== undefined) {
        const limits = { ...(provider.modelMaxOutputTokens ?? {}) };
        limits[trimmed] = fields.maxOutputTokens;
        provider.modelMaxOutputTokens = modelMaxOutputTokensForModels(limits, models);
      }
      const inputTypes = normalizeModelInputTypes(fields.inputTypes);
      if (inputTypes) {
        provider.modelInputTypes = modelInputTypesForModels(
          { ...(provider.modelInputTypes ?? {}), [trimmed]: inputTypes },
          models
        );
      }
      return config;
    });
    setConfigDraft(next);
    void persistConfig(next, setActionError);
  }

  // 20261017_01:批量添加(获取模型面板多选)。单次 buildConfigUpdate +
  // 单次 persist(循环 addModel 会写 N 次库);已存在的模型跳过,不覆盖
  // 既有元数据;元数据值作为批量默认值。全部已存在 = 无操作不落库。
  function addModels(providerIndex: number, names: string[], fields: { contextWindow?: number; inputTypes?: ProviderModelInputType[]; maxOutputTokens?: number }) {
    let changed = false;
    const next = buildConfigUpdate((config) => {
      const provider = config.Providers[providerIndex];
      if (!provider) {
        return config;
      }
      const fresh = names
        .map((name) => name.trim())
        .filter((name) => name && !provider.models.includes(name));
      if (fresh.length === 0) {
        return config;
      }
      changed = true;
      const models = mergeProviderModelLists([...provider.models, ...fresh]);
      provider.models = models;
      if (fields.contextWindow !== undefined) {
        const windows = { ...(provider.modelContextWindow ?? {}) };
        for (const name of fresh) {
          windows[name] = fields.contextWindow;
        }
        provider.modelContextWindow = modelContextWindowForModels(windows, models);
      }
      if (fields.maxOutputTokens !== undefined) {
        const limits = { ...(provider.modelMaxOutputTokens ?? {}) };
        for (const name of fresh) {
          limits[name] = fields.maxOutputTokens;
        }
        provider.modelMaxOutputTokens = modelMaxOutputTokensForModels(limits, models);
      }
      const inputTypes = normalizeModelInputTypes(fields.inputTypes);
      if (inputTypes) {
        const map = { ...(provider.modelInputTypes ?? {}) };
        for (const name of fresh) {
          map[name] = inputTypes;
        }
        provider.modelInputTypes = modelInputTypesForModels(map, models);
      }
      return config;
    });
    if (!changed) {
      return;
    }
    setConfigDraft(next);
    void persistConfig(next, setActionError);
  }

  function removeModel(providerIndex: number, model: string) {
    const next = buildConfigUpdate((config) => {
      const provider = config.Providers[providerIndex];
      if (!provider) {
        return config;
      }
      provider.models = provider.models.filter((item) => item !== model);
      const models = mergeProviderModelLists(provider.models);
      if (provider.modelContextWindow) {
        provider.modelContextWindow = modelContextWindowForModels(provider.modelContextWindow, models);
      }
      if (provider.modelDescriptions) {
        provider.modelDescriptions = modelDescriptionsForModels(provider.modelDescriptions, models);
      }
      if (provider.modelDisplayNames) {
        provider.modelDisplayNames = modelDisplayNamesForModels(provider.modelDisplayNames, models);
      }
      if (provider.modelInputTypes) {
        provider.modelInputTypes = modelInputTypesForModels(provider.modelInputTypes, models);
      }
      if (provider.modelMaxOutputTokens) {
        provider.modelMaxOutputTokens = modelMaxOutputTokensForModels(provider.modelMaxOutputTokens, models);
      }
      if (provider.modelMetadata) {
        provider.modelMetadata = modelMetadataForModels(provider.modelMetadata, models);
      }
      return config;
    });
    setConfigDraft(next);
    void persistConfig(next, setActionError);
  }

  async function confirmProviderDelete() {
    if (providerDeleteIndex === undefined) {
      return;
    }
    const index = providerDeleteIndex;
    if (await removeProvider(index)) {
      setProviderDeleteIndex(undefined);
    }
  }

  return {
    providerTemporary,
    providerDeleteIndex,
    setProviderDeleteIndex,
    providerEditIndex,
    setProviderEditIndex,
    providerCreatedIndex,
    setProviderCreatedIndex,
    providerDraft,
    setProviderDraft,
    providerConnectivityProbe,
    setProviderConnectivityProbe,
    providerConnectivityLoading,
    setProviderConnectivityLoading,
    providerProbeError,
    setProviderProbeError,
    providerConnectivityRequestId,
    openAddProviderDialog,
    updateProviderDraft,
    checkProviderDraft,
    checkProviderConnectivity,
    closeProviderDraft,
    submitProviderDraft,
    removeProvider,
    reorderProviders,
    updateProviderModelDescription,
    updateModel,
    setProviderEnabled,
    setProviderBaseUrl,
    setProviderUrlMode,
    setProviderClientFingerprint,
    setProviderHeaders,
    setProviderProtocol,
    setProviderProxy,
    setProviderName,
    setProviderIcon,
    setProviderCredentialMode,
    setProviderFixedKey,
    migrateProviderLegacyKey,
    setProviderAccounts,
    addModel,
    addModels,
    removeModel,
    confirmProviderDelete
  };
}
