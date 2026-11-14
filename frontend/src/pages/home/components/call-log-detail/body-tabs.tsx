import { useCallback, useState } from "react";
import { Copy, Layers, Search, Terminal } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import type { CallLogEntry, RequestLogBody, RequestLogDetail } from "@/contracts/app";

import {
  formatExactNumber,
  useAppNumberLocale,
  useAppText,
  copyTextToClipboard,
} from "../../shared";
import type { RequestBodyPreview } from "../../shared/request-body-preview";
import type { ResponseSummary, ResponseTerminal } from "../../shared/response-summary";
import {
  ConversationToolbarControls,
  ConversationViewer,
  createConversationViewSettings,
  type ConversationViewSettings,
} from "../shared/conversation-viewer";
import { LogJsonPanel } from "../shared/log-json-panel";
import { ResponseFlowView } from "../shared/response-flow";
import { CopyButton, DownloadButton, ModeToggle, TabToolbar } from "./actions";
import { exactOrDash, type RequestBodyMode, type ResponseBodyMode } from "./shared";

export function RequestTab({ body, detail, mode, onShowCurl, preview, setMode }: {
  body?: RequestLogBody;
  detail: RequestLogDetail;
  mode: RequestBodyMode;
  onShowCurl: () => void;
  preview?: RequestBodyPreview;
  setMode: (mode: RequestBodyMode) => void;
}) {
  const t = useAppText();
  // 会话查看器设置:状态在 tab 级(与迁移前查看器内部 state 同生命周期,
  // tab 切换/请求切换重挂载即重置)。
  const [convSettings, setConvSettings] = useState(createConversationViewSettings);
  const patchConvSettings = useCallback(
    (patch: Partial<ConversationViewSettings>) => setConvSettings((current) => ({ ...current, ...patch })),
    []
  );
  const conversationActive = mode === "conversation" && preview?.format === "conversation";
  return (
    <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto">
      <TabToolbar>
        <ModeToggle
          modes={[
            ...(preview?.format === "conversation" ? [{ key: "conversation", label: t("对话阅览") }] : []),
            { key: "json", label: t("JSON 原文") },
          ]}
          onChange={(key) => setMode(key as RequestBodyMode)}
          value={mode}
        />
        <Button className="hover:bg-primary hover:text-primary-foreground" onClick={onShowCurl} size="sm" variant="outline">
          <Terminal className="h-3.5 w-3.5" />
          {t("cURL 重放")}
        </Button>
        <div className="ml-auto flex flex-wrap items-center gap-2">
          {conversationActive ? (
            <ConversationToolbarControls
              onSettingsChange={patchConvSettings}
              preview={preview}
              settings={convSettings}
            />
          ) : null}
          {body ? <CopyButton getText={() => body.text} label={t("Copy")} /> : null}
          {body ? <DownloadButton filename={`request-body-${Date.now()}.json`} getText={() => body.text} /> : null}
        </div>
      </TabToolbar>
      {conversationActive ? (
        <div className="max-h-[68vh] min-h-0 overflow-y-auto rounded-lg border border-border/60 p-3">
          <ConversationViewer onSettingsChange={patchConvSettings} preview={preview} settings={convSettings} />
        </div>
      ) : body ? (
        <div className="min-h-[320px] flex-1">
          <LogJsonPanel body={body} className="h-full" hideHeaderRow title={t("请求正文")} />
        </div>
      ) : (
        <div className="flex min-h-[320px] items-center justify-center text-body text-muted-foreground">
          {detail.request_body_capture_status === "unavailable" ? t("未捕获正文（正文捕获已关闭或该记录无正文，可在 设置 → 可观测性 中开启）") : t("无请求正文")}
        </div>
      )}
      <HeadersBlock
        emptyLabel={t("无请求头")}
        headers={detail.request_headers ?? {}}
        onCopy={(text) => void copyTextToClipboard(text)}
        title={t("请求头")}
      />
    </div>
  );
}

/** 响应 tab(2026-10-18):「内容预览 / JSON 原文」双模式;
 *  JSON 搜索框上提到本工具栏行(tab 行内),JSON 层级默认展开,树 ⇄ 完整
 *  内容切换内建在面板工具栏。 */
export function ResponseTab({ body, detail, item, mode, onOpenChunks, preview, query, setMode, setQuery, summary, traceCount }: {
  body?: RequestLogBody;
  detail: RequestLogDetail;
  item: CallLogEntry;
  mode: ResponseBodyMode;
  onOpenChunks: () => void;
  preview?: RequestBodyPreview;
  query: string;
  setMode: (mode: ResponseBodyMode) => void;
  setQuery: (query: string) => void;
  summary?: ResponseSummary;
  traceCount: number;
}) {
  const t = useAppText();
  return (
    <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto">
      <TabToolbar>
        <ModeToggle
          modes={[
            ...(preview?.format === "response" ? [{ key: "preview", label: t("内容预览") }] : []),
            { key: "json", label: t("JSON 原文") },
          ]}
          onChange={(key) => setMode(key as ResponseBodyMode)}
          value={mode}
        />
        {mode === "json" && body ? (
          <div className="relative">
            <Search className="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
            <Input
              className="h-8 w-40 pl-8 text-caption md:w-52"
              onChange={(event) => setQuery(event.target.value)}
              placeholder={t("筛选 JSON...")}
              value={query}
            />
          </div>
        ) : null}
        <div className="ml-auto flex gap-2">
          <Button disabled={!traceCount} onClick={onOpenChunks} size="sm" variant="outline">
            <Layers className="h-3.5 w-3.5" />
            {t("流事件")}
            {traceCount > 0 ? <Badge variant="secondary">{traceCount}</Badge> : null}
          </Button>
          <span className={`self-center text-caption font-semibold ${item.ok ? "text-muted-foreground" : "text-destructive"}`}>HTTP {item.statusCode || "-"}</span>
          {body ? <CopyButton getText={() => body.text} label={t("Copy")} /> : null}
          {body ? <DownloadButton filename={`response-body-${Date.now()}.json`} getText={() => body.text} /> : null}
        </div>
      </TabToolbar>
      {/* 2026-10-18 用户定版:纯「终态未知」徽章行是噪音 —— 无终态证据、
          无结束原因、无 usage 时整行不渲染;有任一证据仍照常展示。 */}
      {summary && (summary.terminal !== "unknown" || summary.finishReason || summary.usage) ? (
        <ResponseSummaryBar summary={summary} />
      ) : null}
      {mode === "preview" && preview?.format === "response" ? (
        preview.sections?.length ? (
          <ResponseFlowView protocol={preview.protocol} sections={preview.sections} />
        ) : (
          <div className="flex min-h-[200px] items-center justify-center rounded-lg border border-border/60 bg-muted/20 text-body text-muted-foreground">{t("无响应正文")}</div>
        )
      ) : body ? (
        <div className="min-h-[320px] flex-1">
          <LogJsonPanel
            body={body}
            className="h-full"
            hideHeaderRow
            hideToolbarSearch
            onQueryChange={setQuery}
            query={query}
            statusTone={item.ok ? "ok" : "error"}
            subtitle={`HTTP ${item.statusCode || "-"}`}
            title={t("响应正文")}
          />
        </div>
      ) : (
        <div className="flex min-h-[320px] items-center justify-center text-body text-muted-foreground">
          {detail.response_body_capture_status === "unavailable" ? t("未捕获正文（正文捕获已关闭或该记录无正文，可在 设置 → 可观测性 中开启）") : t("无响应正文")}
        </div>
      )}
      <HeadersBlock
        emptyLabel={t("无响应头")}
        headers={detail.response_headers ?? {}}
        onCopy={(text) => void copyTextToClipboard(text)}
        title={t("响应头")}
      />
    </div>
  );
}

export function HeadersBlock({ headers, onCopy, title, emptyLabel }: {
  emptyLabel: string;
  headers: Record<string, string | string[]>;
  onCopy: (text: string) => void;
  title: string;
}) {
  const t = useAppText();
  const entries = Object.entries(headers);
  const text = JSON.stringify(headers, null, 2);
  return (
    <details className="shrink-0 rounded-lg border border-border/60" open={false}>
      <summary className="flex cursor-pointer items-center gap-2 px-3 py-2 text-caption font-semibold text-muted-foreground">
        {title}
        <span className="font-normal">({entries.length})</span>
        <span className="ml-auto flex gap-1" onClick={(event) => event.stopPropagation()}>
          {entries.length ? (
            <>
              <button className="text-muted-foreground hover:text-foreground" onClick={() => onCopy(text)} title={t("Copy")} type="button"><Copy className="h-3.5 w-3.5" /></button>
              <DownloadButton filename={`${title}-${Date.now()}.json`} getText={() => text} />
            </>
          ) : null}
        </span>
      </summary>
      {entries.length ? (
        <dl className="divide-y divide-border/50 border-t border-border/60">
          {entries.map(([name, value]) => (
            <div className="flex gap-3 px-3 py-1.5 text-caption" key={name}>
              <dt className="shrink-0 font-mono text-muted-foreground">{name}</dt>
              <dd className="min-w-0 break-all font-mono">{Array.isArray(value) ? value.join(", ") : value}</dd>
            </div>
          ))}
        </dl>
      ) : (
        <div className="border-t border-border/60 px-3 py-3 text-caption text-muted-foreground">{emptyLabel}</div>
      )}
    </details>
  );
}

const TERMINAL_TONE: Record<ResponseTerminal, { label: string; variant: "success" | "warning" | "danger" | "outline" }> = {
  complete: { label: "已完成", variant: "success" },
  failed: { label: "失败", variant: "danger" },
  incomplete: { label: "不完整", variant: "warning" },
  unknown: { label: "终态未知", variant: "outline" },
};

export function ResponseSummaryBar({ summary }: { summary: ResponseSummary }) {
  const t = useAppText();
  const numberLocale = useAppNumberLocale();
  const terminal = TERMINAL_TONE[summary.terminal];
  const usage = summary.usage;
  return (
    <div className="flex shrink-0 flex-wrap items-center gap-2 rounded-lg border border-border/60 bg-muted/20 px-3 py-2 text-caption">
      <Badge variant={terminal.variant}>{t(terminal.label)}</Badge>
      {summary.finishReason ? <span className="text-muted-foreground">{t("结束原因")}: <span className="font-mono">{summary.finishReason}</span></span> : null}
      {summary.incompleteReason ? <span className="text-destructive">{t("不完整原因")}: <span className="font-mono">{summary.incompleteReason}</span></span> : null}
      {usage ? (
        <span className="ml-auto flex flex-wrap gap-3 font-mono">
          <span>{t("输入")} {exactOrDash(usage.input, numberLocale)}</span>
          <span>{t("输出")} {exactOrDash(usage.output, numberLocale)}</span>
          {usage.reasoning != null ? <span>Thinking {formatExactNumber(usage.reasoning, numberLocale)}</span> : null}
          {usage.cacheRead != null ? <span>{t("缓存读取")} {formatExactNumber(usage.cacheRead, numberLocale)}</span> : null}
          <span>{t("总计")} {exactOrDash(usage.total, numberLocale)}</span>
        </span>
      ) : null}
    </div>
  );
}
