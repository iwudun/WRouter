import { useMemo, useState } from "react";
import {
  ChevronDown, ChevronsDownUp, ChevronsUpDown, CircleAlert, Layers, Terminal,
} from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import type { CallLogEntry, CallLogRetryAttempt, RequestLogDetail, RequestLogTraceItem } from "@/contracts/app";
import { cn } from "@/lib/utils";

import { formatBytes, formatDuration, useAppText } from "../../shared";
import { generateCurl } from "../shared/curl";
import { jsonTreeContainerPaths, JsonTreePane } from "../shared/json-tree-pane";
import { CopyButton, DownloadButton, TabToolbar } from "./actions";
import { renderCaptured } from "./shared";

export function AttemptsTab({ detail, item, onShowCurl, onOpenChunks, traceError, traceHasMore, traceItems, traceLoading, traceLoadingMore, onLoadMoreTrace }: {
  detail: RequestLogDetail | undefined;
  item: CallLogEntry;
  onShowCurl: (curl: string) => void;
  onOpenChunks: () => void;
  traceError: string;
  traceHasMore: boolean;
  traceItems: RequestLogTraceItem[];
  traceLoading: boolean;
  traceLoadingMore: boolean;
  onLoadMoreTrace: () => void;
}) {
  const t = useAppText();
  // 20261017_02 批5:列表条目的 retryAttempts 已瘦身(采集正文拆侧表按需
  // 读取);详情 RPC 返回的 retryAttempts 由后端并回侧表数据后投影,
  // 有则优先,详情页尝试表正文完整。
  const listedAttempts = detail?.retryAttempts?.length ? detail.retryAttempts : item.retryAttempts;
  // 与列表页一致:未触达上游的失败行没有 attempt,合成一行承载错误。
  const synthesized: CallLogRetryAttempt | undefined = listedAttempts.length === 0 && item.error
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
  const attempts = synthesized ? [synthesized] : listedAttempts;
  return (
    <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto">
      <TabToolbar>
        <span className="text-body font-semibold">{t("请求尝试")}</span>
        <Button disabled={!traceItems.length} onClick={onOpenChunks} size="sm" variant="outline">
          <Layers className="h-3.5 w-3.5" />
          {t("流事件")}
          {traceItems.length ? <Badge variant="secondary">{traceItems.length}</Badge> : null}
        </Button>
      </TabToolbar>
      {attempts.length === 0 ? (
        <div className="flex min-h-[120px] items-center justify-center text-body text-muted-foreground">{t("No retry attempts")}</div>
      ) : (
        <div className="space-y-4">
          {attempts.map((attempt, index) => (
            <AttemptExecutionCard attempt={attempt} index={index} key={attempt.attemptId ?? `${attempt.attempt}-${index}`} onShowCurl={onShowCurl} />
          ))}
        </div>
      )}
      <TraceItems
        error={traceError}
        hasMore={traceHasMore}
        items={traceItems}
        loading={traceLoading}
        loadingMore={traceLoadingMore}
        onLoadMore={onLoadMoreTrace}
      />
    </div>
  );
}

/** 执行式尝试卡(2026-10-18 重排):cURL 重放移入标题行右侧;上游请求/
 *  响应收为 chevron 折叠卡,正文统一 JSON 树展示;请求/响应卡左右分栏 ——
 *  左窄列请求(响应)头、右宽列正文。收展图标分别落四个子卡(CapturedSection)
 *  标题行最右。上游请求头自 155dc68 起随尝试采集(脱敏),旧数据无此键时
 *  请求卡回落整宽。 */
function AttemptExecutionCard({ attempt, index, onShowCurl }: { attempt: CallLogRetryAttempt; index: number; onShowCurl: (curl: string) => void }) {
  const t = useAppText();
  const [showRequest, setShowRequest] = useState(false);
  const [showResponse, setShowResponse] = useState(false);
  const hasRequest = attempt.upstreamRequest != null;
  const hasRequestHeaders = attempt.upstreamRequestHeaders != null
    && Object.keys(attempt.upstreamRequestHeaders).length > 0;
  const hasResponse = typeof attempt.upstreamResponse === "string" && attempt.upstreamResponse.length > 0;
  const hasHeaders = attempt.upstreamHeaders != null && Object.keys(attempt.upstreamHeaders).length > 0;
  const conversion = attempt.conversion ?? undefined;
  const requestText = hasRequest ? renderCaptured(attempt.upstreamRequest) : "";
  const requestHeadersText = hasRequestHeaders ? JSON.stringify(attempt.upstreamRequestHeaders, null, 2) : "";
  const headersText = hasHeaders ? JSON.stringify(attempt.upstreamHeaders, null, 2) : "";
  const responseText = attempt.upstreamResponse ?? "";
  const isOriginal = attempt.attempt <= 1;
  // upstreamHeaders 是上游响应头(非请求头),不能拼进 curl;重放命令只带
  // 捕获的请求体,端点按 body 形状推断,认证留 $WR_API_KEY 占位。
  const upstreamCurl = hasRequest ? generateCurl({ body: attempt.upstreamRequest }) : "";
  // 每棵 JSON 树的展开状态收在本卡,由各子卡(请求头/请求正文/响应头/
  // 响应正文)标题行右侧的收展按钮分别驱动;默认仅根展开,切换尝试由 key
  // 重挂载自然复位。
  const [requestExpanded, setRequestExpanded] = useState<Set<string>>(() => new Set([""]));
  const [requestHeadExpanded, setRequestHeadExpanded] = useState<Set<string>>(() => new Set([""]));
  const [responseExpanded, setResponseExpanded] = useState<Set<string>>(() => new Set([""]));
  const [responseHeadExpanded, setResponseHeadExpanded] = useState<Set<string>>(() => new Set([""]));
  return (
    <div className="overflow-hidden rounded-xl border border-border/60 bg-muted/20">
      <div className="flex flex-wrap items-center gap-2 border-b border-border/60 px-4 py-3">
        <span className="flex h-6 w-6 items-center justify-center rounded-full bg-primary/10 text-caption font-bold text-primary">{index + 1}</span>
        <span className="text-body font-semibold">{t("尝试")} #{attempt.attempt}</span>
        {attempt.attemptId ? <span className="font-mono text-caption text-muted-foreground" title={t("尝试 ID")}>{attempt.attemptId}</span> : null}
        <Badge variant={isOriginal ? "outline" : "secondary"}>{isOriginal ? t("原始请求") : t("重试请求")}</Badge>
        {attempt.statusCode != null ? (
          <Badge variant={attempt.statusCode > 0 ? (attempt.statusCode < 400 ? "success" : "danger") : "warning"}>
            {attempt.statusCode > 0 ? `HTTP ${attempt.statusCode}` : t("No response")}
          </Badge>
        ) : null}
        {attempt.final ? <Badge variant="success">{t("最终尝试")}</Badge> : null}
        {conversion?.strategy ? <Badge variant="outline" className="font-mono">{conversion.strategy}</Badge> : null}
        {conversion?.diagnostics?.length ? <Badge variant="warning">{t("诊断")} {conversion.diagnostics.length}</Badge> : null}
        <div className="ml-auto flex items-center gap-2">
          <Button disabled={!upstreamCurl} onClick={() => onShowCurl(upstreamCurl)} size="sm" variant="outline">
            <Terminal className="h-3.5 w-3.5" />
            {t("cURL 重放")}
          </Button>
        </div>
      </div>
      <div className="space-y-4 p-4">
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-5">
          <AttemptMetaCell label={t("供应商")} mono value={`${attempt.provider ?? "-"}${attempt.model ? ` · ${attempt.model}` : ""}`} />
          <AttemptMetaCell label={t("发起时间")} mono value={attempt.startedAt ?? "-"} />
          <AttemptMetaCell label={t("Attempt duration")} mono value={attempt.durationMs != null ? formatDuration(attempt.durationMs) : "-"} />
          <AttemptMetaCell label={t("Next retry wait")} mono title={attempt.waitSource === "retry-after" ? t("Honored upstream Retry-After") : attempt.waitSource === "backoff" ? t("Exponential backoff") : undefined} value={attempt.final ? "-" : formatDuration(attempt.delayMs)} />
          <AttemptMetaCell label={t("账号")} mono value={attempt.account ?? "-"} />
        </div>
        {attempt.error ? (
          <div className="space-y-2 rounded-lg border border-destructive/20 bg-destructive/5 p-3">
            <span className="flex items-center gap-2 text-caption font-semibold text-destructive"><CircleAlert className="h-3.5 w-3.5" />{t("错误信息")}</span>
            <p className="whitespace-pre-wrap break-all rounded border border-destructive/20 bg-destructive/10 p-2 text-caption text-destructive">{attempt.error}</p>
          </div>
        ) : null}
        {!hasRequest || !hasResponse ? (
          <div className="flex flex-wrap gap-4 text-caption text-muted-foreground">
            {!hasRequest ? <span>{t("未捕获上游请求")}</span> : null}
            {!hasResponse ? <span>{t("未捕获上游响应")}</span> : null}
          </div>
        ) : null}
        {hasRequest ? (
          <CapturedCard
            actions={<>
              <CopyButton getText={() => requestText} label={t("Copy")} />
              <DownloadButton filename={`attempt-${attempt.attempt}-request.json`} getText={() => requestText} />
            </>}
            label={t("上游请求")}
            note={t("已脱敏,认证头已过滤")}
            onToggle={() => setShowRequest((value) => !value)}
            open={showRequest}
          >
            {hasRequestHeaders ? (
              <div className="grid grid-cols-1 gap-3 p-3 lg:grid-cols-[minmax(220px,0.75fr)_1.6fr]">
                <CapturedSection expanded={requestHeadExpanded} label={t("请求头")} onExpandedChange={setRequestHeadExpanded} text={requestHeadersText} />
                <CapturedSection expanded={requestExpanded} label={t("请求正文")} onExpandedChange={setRequestExpanded} text={requestText} />
              </div>
            ) : (
              <div className="p-3">
                <CapturedSection expanded={requestExpanded} label={t("请求正文")} onExpandedChange={setRequestExpanded} text={requestText} />
              </div>
            )}
          </CapturedCard>
        ) : null}
        {hasResponse ? (
          <CapturedCard
            actions={<>
              <CopyButton getText={() => responseText} label={t("Copy")} />
              <DownloadButton filename={`attempt-${attempt.attempt}-response.txt`} getText={() => responseText} />
            </>}
            label={t("上游响应")}
            note={attempt.upstreamResponseSize != null ? `${formatBytes(attempt.upstreamResponseSize)}${attempt.upstreamResponseTruncated ? ` · ${t("truncated")}` : ""}` : undefined}
            onToggle={() => setShowResponse((value) => !value)}
            open={showResponse}
          >
            {hasHeaders ? (
              <div className="grid grid-cols-1 gap-3 p-3 lg:grid-cols-[minmax(220px,0.75fr)_1.6fr]">
                <CapturedSection expanded={responseHeadExpanded} label={t("响应头")} onExpandedChange={setResponseHeadExpanded} text={headersText} />
                <CapturedSection expanded={responseExpanded} label={t("响应正文")} onExpandedChange={setResponseExpanded} text={responseText} />
              </div>
            ) : (
              <div className="p-3">
                <CapturedSection expanded={responseExpanded} label={t("响应正文")} onExpandedChange={setResponseExpanded} text={responseText} />
              </div>
            )}
          </CapturedCard>
        ) : null}
        {conversion?.diagnostics?.length ? (
          <details className="rounded-lg border border-border/60">
            <summary className="cursor-pointer px-3 py-2 text-caption font-semibold text-muted-foreground">{t("转换诊断")}</summary>
            <ul className="space-y-1 border-t border-border/60 p-3">
              {conversion.diagnostics.map((diagnostic, diagnosticIndex) => (
                <li className="text-caption" key={`${diagnostic.code}-${diagnosticIndex}`}>
                  <span className="font-mono">{diagnostic.code}</span> <span className="text-muted-foreground">{diagnostic.path}</span> — {diagnostic.detail}
                </li>
              ))}
            </ul>
          </details>
        ) : null}
      </div>
    </div>
  );
}

/** 尝试卡内子卡(请求头/请求正文/响应头/响应正文):标题行右侧一枚收展
 *  按钮只驱动本卡 JSON 树(2026-10-18 用户定版:图标分别落四个子卡标题行
 *  最右,不再放上级卡标题行);树不可解析时不渲染按钮。 */
function CapturedSection({ expanded, label, onExpandedChange, text }: {
  expanded: Set<string>;
  label: string;
  onExpandedChange: (next: Set<string>) => void;
  text: string;
}) {
  const t = useAppText();
  const paths = useMemo(() => jsonTreeContainerPaths(text), [text]);
  const allOpen = paths.length > 0 && expanded.size >= paths.length;
  const toggleLabel = t(allOpen ? "全部折叠" : "全部展开");
  return (
    <section className="flex min-h-0 flex-col overflow-hidden rounded-lg border border-border/60">
      <div className="flex shrink-0 items-center justify-between gap-2 border-b border-border/50 py-1 pl-2.5 pr-1.5">
        <span className="text-caption font-semibold text-muted-foreground">{label}</span>
        {paths.length > 0 ? (
          <button
            aria-label={toggleLabel}
            className="flex h-5 w-5 shrink-0 items-center justify-center rounded border border-border/60 text-muted-foreground outline-none hover:text-foreground"
            onClick={() => onExpandedChange(allOpen ? new Set() : new Set(paths))}
            title={toggleLabel}
            type="button"
          >
            {allOpen ? <ChevronsDownUp className="h-3 w-3" /> : <ChevronsUpDown className="h-3 w-3" />}
          </button>
        ) : null}
      </div>
      <JsonTreePane className="max-h-[420px]" expanded={expanded} onExpandedChange={onExpandedChange} text={text} />
    </section>
  );
}

/** 折叠捕获卡:标题行(chevron + 标题 + 备注)整段可点展开;操作按钮为
 *  兄弟元素(button 不可嵌套),仅在展开时渲染。 */
function CapturedCard({ actions, children, label, note, onToggle, open }: {
  actions?: React.ReactNode;
  children: React.ReactNode;
  label: string;
  note?: string;
  onToggle: () => void;
  open: boolean;
}) {
  return (
    <div className="overflow-hidden rounded-lg border border-border/60">
      <div className="flex w-full flex-wrap items-center gap-2 px-3 py-2">
        <button className="flex min-w-0 flex-1 items-center gap-2 text-left" onClick={onToggle} type="button">
          <ChevronDown className={cn("h-3.5 w-3.5 shrink-0 text-muted-foreground transition-transform", open ? "" : "-rotate-90")} />
          <span className="text-caption font-semibold text-primary">{label}</span>
          {note ? <span className="min-w-0 truncate text-caption text-muted-foreground">{note}</span> : null}
        </button>
        {open && actions ? <span className="flex flex-wrap gap-2">{actions}</span> : null}
      </div>
      {open ? <div className="border-t border-border/60">{children}</div> : null}
    </div>
  );
}

function AttemptMetaCell({ label, mono = false, title, value }: { label: string; mono?: boolean; title?: string; value: string }) {
  return (
    <div className="space-y-1.5 rounded-lg border border-border/60 bg-background/60 p-3">
      <span className="text-caption font-medium text-muted-foreground">{label}</span>
      <p className={`min-w-0 break-all text-caption text-muted-foreground ${mono ? "font-mono" : ""}`} title={title ?? value}>{value}</p>
    </div>
  );
}


function TraceItems({ items, loading, error, hasMore, loadingMore, onLoadMore }: { items: RequestLogTraceItem[]; loading: boolean; error: string; hasMore: boolean; loadingMore: boolean; onLoadMore: () => void }) {
  const t = useAppText();
  if (loading) return <div className="text-caption text-muted-foreground">{t("正在加载详情...")}</div>;
  if (error) return <div className="text-caption text-destructive">{error}</div>;
  if (!items.length) return <div className="rounded-lg border border-border/60 px-3 py-2 text-caption text-muted-foreground">{t("未捕获执行尝试或流事件详情。")}</div>;
  return (
    <div className="space-y-2">
      <div className="text-body font-semibold text-muted-foreground">{t("执行轨迹")} <span className="opacity-70">({items.length})</span></div>
      {items.map((traceItem, index) => (
        <details className="rounded-lg border border-border/60" key={`${traceItem.kind}-${index}`}>
          <summary className="cursor-pointer px-3 py-2 text-caption font-semibold">{traceItem.kind}{traceItem.createdAt ? ` · ${traceItem.createdAt}` : ""}</summary>
          <pre className="network-code max-h-[320px] overflow-auto whitespace-pre-wrap break-words border-t border-border/60 p-3 font-mono text-caption">{JSON.stringify(traceItem.payload, null, 2)}</pre>
        </details>
      ))}
      {hasMore ? <Button className="w-full" disabled={loadingMore} onClick={onLoadMore} size="sm" variant="outline">{loadingMore ? t("正在加载详情...") : t("加载更多")}</Button> : null}
    </div>
  );
}
