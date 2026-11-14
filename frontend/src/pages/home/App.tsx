import { useEffect, useMemo, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { QueryClientProvider } from "@tanstack/react-query";
import { LayoutGroup, useReducedMotion } from "motion/react";
import {
    appCopy, AppI18nContext, AppInfo,
  AppLanguagePreference,  AppToast,
    detectSystemLanguage, detectSystemTheme,
  fallbackGatewayStatus, fallbackInfo,
    formatAppError,
  GatewayStatus,
  isMacPlatform, isWindowsPlatform,
    mergeProviderModelLists,
  NavigationId,
   readLanguagePreference,   ResolvedLanguage,
  ResolvedTheme,
  splitLines, translateAppErrorMessage, translateText,
   SettingsPageId,
   ViewId,
} from "./shared/index";
import { availableGatewayModelIds } from "@/contracts/app";
import { api, hasBridge } from "@/lib/api";
import { queryClient } from "@/lib/queryClient";
import { queryKeys } from "@/lib/queryKeys";
import { applyWindowTheme, readPageBackground } from "@/lib/tauri-window";
import {
  AppDialogStack, LightToast, MainLayout
} from "./components/index";
// 20261102_01 P6:「服务停止」全屏落地态(2026-11-03 拆成「正在停止 / 已停止」两阶段)。
import { ServiceStopScreen } from "./components/service-stop";
import { useConfigState } from "./hooks/useConfigState";
import { useApiKeysState } from "./hooks/useApiKeysState";
import { useUpdateFlowState } from "./hooks/useUpdateFlowState";
import { useCallLogsState } from "./hooks/useCallLogsState";
import { useUsageStatsViewState } from "./hooks/useUsageStatsViewState";
import { useSettingsPrefsState } from "./hooks/useSettingsPrefsState";
import { useProvidersState } from "./hooks/useProvidersState";
import { useModelGroupsState } from "./hooks/useModelGroupsState";
import { useGatewayEvents } from "./hooks/useGatewayEvents";
import { useQueryErrorBridge } from "./hooks/useQueryErrorBridge";
import { useServiceStopPhase } from "./hooks/useServiceStopPhase";

// F2a.1: composition root holds the QueryClientProvider so the domain hooks
// (which call useQuery/useMutation) sit underneath it. Tests render <App/>
// unchanged.
function App() {
  return (
    <QueryClientProvider client={queryClient}>
      <AppCore />
    </QueryClientProvider>
  );
}

function AppCore() {
  // 20261101_03:「快速开始」为应用第一页(接入指引);「返回应用」回到的
  // 首页视图(设置视图外的最近一次导航)同源默认。
  const [activeView, setActiveView] = useState<ViewId>("quickstart");
  const lastHomeViewRef = useRef<ViewId>("quickstart");
  const [selectedCallLog, setSelectedCallLog] = useState<import("@/contracts/app").CallLogEntry>();
  const [actionError, setActionError] = useState("");
  const [compactLayout, setCompactLayout] = useState(() => window.matchMedia("(max-width: 720px)").matches);
  const [toast, setToast] = useState<AppToast>();
  const [languagePreference, setLanguagePreference] = useState<AppLanguagePreference>(() => readLanguagePreference());
  const [systemLanguage, setSystemLanguage] = useState<ResolvedLanguage>(() => detectSystemLanguage());
  const [systemTheme, setSystemTheme] = useState<ResolvedTheme>(() => detectSystemTheme());
  const resolvedLanguage = languagePreference === "system" ? systemLanguage : languagePreference;
  const copy = appCopy[resolvedLanguage];
  const t = useMemo(() => (value: string) => translateText(copy, value), [copy]);
  const formatError = useMemo(() => (error: unknown) => formatAppError(copy, error), [copy]);

  useQueryErrorBridge(formatError, setActionError);

  // 20261101_02 T1:背景特效已整体下线,顺手清一次历史 localStorage 偏好键。
  useEffect(() => {
    localStorage.removeItem("wr:particle-effect");
  }, []);

  // 20261102_01 P6:服务停止落地态(阶段与确认轮询见 useServiceStopPhase)。
  const {
    servicePhase, serviceRestartHint, handleServiceStopping,
    handleServiceRestored, markStopped,
  } = useServiceStopPhase();

  // F2b.1: 管理面 SSE 订阅——事件驱动 Query 失效,轮询已退役(F2b.5)。
  //
  // 2026-11-03:进入停止态后**取消订阅 + 停止自动重连** —— 后端正在退出,
  // 重连只会徒劳地刷失败请求。
  const { status: gatewayEventsStatus } = useGatewayEvents({
    enabled: hasBridge() && servicePhase === "normal",
    onServiceStopping: () => handleServiceStopping()
  });
  const {
    draftConfig,
    configLoaded,
    updateConfig,
    buildConfigUpdate,
    setConfigDraft,
    persistConfig,
    persistApiKeys
  } = useConfigState({ formatError, setActionError, t });
  // F2a.5: appInfo / 运行状态迁移到 Query。自适应轮询节奏已随 F2b.5 轮询
  // 退役移除：运行状态由 /events 的 gateway-status 事件（3s 节流）驱动失效
  // 刷新；SSE 断连期间由 useGatewayEvents 的降级轮询兜底。
  const appInfoQuery = useQuery({
    queryKey: queryKeys.appInfo,
    queryFn: () => api.getAppInfo(),
    enabled: hasBridge(),
    staleTime: Infinity,
    retry: false
  });
  const appInfo: AppInfo = appInfoQuery.data ?? fallbackInfo;

  const gatewayStatusQuery = useQuery({
    queryKey: queryKeys.gatewayStatus,
    queryFn: () => api.getGatewayStatus(),
    enabled: hasBridge(),
    retry: false
  });
  const gatewayStatus: GatewayStatus = gatewayStatusQuery.data ?? fallbackGatewayStatus;

  // 后端不可达检测(3s 去抖):单次轮询抖动不该把整页换成落地页。
  useEffect(() => {
    if (!gatewayStatusQuery.isError || !hasBridge()) {
      return;
    }
    const timer = window.setTimeout(() => {
      markStopped();
    }, 3000);
    return () => window.clearTimeout(timer);
  }, [gatewayStatusQuery.isError, gatewayStatusQuery.errorUpdatedAt, markStopped]);

  const {
    apiKeys,
    apiKeyAddOpen,
    setApiKeyAddOpen,
    apiKeyDraft,
    apiKeyEditDraft,
    setApiKeyEditIndex,
    apiKeyError,
    createdApiKey,
    setCreatedApiKey,
    apiKeyEditItem,
    canSubmitApiKey,
    canSubmitApiKeyEdit,
    openAddApiKeyDialog,
    updateApiKeyDraft,
    openEditApiKeyDialog,
    updateApiKeyEditDraft,
    submitApiKeyDraft,
    submitApiKeyEditDraft,
    removeApiKey,
    toggleApiKey
  } = useApiKeysState({ draftConfig, buildConfigUpdate, setConfigDraft, persistApiKeys, t });
  const apiKeyModelOptions = useMemo(() => availableGatewayModelIds(draftConfig), [draftConfig]);
  const {
    updateActionBusy,
    updateActionError,
    setUpdateActionError,
    updateDialogOpen,
    setUpdateDialogOpen,
    updateDialogStatus,
    openUpdateDialog,
    checkForAppUpdate,
    downloadAppUpdate,
    installAppUpdate
  } = useUpdateFlowState({ appInfoVersion: appInfo.version, formatError, t });
  const {
    callLogError,
    callLogFilter,
    callLogLoading,
    callLogPage,
    refreshCallLogs,
    updateCallLogFilter
  } = useCallLogsState({ activeView, formatError });
  const {
    usageStats: usageStatsSnapshot,
    usageStatsActivityModelSeries,
    usageStatsActivitySeries,
    usageStatsClient,
    usageStatsClientOptions,
    usageStatsCredential,
    usageStatsLoading,
    usageStatsModel,
    usageStatsModelOptions,
    usageStatsRange,
    usageStatsSavings,
    refreshUsageStats,
    setUsageStatsClient,
    setUsageStatsCredential,
    setUsageStatsModel,
    setUsageStatsRange
  } = useUsageStatsViewState({ activeView, draftConfig });
  const {
    settingsInitialPage,
    changeThemePreference,
    changeLaunchAtLogin,
    changeObservabilityConfig,
    changeProxyConfig,
    openSettingsPage,
    changeLanguagePreference
  } = useSettingsPrefsState({
    draftConfig, updateConfig, setLanguagePreference, setSystemLanguage, setSystemTheme
  });
  const {
    providerTemporary,
    providerDeleteIndex,
    setProviderDeleteIndex,
    providerCreatedIndex,
    setProviderCreatedIndex,
    providerDraft,
    providerConnectivityProbe,
    providerConnectivityLoading,
    providerProbeError,
    openAddProviderDialog,
    updateProviderDraft,
    checkProviderDraft,
    checkProviderConnectivity,
    closeProviderDraft,
    submitProviderDraft,
    reorderProviders,
    updateModel,
    setProviderEnabled,
    setProviderBaseUrl,
    setProviderUrlMode,
    setProviderClientFingerprint,
    setProviderHeaders,
    setProviderProtocol,
    setProviderProxy,
    setProviderName,
    setProviderCredentialMode,
    setProviderFixedKey,
    migrateProviderLegacyKey,
    setProviderAccounts,
    addModel,
    addModels,
    removeModel,
    confirmProviderDelete,
  } = useProvidersState({
    draftConfig, configLoaded, copy,
    showToast, t, formatError,
    buildConfigUpdate, setConfigDraft, persistConfig, setActionError
  });

  // 聚合模型(20260905_04 M6):草稿编辑 + 保存/删除走同一配置链路。
  const {
    modelGroups,
    groupDraft: modelGroupDraft,
    temporaryModelGroupDraft,
    groupDraftError: modelGroupError,
    modelGroupCreatedId,
    setModelGroupCreatedId,
    openAddModelGroup,
    selectTemporaryModelGroup,
    openEditModelGroup,
    closeModelGroupDraft,
    updateModelGroupDraft,
    updateModelGroupTarget,
    addModelGroupTarget,
    addModelGroupTargetsForModelName,
    removeModelGroupTarget,
    reorderModelGroupTargets,
    submitModelGroupDraft,
    setModelGroupEnabled,
    reorderModelGroups,
    removeModelGroup
  } = useModelGroupsState({
    draftConfig, buildConfigUpdate, setConfigDraft, persistConfig, setActionError, t
  });

  useEffect(() => {
    const mediaQuery = window.matchMedia("(max-width: 720px)");
    const updateCompactLayout = () => setCompactLayout(mediaQuery.matches);
    updateCompactLayout();
    mediaQuery.addEventListener("change", updateCompactLayout);
    return () => mediaQuery.removeEventListener("change", updateCompactLayout);
  }, []);

  useEffect(() => {
    const root = document.documentElement;
    const theme = draftConfig.theme || "system";
    // Always resolve to an explicit attribute (system tracks the OS via
    // systemTheme) so the CSS variables and the dark: custom variant share
    // one source of truth.
    root.dataset.theme = theme === "system" ? systemTheme : theme;
    // 20261102_01 P4-5:Tauri 壳的标题栏配色随应用主题同步。
    // 底色取自 CSS 变量 `--background`(与页面背景同源,不另写一份常量);
    // `system` 偏好**不强制**窗口主题 —— 强制会把 webview 的
    // prefers-color-scheme 钉死,系统主题在运行期切换就传不到这里。
    // 非 Tauri 壳里是空操作(浏览器没有可控制的窗口)。
    void applyWindowTheme(theme, readPageBackground());
  }, [draftConfig.theme, systemTheme]);

  useEffect(() => {
    document.documentElement.lang = resolvedLanguage === "zh" ? "zh-CN" : "en";
    // Keep the shell-side language state in sync with the UI language.
    void window.wr?.setAppLanguage?.(resolvedLanguage);
  }, [resolvedLanguage]);

  useEffect(() => {
    const updateSystemLanguage = () => setSystemLanguage(detectSystemLanguage());
    window.addEventListener("languagechange", updateSystemLanguage);
    return () => window.removeEventListener("languagechange", updateSystemLanguage);
  }, []);

  useEffect(() => {
    const mediaQuery = window.matchMedia("(prefers-color-scheme: dark)");
    const updateSystemTheme = () => setSystemTheme(mediaQuery.matches ? "dark" : "light");
    updateSystemTheme();
    mediaQuery.addEventListener("change", updateSystemTheme);
    return () => mediaQuery.removeEventListener("change", updateSystemTheme);
  }, []);

  // 桌面事件订阅保持不变（config 拉取已迁移到 useConfigState 的 Query、
  // appInfo/状态轮询已迁移到上方 Query，F2a.5）。
  useEffect(() => {
    if (!window.wr) {
      return;
    }

    const unsubscribeOpenSettings = window.wr.onOpenSettingsRequest(() => openSettingsView("general"));
    const unsubscribeOpenUpdate = window.wr.onOpenUpdateRequest(openUpdateDialog);
    return () => {
      unsubscribeOpenSettings();
      unsubscribeOpenUpdate();
    };
  }, []);

  const providers = useMemo(() => draftConfig.Providers.map((provider, index) => ({ provider, index })), [draftConfig.Providers]);
  const providerDeleteItem = providerDeleteIndex === undefined ? undefined : draftConfig.Providers[providerDeleteIndex];
  const gatewayEndpoint = gatewayStatus.endpoint || draftConfig.routerEndpoint;
  const gatewayStartupError = gatewayStatus.state === "error"
    ? translateAppErrorMessage(copy, gatewayStatus.lastError || "Service did not start.")
    : "";
  const toastTimer = useRef<number>();

  const shouldReduceMotion = useReducedMotion();
  const isMac = isMacPlatform(appInfo.platform);
  // 20261101_03:Windows 隐藏式标题栏(titleBarOverlay)需要应用内拖拽区。
  const isWindows = isWindowsPlatform(appInfo.platform);
  // 左下角「开发」入口仅桌面 dev 构建可见(打包构建不再提供 DevTools 快捷入口)。
  const isDevShell = Boolean(window.wr?.isDev);
  const providerTypedModels = splitLines(providerDraft.modelsText);
  const providerDialogModels = mergeProviderModelLists(providerDraft.selectedModels, providerTypedModels);
  const canSubmitProvider =
    Boolean(providerDraft.name.trim() && providerDraft.baseUrl.trim()) &&
    providerDialogModels.length > 0;

  function showToast(message: string) {
    if (toastTimer.current !== undefined) {
      window.clearTimeout(toastTimer.current);
    }
    setToast({ id: Date.now(), message });
    toastTimer.current = window.setTimeout(() => {
      setToast(undefined);
      toastTimer.current = undefined;
    }, 1800);
  }

  function selectNavigationItem(id: NavigationId) {
    // 20261101_03:「返回应用」目标 —— 记录最近一次的首页视图(设置视图除外)。
    if (id !== "settings") {
      lastHomeViewRef.current = id;
    }
    setActiveView(id);
  }

  // 设置模式:侧栏顶部「返回应用」回到进入设置前的首页视图。
  function exitSettings() {
    setActiveView(lastHomeViewRef.current);
  }

  // 2026-09-10:设置视图化。深链(供应商页代理空态/启动错误横幅/桌面桥事件)
  // 先定分区,再切到 settings 视图;左下角「设置」入口走 selectNavigationItem,
  // 保留用户在设置内停留的分区(KeepAlive)。
  function openSettingsView(page: SettingsPageId) {
    openSettingsPage(page);
    setActiveView("settings");
  }

  return (
    <AppI18nContext.Provider value={copy}>
      <LayoutGroup id="home-shell">
        <div className="relative flex h-full min-h-0 w-full min-w-0 overflow-hidden bg-background text-foreground max-[720px]:flex-col">
          {servicePhase !== "normal" ? (
            <ServiceStopScreen
              onRestored={handleServiceRestored}
              phase={servicePhase}
              restartHint={serviceRestartHint}
            />
          ) : (
            <MainLayout
              actionError={actionError}
              activeView={activeView}
              compactLayout={compactLayout}
              copy={copy}
              // 20261101_03:左下角网关行展示的运行地址(去协议前缀)。
              gatewayAddress={(gatewayStatus.endpoint || draftConfig.routerEndpoint).replace(/^https?:\/\//, "")}
              gatewayEventsStatus={gatewayEventsStatus}
              gatewayStartupError={gatewayStartupError}
              isDevShell={isDevShell}
              isMac={isMac}
              isWindows={isWindows}
              onDismissActionError={() => setActionError("")}
              onExitSettings={exitSettings}
              onOpenServerSettings={() => openSettingsView("general")}
              onSelectNavigationItem={selectNavigationItem}
              onSelectSettingsPage={openSettingsPage}
              settingsPage={settingsInitialPage}
              shouldReduceMotion={shouldReduceMotion}
              sidebarOpen={true}
              viewProps={{
                  apiKeys: {
                    addApiKey: openAddApiKeyDialog,
                    apiKeys,
                    config: draftConfig,
                    editApiKey: openEditApiKeyDialog,
                    endpoint: gatewayEndpoint,
                    error: apiKeyError,
                    gatewayStatus,
                    notify: showToast,
                    removeApiKey,
                    toggleApiKey
                  },
                  callLogs: {
                    error: callLogError,
                    filter: callLogFilter,
                    loading: callLogLoading,
                    onOpenDetail: (item) => {
                      setSelectedCallLog(item);
                      setActiveView("call-log-detail");
                    },
                    page: callLogPage,
                    refreshLogs: refreshCallLogs,
                    updateFilter: updateCallLogFilter
                  },
                  callLogDetail: {
                    item: selectedCallLog,
                    onBack: () => setActiveView("call-logs")
                  },
                  // 2026-11-01:模型指纹从设置分区升级为独立视图。
                  modelFingerprints: {
                    providers: draftConfig.Providers
                  },
                  // 20261101_03:快速开始 —— 三步接入指引,应用第一页。
                  quickstart: {
                    endpoint: gatewayEndpoint,
                    onOpenApiKeys: () => setActiveView("api-keys"),
                    onOpenCallLogs: () => setActiveView("call-logs"),
                    onOpenProviders: () => setActiveView("providers"),
                    running: gatewayStatus.state === "running"
                  },
                  usageStats: {
                    activityModelSeries: usageStatsActivityModelSeries,
                    activitySeries: usageStatsActivitySeries,
                    apiKeys: draftConfig.APIKEYS ?? [],
                    client: usageStatsClient,
                    clientOptions: usageStatsClientOptions,
                    credential: usageStatsCredential,
                    gatewayStatus,
                    loading: usageStatsLoading,
                    model: usageStatsModel,
                    modelOptions: usageStatsModelOptions,
                    range: usageStatsRange,
                    refreshUsageStats,
                    savings: usageStatsSavings,
                    setClient: setUsageStatsClient,
                    setCredential: setUsageStatsCredential,
                    setModel: setUsageStatsModel,
                    setRange: setUsageStatsRange,
                    stats: usageStatsSnapshot
                  },
                  providers: {
                    addModel,
                    addModels,
                    addProvider: openAddProviderDialog,
                    canSubmitProvider,
                    checkProviderConnectivity,
                    closeProviderDraft,
                    draft: providerDraft,
                    notify: showToast,
                    onCheckProviderDraft: checkProviderDraft,
                    onUpdateProviderDraft: updateProviderDraft,
                    providerTemporary,
                    providerConnectivityLoading,
                    providerConnectivityProbe,
                    providerCreatedIndex,
                    providerDraftError: providerProbeError,
                    providers,
                    removeModel,
                    removeProvider: setProviderDeleteIndex,
                    onReorderProviders: reorderProviders,
                    setProviderBaseUrl,
                    setProviderUrlMode,
                    setProviderClientFingerprint,
                    setProviderHeaders,
                    setProviderCreatedIndex,
                    setProviderCredentialMode,
                    setProviderEnabled,
                    setProviderName,
                    setProviderFixedKey,
                    migrateProviderLegacyKey,
                    setProviderProtocol,
                    setProviderProxy,
                    setProviderAccounts,
                    submitProviderDraft,
                    updateModel,
                    // 供应商级网络代理(20260906_01 F3):预设在设置页集中管理,
                    // 供应商页只做选择;预设列表为空时跳设置页 general 分区。
                    onOpenProxySettings: () => openSettingsView("general"),
                    proxyPresets: draftConfig.proxy.presets ?? [],
                    proxyUpstream: draftConfig.proxy.upstream,
                    // 聚合模型(20260905_04 M6)
                    modelGroups,
                    modelGroupDraft,
                    temporaryModelGroupDraft,
                    modelGroupError,
                    modelGroupCreatedId,
                    setModelGroupCreatedId,
                    openAddModelGroup,
                    selectTemporaryModelGroup,
                    openEditModelGroup,
                    closeModelGroupDraft,
                    submitModelGroupDraft,
                    updateModelGroupDraft,
                    updateModelGroupTarget,
                    addModelGroupTarget,
                    addModelGroupTargetsForModelName,
                    removeModelGroupTarget,
                    reorderModelGroupTargets,
                    setModelGroupEnabled,
                    onReorderModelGroups: reorderModelGroups,
                    removeModelGroup
                  },
                  settings: {
                    appInfo,
                    config: draftConfig,
                    copy,
                    languagePreference,
                    launchAtLogin: Boolean(draftConfig.launchAtLogin),
                    // 20261102_01 P6:服务控制分区需要 toast 提示与落地页回调。
                    notify: showToast,
                    onChangeLaunchAtLogin: changeLaunchAtLogin,
                    onChangeLanguage: changeLanguagePreference,
                    onChangeObservability: changeObservabilityConfig,
                    onChangeProxy: changeProxyConfig,
                    onChangeTheme: changeThemePreference,
                    onServiceStopping: handleServiceStopping,
                    observability: draftConfig.observability,
                    page: settingsInitialPage,
                    systemLanguage,
                    systemTheme,
                    themePreference: draftConfig.theme || "system",
                    updateConfig
                  },
                }}
            />
          )}

          <AppDialogStack
            apiKeyAdd={apiKeyAddOpen ? {
              canSubmit: canSubmitApiKey,
              draft: apiKeyDraft,
              error: apiKeyError,
              modelOptions: apiKeyModelOptions,
              onChange: updateApiKeyDraft,
              onClose: () => setApiKeyAddOpen(false),
              onSubmit: submitApiKeyDraft
            } : undefined}
            apiKeyCreated={createdApiKey ? {
              apiKeyName: createdApiKey.name?.trim() || t("Routing key"),
              apiKeyValue: createdApiKey.key,
              onClose: () => setCreatedApiKey(undefined)
            } : undefined}
            apiKeyEdit={apiKeyEditItem ? {
              canSubmit: canSubmitApiKeyEdit,
              draft: apiKeyEditDraft,
              error: apiKeyError,
              keyId: apiKeyEditItem.key.id,
              modelOptions: apiKeyModelOptions,
              onChange: updateApiKeyEditDraft,
              onClose: () => setApiKeyEditIndex(undefined),
              onSubmit: submitApiKeyEditDraft
            } : undefined}
            providerDelete={providerDeleteItem ? {
              onClose: () => setProviderDeleteIndex(undefined),
              onConfirm: confirmProviderDelete,
              provider: providerDeleteItem
            } : undefined}
            update={updateDialogOpen ? {
              actionBusy: updateActionBusy,
              actionError: updateActionError,
              copy,
              onCheck: checkForAppUpdate,
              onClose: () => {
                if (updateActionBusy !== "install") {
                  setUpdateDialogOpen(false);
                  setUpdateActionError("");
                }
              },
              onDownload: downloadAppUpdate,
              onInstall: installAppUpdate,
              status: updateDialogStatus
            } : undefined}
          />
          <LightToast toast={toast} />
        </div>
      </LayoutGroup>
    </AppI18nContext.Provider>
  );
}

export default App;
