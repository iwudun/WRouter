// F1.5: recharts-dependent chart tab for the usage-stats view, split into its
// own lazy chunk. usage-stats.tsx references this component ONLY via
// React.lazy, so recharts (the heaviest vendor dependency) never enters the
// initial module graph — it is fetched after the view mounts, off the critical
// path. Chart-only primitives shared with the eager module (ChartEmptyState,
// MODEL_PALETTE) come from list-charts.tsx to avoid a static edge back to
// usage-stats.tsx.
//
// 20261102_01: 分布/排行 tab 合并进主卡片右侧排行榜后,本文件只剩趋势图。
import {
  Area, Bar, BarChart, CartesianGrid, ComposedChart, Tooltip, XAxis, YAxis
} from "recharts";
import {
  formatAxisNumber, formatCompactNumber, useAppText
} from "../../shared/index";
import { ChartFrame } from "./list-charts";
import type { TrendChartKind, TrendMetric, TrendModel, TrendRow } from "./usage-stats";

// Trend tooltip bounds its height without folding models away: entries lay
// out in up to 3 grid columns with at most this many rows per column, so a
// dozen-plus models stay fully visible inside the overflow-hidden card.
const TOOLTIP_MAX_ROWS_PER_COLUMN = 6;

export function UsageStatsTrendTab({
  kind,
  metric,
  models,
  rows
}: {
  kind: TrendChartKind;
  metric: TrendMetric;
  models: TrendModel[];
  rows: TrendRow[];
}) {
  const chartMargin = { bottom: 4, left: 0, right: 8, top: 8 };

  return (
    <div className="h-full min-h-0 min-w-0">
      <ChartFrame fill>
        {({ height, width }) => (
          kind === "bar" ? (
            <BarChart data={rows} height={height} margin={chartMargin} width={width}>
              <CartesianGrid stroke="var(--usage-stats-chart-grid)" strokeDasharray="2 5" vertical={false} />
              <XAxis axisLine={false} dataKey="label" minTickGap={28} tick={{ fill: "var(--muted-foreground)", fontSize: 10 }} tickLine={false} />
              <YAxis axisLine={false} tick={{ fill: "var(--muted-foreground)", fontSize: 11 }} tickFormatter={formatAxisNumber} tickLine={false} />
              <Tooltip content={<TrendTooltip metric={metric} />} cursor={{ fill: "rgba(120,120,128,0.08)" }} />
              {models.map((model, index) => (
                <Bar
                  dataKey={model.key}
                  fill={model.color}
                  isAnimationActive={false}
                  key={model.key}
                  name={model.name}
                  // 只有最顶层那段保留顶部圆角；其余段用直角，避免每段顶部圆角
                  // 在两端凹进 3px、被下一段直角底边盖不住而透出背景色，
                  // 看起来像独立砖块堆叠而非一根连贯柱子。
                  radius={index === models.length - 1 ? [3, 3, 0, 0] : [0, 0, 0, 0]}
                  stackId="usage"
                />
              ))}
            </BarChart>
          ) : (
            <ComposedChart data={rows} height={height} margin={chartMargin} width={width}>
              <CartesianGrid stroke="var(--usage-stats-chart-grid)" strokeDasharray="2 5" vertical={false} />
              <XAxis axisLine={false} dataKey="label" minTickGap={28} tick={{ fill: "var(--muted-foreground)", fontSize: 10 }} tickLine={false} />
              <YAxis axisLine={false} tick={{ fill: "var(--muted-foreground)", fontSize: 11 }} tickFormatter={formatAxisNumber} tickLine={false} />
              <Tooltip content={<TrendTooltip metric={metric} />} />
              {models.map((model) => (
                <Area
                  dataKey={model.key}
                  fill={model.color}
                  fillOpacity={0.16}
                  isAnimationActive={false}
                  key={model.key}
                  name={model.name}
                  stroke={model.color}
                  strokeWidth={2}
                  type="monotone"
                />
              ))}
            </ComposedChart>
          )
        )}
      </ChartFrame>
    </div>
  );
}

function TrendTooltip({
  active,
  label,
  metric,
  payload
}: {
  active?: boolean;
  label?: string;
  metric: TrendMetric;
  payload?: Array<{ color?: string; name?: string; value?: number | string }>;
}) {
  const t = useAppText();
  const valueLabel = metric === "tokens" ? t("Tokens") : t("Requests");
  if (!active || !payload?.length) {
    return null;
  }
  const total = payload.reduce((sum, item) => sum + (Number(item.value) || 0), 0);
  // 弹窗高度封顶但不折叠模型：卡片 overflow-hidden 会裁掉超出部分，单列
  // 展开十几个模型必然超高。改为按数值降序 + 多列网格（每列至多
  // TOOLTIP_MAX_ROWS_PER_COLUMN 行），全部模型都可见且总高度有界。
  const sorted = [...payload].sort((a, b) => (Number(b.value) || 0) - (Number(a.value) || 0));
  const entries = sorted.filter((item) => (Number(item.value) || 0) > 0);
  const columnCount = entries.length <= TOOLTIP_MAX_ROWS_PER_COLUMN
    ? 1
    : Math.min(3, Math.ceil(entries.length / TOOLTIP_MAX_ROWS_PER_COLUMN));
  return (
    <div className="overview-tooltip rounded-xl border px-3 py-2.5 text-caption">
      <div className="mb-1 font-semibold">{label}</div>
      <div
        className="grid gap-x-5 gap-y-1"
        style={{ gridTemplateColumns: `repeat(${columnCount}, minmax(0, 1fr))` }}
      >
        {entries.map((item) => (
          <div className="flex min-w-[150px] items-center justify-between gap-3" key={item.name}>
            <span className="flex min-w-0 items-center gap-1.5 text-muted-foreground">
              <span className="h-2 w-2 shrink-0 rounded-full" style={{ backgroundColor: item.color || "#e8874a" }} />
              <span className="truncate">{item.name}</span>
            </span>
            <span className="shrink-0 font-medium tabular-nums">{formatCompactNumber(Number(item.value) || 0)}</span>
          </div>
        ))}
      </div>
      {payload.length > 1 ? (
        <div className="mt-1 flex min-w-[150px] items-center justify-between gap-4 border-t border-border/60 pt-1">
          <span className="text-muted-foreground">{t("Total")} {valueLabel}</span>
          <span className="font-medium">{formatCompactNumber(total)}</span>
        </div>
      ) : null}
    </div>
  );
}
