import { useEffect, useMemo, useState } from "react";
import { createPortal } from "react-dom";

import { X } from "lucide-react";

import { Button } from "@/components/ui/button";

import { Dialog, DialogBody, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { cn } from "@/lib/utils";
import { activityDateKey, buildTokenActivity, type TokenActivityCell } from "@/lib/usage-activity";

import {
  formatCompactNumber, formatDuration, formatPercent, formatUsdCost,
  useAppNumberLocale, useAppText
} from "../../../shared/index";
import { ChartEmptyState, MODEL_PALETTE } from "../list-charts";
import type { UsageStatsSnapshot } from "../../../shared/index";

const ACTIVITY_WEEKS = 53;

export function UsageStatsActivityCalendar({ modelSeries, series }: { modelSeries: UsageStatsSnapshot["modelSeries"]; series: UsageStatsSnapshot["series"] }) {
  const t = useAppText();
  const [selectedCell, setSelectedCell] = useState<TokenActivityCell>();
  const activity = useMemo(
    () => buildTokenActivity(series, { maxWeeks: ACTIVITY_WEEKS, minWeeks: ACTIVITY_WEEKS }),
    [series]
  );

  // 当日模型明细：modelSeries 的点按本地日归组（dateKey 与格子一致）。
  const modelsByDay = useMemo(() => {
    const map = new Map<string, Array<{ label: string; requestCount: number; totalTokens: number }>>();
    for (const entry of modelSeries) {
      for (const point of entry.points) {
        const date = new Date(point.bucket);
        if (!Number.isFinite(date.getTime())) {
          continue;
        }
        const key = activityDateKey(date);
        const rows = map.get(key);
        const row = { label: entry.model, requestCount: point.requestCount, totalTokens: point.totalTokens };
        if (rows) {
          rows.push(row);
        } else {
          map.set(key, [row]);
        }
      }
    }
    return map;
  }, [modelSeries]);

  if (activity.totalTokens <= 0) {
    return <ChartEmptyState className="py-16" label={t("No usage data available.")} />;
  }

  return (
    <div className="flex flex-col gap-4">
      <UsageStatsActivityGrid activity={activity} onSelect={setSelectedCell} />

      <div className="flex shrink-0 items-center gap-1.5 text-caption font-medium text-muted-foreground">
        <span>{t("Less")}</span>
        {[0, 1, 2, 3, 4].map((intensity) => (
          <span
            aria-hidden="true"
            className="overview-activity-cell h-3 w-3 rounded-[3px]"
            key={intensity}
            style={{ backgroundColor: activityColor(intensity as TokenActivityCell["intensity"], true) }}
          />
        ))}
        <span>{t("More")}</span>
      </div>

      {selectedCell ? (
        <UsageStatsDayDialog cell={selectedCell} models={modelsByDay.get(selectedCell.dateKey) ?? []} onClose={() => setSelectedCell(undefined)} />
      ) : null}
    </div>
  );
}

function UsageStatsActivityGrid({ activity, onSelect }: { activity: ReturnType<typeof buildTokenActivity>; onSelect: (cell: TokenActivityCell) => void }) {
  const t = useAppText();
  const locale = useAppNumberLocale();
  // 始终展示星期 / 月份标签：使用统计页是全宽卡片，有足够空间。
  // 网格按周一对齐（dayIndex 0=周一 … 6=周日），七行全部标注星期。
  const dayLabels = [t("M"), t("Tu"), t("W"), t("Th"), t("F"), t("Sa"), t("Su")];
  const cellGap = 3;
  const labelColumnWidth = 20;

  // 共享 tooltip：361 格不再各自挂一个常驻 tooltip（≈2000+ 节点），改为
  // 单个浮层按 hover/focus 定位复用。浮层走 fixed + portal：使用统计页
  // 外层是 overflow-auto,绝对定位会被滚动容器裁掉(AGENTS)。
  const tooltipWidth = 190;
  const [hovered, setHovered] = useState<{ cell: TokenActivityCell; cellHeight: number; left: number; top: number }>();
  const showCellTooltip = (cell: TokenActivityCell, target: HTMLElement) => {
    const rect = target.getBoundingClientRect();
    const preferredLeft =
      cell.weekIndex <= 1
        ? rect.left
        : cell.weekIndex >= activity.weekCount - 2
          ? rect.right - tooltipWidth
          : rect.left + rect.width / 2 - tooltipWidth / 2;
    setHovered({
      cell,
      cellHeight: rect.height,
      left: Math.max(8, Math.min(preferredLeft, window.innerWidth - tooltipWidth - 8)),
      top: rect.top
    });
  };
  const hideCellTooltip = () => setHovered(undefined);

  useEffect(() => {
    if (!hovered) {
      return;
    }
    window.addEventListener("scroll", hideCellTooltip, true);
    window.addEventListener("resize", hideCellTooltip);
    return () => {
      window.removeEventListener("scroll", hideCellTooltip, true);
      window.removeEventListener("resize", hideCellTooltip);
    };
  }, [hovered]);

  return (
    <div className="min-w-0 overflow-visible">
      <div className="w-full">
        <div
          className="mb-1 grid text-caption font-medium text-muted-foreground"
          style={{
            columnGap: `${cellGap}px`,
            gridTemplateColumns: `repeat(${activity.weekCount}, minmax(0, 1fr))`,
            marginLeft: `${labelColumnWidth + cellGap}px`
          }}
        >
          {activity.months.map((month) => (
<span
                  className="truncate"
                  key={`${month.label}-${month.weekIndex}`}
                  // 用月份实际跨越的列数；与下个月标签零重叠避免换行
                  style={{ gridColumn: `${month.weekIndex + 1} / span ${Math.max(1, month.lastWeekIndex - month.weekIndex + 1)}` }}
                >
                  {month.label}
                </span>
          ))}
        </div>
        <div
          className="grid min-h-[64px]"
          role="img"
          aria-label={`${t("Activity")} ${t("Tokens")}`}
          style={{
            gap: `${cellGap}px`,
            gridTemplateColumns: `${labelColumnWidth}px repeat(${activity.weekCount}, minmax(0, 1fr))`,
            gridTemplateRows: "repeat(7, auto)"
          }}
        >
          {dayLabels.map((label, index) => (
            <span
              className="self-center truncate text-caption font-medium leading-none text-muted-foreground"
              key={`${label}-${index}`}
              style={{ gridColumn: 1, gridRow: index + 1 }}
            >
              {label}
            </span>
          ))}
          {activity.cells.map((cell) => (
            <span
              aria-label={`${cell.dateKey}: ${formatActivityCount(cell.totalTokens)} ${t("Tokens")}, ${formatActivityCount(cell.point?.requestCount ?? 0)} ${t("Requests")}`}
              className={cn(
                "overview-activity-cell group relative aspect-square w-full rounded-[4px]",
                cell.point && "cursor-pointer hover:ring-1 hover:ring-ring/50"
              )}
              key={cell.dateKey}
              onBlur={cell.point ? hideCellTooltip : undefined}
              onClick={cell.point ? () => onSelect(cell) : undefined}
              onFocus={cell.point ? (event) => showCellTooltip(cell, event.currentTarget) : undefined}
              onMouseEnter={cell.point ? (event) => showCellTooltip(cell, event.currentTarget) : undefined}
              onMouseLeave={cell.point ? hideCellTooltip : undefined}
              role={cell.point ? "button" : undefined}
              style={{
                backgroundColor: activityColor(cell.intensity, cell.inObservedRange),
                gridColumn: cell.weekIndex + 2,
                gridRow: cell.dayIndex + 1
              }}
              tabIndex={cell.point ? 0 : undefined}
              title={cell.point ? t("Click to view day summary") : undefined}
            />
          ))}
        </div>
      </div>
      {hovered
        ? createPortal(
            <span
              className="pointer-events-none fixed z-30 w-[190px] max-w-[calc(100vw-32px)] rounded-md border border-border/70 bg-popover px-2 py-1.5 text-left text-caption text-popover-foreground shadow-card-elevated"
              style={{
                left: hovered.left,
                top: hovered.cell.dayIndex <= 1 ? hovered.top + hovered.cellHeight + 6 : hovered.top - 6,
                transform: hovered.cell.dayIndex <= 1 ? undefined : "translateY(-100%)"
              }}
            >
              <span className="block font-semibold">{formatActivityFullDate(hovered.cell.date, locale)} {formatActivityWeekday(hovered.cell.date, locale)}</span>
              <span className="mt-1 flex justify-between gap-3">
                <span className="text-muted-foreground">{t("Tokens")}</span>
                <span className="font-medium">{formatActivityCount(hovered.cell.totalTokens)}</span>
              </span>
              <span className="flex justify-between gap-3">
                <span className="text-muted-foreground">{t("Requests")}</span>
                <span className="font-medium">{formatActivityCount(hovered.cell.point?.requestCount ?? 0)}</span>
              </span>
            </span>,
            document.body
          )
        : null}
    </div>
  );
}

function formatActivityFullDate(date: Date, locale: Intl.LocalesArgument = undefined): string {
  return new Intl.DateTimeFormat(locale, { day: "numeric", month: "long", year: "numeric" }).format(date);
}

function formatActivityWeekday(date: Date, locale: Intl.LocalesArgument = undefined): string {
  return new Intl.DateTimeFormat(locale, { weekday: "long" }).format(date);
}

function UsageStatsDayDialog({
  cell,
  models,
  onClose
}: {
  cell: TokenActivityCell;
  models: Array<{ label: string; requestCount: number; totalTokens: number }>;
  onClose: () => void;
}) {
  const t = useAppText();
  const locale = useAppNumberLocale();
  const point = cell.point;
  if (!point) {
    return null;
  }
  const successful = Math.max(0, point.requestCount - point.errorCount);
  const topModels = [...models]
    .sort((a, b) => b.totalTokens - a.totalTokens || b.requestCount - a.requestCount)
    .slice(0, 8);

  const metrics: Array<{ label: string; value: string }> = [
    { label: t("Requests"), value: formatActivityCount(point.requestCount) },
    { label: t("Successful requests"), value: formatActivityCount(successful) },
    { label: t("Failed requests"), value: formatActivityCount(point.errorCount) },
    { label: t("Success rate"), value: formatPercent(point.successRate) },
    { label: t("Total tokens"), value: formatActivityCount(point.totalTokens) },
    { label: t("Input tokens"), value: formatActivityCount(point.inputTokens) },
    { label: t("Output tokens"), value: formatActivityCount(point.outputTokens) },
    { label: t("Cached tokens"), value: formatActivityCount(point.cacheTokens) },
    { label: t("Cache hit rate"), value: formatPercent(point.cacheRatio) },
    { label: t("Cost"), value: formatUsdCost(point.costUsd ?? 0) },
    { label: t("Duration"), value: formatDuration(point.avgDurationMs ?? 0) },
  ];

  return (
    <Dialog onOpenChange={(open) => !open && onClose()} open>
      <DialogContent className="max-w-[560px]">
        <DialogHeader>
          <DialogTitle>{formatActivityFullDate(cell.date, locale)} {formatActivityWeekday(cell.date, locale)}</DialogTitle>
          <DialogDescription>{t("Day summary")}</DialogDescription>
          <Button aria-label={t("Close")} className="h-8 w-8 rounded-md p-0 text-muted-foreground hover:text-foreground" onClick={onClose} type="button" unstyled variant="ghost">
            <X className="h-4 w-4" />
          </Button>
        </DialogHeader>
        <DialogBody className="space-y-4">
          <div className="grid grid-cols-2 gap-x-4 gap-y-1.5 sm:grid-cols-3">
            {metrics.map((metric) => (
              <div className="flex min-w-0 items-center justify-between gap-2 rounded-md border border-border/60 bg-background/50 px-2.5 py-1.5" key={metric.label}>
                <span className="truncate text-caption text-muted-foreground">{metric.label}</span>
                <span className="shrink-0 text-caption font-semibold tabular-nums">{metric.value}</span>
              </div>
            ))}
          </div>
          {topModels.length > 0 ? (
            <div>
              <div className="mb-1.5 text-caption font-medium text-muted-foreground">{t("Top models")}</div>
              <div className="space-y-1">
                {topModels.map((row, index) => (
                  <div className="flex min-w-0 items-center gap-2" key={row.label}>
                    <span aria-hidden="true" className="h-2 w-2 shrink-0 rounded-full" style={{ backgroundColor: MODEL_PALETTE[index % MODEL_PALETTE.length] }} />
                    <span className="min-w-0 flex-1 truncate text-body" title={row.label}>{row.label}</span>
                    <span className="shrink-0 text-caption text-muted-foreground tabular-nums">{formatActivityCount(row.requestCount)} {t("Requests")}</span>
                    <span className="shrink-0 w-16 text-right text-caption font-medium tabular-nums">{formatCompactNumber(row.totalTokens)}</span>
                  </div>
                ))}
              </div>
              <div className="mt-1.5 h-1 overflow-hidden rounded-full bg-muted">
                {topModels.map((row, index) => (
                  <span
                    aria-hidden="true"
                    className="inline-block h-full"
                    key={row.label}
                    style={{ backgroundColor: MODEL_PALETTE[index % MODEL_PALETTE.length], width: `${(row.totalTokens / Math.max(1, point.totalTokens)) * 100}%` }}
                  />
                ))}
              </div>
            </div>
          ) : null}
        </DialogBody>
      </DialogContent>
    </Dialog>
  );
}

// 与概览页同色阶：observed 区间外用极淡灰蓝做上下文底，区间内按强度递增。
function activityColor(intensity: TokenActivityCell["intensity"], inRange: boolean): string {
  if (!inRange) return "rgba(0,122,255,.05)";
  if (intensity === 0) return "rgba(0,122,255,.12)";
  if (intensity === 1) return "rgba(0,122,255,.30)";
  if (intensity === 2) return "rgba(0,122,255,.50)";
  if (intensity === 3) return "rgba(0,122,255,.72)";
  return "rgba(0,122,255,.94)";
}

function formatActivityCount(value: number): string {
  // 与数据总览统一：大数字走 compact（万/亿），小数字保持整数。
  return formatCompactNumber(Math.max(0, value));
}
