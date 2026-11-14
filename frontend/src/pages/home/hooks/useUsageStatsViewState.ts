// R?: usage-stats view domain — range / credential / provider / model
// filters + snapshot queries, gated on the usage-stats view being active
// (same pattern as useLogsState).
//
// F2a.4/F2a.5: 双快照迁移到 Query——range 驱动的指标快照 + 固定 30 天活跃度
// 快照，5s refetchInterval；deepEqual 去重由 Query structuralSharing 天然承担。
import { useCallback, useEffect, useMemo, useState } from "react";
import { keepPreviousData, useQuery } from "@tanstack/react-query";
import {
  AppConfig, createEmptyUsageStats, fallbackUsageStats,
  UsageStatsFilter, UsageStatsRange, UsageStatsSnapshot, ViewId
} from "../shared/index";
import { api, hasBridge } from "@/lib/api";
import { queryKeys } from "@/lib/queryKeys";

export type UsageStatsViewStateOptions = {
  activeView: ViewId;
  draftConfig: AppConfig;
};

export function useUsageStatsViewState({ activeView, draftConfig }: UsageStatsViewStateOptions) {
  const [usageStatsRange, setUsageStatsRange] = useState<UsageStatsRange>("today");
  const [usageStatsCredential, setUsageStatsCredential] = useState("");
  const [usageStatsModel, setUsageStatsModel] = useState("");
  // 20261104_01 阶段三 · B-1a:调用方筛选;20261106(B-1b)起值是**归一 id**
  // (产品名,如 `ZCode`),不再是原始 UA。
  const [usageStatsClient, setUsageStatsClient] = useState("");
  // Activity calendar always reads the last 30 days so the heatmap stays a
  // stable "recent history" view: it follows the credential filter only —
  // the range selector and model filter drive the metric cards + trend
  // charts. Keeping model out of activityFilter also keeps its `models`
  // list stable as the model-filter dropdown options.

  const filter = useMemo<UsageStatsFilter>(() => ({
    ...(usageStatsRange === "today" ? { includeProxy: true } : {}),
    ...(usageStatsCredential ? { credential: usageStatsCredential } : {}),
    ...(usageStatsModel ? { model: usageStatsModel } : {}),
    // 20261104_01 阶段三 · B-1a:调用方过滤(后端按归一 id 精确匹配)。
    ...(usageStatsClient ? { client: usageStatsClient } : {})
  }), [usageStatsClient, usageStatsCredential, usageStatsModel, usageStatsRange]);

  const activityFilter = useMemo<UsageStatsFilter>(() => ({
    ...(usageStatsCredential ? { credential: usageStatsCredential } : {})
  }), [usageStatsCredential]);

  const viewActive = activeView === "usage-stats";

  const statsQuery = useQuery({
    queryKey: queryKeys.usageStats(usageStatsRange, filter),
    queryFn: () => (hasBridge() ? api.getUsageStats(usageStatsRange, filter) : createEmptyUsageStats(usageStatsRange)),
    enabled: viewActive,
    placeholderData: keepPreviousData,
    retry: false
  });

  const activityQuery = useQuery({
    queryKey: queryKeys.usageActivity(activityFilter),
    queryFn: () => (hasBridge() ? api.getUsageStats("30d", activityFilter) : createEmptyUsageStats("30d")),
    enabled: viewActive,
    placeholderData: keepPreviousData,
    retry: false
  });

  // 成本节省三本账(方案 20260905_03 §4.6):随 range 切换;金额为估算。
  const savingsQuery = useQuery({
    queryKey: queryKeys.savingsStats(usageStatsRange),
    queryFn: () => api.getSavingsStats(usageStatsRange),
    enabled: viewActive && hasBridge(),
    placeholderData: keepPreviousData,
    retry: false
  });

  const usageStats: UsageStatsSnapshot = statsQuery.data ?? fallbackUsageStats;
  const usageStatsActivity: UsageStatsSnapshot = activityQuery.data ?? fallbackUsageStats;

  // 模型筛选下拉选项:活动快照(仅按密钥过滤)的 models[].label,即
  // "供应商/模型名" 展示串;后端 model_filter 按 label 三选一匹配。
  const usageStatsModelOptions = useMemo(() => {
    const labels = usageStatsActivity.models.map((row) => row.label).filter(Boolean);
    return Array.from(new Set(labels)).sort((a, b) => a.localeCompare(b));
  }, [usageStatsActivity.models]);

  // 20261104_01 阶段三 · B-1a:调用方下拉选项。**不能用 `clientModels[].label`**
  // —— 那是 "{client} / {模型展示名}" 的拼接串(usage_store.rs 的 client_models
  // 分组 label);纯 client 取 `clientModels[].client` 再去重。
  // 20261106(B-1b):`client` 已是**归一 id**(产品名)→ 同一客户端的多个版本号
  // 在后端就合成了一条,下拉里只出现一次。
  // 选项同样取自活动快照(仅按密钥过滤)→ 换 range / 模型筛选时下拉不塌缩。
  const usageStatsClientOptions = useMemo(() => {
    const clients = usageStatsActivity.clientModels
      .map((row) => row.client ?? "")
      .filter(Boolean);
    return Array.from(new Set(clients)).sort((a, b) => a.localeCompare(b));
  }, [usageStatsActivity.clientModels]);

  // Manual refresh (the toolbar RefreshButton). Returns a promise so the
  // button can animate until both snapshots settle; the spinning state is
  // derived from isFetching (set by this refetch + the poll alike).
  const refreshUsageStats = useCallback((showLoading = false) => {
    void showLoading;
    return Promise.all([statsQuery.refetch(), activityQuery.refetch(), savingsQuery.refetch()])
      .then(() => undefined).catch(() => undefined);
  }, [activityQuery, savingsQuery, statsQuery]);

  // Clear stale credential filter when the config no longer contains the key.
  useEffect(() => {
    if (!usageStatsCredential) {
      return;
    }
    if (!(draftConfig.APIKEYS ?? []).some((apiKey) => apiKey.id === usageStatsCredential)) {
      setUsageStatsCredential("");
    }
  }, [draftConfig.APIKEYS, usageStatsCredential]);

  // 所选模型从选项列表消失(数据过期/供应商改名)时清空,避免筛出恒空结果;
  // 仅在选项非空时判定,避免快照加载间隙误清。
  useEffect(() => {
    if (!usageStatsModel) {
      return;
    }
    if (usageStatsModelOptions.length > 0 && !usageStatsModelOptions.includes(usageStatsModel)) {
      setUsageStatsModel("");
    }
  }, [usageStatsModel, usageStatsModelOptions]);

  // 所选调用方从选项列表消失(数据过期/窗口内无调用)时清空,避免筛出恒空
  // 结果;仅在选项非空时判定,避免快照加载间隙误清。
  useEffect(() => {
    if (!usageStatsClient) {
      return;
    }
    if (usageStatsClientOptions.length > 0 && !usageStatsClientOptions.includes(usageStatsClient)) {
      setUsageStatsClient("");
    }
  }, [usageStatsClient, usageStatsClientOptions]);

  return {
    usageStats,
    usageStatsActivityModelSeries: usageStatsActivity.modelSeries,
    usageStatsActivitySeries: usageStatsActivity.series,
    usageStatsClient,
    usageStatsClientOptions,
    usageStatsCredential,
    usageStatsLoading: statsQuery.isFetching || activityQuery.isFetching || savingsQuery.isFetching,
    usageStatsSavings: savingsQuery.data,
    usageStatsModel,
    usageStatsModelOptions,
    usageStatsRange,
    refreshUsageStats,
    setUsageStatsClient,
    setUsageStatsCredential,
    setUsageStatsModel,
    setUsageStatsRange
  };
}
