import { useMemo, useState } from "react";
import { createPortal } from "react-dom";
import { HeartPulse } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { CardTitle } from "@/components/ui/card";
import type { UsageSeriesPoint, UsageStatsRange, UsageStatsSnapshot } from "@/contracts/app";
import {
  formatDuration,
  formatPercent,
  parseStatusBucketDate,
  usageStatusTone,
  useAppText,
} from "../../shared/index";
import { ChartEmptyState } from "./list-charts";

// 使用统计页「健康度」区(2026-09-11):可用性/健康条从供应商模型条目收拢到此。
// 仅展示「所选 range 内有过调用」的模型,按整体成功率(健康度)降序排名;每条 =
// 上「供应商/模型名」+ 下健康条。健康条宽度随 range 伸缩:每个时间桶一个
// flex-1 min-w-[3px] 段(同修改前 UsageStatsStatusStrip 的形态),桶色按
// usageStatusTone。单桶 hover 经 createPortal+fixed 弹出「统计时间/成功率/
// 首字延迟/解码速度」(页面滚动容器内行内 absolute 浮层会被裁剪)。
type HealthRow = {
  health: number;
  label: string;
  points: UsageSeriesPoint[];
  requests: number;
};

type HoverState = { index: number; rect: DOMRect } | null;

const HOUR_MS = 3_600_000;

// 数据总览 / 健康度 两区块的统一固定高度 = 健康度显示 4 条模型健康条的高度:
// py-3(24) + 头部 min-h-9(36) + gap-3(12) + 4×行高 h-11(44) + 3×行间距 gap-3(12) = 284。
// 20261102:5 行 → 4 行(健康度卡压缩,总览卡同步);超出 4 条仍可滚动。
export const USAGE_PANEL_HEIGHT_PX = 284;

// 桶区间文案:小时桶 → 「MM-DD HH:00~HH+1:00」;日桶 → 「MM-DD 00:00~24:00」。
function formatBucketWindow(bucket: string, range: UsageStatsRange): string {
  const start = parseStatusBucketDate(bucket);
  if (!start) {
    return bucket;
  }
  const pad = (value: number) => String(value).padStart(2, "0");
  const md = `${pad(start.getMonth() + 1)}-${pad(start.getDate())}`;
  const hourly = range === "today" || range === "24h";
  if (!hourly) {
    return `${md} 00:00~24:00`;
  }
  const end = new Date(start.getTime() + HOUR_MS);
  return `${md} ${pad(start.getHours())}:00~${pad(end.getHours())}:00`;
}

function decodeLabel(point: UsageSeriesPoint): string {
  return point.avgDecodeTps > 0 ? `${Math.round(point.avgDecodeTps)}tok/s` : "-";
}

export function UsageStatsHealthSection({
  range,
  stats
}: {
  range: UsageStatsRange;
  stats: UsageStatsSnapshot;
}) {
  const t = useAppText();
  // 仅保留 range 内有调用的模型,并按健康度(整体成功率)降序排名。
  const rows = useMemo<HealthRow[]>(() => {
    const out: HealthRow[] = [];
    for (const entry of stats.modelSeries) {
      let requests = 0;
      let errors = 0;
      for (const point of entry.points) {
        requests += point.requestCount;
        errors += point.errorCount;
      }
      if (requests <= 0) {
        continue;
      }
      out.push({
        health: (requests - errors) / requests,
        label: entry.model,
        points: entry.points,
        requests
      });
    }
    out.sort(
      (a, b) =>
        b.health - a.health ||
        b.requests - a.requests ||
        a.label.localeCompare(b.label)
    );
    return out;
  }, [stats.modelSeries]);

  return (
    <section
      aria-label={t("Health")}
      className="rounded-xl border border-border bg-card shadow-card flex min-h-0 min-w-0 flex-col gap-3 px-4 py-3"
      data-module="health"
      style={{ height: USAGE_PANEL_HEIGHT_PX }}
    >
      <div className="flex min-h-9 shrink-0 flex-row flex-wrap items-center justify-between gap-x-3 gap-y-2">
        <div className="flex min-w-0 items-center gap-2.5">
          <span aria-hidden="true" className="overview-heading-icon" data-tone="green">
            <HeartPulse className="h-3.5 w-3.5" />
          </span>
          <CardTitle>{t("Health")}</CardTitle>
          <Badge variant="outline">{rows.length} {t("Models")}</Badge>
        </div>
      </div>
      {rows.length === 0 ? (
        <ChartEmptyState className="py-10" label={t("No usage data available.")} />
      ) : (
        <div className="min-h-0 flex-1 space-y-3 overflow-y-auto pr-1">
          {rows.map((row) => (
            <HealthRowView key={row.label} range={range} row={row} />
          ))}
        </div>
      )}
    </section>
  );
}

function HealthRowView({ range, row }: { range: UsageStatsRange; row: HealthRow }) {
  const t = useAppText();
  const [hover, setHover] = useState<HoverState>(null);
  const hoveredPoint = hover ? row.points[hover.index] : undefined;

  return (
    <div className="h-11 space-y-1">
      <div className="flex min-w-0 items-center justify-between gap-2">
        <span className="min-w-0 truncate font-mono text-body font-medium" title={row.label}>
          {row.label}
        </span>
        <span className="shrink-0 text-body font-semibold">{formatPercent(row.health)}</span>
      </div>
      <div className="flex h-5 min-w-0 gap-[2px]">
        {row.points.map((point, index) => (
          <span
            className="min-w-[3px] flex-1"
            key={`${point.bucket}-${index}`}
            onMouseEnter={(event) => setHover({ index, rect: event.currentTarget.getBoundingClientRect() })}
            onMouseLeave={() => setHover(null)}
          >
            <span
              className="overview-status-segment block h-full w-full rounded-[4px]"
              data-tone={usageStatusTone(point)}
            />
          </span>
        ))}
      </div>
      {hover && hoveredPoint && createPortal(
        <div
          className="pointer-events-none fixed z-[100] w-[200px] max-w-[calc(100vw-16px)] -translate-x-1/2 -translate-y-full rounded-md border border-border/70 bg-popover px-3 py-2 text-left text-caption text-popover-foreground shadow-card-elevated"
          style={{
            left: Math.min(
              Math.max(hover.rect.left + hover.rect.width / 2, 8 + Math.min(100, (window.innerWidth - 16) / 2)),
              window.innerWidth - 8 - Math.min(100, (window.innerWidth - 16) / 2)
            ),
            top: hover.rect.top - 6
          }}
        >
          <span className="flex justify-between gap-3">
            <span className="text-muted-foreground">{t("Stat time")}</span>
            <span className="font-medium">{formatBucketWindow(hoveredPoint.bucket, range)}</span>
          </span>
          <span className="flex justify-between gap-3">
            <span className="text-muted-foreground">{t("Success rate")}</span>
            <span className="font-medium">{hoveredPoint.requestCount > 0 ? formatPercent(hoveredPoint.successRate) : "-"}</span>
          </span>
          <span className="flex justify-between gap-3">
            <span className="text-muted-foreground">{t("First token latency")}</span>
            <span className="font-medium">{hoveredPoint.avgFirstByteMs > 0 ? formatDuration(hoveredPoint.avgFirstByteMs) : "-"}</span>
          </span>
          <span className="flex justify-between gap-3">
            <span className="text-muted-foreground">{t("Decode speed")}</span>
            <span className="font-medium">{decodeLabel(hoveredPoint)}</span>
          </span>
        </div>,
        document.body
      )}
    </div>
  );
}
