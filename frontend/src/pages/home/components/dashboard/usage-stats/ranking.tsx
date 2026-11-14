import type { UsageStatsSnapshot } from "../../../shared/index";
import { formatCompactNumber, formatPercent, useAppText } from "../../../shared/index";
import { MODEL_PALETTE } from "../list-charts";
import type { ClientUsageRow, RankingDimension, TrendMetric } from "./types";

export function UsageStatsRanking({
  clientRows,
  colors,
  dimension,
  maxValue,
  metric,
  modelRows
}: {
  clientRows: ClientUsageRow[];
  colors: Map<string, string>;
  dimension: RankingDimension;
  maxValue: number;
  metric: TrendMetric;
  modelRows: UsageStatsSnapshot["models"];
}) {
  const t = useAppText();
  const tokenMode = metric === "tokens";
  const clientMode = dimension === "clients";

  // 两套数据源归一成同一行形态 → 渲染只有一份(避免两份实现漂移)。
  const rows = clientMode
    ? clientRows.map((row) => ({
        cacheRatio: row.cacheRatio,
        errorCount: row.errorCount,
        key: row.client,
        name: row.client,
        requestCount: row.requestCount,
        // tooltip 带该客户端用过的模型(窄列里放不下,挪到悬浮提示)。
        title: row.models.length > 0 ? `${row.client} · ${row.models.join(" · ")}` : row.client,
        totalTokens: row.totalTokens
      }))
    : modelRows.map((row) => ({
        cacheRatio: row.cacheRatio,
        errorCount: row.errorCount,
        key: row.key,
        name: row.label,
        requestCount: row.requestCount,
        title: row.label,
        totalTokens: row.totalTokens
      }));

  const ariaLabel = clientMode
    ? (tokenMode ? t("Client token ranking") : t("Client ranking"))
    : (tokenMode ? t("Model token ranking") : t("Model ranking"));

  return (
    <aside
      aria-label={ariaLabel}
      // 上下沿与图表绘图区对齐:图表盒 340 高,顶部 margin 8(图表 plot 顶)。
      // 20261102 终版:高度收为 294 = 恰好 5 行(实测行高 47.5×5 + gap 14×4),
      // 第 6 行不再露出半截;首行顶仍=plot 顶,底部较横轴数字略高属预期
      // (固定显示 5 条优先);更多模型在容器内滚动。
      className="flex min-w-0 shrink-0 flex-col border-t border-border/60 pt-3 lg:mt-2 lg:h-[294px] lg:w-[280px] lg:border-l lg:border-t-0 lg:pl-4 lg:pt-0"
      data-module={clientMode ? "client-ranking" : "model-ranking"}
    >
      {rows.length === 0 ? (
        <div className="flex flex-1 items-center justify-center text-caption text-muted-foreground">{t("No usage data available.")}</div>
      ) : (
        <div className="flex min-h-0 flex-1 flex-col gap-3.5 overflow-y-auto pr-1">
          {rows.map((row, index) => {
            const color = clientMode
              ? "var(--primary)"
              : (colors.get(row.name) ?? MODEL_PALETTE[index % MODEL_PALETTE.length]);
            const value = tokenMode ? row.totalTokens : row.requestCount;
            const other = tokenMode ? row.requestCount : row.totalTokens;
            const share = maxValue > 0 ? value / maxValue : 0;
            return (
              <div className="min-w-0" data-ranking-name={row.name} key={row.key}>
                <div className="flex min-w-0 items-center justify-between gap-2">
                  <span className="flex min-w-0 items-center gap-1.5">
                    <span aria-hidden="true" className="h-2 w-2 shrink-0 rounded-full" style={{ backgroundColor: color }} />
                    <span className="min-w-0 truncate text-body font-medium" title={row.title}>{row.name}</span>
                  </span>
                  <span className="shrink-0 text-body font-semibold tabular-nums" data-ranking-value>{formatCompactNumber(value)}</span>
                </div>
                <div className="mt-1.5 h-[3px] min-w-0 overflow-hidden rounded-full bg-muted">
                  <span className="block h-full rounded-full" style={{ backgroundColor: color, width: `${share * 100}%` }} />
                </div>
                <div className="mt-1 flex min-w-0 flex-wrap items-center gap-x-1.5 text-caption text-muted-foreground">
                  <span className="whitespace-nowrap">
                    {formatCompactNumber(other)} {tokenMode ? t("Requests") : t("Tokens")}
                  </span>
                  <span aria-hidden="true">·</span>
                  <span className="whitespace-nowrap">{t("Cache hit rate")} {formatPercent(row.cacheRatio)}</span>
                  {row.errorCount > 0 ? (
                    <>
                      <span aria-hidden="true">·</span>
                      <span className="whitespace-nowrap text-destructive">{formatCompactNumber(row.errorCount)} {t("Failed")}</span>
                    </>
                  ) : null}
                </div>
              </div>
            );
          })}
        </div>
      )}
    </aside>
  );
}

// 20261106:原「客户端用量」独立卡片已下线 —— 客户端用量改由「用量趋势」
// 卡右侧排行列承载(卡头「模型/客户端」tab 切维度),数据口径与原卡一致
// (归并后的 client 行 + 缓存命中率 + 失败数)。

// 活跃度日历：复用概览页同款 buildTokenActivity 构建器，每个调用方各自本地渲染）。固定读取最近 30 天
// 序列，铺到 53 周网格里，最近的数据落在右侧、其余为淡化上下文。hover 展示
