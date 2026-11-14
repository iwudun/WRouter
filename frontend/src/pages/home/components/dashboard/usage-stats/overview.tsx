import type { ReactNode } from "react";
import { Badge } from "@/components/ui/badge";
import { CardTitle } from "@/components/ui/card";
import { LayoutDashboard } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import type { SavingsStatsSnapshot } from "@/contracts/app";
import {
  formatCompactNumber, formatDuration, formatPercent,
  UsageStatsRange, UsageStatsSnapshot, useAppText
} from "../../../shared/index";
import { USAGE_PANEL_HEIGHT_PX } from "../usage-stats-health";

export function UsageStatsCardHeader({
  badge,
  controls,
  icon: Icon,
  title,
  tone
}: {
  badge: ReactNode;
  controls?: ReactNode;
  icon: LucideIcon;
  title: string;
  tone: "blue" | "green" | "orange" | "purple" | "red" | "slate";
}) {
  return (
    <div className="flex min-h-9 shrink-0 flex-row flex-wrap items-center justify-between gap-x-3 gap-y-2">
      <div className="flex min-w-0 items-center gap-2.5">
        <span aria-hidden="true" className="overview-heading-icon" data-tone={tone}>
          <Icon className="h-3.5 w-3.5" />
        </span>
        <CardTitle>{title}</CardTitle>
        {badge}
      </div>
      {controls ? (
        <div className="flex min-w-0 shrink-0 flex-wrap items-center justify-end gap-3">
          {controls}
        </div>
      ) : null}
    </div>
  );
}

// Tab 面板保活已随「分布/排行」tab 下线删除(20261102_01 合并图表);图表
// 由父组件直接渲染单个 UsageStatsTrendTab。

// 总览指标块:标签 / 大数值 / 说明行(可选)/ 附加行(可选)。
// 2×2 网格由父容器给出,分隔线由 globals.css 的 :nth-child 规则绘制。
export function OverviewStatBlock({
  extra,
  label,
  sub,
  title,
  value
}: {
  extra?: ReactNode;
  label: string;
  sub?: ReactNode;
  title?: string;
  value: string;
}) {
  return (
    <div className="usage-stats-metric" title={title}>
      <div className="min-w-0 truncate text-caption font-medium text-muted-foreground">{label}</div>
      <div className="mt-2 min-w-0 truncate text-display-lg font-semibold tracking-[-0.02em] text-foreground">{value}</div>
      {sub ? <div className="mt-1 min-w-0 truncate text-caption text-muted-foreground">{sub}</div> : null}
      {extra}
    </div>
  );
}

// ---------------------------------------------------------------------------

export function DataOverviewSection({
  range,
  savings,
  stats
}: {
  range: UsageStatsRange;
  savings?: SavingsStatsSnapshot;
  stats: UsageStatsSnapshot;
}) {
  const t = useAppText();
  const totals = stats.totals;
  const compression = savings?.compression;
  const responseCache = savings?.responseCache;

  const savingsParts: string[] = [];
  if ((compression?.tokensSaved ?? 0) > 0) {
    savingsParts.push(`${t("Compression savings")} ${formatCompactNumber(compression!.tokensSaved)}`);
  }
  if ((responseCache?.tokensSaved ?? 0) > 0) {
    savingsParts.push(`${t("Response cache savings")} ${formatCompactNumber(responseCache!.tokensSaved)}`);
  }
  const speedValue = totals.avgDecodeTps > 0 ? `${Math.round(totals.avgDecodeTps)} tok/s` : "—";
  const cachePercent = Math.min(100, Math.max(0, totals.cacheRatio * 100));

  return (
    <section
      aria-label={t("Data overview")}
      className="rounded-xl border border-border bg-card shadow-card flex min-h-0 min-w-0 flex-col gap-3 px-4 py-3"
      data-module="overview"
      style={{ height: USAGE_PANEL_HEIGHT_PX }}
    >
      <UsageStatsCardHeader
        badge={<Badge variant="outline">{t("Range")}: {range}</Badge>}
        controls={totals.p95FirstByteMs != null && totals.p95FirstByteMs > 0 ? (
          // 阶段四 C-8:首字延迟的分位数(均值会被长尾污染,排障看 P95)。
          <span className="whitespace-nowrap text-caption text-muted-foreground">
            {t("First token latency")} P50 {formatDuration(totals.p50FirstByteMs ?? 0)} · P95 {formatDuration(totals.p95FirstByteMs)}
          </span>
        ) : null}
        icon={LayoutDashboard}
        title={t("Data overview")}
        tone="blue"
      />
      <div className="grid min-h-0 flex-1 grid-cols-2 grid-rows-2">
        <OverviewStatBlock
          extra={savingsParts.length > 0 ? (
            <div className="mt-1 min-w-0 truncate text-caption text-muted-foreground/70">{savingsParts.join(" · ")}</div>
          ) : null}
          label={t("Tokens")}
          sub={`${t("Input")} ${formatCompactNumber(totals.inputTokens)} · ${t("Output")} ${formatCompactNumber(totals.outputTokens)} · ${t("Cache")} ${formatCompactNumber(totals.cacheTokens)}`}
          value={formatCompactNumber(totals.totalTokens)}
        />
        <OverviewStatBlock
          label={t("Requests")}
          sub={(
            <>
              <span className={totals.errorCount > 0 ? "text-destructive" : undefined}>
                {formatCompactNumber(totals.errorCount)} {t("Failed")}
              </span>
              {" · "}
              {t("Success rate")} {formatPercent(totals.successRate)}
            </>
          )}
          value={formatCompactNumber(totals.requestCount)}
        />
        <OverviewStatBlock
          extra={(
            <div className="mt-2 h-1 min-w-0 overflow-hidden rounded-full bg-muted">
              <span
                className="block h-full rounded-full"
                style={{ backgroundColor: "var(--tone-green)", width: `${cachePercent}%` }}
              />
            </div>
          )}
          label={t("Cache hit rate")}
          title={t("Cached tokens divided by input tokens in the selected range")}
          value={formatPercent(totals.cacheRatio)}
        />
        <OverviewStatBlock
          label={t("Decode speed")}
          sub={totals.avgDecodeTps > 0 ? (
            // 阶段四 C-8:吞吐的 P50 / P95(分桶上界;均值仍在上方大字)。
            (totals.p50DecodeTps ?? 0) > 0 || (totals.p95DecodeTps ?? 0) > 0 ? (
              <span className="whitespace-nowrap">
                P50 {Math.round(totals.p50DecodeTps ?? 0)} · P95 {Math.round(totals.p95DecodeTps ?? 0)} tok/s
              </span>
            ) : null
          ) : t("No timed stream responses")}
          title={t("可见 Token ÷ 解码窗口实测时长；仅成功流式行，隐藏思考行剔除思考 Token 并用答案流；批量投递与短样本作废。统计只聚合新口径样本（旧行解码窗口口径不参与）。")}
          value={speedValue}
        />
      </div>
    </section>
  );
}
