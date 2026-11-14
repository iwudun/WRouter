// R?: usage-stats view — four metrics + trend / distribution / ranking
// tabs. Trend chart is driven by the backend ``modelSeries`` aggregation so
// each model gets its own color; filters (range / API key / model) are
// plain selects in the toolbar.
//
// F1.5: the recharts tabs live in ../usage-stats-charts and are referenced
// ONLY via React.lazy, so recharts stays out of the initial module graph and
// the dashboard first paint does not load the chart chunk.
import { Suspense, lazy, useMemo, useState } from "react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Select } from "@/components/ui/select";
import { CalendarDays, ChartArea, ChartColumn, ChartNoAxesCombined, LoaderCircle } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { cn } from "@/lib/utils";
import type { GatewayStatus, SavingsStatsSnapshot } from "@/contracts/app";

import {
  ApiKeyConfig, GatewayStatusPill, RefreshButton,
  UsageStatsRange, UsageStatsSnapshot, useAppText
} from "../../../shared/index";
import { ChartEmptyState, MODEL_PALETTE } from "../list-charts";
import { UsageStatsHealthSection } from "../usage-stats-health";
import { UsageStatsRangeSelector } from "../usage-stats-range";
import { UsageStatsActivityCalendar } from "./activity";
import { UsageStatsRanking } from "./ranking";
import { DataOverviewSection, UsageStatsCardHeader } from "./overview";
import type {
  ClientUsageRow, RankingDimension, TrendChartKind, TrendData, TrendMetric, TrendModel, TrendRow
} from "./types";

const UsageStatsTrendTab = lazy(() => import("../usage-stats-charts").then((m) => ({ default: m.UsageStatsTrendTab })));

// Suspense skeleton while the charts chunk loads: same centering as the chart
// area so the layout does not shift when the real chart resolves.
function ChartTabFallback() {
  return (
    <div className="flex h-full min-h-0 items-center justify-center">
      <LoaderCircle className="h-5 w-5 animate-spin text-muted-foreground" />
    </div>
  );
}

// Type-only exports (erased at runtime): the lazy charts chunk imports them
// without creating a static edge back to this module.
export function aggregateClientUsage(rows: UsageStatsSnapshot["clientModels"]): ClientUsageRow[] {
  const byClient = new Map<string, ClientUsageRow>();
  for (const row of rows) {
    const client = row.client ?? "";
    if (!client) {
      continue;
    }
    let acc = byClient.get(client);
    if (!acc) {
      acc = {
        cacheRatio: 0,
        cacheTokens: 0,
        client,
        costUsd: 0,
        errorCount: 0,
        inputTokens: 0,
        models: [],
        requestCount: 0,
        totalTokens: 0
      };
      byClient.set(client, acc);
    }
    acc.requestCount += row.requestCount;
    acc.errorCount += row.errorCount;
    acc.totalTokens += row.totalTokens;
    acc.inputTokens += row.inputTokens;
    acc.cacheTokens += row.cacheTokens;
    acc.costUsd += row.costUsd;
    // 模型展示名:后端 label = "{client} / {model_label}" → 截掉前缀段。
    const prefix = `${client} / `;
    const modelLabel = row.label.startsWith(prefix)
      ? row.label.slice(prefix.length)
      : (row.model ?? "");
    if (modelLabel && !acc.models.includes(modelLabel)) {
      acc.models.push(modelLabel);
    }
  }
  for (const acc of byClient.values()) {
    acc.cacheRatio = acc.inputTokens > 0 ? acc.cacheTokens / acc.inputTokens : 0;
    acc.models.sort((a, b) => a.localeCompare(b));
  }
  return Array.from(byClient.values());
}

export function UsageStatsView({
  activityModelSeries,
  activitySeries,
  apiKeys,
  client,
  clientOptions,
  credential,
  gatewayStatus,
  loading,
  model,
  modelOptions,
  range,
  refreshUsageStats,
  savings,
  setClient,
  setCredential,
  setModel,
  setRange,
  stats
}: {
  // 固定按最近 30 天拉取的活跃度序列，驱动日历热力图（独立于 range 选择器）。
  activityModelSeries: UsageStatsSnapshot["modelSeries"];
  activitySeries: UsageStatsSnapshot["series"];
  apiKeys: ApiKeyConfig[];
  // 20261104_01 阶段三 · B-1a:调用方筛选 —— 值为纯 client 串(20261106 起
  // 是**归一 id**,如 `ZCode`)。
  client: string;
  clientOptions: string[];
  credential: string;
  gatewayStatus: GatewayStatus;
  loading: boolean;
  // 模型筛选:值为展示 label("供应商/模型名"),空串 = 全部模型。
  model: string;
  modelOptions: string[];
  range: UsageStatsRange;
  refreshUsageStats: (showLoading?: boolean) => Promise<void>;
  savings?: SavingsStatsSnapshot;
  setClient: (value: string) => void;
  setCredential: (value: string) => void;
  setModel: (value: string) => void;
  setRange: (range: UsageStatsRange) => void;
  stats: UsageStatsSnapshot;
}) {
  const t = useAppText();
// 20261102_01(合并图表):指标 tab(Token/请求)切换图表内容,
  // 面积/柱状图标 tab 切换图型;右侧常驻模型调用排行榜。
  const [activeMetric, setActiveMetric] = useState<TrendMetric>("tokens");
  const [chartKind, setChartKind] = useState<TrendChartKind>("area");
  // 20261106:排行维度 tab(模型 / 客户端)—— 原「模型 Token/调用 排行」
  // 标题换成这个 tab,客户端用量从独立卡片搬进同一列。
  const [rankingDimension, setRankingDimension] = useState<RankingDimension>("models");

  const totals = stats.totals;

  // Trends use bucket-aligned rows with one column per model.
  const requestTrend = useMemo(() => buildTrendData(stats.modelSeries, "requests"), [stats.modelSeries]);
  const tokenTrend = useMemo(() => buildTrendData(stats.modelSeries, "tokens"), [stats.modelSeries]);
  const activeTrend = activeMetric === "tokens" ? tokenTrend : requestTrend;

  // 图表与排行榜共用同一模型配色(tokenTrend/requestTrend 均按 modelSeries 顺序
  // 取色),避免同模型在两处颜色不一致。
  const modelColors = useMemo(
    () => new Map(tokenTrend.models.map((model) => [model.name, model.color])),
    [tokenTrend.models]
  );
  // 排行榜随指标 tab 切换:Token → 按 totalTokens 降序,请求 → 按 requestCount
  // 降序(次级键为请求数);不设条数上限,容器内滚动。
  const modelLeaderboardRows = useMemo(() => {
    const metricKey = activeMetric === "tokens" ? "totalTokens" : "requestCount";
    return stats.models
      .filter((row) => row[metricKey] > 0)
      .sort((a, b) => b[metricKey] - a[metricKey] || b.requestCount - a.requestCount);
  }, [stats.models, activeMetric]);
  const modelLeaderboardMax = modelLeaderboardRows.length > 0
    ? (activeMetric === "tokens" ? modelLeaderboardRows[0].totalTokens : modelLeaderboardRows[0].requestCount)
    : 0;

  // 20261104_01 阶段三 · B-1a:按调用方归并的用量行(后端 clientModels 是
  // (client, model, provider) 三元组维度行 → 前端按 client 合并)。
  // 20261106(B-1b):`client` 已是归一 id(产品名)→ 同一客户端的多个版本号
  // 在后端就合成了一条,这里不会再看到同客户端的多行。
  const clientUsageRows = useMemo(() => aggregateClientUsage(stats.clientModels), [stats.clientModels]);
  // 排行随指标 tab 切换(Token → 按 totalTokens,请求 → 按 requestCount),
  // 与模型排行榜同一口径。
  const clientLeaderboardRows = useMemo(() => {
    const metricKey = activeMetric === "tokens" ? "totalTokens" : "requestCount";
    return clientUsageRows
      .filter((row) => row[metricKey] > 0)
      .sort((a, b) => b[metricKey] - a[metricKey] || b.requestCount - a.requestCount);
  }, [activeMetric, clientUsageRows]);
  const clientLeaderboardMax = clientLeaderboardRows.length > 0
    ? (activeMetric === "tokens" ? clientLeaderboardRows[0].totalTokens : clientLeaderboardRows[0].requestCount)
    : 0;

  const apiKeyOptions = [
    { label: t("All routing keys"), value: "" },
    ...apiKeys.map((apiKey) => ({ label: apiKey.name?.trim() || apiKey.id, value: apiKey.id }))
  ];

  const modelFilterOptions = [
    { label: t("All models"), value: "" },
    ...modelOptions.map((name) => ({ label: name, value: name }))
  ];

  // 20261104_01 阶段三 · B-1a:调用方下拉(选项是**纯 client 串**(归一 id),
  // 不是 "{client} / {模型}" 的拼接 label)。
  const clientFilterOptions = [
    { label: t("All clients"), value: "" },
    ...clientOptions.map((name) => ({ label: name, value: name }))
  ];

  const metricTabs: Array<{ id: TrendMetric; label: string }> = [
    { id: "tokens", label: t("Tokens") },
    { id: "requests", label: t("Requests") }
  ];
  const chartKindOptions: Array<{ icon: LucideIcon; id: TrendChartKind; label: string }> = [
    { icon: ChartArea, id: "area", label: t("Area chart") },
    { icon: ChartColumn, id: "bar", label: t("Bar chart") }
  ];
  // 20261106:排行维度 tab(模型 / 客户端),样式与左侧 Token/请求 tab 一致。
  // 客户端 tab 挂一句口径说明(归一规则),模型 tab 无需说明。
  const rankingDimensionTabs: Array<{ id: RankingDimension; label: string; title?: string }> = [
    { id: "models", label: t("Models") },
    {
      id: "clients",
      label: t("Clients"),
      title: t("Clients are identified by the User-Agent product name, so different versions of the same client are merged into one entry.")
    }
  ];

  return (
    <div className="usage-stats-view relative space-y-6 pb-5">
      <div
        aria-hidden="true"
        className="pointer-events-none absolute inset-x-0 top-0 -z-10 h-72"
        style={{ background: "radial-gradient(60% 46% at 50% 0%, color-mix(in oklab, var(--primary) 11%, transparent), transparent 72%)" }}
      />
      <div className="overview-toolbar flex flex-wrap items-center justify-between gap-3">
        <div className="flex min-w-0 flex-wrap items-center gap-2">
          <UsageStatsRangeSelector range={range} setRange={setRange} />
          <Select
            aria-label={t("Routing key")}
            className="h-9 w-[168px] rounded-[10px] bg-[length:14px] px-3 pr-8 text-body shadow-none"
            onValueChange={setCredential}
            options={apiKeyOptions}
            value={credential}
          />
          <Select
            aria-label={t("Model")}
            className="h-9 w-[210px] rounded-[10px] bg-[length:14px] px-3 pr-8 text-body shadow-none"
            onValueChange={setModel}
            options={modelFilterOptions}
            value={model}
          />
          {/* 20261104_01 阶段三 · B-1a:调用方筛选(原始 UA 串)。 */}
          <Select
            aria-label={t("Client")}
            className="h-9 w-[210px] rounded-[10px] bg-[length:14px] px-3 pr-8 text-body shadow-none"
            onValueChange={setClient}
            options={clientFilterOptions}
            value={client}
          />
        </div>
        <div className="flex shrink-0 items-center gap-2">
          <RefreshButton
            ariaLabel={t("Refresh usage stats")}
            loading={loading}
            onRefresh={() => refreshUsageStats(true)}
            title={t("Refresh")}
          />
          <GatewayStatusPill className="shrink-0" state={gatewayStatus.state} />
        </div>
      </div>

      {/* 2026-09-11:成本节省收缩为左半宽;右半宽新增「健康度」区(逐模型健康条,
          仅显示 range 内有调用的模型,按健康度降序)。 */}
      <div className="grid min-w-0 gap-4 lg:grid-cols-2">
        <DataOverviewSection range={range} savings={savings} stats={stats} />
        <UsageStatsHealthSection range={range} stats={stats} />
      </div>

{/* 活跃度热力图:独立卡片——边框 + 卡片底色。 */}
      <section
        aria-label={t("Activity")}
        className="rounded-xl border border-border bg-card shadow-card flex min-h-0 min-w-0 flex-col gap-3 px-4 py-3"
        data-module="activity"
      >
        <UsageStatsCardHeader
          badge={<Badge variant="outline">{t("Last 30 days")}</Badge>}
          controls={null}
          icon={CalendarDays}
          title={t("Activity")}
          tone="green"
        />
        <UsageStatsActivityCalendar modelSeries={activityModelSeries} series={activitySeries} />
      </section>

{/* 用量趋势 + 排行列(替代原「Token 消耗分布」「模型调用
          分布」两张卡):指标 tab(Token/请求)切换图表内容,面积/柱状图标 tab 切换
          图型;右侧排行列由卡头的「模型/客户端」维度 tab 切换数据源。 */}
      <section
        aria-label={t("Usage trend")}
        className="rounded-xl border border-border bg-card shadow-card flex min-h-0 min-w-0 flex-col gap-3 px-4 py-3"
        data-module="usage-trend"
      >
        <UsageStatsCardHeader
          badge={(
// 20261102:两组切换 tab 移到卡头左侧(标题之后)。
            <div className="flex flex-wrap items-center gap-2">
              <div className="overview-segmented flex" role="group">
                {metricTabs.map((tab) => (
                  <Button
                    aria-pressed={activeMetric === tab.id}
                    className={cn(
                      "overview-segmented-item h-7 px-2.5 text-caption font-medium text-muted-foreground hover:text-foreground",
                      activeMetric === tab.id && "text-foreground"
                    )}
                    data-active={activeMetric === tab.id}
                    key={tab.id}
                    onClick={() => setActiveMetric(tab.id)}
                    type="button"
                    unstyled
                  >
                    {tab.label}
                  </Button>
                ))}
              </div>
              <div className="overview-segmented flex" role="group">
                {chartKindOptions.map((option) => (
                  <Button
                    aria-label={option.label}
                    aria-pressed={chartKind === option.id}
                    className={cn(
                      "overview-segmented-item flex h-7 w-7 items-center justify-center text-muted-foreground hover:text-foreground",
                      chartKind === option.id && "text-foreground"
                    )}
                    data-active={chartKind === option.id}
                    key={option.id}
                    onClick={() => setChartKind(option.id)}
                    title={option.label}
                    type="button"
                    unstyled
                  >
                    <option.icon className="h-3.5 w-3.5" />
                  </Button>
                ))}
              </div>
            </div>
          )}
          controls={(
            // 20261106:原「模型 Token/调用 排行」标题换成**排行维度 tab**
            // (模型 / 客户端)。控件区保持与排行榜列同宽(280,内部 pl-4),
            // tab 左缘即排行行文字左缘 → 视觉上「这个 tab 管这一列」。
            <div className="lg:w-[280px] lg:shrink-0 lg:pl-4">
              <div className="overview-segmented flex" role="group">
                {rankingDimensionTabs.map((tab) => (
                  <Button
                    aria-pressed={rankingDimension === tab.id}
                    className={cn(
                      "overview-segmented-item h-7 px-2.5 text-caption font-medium text-muted-foreground hover:text-foreground",
                      rankingDimension === tab.id && "text-foreground"
                    )}
                    data-active={rankingDimension === tab.id}
                    data-module={`ranking-dimension-${tab.id}`}
                    key={tab.id}
                    onClick={() => setRankingDimension(tab.id)}
                    title={tab.title}
                    type="button"
                    unstyled
                  >
                    {tab.label}
                  </Button>
                ))}
              </div>
            </div>
          )}
          icon={ChartNoAxesCombined}
          title={t("Usage trend")}
          tone="purple"
        />
        {totals.totalTokens <= 0 && totals.requestCount <= 0 ? (
          <ChartEmptyState className="py-16" label={t("No usage data available.")} />
        ) : (
          <div className="flex min-h-0 flex-col gap-4 lg:flex-row">
            <div className="relative h-[340px] min-h-0 w-full lg:flex-1">
              <Suspense fallback={<ChartTabFallback />}>
                <UsageStatsTrendTab
                  kind={chartKind}
                  metric={activeMetric}
                  models={activeTrend.models}
                  rows={activeTrend.rows}
                />
              </Suspense>
            </div>
            <UsageStatsRanking
              clientRows={clientLeaderboardRows}
              colors={modelColors}
              dimension={rankingDimension}
              maxValue={rankingDimension === "models" ? modelLeaderboardMax : clientLeaderboardMax}
              metric={activeMetric}
              modelRows={modelLeaderboardRows}
            />
          </div>
        )}
      </section>
    </div>
  );
}
function buildTrendData(modelSeries: UsageStatsSnapshot["modelSeries"], metric: TrendMetric): TrendData {
  const models: TrendModel[] = [];
  const buckets = new Map<string, TrendRow>();
  modelSeries.forEach((entry, index) => {
    const key = `m${index}`;
    models.push({ color: MODEL_PALETTE[index % MODEL_PALETTE.length], key, name: entry.model });
    for (const point of entry.points) {
      let row = buckets.get(point.bucket);
      if (!row) {
        row = { bucket: point.bucket, label: point.label };
        buckets.set(point.bucket, row);
      }
      const value = metric === "tokens" ? point.totalTokens : point.requestCount;
      row[key] = (Number(row[key]) || 0) + value;
    }
  });
  const rows = Array.from(buckets.values()).sort((a, b) => a.bucket.localeCompare(b.bucket));
  // Zero-fill every model column so each series is continuous; without this
  // the area chart would connect across gaps instead of dropping to zero.
  for (const row of rows) {
    for (const model of models) {
      if (row[model.key] === undefined) {
        row[model.key] = 0;
      }
    }
  }
  return { models, rows };
}

// Header for the two analysis cards: the badge sits flush against the title
// (left group) while the chart/tab controls take the top-right corner —
// matches the requested layout where the "总计 / 总 Token 数" tag follows the

export * from "./types";
