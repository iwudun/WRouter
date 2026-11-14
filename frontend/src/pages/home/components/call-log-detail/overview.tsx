import { Gauge } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import type { CallLogEntry } from "@/contracts/app";
import { cn } from "@/lib/utils";

import {
  formatDuration,
  formatExactNumber,
  formatUsdCost,
  useAppNumberLocale,
  useAppText,
} from "../../shared";

export function OverviewCard({ item }: { item: CallLogEntry }) {
  const t = useAppText();
  const numberLocale = useAppNumberLocale();
  // 20261108_01 §13:解码/输出双指标由后端统一计算,前端只展示。
  const decodeTps = item.decodeTps ?? undefined;
  const outputTps = item.outputTps ?? undefined;
  const hasReadCache = item.cacheReadTokens > 0;
  const hasWriteCache = item.cacheWriteTokens > 0;
  const cacheRate = hasReadCache && item.inputTokens > 0 ? ((item.cacheReadTokens / item.inputTokens) * 100).toFixed(1) : null;
  return (
    <Card>
      <CardHeader className="pb-2">
        <CardTitle className="flex items-center justify-between gap-2">
          <span className="flex shrink-0 items-center gap-2">
            <span className="flex h-7 w-7 items-center justify-center rounded-lg bg-primary/10"><Gauge className="h-3.5 w-3.5 text-primary" /></span>
            {t("请求概览")}
          </span>
          <span className="flex min-w-0 items-center gap-2">
            {item.conversionChain ? (
              <Badge className="min-w-0 font-mono" title={item.conversionChain} variant="secondary">
                <span className="truncate">{item.conversionChain}</span>
              </Badge>
            ) : null}
            <Badge variant={item.ok ? "success" : "danger"}>{t(item.status)}</Badge>
          </span>
        </CardTitle>
      </CardHeader>
      <CardContent className="flex flex-wrap items-stretch gap-x-5 gap-y-3">
        <OverviewStat label={t("请求模型")} maxW="max-w-[200px]" mono value={item.modelLabel} />
        <OverviewStat label={t("供应商")} maxW="max-w-[160px]" mono value={item.provider || t("未路由")} />
        <OverviewStat label={t("密钥")} maxW="max-w-[140px]" value={item.apiKeyName || "-"} />
        <OverviewStat label={t("请求端点")} maxW="max-w-[200px]" mono value={item.endpointPath || item.path} />
        <OverviewDivider />
        <OverviewStat label={t("请求耗时")} value={formatDuration(item.durationMs)} />
        {/* 阶段四 C-4:耗时分解(出站偏移 / 排队 / 重试)——「厂商慢」还是
         * 「我方慢」直接读数,不用猜。阶段四 C-7:内容跨度(实测)。 */}
        {item.sentMs != null ? (
          <OverviewStat
            label={t("出站偏移")}
            title={t("从请求开始到上游请求发出的时间；首字延迟减去它就是供应商侧的耗时。")}
            value={formatDuration(item.sentMs)}
          />
        ) : null}
        {item.queuedMs != null ? (
          <OverviewStat label={t("排队等待")} value={formatDuration(item.queuedMs)} />
        ) : null}
        {item.retriedMs != null ? (
          <OverviewStat label={t("重试累计")} value={formatDuration(item.retriedMs)} />
        ) : null}
        {item.flowMs != null ? (
          <OverviewStat
            label={t("内容跨度")}
            title={t("内容 10% 到 90% 的实测跨度；接近 0 说明上游把内容一次性倒了出来。")}
            value={formatDuration(item.flowMs)}
          />
        ) : null}
        {item.firstAnswerByteMs != null ? (
          <OverviewStat
            label={t("答案首字")}
            title={t("首个答案 token 的时刻；不含思考。")}
            value={formatDuration(item.firstAnswerByteMs)}
          />
        ) : null}
        {item.reasoningDecodeMs != null ? (
          <OverviewStat
            label={t("思考耗时")}
            title={t("思考内容的 10% 到 90% 跨度；仅诊断，不计入吞吐。")}
            value={formatDuration(item.reasoningDecodeMs)}
          />
        ) : null}
        {item.streamTailMs != null && item.streamTailMs > 0 ? (
          <OverviewStat
            label={t("收尾排空")}
            title={t("答案完成到协议终态帧之间的拖尾；不计入请求耗时。")}
            value={formatDuration(item.streamTailMs)}
          />
        ) : null}
        <OverviewStat
          label={t("解码速度")}
          title={t("可见 Token ÷ 解码窗口实测时长；隐藏思考行剔除思考 Token 并用答案流时长。仅成功流式行；批量投递与短样本（<100ms 或 <1s 且长等待）作废。")}
          value={decodeTps != null ? `${Math.round(decodeTps)}tok/s` : "-"}
        />
        <OverviewStat
          label={t("输出速度")}
          title={t("全部上报输出 Token ÷ 请求总耗时；含首字、思考等待、排队与代理。仅成功行。")}
          value={outputTps != null ? `${Math.round(outputTps)}tok/s` : "-"}
        />
        <OverviewDivider />
        <OverviewStat
          label={t("输入")}
          sub={hasReadCache ? `${t("缓存读")} ${formatExactNumber(item.cacheReadTokens, numberLocale)}` : undefined}
          value={formatExactNumber(item.inputTokens, numberLocale)}
        />
        <OverviewStat
          label={t("输出")}
          sub={item.reasoningTokens > 0 ? `${t("Thinking")} ${formatExactNumber(item.reasoningTokens, numberLocale)}` : undefined}
          value={formatExactNumber(item.outputTokens, numberLocale)}
        />
        <OverviewStat
          badge={hasReadCache && cacheRate ? `${cacheRate}%` : undefined}
          label={t("缓存读取")}
          sub={hasWriteCache ? `${t("缓存写")} ${formatExactNumber(item.cacheWriteTokens, numberLocale)}` : undefined}
          value={formatExactNumber(item.cacheReadTokens, numberLocale)}
        />
        <OverviewStat
          label={t("总计")}
          sub={item.costUsd != null ? formatUsdCost(item.costUsd) : undefined}
          value={formatExactNumber(item.totalTokens, numberLocale)}
        />
      </CardContent>
    </Card>
  );
}

/** 组间细分隔线(仅在宽屏单行布局时出现,窄域 wrap 后隐藏)。 */
function OverviewDivider() {
  return <span aria-hidden className="hidden w-px shrink-0 self-stretch bg-border/60 lg:block" />;
}

/** 迷你统计格:「标签(上,灰小字) / 值(下,半粗 tabular,子值内联随行)」
 *  两行封顶(2026-10-18 用户定版:子值独立成行会到 3 行);
 *  model/端点等长 mono 值用 maxW 截断(title 承载全量)。 */
function OverviewStat({ badge, label, maxW, mono = false, sub, title, value }: {
  badge?: string;
  label: string;
  maxW?: string;
  mono?: boolean;
  sub?: string;
  title?: string;
  value: string;
}) {
  return (
    <div className="flex min-w-0 flex-col justify-center gap-0.5" title={title}>
      <span className="text-micro font-medium uppercase tracking-wider text-muted-foreground">{label}</span>
      <span className="flex min-w-0 items-center gap-1.5">
        <span className={cn("min-w-0 truncate text-body font-semibold tabular-nums", maxW, mono && "font-mono")} title={title ?? value}>{value}</span>
        {badge ? (
          <span className="shrink-0 rounded border border-emerald-200 bg-emerald-50 px-1 text-micro font-semibold text-emerald-600 dark:border-emerald-500/25 dark:bg-emerald-500/10 dark:text-emerald-400">{badge}</span>
        ) : null}
        {sub ? <span className="min-w-0 truncate text-caption text-muted-foreground" title={sub}>({sub})</span> : null}
      </span>
    </div>
  );
}
