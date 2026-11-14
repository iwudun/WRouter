// 调用日志 (call logs) - the audit view over every logged request. Unlike
// the network-logs view (packet-level debugging), this page shows one row per
// request with model / credential attribution, token breakdown, and an
// expandable summary; rejected requests (auth failures, policy denials,
// routing failures) are included on purpose - they are the audit trail.
// 2026-09-26 状态/列重构:五档状态(等待中/处理中为网关
// 在途注册表的实时行)、输入/输出合并、推理 token、缓存命中率、
// 首字/总耗时、请求方式列、模型列思考强度徽章。
import { memo, useCallback, useEffect, useMemo, useState } from "react";
import { motion } from "motion/react";
import {
  ArrowUpRight, Brain, Check, ChevronDown, ChevronLeft, ChevronRight, Copy, FileText, Gauge, Info, ScrollText, Search
} from "lucide-react";
import { LucideIcon } from "lucide-react";
import type {
  CallLogEntry,
  CallLogEntryStatus,
  CallLogListFilter,
  CallLogPage,
  CallLogRetryAttempt,
} from "@/contracts/app";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { Select } from "@/components/ui/select";
import { cn } from "@/lib/utils";
import {
  callLogPageSizeOptions, callLogStatusChips, copyTextToClipboard,
  credentialSelectOptions, formatCompactNumber, formatDuration, formatExactNumber, formatLogDateTime, formatUsdCost,
  logSelectOptions, RefreshButton, translateOptions, useAppNumberLocale, useAppText
} from "../shared/index";

// 调用日志列按内容设置最小宽度；小视口下由列表容器横向滚动承载全部字段。
// (请求方式 / 密钥 / 状态 / 输入输出推理 / 缓存读写 / 命中率 / 耗时)。
// 表头/单元格宽度按「字数 × ~13px + 16 padding」反推 min,避免截断。
// 调用日志列按内容设置最小宽度；小视口下由列表容器横向滚动承载全部字段。
// (请求方式+思考 / 密钥 / 状态 / 输入输出思考 / 缓存读写 / 命中率 / 耗时)。
// 列 min 由 DOM cell.scrollWidth 实测反推,避免内容截断。
const CALL_LOG_GRID = "grid-cols-[24px_minmax(166px,1fr)_minmax(92px,0.6fr)_minmax(126px,0.5fr)_minmax(164px,1.1fr)_minmax(84px,0.55fr)_minmax(80px,0.5fr)_minmax(176px,0.9fr)_minmax(116px,0.7fr)_minmax(84px,0.5fr)_minmax(122px,0.7fr)_minmax(112px,0.65fr)]";

// 展开区「请求尝试」表的列模板(行/头共用,改列必须只改这里)。
// 2026-10-17:错误信息移到倒数第二;短值列(序号/类型/结果/耗时/等待)
// 定宽收紧,请求模型与错误信息按内容弹性,错误信息最宽;「下次重试等待」
// 6 字表头需 ≥100px 才不折行。
const ATTEMPT_GRID_COLS =
  "grid-cols-[56px_88px_minmax(96px,0.7fr)_92px_92px_104px_minmax(220px,2.4fr)_132px]";
const CALL_LOG_MIN_WIDTH = 1347;
const SEARCH_DEBOUNCE_MS = 300;

// 思考强度徽章的已知归一枚举词(直接作为 i18n key:low→低 等);
// 未知取值按原文显示,不经过 i18n。
const REASONING_EFFORT_LEVELS = new Set(["minimal", "low", "medium", "high", "xhigh", "none"]);

/** 紧凑耗时:<1s 用毫秒(820ms),<1min 用秒(3.2s / 23s),≥1min 用
 *  "分+秒"(1m12s)而非小数分钟 —— 小数分钟不直观且精度损失大。 */
function formatCompactDuration(value: number | undefined): string {
  if (value === undefined) {
    return "-";
  }
  if (value < 1000) {
    return `${Math.max(0, Math.round(value))}ms`;
  }
  if (value < 60_000) {
    return `${(value / 1000).toFixed(value < 10_000 ? 1 : 0)}s`;
  }
  const minutes = Math.floor(value / 60_000);
  const seconds = Math.round((value % 60_000) / 1000);
  // 秒四舍五入进位(如 1m59.7s):归到下一分钟的 0s。
  return seconds === 60 ? `${minutes + 1}m0s` : `${minutes}m${seconds}s`;
}

/** 在途行本地秒级计时:在途期间查询不会重取(事件只在 登记/进处理中/首字/
 *  答案完成/完成五个瞬间发生),总耗时由前端按 createdAt 插值跳动;观察到
 *  答案完成(answerEndMs)即冻结,行离开在途态后直接用落库值。 */
function useInFlightTicker(inFlight: boolean): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!inFlight) {
      return;
    }
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [inFlight]);
  return now;
}

/** 缓存命中率(缓存读 / 输入侧总量)。输入侧口径随协议:
 *  OpenAI/Gemini 的 prompt 口径**含**缓存读,直接除;Anthropic 的
 *  input_tokens 不含缓存(且带 cache_write),分母补齐三段。`expanded` 标记
 *  Anthropic 行:单元格直接展示「缓存读 ÷ 三段和 = 命中率」的计算式,
 *  tooltip 再展开三段各自的数值。 */
function cacheHitRate(item: CallLogEntry): {
  percent: number;
  numerator: number;
  denominator: number;
  expanded: boolean;
} | undefined {
  if (item.cacheReadTokens <= 0) {
    return undefined;
  }
  const expanded = item.protocol.startsWith("anthropic");
  const denominator = expanded
    ? item.inputTokens + item.cacheReadTokens + item.cacheWriteTokens
    : item.inputTokens;
  if (denominator <= 0) {
    return undefined;
  }
  return {
    // 保留两位小数(98.32%);展示层统一 toFixed(2) 补齐位数。
    percent: Math.min(100, Math.round((item.cacheReadTokens / denominator) * 10000) / 100),
    numerator: item.cacheReadTokens,
    denominator,
    expanded
  };
}

export function CallLogsView({
  error,
  filter,
  loading,
  onOpenDetail,
  page,
  refreshLogs,
  updateFilter
}: {
  error: string;
  filter: CallLogListFilter;
  loading: boolean;
  onOpenDetail: (item: CallLogEntry) => void;
  page: CallLogPage;
  refreshLogs: () => void | Promise<void>;
  updateFilter: (patch: CallLogListFilter, resetPage?: boolean) => void;
}) {
  const t = useAppText();
  const [expandedId, setExpandedId] = useState<number>();
  const [searchText, setSearchText] = useState(filter.query ?? "");
  const firstItem = page.total === 0 ? 0 : (page.page - 1) * page.pageSize + 1;
  const lastItem = Math.min(page.total, page.page * page.pageSize);
  const windowTruncated = page.totalRows > page.windowSize && page.windowSize > 0;

  // Debounced search commit - typing should not fire an RPC per keystroke.
  useEffect(() => {
    const timer = setTimeout(() => {
      if ((filter.query ?? "") !== searchText) {
        updateFilter({ query: searchText });
      }
    }, SEARCH_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [searchText]);

  useEffect(() => {
    if (!expandedId || page.items.some((item) => item.id === expandedId)) {
      return;
    }
    setExpandedId(undefined);
  }, [expandedId, page.items]);

  const toggleExpanded = useCallback((id: number) => {
    setExpandedId((current) => (current === id ? undefined : id));
  }, []);

  return (
    <motion.div
      animate={{ opacity: 1 }}
      className="network-view flex min-w-0 flex-col"
      initial={{ opacity: 0 }}
      transition={{ duration: 0.15 }}
    >
{/* 工具行裸排:筛选/刷新留在页面上;日志表与分页回到卡片壳里。 */}
      <div className="flex min-h-0 flex-1 flex-col gap-2">
        <div className="flex min-h-10 min-w-0 shrink-0 flex-wrap items-center gap-2 px-1 py-0.5">
          <div className="relative min-w-[200px] flex-1">
            <Search className="network-search-icon pointer-events-none absolute left-2.5 top-1/2 z-[1] h-3.5 w-3.5 -translate-y-1/2" />
            <input
              aria-label={t("Search call logs")}
              className="network-filter-input h-7 w-full rounded-md border pl-8 pr-2 text-body font-semibold outline-none"
              onChange={(event) => setSearchText(event.target.value)}
              placeholder={t("搜索请求 ID、模型、密钥")}
              value={searchText}
            />
          </div>
          <div aria-label={t("Filter call log status")} className="overview-segmented flex shrink-0" role="group">
            {callLogStatusChips.map((chip) => {
              const active = (filter.status ?? "all") === chip.value;
              return (
                <Button
                  aria-pressed={active}
                  className={cn(
                    "overview-segmented-item h-7 gap-1.5 px-2.5 text-caption font-medium text-muted-foreground hover:text-foreground",
                    active && "text-foreground"
                  )}
                  data-active={active}
                  key={chip.value}
                  onClick={() => updateFilter({ status: chip.value })}
                  type="button"
                  unstyled
                >
                  {t(chip.label)}
                  <span className="network-count rounded-full px-1.5 py-px text-micro font-bold">
                    {formatCompactNumber(page.statusCounts[chip.value] ?? 0)}
                  </span>
                </Button>
              );
            })}
          </div>
          <Select
            aria-label={t("Filter call log model")}
            className="h-7 w-[168px] bg-[length:14px] px-2 pr-7 text-caption"
            onValueChange={(value) => updateFilter({ model: value || undefined })}
            options={logSelectOptions(t("全部模型"), page.options.models, filter.model)}
            value={filter.model ?? ""}
          />
          <Select
            aria-label={t("Filter call log credential")}
            className="h-7 w-[150px] bg-[length:14px] px-2 pr-7 text-caption"
            onValueChange={(value) => updateFilter({ credential: value || undefined })}
            options={credentialSelectOptions(t("全部密钥"), page.options.credentials, filter.credential)}
            value={filter.credential ?? ""}
          />
          <RefreshButton ariaLabel={t("Refresh call logs")} loading={loading} onRefresh={refreshLogs} title={t("Refresh")} />
        </div>

        {error ? (
          <div className="network-error-box mx-1 rounded-md border px-3 py-2 text-body">{error}</div>
        ) : null}

        {/* 20261104_01:恢复卡片壳 —— 此前为「透出背景」把日志表摊在页面上
            (无边框、无卡片底色),现在表头浮层与行分隔线都在卡片里。 */}
        <Card className="flex min-h-0 min-w-0 flex-1 flex-col" data-module="call-logs-card">
          <CardContent className="flex min-h-0 flex-1 flex-col overflow-hidden p-0">
            <div className="flex min-h-0 flex-1 flex-col">
              <div className="min-h-0 flex-1 overflow-auto">
                <div className="w-full min-w-0" style={{ minWidth: CALL_LOG_MIN_WIDTH }}>
                  <div className={cn("sticky top-0 z-10 grid h-9 items-center border-b border-border/60 bg-card/95 backdrop-blur-md", CALL_LOG_GRID)}>
                    <CallLogHeaderCell label="" />
                    <CallLogHeaderCell label={t("时间")} />
                    <CallLogHeaderCell label={t("请求 ID")} />
                    <CallLogHeaderCell label={t("请求方式")} />
                    <CallLogHeaderCell label={t("模型")} />
                    <CallLogHeaderCell label={t("密钥")} />
                    <CallLogHeaderCell label={t("状态")} />
                    <CallLogHeaderCell label={t("输入/输出(思考)")} />
                    <CallLogHeaderCell label={t("缓存读/写")} />
                    <CallLogHeaderCell label={t("缓存命中率")} />
                    <CallLogHeaderCell label={t("首字/总耗时")} />
                    <CallLogHeaderCell label={t("解码/输出速度")} unit="tok/s" />
                  </div>

                  {page.items.length === 0 ? (
                    <div className="flex h-[240px] flex-col items-center justify-center gap-2 text-center text-body text-muted-foreground">
                      <ScrollText className="h-7 w-7 text-muted-foreground/40" />
                      <div>{loading ? t("正在加载调用日志") : t("暂无调用日志")}</div>
                    </div>
                  ) : null}

                  <div className="divide-y divide-border/60">
                    {page.items.map((item) => (
                      <CallLogRow
                        expanded={expandedId === item.id}
                        item={item}
                        key={item.id}
                        onOpenDetail={onOpenDetail}
                        onToggle={toggleExpanded}
                      />
                    ))}
                  </div>
                </div>
              </div>

              <div className="flex min-h-10 shrink-0 flex-wrap items-center gap-2 border-t border-border/60 px-1 py-1">
                <span className="network-count min-w-[132px] rounded-full px-2 py-0.5 text-center text-caption font-semibold">
                  {firstItem}-{lastItem} / {page.total}
                </span>
                {windowTruncated ? (
                  <span className="network-muted text-micro">
                    {t("仅显示最近 {n} 条记录").replace("{n}", String(page.windowSize))}
                  </span>
                ) : null}
                <div className="flex-1" />
                <span className="network-muted text-caption">{t("每页")}</span>
                <Select
                  aria-label={t("Call log page size")}
                  className="h-7 w-[80px] bg-[length:14px] px-2 pr-7 text-caption"
                  onValueChange={(value) => updateFilter({ pageSize: Number(value) })}
                  options={translateOptions(callLogPageSizeOptions, t)}
                  value={String(page.pageSize)}
                />
                <button
                  aria-label={t("Previous page")}
                  className="network-control-button flex h-7 w-7 items-center justify-center rounded-md border outline-none disabled:cursor-not-allowed disabled:opacity-40 focus-visible:ring-2 focus-visible:ring-ring/30"
                  disabled={page.page <= 1}
                  onClick={() => updateFilter({ page: page.page - 1 }, false)}
                  title={t("上一页")}
                  type="button"
                >
                  <ChevronLeft className="h-3.5 w-3.5" />
                </button>
                <span className="network-count min-w-[52px] rounded-full px-2 py-0.5 text-center text-caption font-semibold">
                  {page.page} / {page.totalPages}
                </span>
                <button
                  aria-label={t("Next page")}
                  className="network-control-button flex h-7 w-7 items-center justify-center rounded-md border outline-none disabled:cursor-not-allowed disabled:opacity-40 focus-visible:ring-2 focus-visible:ring-ring/30"
                  disabled={page.page >= page.totalPages}
                  onClick={() => updateFilter({ page: page.page + 1 }, false)}
                  title={t("下一页")}
                  type="button"
                >
                  <ChevronRight className="h-3.5 w-3.5" />
                </button>
              </div>
            </div>
          </CardContent>
        </Card>
      </div>
    </motion.div>
  );
}

function CallLogHeaderCell({ label, unit }: { label: string; unit?: string }) {
  if (!label) {
    return <div />;
  }
  return (
    <div className="truncate px-2 text-center text-body-lg font-semibold uppercase tracking-wide text-muted-foreground">
      {label}
      {/* 单位按原样大小写渲染(不走 uppercase):大写单位太占列宽。 */}
      {unit ? <span className="normal-case"> ({unit})</span> : null}
    </div>
  );
}

const CallLogRow = memo(function CallLogRow({
  expanded,
  item,
  onOpenDetail,
  onToggle
}: {
  expanded: boolean;
  item: CallLogEntry;
  onOpenDetail: (item: CallLogEntry) => void;
  onToggle: (id: number) => void;
}) {
  const t = useAppText();
  const numberLocale = useAppNumberLocale();
  const createdAt = useMemo(() => formatLogDateTime(item.createdAt), [item.createdAt]);
  const hitRate = cacheHitRate(item);
  // 单元格只显示百分比(两位小数);计算过程(Anthropic 为三段式)放 tooltip,
  // 公式用精确数,保证与两位小数的结果可核对。
  const hitRateCell = hitRate ? `${hitRate.percent.toFixed(2)}%` : "-";
  const hitRateTitle = hitRate
    ? hitRate.expanded
      ? `${t("缓存命中率")} = ${formatExactNumber(hitRate.numerator, numberLocale)} ÷ (${formatExactNumber(item.inputTokens, numberLocale)} + ${formatExactNumber(hitRate.numerator, numberLocale)} + ${formatExactNumber(item.cacheWriteTokens, numberLocale)}) = ${hitRate.percent.toFixed(2)}%`
      : `${t("缓存命中率")} = ${formatExactNumber(hitRate.numerator, numberLocale)} ÷ ${formatExactNumber(hitRate.denominator, numberLocale)} = ${hitRate.percent.toFixed(2)}%`
    : undefined;
  const inFlight = item.status === "pending" || item.status === "processing";
  // 在途行总耗时的本地插值:查询快照冻结在取数瞬间,前端按登记时刻续走;
  // 20261108_01 §4.1:观察到答案完成(answerEndMs)即冻结,不再等落库。
  const tickerNow = useInFlightTicker(inFlight);
  const answerEndAt = item.answerEndMs != null ? Date.parse(item.createdAt) + item.answerEndMs : undefined;
  const liveDurationMs = inFlight
    ? Math.max(0, (answerEndAt != null ? Math.min(tickerNow, answerEndAt) : tickerNow) - Date.parse(item.createdAt))
    : item.durationMs;
  // 20261108_01 §13:解码速度 / 输出速度由后端统一计算(列表与统计同口径)。
  const decodeTps = item.decodeTps ?? undefined;
  const outputTps = item.outputTps ?? undefined;

  return (
    <div>
      <button
        aria-expanded={expanded}
        className={cn(
          "network-log-row grid h-[42px] w-full items-center border-0 px-0 text-left outline-none transition-colors hover:bg-muted/35",
          CALL_LOG_GRID,
          expanded && "network-row-selected"
        )}
        onClick={() => onToggle(item.id)}
        type="button"
      >
        <div className="flex justify-center">
          <ChevronDown className={cn("h-3.5 w-3.5 text-muted-foreground transition-transform", expanded && "rotate-180")} />
        </div>
        <div className="truncate px-2 text-center font-mono text-data font-medium tabular-nums" title={createdAt}>
          {createdAt}
        </div>
        <div className="flex min-w-0 items-center justify-center gap-1 px-2">
          <span className="network-row-secondary truncate font-mono text-data font-medium" title={item.requestId}>
            {item.requestId}
          </span>
          <RequestIdCopyButton requestId={item.requestId} />
        </div>
        <div className="flex min-w-0 items-center justify-center gap-1.5 px-2">
          {item.isStream ? (
            <span
              className="network-state-pill-active-soft shrink-0 whitespace-nowrap rounded-full px-2 py-0.5 text-caption font-semibold"
              title={t("Streaming")}
            >
              {t("Streaming")}
            </span>
          ) : (
            <span
              className="network-service-muted shrink-0 whitespace-nowrap rounded-full px-2 py-0.5 text-caption font-semibold"
              title={t("Non-streaming")}
            >
              {t("Non-streaming")}
            </span>
          )}
          {/* 思考强度徽章(与流式/非流式同列;品牌琥珀色区分蓝/灰方式徽章)。 */}
          {item.reasoningEffort ? (
            <span
              className="flex shrink-0 items-center gap-0.5 whitespace-nowrap rounded-full bg-primary/10 px-1.5 py-0.5 text-caption font-bold text-primary"
              title={`${t("Reasoning effort")}: ${item.reasoningEffort}`}
            >
              <Brain aria-hidden className="h-3 w-3" />
              {REASONING_EFFORT_LEVELS.has(item.reasoningEffort)
                ? t(item.reasoningEffort)
                : item.reasoningEffort}
            </span>
          ) : null}
        </div>
        <div className="flex min-w-0 items-center justify-center px-2">
          <span className="truncate text-data font-semibold" title={item.modelLabel}>{item.modelLabel}</span>
        </div>
        <div className="network-row-secondary truncate px-2 text-center text-data font-medium" title={item.apiKeyName ?? undefined}>
          {item.apiKeyName || "-"}
        </div>
        <div className="flex min-w-0 items-center justify-center px-2">
          <CallLogStatusPill inFlight={inFlight} item={item} />
        </div>
        <div
          className="network-row-secondary truncate px-2 text-center font-mono text-data font-medium tabular-nums"
          title={`${t("输入")}: ${formatCompactNumber(item.inputTokens, numberLocale)} · ${t("输出")}: ${formatCompactNumber(item.outputTokens, numberLocale)}${item.reasoningTokens > 0 ? ` · ${t("Thinking")}: ${formatCompactNumber(item.reasoningTokens, numberLocale)}` : ""}`}
        >
          {formatCompactNumber(item.inputTokens, numberLocale)} / {formatCompactNumber(item.outputTokens, numberLocale)}
          {item.reasoningTokens > 0 ? ` (${formatCompactNumber(item.reasoningTokens, numberLocale)})` : ""}
        </div>
        <div
          className="network-row-secondary truncate px-2 text-center font-mono text-data font-medium tabular-nums"
          title={`${t("缓存读")}: ${formatCompactNumber(item.cacheReadTokens, numberLocale)} · ${t("缓存写")}: ${formatCompactNumber(item.cacheWriteTokens, numberLocale)}`}
        >
          {formatCompactNumber(item.cacheReadTokens, numberLocale)} / {item.cacheWriteTokens > 0 ? formatCompactNumber(item.cacheWriteTokens, numberLocale) : "-"}
        </div>
        <div
          className="network-row-secondary truncate px-2 text-center font-mono text-data font-medium tabular-nums"
          title={hitRateTitle}
        >
          {hitRateCell}
        </div>
        <div
          className="network-row-secondary truncate px-2 text-center font-mono text-data font-medium tabular-nums"
          title={inFlight ? t("正在执行") : undefined}
        >
          {item.firstByteMs != null ? `${formatCompactDuration(item.firstByteMs)} / ` : "- / "}
          {formatCompactDuration(liveDurationMs)}
        </div>
        <div
          className="network-row-secondary truncate px-2 text-center font-mono text-data font-medium tabular-nums"
          title={t("解码速度 = 可见 Token ÷ 解码窗口实测时长（隐藏思考行用答案流）；输出速度 = 全部输出 Token ÷ 请求总耗时。仅成功行。")}
        >
          {/* 2026-10-14:单位简写 tok/s 且固定在表头,数据格只放数值。 */}
          {decodeTps != null ? Math.round(decodeTps) : "-"}
          {" / "}
          {outputTps != null ? Math.round(outputTps) : "-"}
        </div>
      </button>
      {expanded ? <CallLogExpandedDetails item={item} onOpenDetail={onOpenDetail} /> : null}
    </div>
  );
});

/** 状态列:五档 pill(等待中=琥珀 / 处理中=蓝 / 已完成=绿 / 失败=红 /
 *  已取消=灰);title 保留 HTTP 码与结束原因供审计。状态 → 大写 i18n key
 * (小写 "pending" 已被其他视图占用)。 */
const STATUS_LABEL_KEYS: Record<CallLogEntryStatus, string> = {
  canceled: "Canceled",
  completed: "Completed",
  failed: "Failed",
  pending: "Pending",
  processing: "Processing"
};

function CallLogStatusPill({ inFlight, item }: { inFlight: boolean; item: CallLogEntry }) {
  const t = useAppText();
  // 兼容混版本(旧后端行无 status 字段):按 ok 推导。
  const status: CallLogEntryStatus = item.status ?? (item.ok ? "completed" : "failed");
  const inFlightStatus = inFlight || status === "pending" || status === "processing";
  const tone = inFlightStatus
    ? status === "pending"
      ? "network-state-pill-warn-soft"
      : "network-state-pill-active-soft"
    : status === "completed"
      ? "network-state-pill-completed-soft"
      : status === "failed"
        ? "network-state-pill-error-soft"
        : "network-service-muted";
  const title = item.statusCode
    ? `HTTP ${item.statusCode}${item.endReason ? ` · ${item.endReason}` : ""}`
    : (item.endReason ?? undefined);
  return (
    <span className={cn("shrink-0 whitespace-nowrap rounded-full px-2 py-0.5 text-data font-semibold", tone)} title={title}>
      {t(STATUS_LABEL_KEYS[status])}
    </span>
  );
}

function RequestIdCopyButton({ requestId }: { requestId: string }) {
  const t = useAppText();
  const [copied, setCopied] = useState(false);

  const copy = useCallback(() => {
    void copyTextToClipboard(requestId).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1300);
    });
  }, [requestId]);

  return (
    <Button
      aria-label={t("Copy request ID")}
      className="h-5 w-5 shrink-0 text-muted-foreground hover:text-foreground"
      onClick={(event) => {
        event.stopPropagation();
        copy();
      }}
      size="iconSm"
      title={t("Copy request ID")}
      type="button"
      variant="ghost"
    >
      {copied ? <Check className="h-3 w-3" /> : <Copy className="h-3 w-3" />}
    </Button>
  );
}

function CallLogExpandedDetails({ item, onOpenDetail }: { item: CallLogEntry; onOpenDetail: (item: CallLogEntry) => void }) {
  const t = useAppText();
  const numberLocale = useAppNumberLocale();
  const modelChain = item.requestedModel && item.requestedModel !== item.modelLabel
    ? `${item.requestedModel} -> ${item.modelLabel}`
    : item.modelLabel;
  const inFlight = item.status === "pending" || item.status === "processing";
  // 在途行本地秒级计时:尝试表常驻后,活动行的「尝试耗时」格用同一插值跳动。
  const tickerNow = useInFlightTicker(inFlight);
  const liveElapsedMs = inFlight ? Math.max(0, tickerNow - Date.parse(item.createdAt)) : 0;
  const decodeTps = item.decodeTps ?? undefined;
  const outputTps = item.outputTps ?? undefined;
  // 统一「请求尝试」表(2026-10-14):真实 attempt 明细优先;未触达上游的
  // 失败行(认证失败/模型未配置/precheck/策略拒绝/非流式断联等)没有
  // attempt 记录,合成一行承载错误信息 —— 错误展示只剩表格一种形态,
  // 原顶部的红色报错框随之移除。
  const synthesizedAttempt: CallLogRetryAttempt | undefined =
    item.retryAttempts.length === 0 && item.error
      ? {
          attempt: 1,
          delayMs: 0,
          durationMs: item.durationMs,
          error: item.error,
          final: true,
          provider: item.provider,
          statusCode: item.statusCode || undefined,
        }
      : undefined;
  // 2026-10-17:尝试表常驻 —— 在途请求(retryAttempts 尚未落库)也渲染一行
  // 活动行(#1 处理中,耗时本地跳动),完成事件刷新后由真实明细替换。重试
  // 次数在落库瞬间随查询一次性补全(在途期间网关不逐次上报重试)。
  const inFlightRow: CallLogRetryAttempt | undefined =
    inFlight && item.retryAttempts.length === 0
      ? { attempt: 1, delayMs: 0, final: false, provider: item.provider, model: item.baseModel }
      : undefined;
  const attemptRows = synthesizedAttempt
    ? [synthesizedAttempt]
    : item.retryAttempts.length > 0
      ? item.retryAttempts
      : inFlightRow
        ? [inFlightRow]
        : [];
  // 压缩明细(方案 20260905_03 §4.6):估算节省 token + 每引擎徽标
  // (生效引擎显示字符量变化,跳过引擎显示 skip 原因)。
  const compression = item.compression;
  const compressionSavingsTokens = compression?.tokensSavedEstimate ?? 0;
  const compressionEngineBadges = compression && compression.engines.length > 0 ? (
    <div className="mt-3 flex flex-wrap gap-1.5">
      {compression.engines.map((engine) => (
        <Badge
          key={engine.id}
          title={engine.changed
            ? `${engine.charsBefore} → ${engine.charsAfter}`
            : engine.skipReason ?? undefined}
          variant={engine.changed ? "secondary" : "outline"}
        >
          {engine.changed
            ? `${engine.id}: ${formatCompactNumber(engine.charsBefore, numberLocale)}→${formatCompactNumber(engine.charsAfter, numberLocale)}`
            : `${engine.id}: ${engine.skipReason ?? "skipped"}`}
        </Badge>
      ))}
    </div>
  ) : null;

  return (
    <div className="network-detail border-b">
      <div className="grid grid-cols-1 gap-5 px-5 py-4 sm:grid-cols-2 2xl:grid-cols-4">
        <section className="min-w-0">
          <CallLogSectionTitle icon={Info} label={t("请求与路由")} />
          <dl className="mt-2 divide-y divide-border/50">
            <CallLogTokenRow label={t("请求 ID")} mono value={item.requestId} />
            <CallLogTokenRow label={t("请求端点")} mono value={item.endpointPath || item.path || "-"} />
            <CallLogTokenRow label={t("请求模型")} mono value={modelChain} />
            <CallLogTokenRow label={t("转换链路")} mono value={item.conversionChain || "-"} />
            <CallLogTokenRow label={t("供应商")} value={item.provider || t("未路由")} />
            <CallLogTokenRow label={t("密钥")} value={item.apiKeyName || "-"} />
            {item.proxy ? <CallLogTokenRow label={t("代理出口")} mono value={item.proxy} /> : null}
          </dl>
          {compressionSavingsTokens > 0 || compressionEngineBadges ? (
            <div className="mt-4">
              <CallLogSectionTitle icon={Info} label={t("压缩")} />
              <dl className="mt-2 divide-y divide-border/50">
                <CallLogTokenRow label={t("压缩节省")} title={t("估算节省的请求 token（压缩前 - 压缩后，按字符/4 折算）")} value={`${formatCompactNumber(compressionSavingsTokens, numberLocale)} ${t("tokens")} (${t("估算")})`} />
              </dl>
              {compressionEngineBadges}
            </div>
          ) : null}
        </section>
        <section className="min-w-0 border-t border-border/60 pt-4 sm:border-l sm:border-t-0 sm:pl-5 sm:pt-0">
          <CallLogSectionTitle icon={Gauge} label={t("耗时与速度")} />
          <dl className="mt-2 divide-y divide-border/50">
            <CallLogTokenRow label={t("请求耗时")} value={formatDuration(item.durationMs)} />
            <CallLogTokenRow label={t("首字时间")} value={item.firstByteMs != null ? formatDuration(item.firstByteMs) : "-"} />
            <CallLogTokenRow label={t("解码速度")} title={t("可见 Token ÷ 解码窗口实测时长；隐藏思考行剔除思考 Token 并用答案流时长。仅成功流式行；批量投递与短样本（<100ms 或 <1s 且长等待）作废。")} value={decodeTps != null ? `${Math.round(decodeTps)}tok/s` : "-"} />
            <CallLogTokenRow label={t("输出速度")} title={t("全部上报输出 Token ÷ 请求总耗时；含首字、思考等待、排队与代理。仅成功行。")} value={outputTps != null ? `${Math.round(outputTps)}tok/s` : "-"} />
          </dl>
        </section>
        <section className="min-w-0 border-t border-border/60 pt-4 2xl:border-l 2xl:border-t-0 2xl:pl-5 2xl:pt-0">
          <CallLogSectionTitle icon={Gauge} label={t("Token 用量")} />
          {/* 详情面板用精确数(千分位分组);条目行仍为约数,避免单元格放不下。 */}
          <dl className="mt-2 divide-y divide-border/50">
            <CallLogTokenRow label={t("输入")} value={formatExactNumber(item.inputTokens, numberLocale)} />
            <CallLogTokenRow label={t("缓存读")} value={formatExactNumber(item.cacheReadTokens, numberLocale)} />
            <CallLogTokenRow label={t("缓存写")} value={formatExactNumber(item.cacheWriteTokens, numberLocale)} />
            <CallLogTokenRow label={t("输出")} value={formatExactNumber(item.outputTokens, numberLocale)} />
            <CallLogTokenRow label={t("Thinking")} value={formatExactNumber(item.reasoningTokens, numberLocale)} />
            <CallLogTokenRow emphasized label={t("总计")} value={formatExactNumber(item.totalTokens, numberLocale)} />
          </dl>
        </section>
        <section className="min-w-0 border-t border-border/60 pt-4 2xl:border-l 2xl:border-t-0 2xl:pl-5 2xl:pt-0">
          <CallLogSectionTitle icon={Info} label={t("费用与结果")} />
          <dl className="mt-2 divide-y divide-border/50">
            <CallLogTokenRow label={t("成本")} value={item.costUsd != null ? formatUsdCost(item.costUsd) : "-"} />
            <CallLogTokenRow label={t("流式请求")} value={item.isStream ? t("Yes") : t("No")} />
            {item.reasoningEffort ? <CallLogTokenRow label={t("Reasoning effort")} value={item.reasoningEffort} /> : null}
            <CallLogTokenRow label={t("结束原因")} mono value={item.endReason || "-"} />
            {item.cacheSource === "hit" ? <CallLogTokenRow label={t("响应缓存")} value={`${t("命中")} · ${t("节省")} ${formatUsdCost(item.costSavedUsd ?? 0)}`} /> : null}
          </dl>
        </section>
      </div>
      {attemptRows.length > 0 ? (
        <div className="px-5 pb-4">
          <CallLogSectionTitle icon={Info} label={t("请求尝试")} />
          {/* 2026-10-08 丰富化:等待时长/失败状态/错误/耗时为网关真实记录;
              旧行(合成数据)只有 provider id,delayMs 恒 0。
              2026-10-13:标题「重试尝试」→「请求尝试」,每行区分原始/重试,
              并新增「请求与响应」列(捕获状态一目了然,查看详情按钮移入)。
              2026-10-14:错误信息独立成列(原挤在结果列);表格统一化 ——
              无真实 attempt 的失败行合成一行(类型「原始请求」),错误展示
              只剩表格一种形态。 */}
          <div className="mt-3 overflow-x-auto rounded-md border border-border/60">
            {/* 2026-10-14:错误信息独立成列(原挤在结果列下方);2026-10-17:
                移到倒数第二(请求与响应之前),列模板提取为 ATTEMPT_GRID_COLS
                行/头共用。 */}
            <div className={cn("grid min-w-[960px] border-b border-border/60 text-caption font-bold", ATTEMPT_GRID_COLS)}>
              <div className="border-r border-border/60 px-3 py-2">{t("Attempt")}</div>
              <div className="border-r border-border/60 px-3 py-2">{t("类型")}</div>
              <div className="border-r border-border/60 px-3 py-2">{t("请求模型")}</div>
              <div className="border-r border-border/60 px-3 py-2">{t("Result")}</div>
              <div className="border-r border-border/60 px-3 py-2">{t("Attempt duration")}</div>
              <div className="border-r border-border/60 px-3 py-2">{t("Next retry wait")}</div>
              <div className="border-r border-border/60 px-3 py-2">{t("错误信息")}</div>
              <div className="px-3 py-2">{t("请求与响应")}</div>
            </div>
            {attemptRows.map((attempt, index) => {
              const provider = attempt.provider ?? attempt.status ?? "-";
              // 合成行(无真实 attempt):捕获点恒灰,详情按钮恒可用
              // (详情页可能带顶层捕获的请求正文)。
              const synthesized = synthesizedAttempt != null;
              // 活动行(在途):结果/耗时/捕获格均为「处理中」实时形态,
              // 不渲染详情按钮(正文要完成后才有,入口是冗余的)。
              const live = !synthesized && inFlightRow != null && attempt === inFlightRow;
              const statusText =
                attempt.statusCode != null
                  ? attempt.statusCode > 0
                    ? `HTTP ${attempt.statusCode}`
                    : t("No response")
                  : null;
              const waitTitle =
                attempt.waitSource === "retry-after"
                  ? t("Honored upstream Retry-After")
                  : attempt.waitSource === "backoff"
                    ? t("Exponential backoff")
                    : undefined;
              // 原始请求 = 首次尝试(序号 1);其后皆为重试请求。
              const isOriginal = attempt.attempt <= 1;
              // 20261017_02 批5:列表行的采集正文已拆到侧表按需读取,新行
              // 带 `captured` 标记;旧行(含旧格式内嵌)回落键存在性判断。
              const captured =
                attempt.captured === true ||
                attempt.upstreamRequest != null ||
                (typeof attempt.upstreamResponse === "string" && attempt.upstreamResponse.length > 0);
              const captureReason = captured
                ? t("已捕获上游请求/响应")
                : attempt.upstreamRequest == null && !attempt.upstreamResponse
                  ? t("未捕获正文（正文捕获已关闭或该记录无正文，可在 设置 → 可观测性 中开启）")
                  : t("未捕获");
              return (
                <div
                  className={cn("grid min-w-[960px] text-caption font-semibold", ATTEMPT_GRID_COLS)}
                  key={`${index}-${attempt.attempt}-${attempt.final ? "final" : "retry"}`}
                >
                  <div className="border-r border-border/60 px-3 py-2 font-mono">#{attempt.attempt}</div>
                  <div className="border-r border-border/60 px-3 py-2">
                    <Badge variant={isOriginal ? "outline" : "secondary"}>
                      {isOriginal ? t("原始请求") : t("重试请求")}
                    </Badge>
                  </div>
                  <div
                    className="border-r border-border/60 px-3 py-2 font-mono truncate"
                    title={attempt.model ? `${provider} · ${attempt.model}` : provider}
                  >
                    {attempt.model ?? provider}
                  </div>
                  <div className="min-w-0 border-r border-border/60 px-3 py-2 font-mono">
                    {live ? (
                      <span className="network-state-pill-active-soft inline-flex shrink-0 items-center gap-1.5 whitespace-nowrap rounded-full px-2 py-0.5 font-semibold">
                        <span className="h-1.5 w-1.5 animate-pulse rounded-full bg-current" />
                        {t("Processing")}
                      </span>
                    ) : (
                      statusText ?? "-"
                    )}
                  </div>
                  <div className="border-r border-border/60 px-3 py-2 font-mono">
                    {live
                      ? formatCompactDuration(liveElapsedMs)
                      : attempt.durationMs != null
                        ? formatDuration(attempt.durationMs)
                        : "-"}
                  </div>
                  <div className="border-r border-border/60 px-3 py-2 font-mono" title={waitTitle}>
                    {attempt.final ? "-" : formatDuration(attempt.delayMs)}
                  </div>
                  {/* 错误信息独立列(倒数第二):truncate + title 承载完整内容。 */}
                  <div className="min-w-0 border-r border-border/60 px-3 py-2">
                    {attempt.error ? (
                      <div className="truncate" title={attempt.error}>
                        {attempt.error}
                      </div>
                    ) : (
                      "-"
                    )}
                  </div>
                  {/* 请求与响应:直接可见是否有记录,不必点进详情才发现。
                      活动行只有脉冲点(无按钮):正文要完成后才有。 */}
                  <div className="flex min-w-0 items-center gap-2 px-3 py-2">
                    {live ? (
                      <>
                        <span
                          className="h-2 w-2 shrink-0 animate-pulse rounded-full bg-muted-foreground/40"
                          title={t("请求进行中,完成后可查看正文与响应头。")}
                        />
                        <span className="truncate text-caption text-muted-foreground">{t("Processing")}</span>
                      </>
                    ) : synthesized ? (
                      <>
                        <span
                          className="h-2 w-2 shrink-0 rounded-full bg-muted-foreground/40"
                          title={t("无上游尝试明细")}
                        />
                        <button
                          className="min-w-0 truncate text-caption font-semibold text-primary hover:underline"
                          onClick={() => onOpenDetail(item)}
                          type="button"
                        >
                          {t("查看详情")}
                        </button>
                      </>
                    ) : (
                      <>
                        <span
                          className={cn(
                            "flex h-2 w-2 shrink-0 rounded-full",
                            captured ? "bg-emerald-500" : "bg-muted-foreground/40"
                          )}
                          title={captureReason}
                        />
                        <button
                          className="min-w-0 truncate text-caption font-semibold text-primary hover:underline disabled:cursor-default disabled:text-muted-foreground disabled:no-underline"
                          disabled={!captured}
                          onClick={() => onOpenDetail(item)}
                          title={captureReason}
                          type="button"
                        >
                          {captured ? t("查看详情") : t("未捕获")}
                        </button>
                      </>
                    )}
                  </div>
                </div>
              );
            })}
          </div>
        </div>
      ) : null}
      <div className="px-5 pb-5">
        {/* 统一尝试表后,失败行(含合成行)的详情入口都在表内;仅缓存命中
            等无错误且无明细的行仍走此行级入口。 */}
        {attemptRows.length === 0 ? (
          <Button
            className="gap-2"
            onClick={() => onOpenDetail(item)}
            type="button"
            variant="outline"
          >
            <FileText className="h-4 w-4" />
            {t("查看请求/响应详情")}
            <ArrowUpRight className="h-3.5 w-3.5" />
          </Button>
        ) : null}
      </div>
    </div>
  );
}

function CallLogSectionTitle({ className, icon: Icon, label }: { className?: string; icon: LucideIcon; label: string }) {
  return (
    <div className={cn("network-muted flex items-center gap-1.5 text-caption font-semibold uppercase tracking-wide", className)}>
      <Icon className="h-3.5 w-3.5" />
      {label}
    </div>
  );
}

function CallLogTokenRow({
  emphasized = false,
  label,
  mono = false,
  title,
  value
}: {
  emphasized?: boolean;
  label: string;
  mono?: boolean;
  title?: string;
  value: string;
}) {
  return (
    <div className="flex min-w-0 items-baseline justify-between gap-3 py-1.5" title={title}>
      <dt className="network-muted shrink-0 text-caption">{label}</dt>
      <dd className={cn(
        "min-w-0 break-all text-right tabular-nums",
        mono && "font-mono",
        emphasized ? "text-body-lg font-semibold" : "text-body font-medium"
      )}>
        {value}
      </dd>
    </div>
  );
}