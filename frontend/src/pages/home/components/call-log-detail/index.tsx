import { useEffect, useMemo, useState } from "react";
import {
  ArrowLeft, Check, CircleAlert, Copy, FileText, LoaderCircle,
} from "lucide-react";
import { useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import type { CallLogEntry, RequestLogDetail } from "@/contracts/app";
import { api, hasBridge } from "@/lib/api";
import { queryKeys } from "@/lib/queryKeys";
import {
  copyTextToClipboard, formatDuration, formatLogDateTime, useAppText,
} from "../../shared";
import { requestBodyPreview, responseBodyPreview } from "../../shared/request-body-preview";
import { responseSummary } from "../../shared/response-summary";
import { CurlPreviewDialog } from "../shared/curl-dialog";
import { generateCurl } from "../shared/curl";
import { TraceChunksDialog } from "../shared/trace-chunks-dialog";
import { AttemptsTab } from "./attempts";
import { RequestTab, ResponseTab } from "./body-tabs";
import { OverviewCard } from "./overview";
import {
  contentType, pageOrigin, safeJson, toRequestLogBody,
  type DetailTab, type RequestBodyMode, type ResponseBodyMode,
} from "./shared";


export function CallLogDetailView({ item, onBack }: { item?: CallLogEntry; onBack: () => void }) {
  const t = useAppText();
  const [activeTab, setActiveTab] = useState<DetailTab>("request");
  const [curlOpen, setCurlOpen] = useState(false);
  const [curlCommand, setCurlCommand] = useState("");
  const [chunksOpen, setChunksOpen] = useState(false);
  const inFlight = item?.status === "pending" || item?.status === "processing";
  const detailQuery = useQuery({
    queryKey: queryKeys.requestLogDetail(item?.requestId ?? ""),
    queryFn: async () => (await api.getRequestLogDetail({ request_id: item!.requestId })) ?? null,
    enabled: Boolean(item && !inFlight && hasBridge()),
  });
  const traceQuery = useInfiniteQuery({
    queryKey: queryKeys.requestLogTrace(item?.requestId ?? ""),
    queryFn: ({ pageParam }) => api.getRequestLogTrace({ request_id: item!.requestId, offset: pageParam, limit: 100 }),
    initialPageParam: 0,
    getNextPageParam: (page) => page.nextOffset,
    enabled: Boolean(item && !inFlight && (activeTab === "attempts" || activeTab === "response") && hasBridge()),
  });
  const detail: RequestLogDetail | undefined = detailQuery.data ?? undefined;
  const error = detailQuery.error instanceof Error ? detailQuery.error.message : detailQuery.error ? String(detailQuery.error) : "";
  const loading = detailQuery.isLoading;
  const traceItems = useMemo(() => traceQuery.data?.pages.flatMap((page) => page.items) ?? [], [traceQuery.data]);

  const requestBody = useMemo(() => toRequestLogBody(detail?.request_body_text, detail?.request_body_size_bytes, detail?.request_body_truncated), [detail]);
  const responseBody = useMemo(() => toRequestLogBody(detail?.response_body_text, detail?.response_body_size_bytes, detail?.response_body_truncated, contentType(detail?.response_headers)), [detail]);
  const requestPreview = useMemo(() => requestBodyPreview(requestBody), [requestBody]);
  const responsePreview = useMemo(() => responseBodyPreview(responseBody), [responseBody]);
  const summary = useMemo(() => responseSummary(responseBody, traceItems), [responseBody, traceItems]);

  // 正文查看模式:请求侧默认对话阅览(不可解析时回退 JSON 原文),响应侧
  // 默认内容预览。记录切换时手动选择优先,切换请求后重置。
  const [requestMode, setRequestMode] = useState<RequestBodyMode>("conversation");
  const [responseMode, setResponseMode] = useState<ResponseBodyMode>("preview");
  // 响应侧 JSON 搜索框上提到 tab 行,状态随请求切换重置。
  const [responseQuery, setResponseQuery] = useState("");
  useEffect(() => {
    setRequestMode(requestPreview?.format === "conversation" ? "conversation" : "json");
  }, [item?.requestId, requestPreview.format]);
  useEffect(() => {
    setResponseMode(responsePreview?.format === "response" ? "preview" : "json");
  }, [item?.requestId, responsePreview.format]);
  useEffect(() => {
    setResponseQuery("");
  }, [item?.requestId]);

  if (!item) return <EmptyState label={t("未找到请求记录")} onBack={onBack} />;

  const showRequestCurl = () => {
    setCurlCommand(generateCurl({
      body: requestPreview.parsed ? safeJson(requestBody?.text) : requestBody?.text,
      headers: detail?.request_headers,
      inferModelPath: true,
      origin: pageOrigin() || undefined,
      protocol: requestPreview.protocol,
    }));
    setCurlOpen(true);
  };

  return (
    <div className="flex h-full min-h-0 flex-col">
      <header className="flex shrink-0 flex-wrap items-center gap-3 border-b border-border/50 pb-3">
        <Button onClick={onBack} size="sm" type="button" variant="outline"><ArrowLeft className="h-4 w-4" />{t("返回调用日志")}</Button>
        <div className="flex h-8 w-8 items-center justify-center rounded-lg bg-primary/10">
          <FileText className="h-4 w-4 text-primary" />
        </div>
        <div className="min-w-0 flex-1">
          <h1 className="truncate text-title font-semibold">{t("请求/响应详情")}</h1>
          <CopyableRequestId requestId={item.requestId} />
        </div>
        <div className="text-right text-caption text-muted-foreground">
          <div>{formatLogDateTime(item.createdAt)}</div>
          <div>{item.method} {item.endpointPath || item.path} · HTTP {item.statusCode || "-"} · {formatDuration(item.durationMs)}</div>
        </div>
      </header>

      <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto pb-4">
        {/* 2026-10-17:概览 + Token 用量合并单卡(紧凑统计条),纵向空间让给下方详情卡。 */}
        <OverviewCard item={item} />

        {inFlight ? (
          <Card>
            <CardContent className="py-6 text-body text-muted-foreground">{t("请求进行中,完成后可查看正文与响应头。")}</CardContent>
          </Card>
        ) : loading ? (
          <Card><CardContent className="flex min-h-[240px] items-center justify-center gap-2 text-body text-muted-foreground"><LoaderCircle className="h-4 w-4 animate-spin" />{t("正在加载详情...")}</CardContent></Card>
        ) : error ? (
          <Card><CardContent className="network-error-box flex items-start gap-2 text-body"><CircleAlert className="mt-0.5 h-4 w-4 shrink-0" />{error}</CardContent></Card>
        ) : !detail ? (
          <Card><CardContent className="py-6 text-body text-muted-foreground">{t("未找到请求记录")}</CardContent></Card>
        ) : (
          <Card className="flex min-h-0 flex-1 flex-col overflow-hidden">
            <div className="shrink-0 border-b border-border/60 px-4 pt-3">
              <div className="grid w-full grid-cols-3 gap-1 rounded-lg border border-border/60 bg-muted p-1" role="tablist" aria-label={t("请求详情区域")}>
                {(["request", "response", "attempts"] as const).map((tab) => (
                  <button
                    aria-selected={activeTab === tab}
                    className={`rounded-md px-3 py-1.5 text-body font-semibold transition-colors ${activeTab === tab ? "bg-primary text-primary-foreground shadow-sm" : "text-muted-foreground hover:text-foreground"}`}
                    key={tab}
                    onClick={() => setActiveTab(tab)}
                    role="tab"
                    type="button"
                  >
                    {tab === "request" ? t("请求") : tab === "response" ? t("响应") : t("请求尝试")}
                  </button>
                ))}
              </div>
            </div>
            <div className="flex min-h-0 flex-1 flex-col overflow-hidden p-4" role="tabpanel">
              {activeTab === "request" ? (
                <RequestTab
                  body={requestBody}
                  detail={detail}
                  mode={requestMode}
                  onShowCurl={showRequestCurl}
                  preview={requestPreview}
                  setMode={setRequestMode}
                />
              ) : null}
              {activeTab === "response" ? (
                <ResponseTab
                  body={responseBody}
                  detail={detail}
                  item={item}
                  mode={responseMode}
                  onOpenChunks={() => setChunksOpen(true)}
                  preview={responsePreview}
                  query={responseQuery}
                  setMode={setResponseMode}
                  setQuery={setResponseQuery}
                  summary={summary}
                  traceCount={traceItems.length}
                />
              ) : null}
              {activeTab === "attempts" ? (
                <AttemptsTab
                  detail={detail}
                  item={item}
                  onShowCurl={(curl) => { setCurlCommand(curl); setCurlOpen(true); }}
                  onOpenChunks={() => setChunksOpen(true)}
                  traceItems={traceItems}
                  traceLoading={traceQuery.isLoading}
                  traceError={traceQuery.error instanceof Error ? traceQuery.error.message : ""}
                  traceHasMore={Boolean(traceQuery.hasNextPage)}
                  traceLoadingMore={traceQuery.isFetchingNextPage}
                  onLoadMoreTrace={() => void traceQuery.fetchNextPage()}
                />
              ) : null}
            </div>
          </Card>
        )}
      </div>

      <CurlPreviewDialog curlCommand={curlCommand} onOpenChange={setCurlOpen} open={curlOpen} />
      <TraceChunksDialog items={traceItems} onOpenChange={setChunksOpen} open={chunksOpen} />
    </div>
  );
}


function CopyableRequestId({ requestId }: { requestId: string }) {
  const t = useAppText();
  const [copied, setCopied] = useState(false);
  return <button className="flex max-w-full items-center gap-1 font-mono text-caption text-muted-foreground hover:text-foreground" onClick={() => { void copyTextToClipboard(requestId).then(() => { setCopied(true); window.setTimeout(() => setCopied(false), 1200); }); }} title={t("Copy request ID")} type="button"><span className="truncate">{requestId}</span>{copied ? <Check className="h-3 w-3" /> : <Copy className="h-3 w-3" />}</button>;
}

function EmptyState({ label, onBack }: { label: string; onBack: () => void }) {
  const t = useAppText();
  return <div className="flex h-full min-h-0 flex-col items-center justify-center gap-3"><div className="text-body text-muted-foreground">{label}</div><Button onClick={onBack} type="button" variant="outline"><ArrowLeft className="h-4 w-4" />{t("返回调用日志")}</Button></div>;
}
