import { useEffect, useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";
import { Check, ChevronDown, ChevronRight, ChevronsDownUp, ChevronsUpDown, Copy, Maximize2, Search, X } from "lucide-react";
import { cn } from "@/lib/utils";
import {
  AnimatedIconSwap, copyTextToClipboard, formatBytes, formatCompactNumber,
  networkHeaderRows,
  RequestLogBody, useAppText
} from "../../shared/index";
import type {
  LogBodyChildrenBatch,
  LogBodyContainerInfo,
  LogBodyFilterResult,
  LogBodyStringResult,
  LogBodyValueDescriptor
} from "./log-body-core";
import { getLogBodyService } from "./log-body-client";
import type { LogBodyViewMeta } from "./log-body-core";

type LogPayloadTab = "body" | "header";

type TreeContext = {
  childrenCache: Map<string, LogBodyChildrenBatch>;
  expandedPaths: Set<string>;
  onToggleString: (path: string) => void;
  onToggleJsonPath: (path: string) => void;
  stringOverrides: Map<string, LogBodyStringResult>;
};

export function LogJsonPanel({
  body,
  className,
  headerEmptyLabel = "No values",
  headers,
  hideHeaderRow,
  hideToolbarSearch,
  onQueryChange,
  query,
  statusTone,
  subtitle,
  title
}: {
  body?: RequestLogBody;
  className?: string;
  headerEmptyLabel?: string;
  headers?: Record<string, string | string[]>;
  /** 内嵌时不渲染面板自带标题行(tab 已声明内容语义,避免双标题)。 */
  hideHeaderRow?: boolean;
  /** 搜索框上提到宿主工具栏行时隐藏内嵌搜索框(全屏视图不受影响)。 */
  hideToolbarSearch?: boolean;
  onQueryChange?: (value: string) => void;
  query?: string;
  statusTone?: "ok" | "error";
  subtitle?: string;
  title: string;
}) {
  const t = useAppText();
  const service = getLogBodyService();
  const [selectedTab, setSelectedTab] = useState<LogPayloadTab>("body");
  const [preferTextBody, setPreferTextBody] = useState(false);
  const [fullscreenOpen, setFullscreenOpen] = useState(false);
  const [internalQuery, setInternalQuery] = useState("");
  // 搜索框可上提为受控(宿主持有状态);不传时维持内部 state。
  const effectiveQuery = query ?? internalQuery;
  const handleQueryChange = onQueryChange ?? setInternalQuery;
  const bodyKey = logBodyCacheKey(body);
  // F4.1/F4.2: 解析/聚合/行过滤都在 Worker(或同步回退)侧;主线程只持视图元数据
  // 与虚拟化节点批次,5MB 级 payload 不再阻塞输入。
  const [view, setView] = useState<LogBodyViewMeta | null>(null);
  const [expandedPaths, setExpandedPaths] = useState<Set<string>>(new Set());
  const [childrenCache, setChildrenCache] = useState<Map<string, LogBodyChildrenBatch>>(new Map());
  const [stringOverrides, setStringOverrides] = useState<Map<string, LogBodyStringResult>>(new Map());
  const [filterResult, setFilterResult] = useState<LogBodyFilterResult | null>(null);
  const stringToggleGenerations = useRef(new Map<string, number>());
  const hasJson = view?.root != null;
  const showJsonTree = hasJson && effectiveQuery.trim() === "" && !preferTextBody;
  const headerRows = useMemo(() => networkHeaderRows(headers ?? {}), [headers]);
  const treeContext: TreeContext = {
    childrenCache,
    expandedPaths,
    onToggleString: toggleStringValue,
    onToggleJsonPath: toggleJsonPath,
    stringOverrides
  };

  useEffect(() => {
    let cancelled = false;
    setView(null);
    setFilterResult(null);
    setStringOverrides(new Map());
    setChildrenCache(new Map());
    service.format(bodyKey, body).then((meta) => {
      if (cancelled) {
        return;
      }
      setView(meta);
// 2026-10-18 默认全展开:阈值内的
      // payload 直接铺开层级;超限回落根级展开。expandAll 自带容器/层级
      // 上限,不产生无界 DOM。
      if (!meta.initialExpanded) {
        setExpandedPaths(new Set());
        return;
      }
      service.expandAll(bodyKey).then(({ batches, containerPaths }) => {
        if (cancelled) {
          return;
        }
        setExpandedPaths(new Set(containerPaths));
        setChildrenCache(new Map(Object.entries(batches)));
      });
    });
    return () => {
      cancelled = true;
    };
  }, [bodyKey]);

  // 懒展开:每轮 effect 补取一个缺失的子节点批次,取完即停。
  useEffect(() => {
    if (!hasJson) {
      return;
    }
    const missing = [...expandedPaths].find((path) => !childrenCache.has(path));
    if (missing === undefined) {
      return;
    }
    let cancelled = false;
    service.children(bodyKey, missing).then((batch) => {
      if (cancelled) {
        return;
      }
      setChildrenCache((current) => {
        const next = new Map(current);
        next.set(missing, batch);
        return next;
      });
    });
    return () => {
      cancelled = true;
    };
  }, [expandedPaths, childrenCache, hasJson, bodyKey]);

  useEffect(() => {
    if (!view || showJsonTree) {
      setFilterResult(null);
      return;
    }
    let cancelled = false;
    const timer = window.setTimeout(() => {
      service.filter(bodyKey, effectiveQuery).then((result) => {
        if (!cancelled) {
          setFilterResult(result);
        }
      });
    }, effectiveQuery.trim() ? 120 : 0);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [view, showJsonTree, effectiveQuery, bodyKey]);

  useEffect(() => {
    if (!fullscreenOpen) {
      return;
    }
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        setFullscreenOpen(false);
      }
    };
    window.addEventListener("keydown", closeOnEscape);
    return () => window.removeEventListener("keydown", closeOnEscape);
  }, [fullscreenOpen]);

  function toggleJsonPath(path: string) {
    setExpandedPaths((current) => {
      const next = new Set(current);
      if (next.has(path)) {
        next.delete(path);
      } else {
        next.add(path);
      }
      return next;
    });
  }

  function expandAllJsonPaths() {
    service.expandAll(bodyKey).then(({ batches, containerPaths }) => {
      setExpandedPaths(new Set(containerPaths));
      setChildrenCache((current) => {
        const next = new Map(current);
        for (const [path, batch] of Object.entries(batches)) {
          next.set(path, batch);
        }
        return next;
      });
    });
  }

  function collapseAllJsonPaths() {
    setExpandedPaths(new Set());
    setChildrenCache(new Map());
  }

  function toggleStringValue(path: string) {
    const generation = (stringToggleGenerations.current.get(path) ?? 0) + 1;
    stringToggleGenerations.current.set(path, generation);
    if (stringOverrides.has(path)) {
      setStringOverrides((current) => {
        const next = new Map(current);
        next.delete(path);
        return next;
      });
      return;
    }
    service.stringAt(bodyKey, path).then((result) => {
      if (stringToggleGenerations.current.get(path) !== generation) {
        return;
      }
      setStringOverrides((current) => {
        const next = new Map(current);
        next.set(path, result);
        return next;
      });
    });
  }

  return (
    <div className={cn("network-pane-split flex min-h-0 min-w-0 flex-col", className)}>
      {!hideHeaderRow ? (
        <div className="network-pane-header flex h-10 min-w-0 shrink-0 items-center gap-3 border-b px-3">
          <span className="network-pane-title shrink-0 text-title font-bold">{title}</span>
          {subtitle ? (
            <span className={statusTone === "error" ? "text-destructive font-bold shrink-0 text-body" : "network-muted shrink-0 text-body font-semibold"}>
              {subtitle}
            </span>
          ) : null}
          <div className="flex min-w-0 items-center gap-3">
            {(["body", "header"] as const)
              .filter((tab) => tab !== "header" || headerRows.length > 0)
              .map((tab) => (
              <button
                className={cn(
                  "network-tab border-0 bg-transparent p-0 text-body font-semibold capitalize outline-none",
                  selectedTab === tab && "network-tab-active"
                )}
                key={tab}
                onClick={() => setSelectedTab(tab)}
                type="button"
              >
                {t(tab)}
              </button>
            ))}
          </div>
        </div>
      ) : null}
      <div className="network-pane-body flex min-h-0 flex-1 flex-col overflow-hidden">
        {selectedTab === "body" ? (
          <>
            <LogJsonBodyToolbar
              body={body}
              hasJson={hasJson}
              hideSearch={hideToolbarSearch}
              onCollapseAll={collapseAllJsonPaths}
              onExpandAll={expandAllJsonPaths}
              onQueryChange={handleQueryChange}
              onToggleTextBody={() => setPreferTextBody((current) => !current)}
              preferTextBody={preferTextBody}
              query={effectiveQuery}
              showJsonTree={showJsonTree}
              title={title}
            />
            <LogBodyViewer
              copyDisabled={!view}
              copyLabel={`${t("Copy")} ${title} ${t("body")}`}
              copyText={view?.text ?? ""}
              fullscreenLabel={t("Open fullscreen JSON viewer")}
              onFullscreen={() => setFullscreenOpen(true)}
            >
              <LogJsonBodyContent
                body={body}
                ctx={treeContext}
                filterResult={filterResult}
                query={effectiveQuery}
                showJsonTree={showJsonTree}
                view={view}
              />
            </LogBodyViewer>
            {fullscreenOpen ? (
              <LogJsonFullscreenViewer
                body={body}
                copyLabel={`${t("Copy")} ${title} ${t("body")}`}
                copyText={view?.text ?? ""}
                ctx={treeContext}
                filterResult={filterResult}
                onClose={() => setFullscreenOpen(false)}
                onCollapseAll={collapseAllJsonPaths}
                onExpandAll={expandAllJsonPaths}
                onQueryChange={handleQueryChange}
                onToggleTextBody={() => setPreferTextBody((current) => !current)}
                preferTextBody={preferTextBody}
                query={effectiveQuery}
                showJsonTree={showJsonTree}
                statusTone={statusTone}
                subtitle={subtitle}
                title={title}
                view={view}
              />
            ) : null}
          </>
        ) : (
          <div className="min-h-0 flex-1 overflow-auto">
            <NetworkKeyValueTable emptyLabel={headerEmptyLabel} rows={headerRows} />
          </div>
        )}
      </div>
    </div>
  );
}

function LogJsonBodyToolbar({
  body,
  hasJson,
  hideSearch = false,
  onCollapseAll,
  onExpandAll,
  onQueryChange,
  onToggleTextBody,
  preferTextBody,
  query,
  showJsonTree,
  title
}: {
  body?: RequestLogBody;
  hasJson: boolean;
  hideSearch?: boolean;
  onCollapseAll: () => void;
  onExpandAll: () => void;
  onQueryChange: (value: string) => void;
  onToggleTextBody: () => void;
  preferTextBody: boolean;
  query: string;
  showJsonTree: boolean;
  title: string;
}) {
  const t = useAppText();

  return (
    <div className="network-body-meta flex min-h-9 shrink-0 items-center gap-2 border-b px-3 py-1.5">
      {!hideSearch ? (
        <div className="relative min-w-[180px] flex-1">
          <Search className="network-search-icon pointer-events-none absolute left-2 top-1/2 z-[1] h-3 w-3 -translate-y-1/2" />
          <input
            aria-label={`${t("Filter")} ${title} JSON`}
            className="network-filter-input h-6 w-full rounded border pl-7 pr-2 text-caption font-semibold outline-none"
            onChange={(event) => onQueryChange(event.target.value)}
            placeholder={t("筛选 JSON...")}
            value={query}
          />
        </div>
      ) : null}
      {showJsonTree ? (
        <>
          <button
            aria-label={t("全部展开")}
            className="network-control-button flex h-6 w-6 items-center justify-center rounded border outline-none focus-visible:ring-2 focus-visible:ring-ring/30"
            onClick={onExpandAll}
            title={t("全部展开")}
            type="button"
          >
            <ChevronsUpDown className="h-3 w-3" />
          </button>
          <button
            aria-label={t("全部折叠")}
            className="network-control-button flex h-6 w-6 items-center justify-center rounded border outline-none focus-visible:ring-2 focus-visible:ring-ring/30"
            onClick={onCollapseAll}
            title={t("全部折叠")}
            type="button"
          >
            <ChevronsDownUp className="h-3 w-3" />
          </button>
        </>
      ) : null}
      {hasJson && query.trim() === "" ? (
        <button
          className="network-tab shrink-0 border-0 bg-transparent p-0 text-caption font-semibold outline-none"
          onClick={onToggleTextBody}
          type="button"
        >
          {preferTextBody ? "JSON" : t("Show full content")}
        </button>
      ) : null}
      {body?.contentType ? <span className="network-muted hidden shrink-0 text-caption font-semibold sm:inline">{body.contentType}</span> : null}
      {body && body.sizeBytes > 0 ? <span className="network-muted hidden shrink-0 text-caption font-semibold sm:inline">{formatBytes(body.sizeBytes)}</span> : null}
      {body?.truncated ? <span className="network-service-paused rounded-full px-2 py-0.5 text-caption font-semibold">{t("truncated")}</span> : null}
    </div>
  );
}

function LogJsonBodyContent({
  body,
  ctx,
  filterResult,
  query,
  showJsonTree,
  view
}: {
  body?: RequestLogBody;
  ctx: TreeContext;
  filterResult: LogBodyFilterResult | null;
  query: string;
  showJsonTree: boolean;
  view: LogBodyViewMeta | null;
}) {
  const t = useAppText();
  return showJsonTree && view?.root ? (
    <div className="network-code min-h-0 flex-1 overflow-auto p-3 pr-20 font-mono text-caption leading-5">
      <JsonRootRow ctx={ctx} root={view.root} />
    </div>
  ) : (
    <pre className="network-code min-h-0 flex-1 overflow-auto whitespace-pre-wrap break-words p-3 pr-20 font-mono text-caption leading-5">
      {!view && body ? (
        <span className="network-muted">{t("正在解析正文...")}</span>
      ) : filterResult ? (
        <>
          {filterResult.matched
            ? filterResult.lines.map((line, index) => <HighlightedLine key={index} line={line} query={query} />)
            : t("无匹配行")}
          {filterResult.capped ? <div className="network-muted">{t("仅显示前 2000 行")}</div> : null}
        </>
      ) : null}
    </pre>
  );
}

function LogJsonFullscreenViewer({
  body,
  copyLabel,
  copyText,
  ctx,
  filterResult,
  onClose,
  onCollapseAll,
  onExpandAll,
  onQueryChange,
  onToggleTextBody,
  preferTextBody,
  query,
  showJsonTree,
  statusTone,
  subtitle,
  title,
  view
}: {
  body?: RequestLogBody;
  copyLabel: string;
  copyText: string;
  ctx: TreeContext;
  filterResult: LogBodyFilterResult | null;
  onClose: () => void;
  onCollapseAll: () => void;
  onExpandAll: () => void;
  onQueryChange: (value: string) => void;
  onToggleTextBody: () => void;
  preferTextBody: boolean;
  query: string;
  showJsonTree: boolean;
  statusTone?: "ok" | "error";
  subtitle?: string;
  title: string;
  view: LogBodyViewMeta | null;
}) {
  const t = useAppText();

  return (
    <div
      aria-label={`${title} ${t("Fullscreen JSON viewer")}`}
      aria-modal="true"
      className="network-json-fullscreen fixed inset-0 z-[100] flex min-h-0"
      role="dialog"
    >
      <div className="network-json-fullscreen-panel flex min-h-0 flex-1 flex-col overflow-hidden border">
        <div className="network-json-fullscreen-header flex h-12 min-w-0 shrink-0 items-center gap-3 border-b px-4">
          <span className="network-pane-title min-w-0 truncate text-title font-bold">{title}</span>
          {subtitle ? (
            <span className={statusTone === "error" ? "text-destructive font-bold shrink-0 text-body" : "network-muted shrink-0 text-body font-semibold"}>
              {subtitle}
            </span>
          ) : null}
          <button
            aria-label={t("Close fullscreen JSON viewer")}
            className="network-control-button ml-auto flex h-7 w-7 items-center justify-center rounded border outline-none focus-visible:ring-2 focus-visible:ring-ring/30"
            onClick={onClose}
            title={t("Close")}
            type="button"
          >
            <X className="h-3.5 w-3.5" />
          </button>
        </div>
        <LogJsonBodyToolbar
          body={body}
          hasJson={view?.root != null}
          onCollapseAll={onCollapseAll}
          onExpandAll={onExpandAll}
          onQueryChange={onQueryChange}
          onToggleTextBody={onToggleTextBody}
          preferTextBody={preferTextBody}
          query={query}
          showJsonTree={showJsonTree}
          title={title}
        />
        <div className="network-json-fullscreen-body flex min-h-0 flex-1">
          <LogBodyViewer copyLabel={copyLabel} copyText={copyText}>
            <LogJsonBodyContent
              body={body}
              ctx={ctx}
              filterResult={filterResult}
              query={query}
              showJsonTree={showJsonTree}
              view={view}
            />
          </LogBodyViewer>
        </div>
      </div>
    </div>
  );
}

function logBodyCacheKey(body: RequestLogBody | undefined): string {
  if (!body) {
    return "missing";
  }
  const text = body.text ?? "";
  return [
    body.encoding ?? "",
    body.contentType ?? "",
    body.sizeBytes,
    body.truncated ? "truncated" : "complete",
    text.length,
    text.slice(0, 96),
    text.slice(-96)
  ].join("\u001f");
}

function LogBodyViewer({
  children,
  copyDisabled = false,
  copyLabel,
  copyText,
  fullscreenLabel,
  onFullscreen
}: {
  children: ReactNode;
  copyDisabled?: boolean;
  copyLabel: string;
  copyText: string;
  fullscreenLabel?: string;
  onFullscreen?: () => void;
}) {
  const t = useAppText();
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    if (!copied) {
      return;
    }
    const timer = window.setTimeout(() => setCopied(false), 1300);
    return () => window.clearTimeout(timer);
  }, [copied]);

  async function copyBody() {
    await copyTextToClipboard(copyText);
    setCopied(true);
  }

  return (
    <div className="relative flex min-h-0 flex-1">
      <div className="absolute right-2 top-2 z-10 flex items-center gap-1">
        {onFullscreen ? (
          <button
            aria-label={fullscreenLabel ?? t("Open fullscreen JSON viewer")}
            className="network-control-button flex h-7 w-7 items-center justify-center rounded border outline-none focus-visible:ring-2 focus-visible:ring-ring/30"
            onClick={onFullscreen}
            title={fullscreenLabel ?? t("Open fullscreen JSON viewer")}
            type="button"
          >
            <Maximize2 className="h-3.5 w-3.5" />
          </button>
        ) : null}
        <button
          aria-label={copyLabel}
          className={cn(
            "network-control-button flex h-7 w-7 items-center justify-center rounded border outline-none focus-visible:ring-2 focus-visible:ring-ring/30",
            copied && "network-json-copy-success"
          )}
          disabled={copyDisabled}
          onClick={() => void copyBody()}
          title={copied ? t("Copied") : t("复制")}
          type="button"
        >
          <AnimatedIconSwap iconKey={copied ? "copied" : "copy"}>
            {copied ? <Check className="h-3.5 w-3.5" /> : <Copy className="h-3.5 w-3.5" />}
          </AnimatedIconSwap>
        </button>
      </div>
      {children}
    </div>
  );
}

function JsonRootRow({ ctx, root }: { ctx: TreeContext; root: LogBodyValueDescriptor }) {
  const t = useAppText();

  if (!root.container) {
    return (
      <div className="min-w-0 whitespace-pre-wrap break-words">
        <JsonPrimitiveValue onToggleString={ctx.onToggleString} primitive={root.primitive!} stringOverride={root.primitive ? ctx.stringOverrides.get("$") : undefined} stringPath="$" />
      </div>
    );
  }

  const expanded = ctx.expandedPaths.has("$");
  const open = root.container.array ? "[" : "{";
  const close = root.container.array ? "]" : "}";

  if (root.container.count === 0) {
    return (
      <div className="min-w-0 whitespace-pre-wrap break-words">
        <span className="inline-block w-4" />
        <span>{open}{close}</span>
      </div>
    );
  }

  return (
    <div className="min-w-0">
      <div className="min-w-0 whitespace-pre-wrap break-words">
        <button
          aria-expanded={expanded}
          aria-label={expanded ? `${t("Collapse")} JSON` : `${t("Expand")} JSON`}
          className="network-control-button mr-1 inline-flex h-4 w-4 items-center justify-center rounded border align-[-2px] outline-none focus-visible:ring-2 focus-visible:ring-ring/30"
          onClick={() => ctx.onToggleJsonPath("$")}
          title={expanded ? t("Collapse") : t("Expand")}
          type="button"
        >
          {expanded ? <ChevronDown className="h-3 w-3" /> : <ChevronRight className="h-3 w-3" />}
        </button>
        <span>{open}</span>
        {!expanded ? (
          <>
            <span className="network-muted"> {root.container.summary} </span>
            <span>{close}</span>
          </>
        ) : null}
      </div>
      {expanded ? <JsonChildrenRows container={root.container} ctx={ctx} depth={0} path="$" /> : null}
    </div>
  );
}

function JsonEntryRow({
  ctx,
  depth,
  entry,
  trailingComma
}: {
  ctx: TreeContext;
  depth: number;
  entry: { path: string; key: string; kind: "index" | "key" } & LogBodyValueDescriptor;
  trailingComma: boolean;
}) {
  const t = useAppText();

  if (!entry.container) {
    return (
      <div className="min-w-0 whitespace-pre-wrap break-words" style={{ paddingLeft: depth * 16 }}>
        <JsonTreeLabel kind={entry.kind} label={entry.key} />
        <JsonPrimitiveValue
          onToggleString={ctx.onToggleString}
          primitive={entry.primitive!}
          stringOverride={ctx.stringOverrides.get(entry.path)}
          stringPath={entry.path}
        />
        {trailingComma ? <span>,</span> : null}
      </div>
    );
  }

  const expanded = ctx.expandedPaths.has(entry.path);
  const open = entry.container.array ? "[" : "{";
  const close = entry.container.array ? "]" : "}";

  if (entry.container.count === 0) {
    return (
      <div className="min-w-0 whitespace-pre-wrap break-words" style={{ paddingLeft: depth * 16 }}>
        <span className="inline-block w-4" />
        <JsonTreeLabel kind={entry.kind} label={entry.key} />
        <span>{open}{close}</span>
        {trailingComma ? <span>,</span> : null}
      </div>
    );
  }

  return (
    <div className="min-w-0">
      <div className="min-w-0 whitespace-pre-wrap break-words" style={{ paddingLeft: depth * 16 }}>
        <button
          aria-expanded={expanded}
          aria-label={expanded ? `${t("Collapse")} JSON` : `${t("Expand")} JSON`}
          className="network-control-button mr-1 inline-flex h-4 w-4 items-center justify-center rounded border align-[-2px] outline-none focus-visible:ring-2 focus-visible:ring-ring/30"
          onClick={() => ctx.onToggleJsonPath(entry.path)}
          title={expanded ? t("Collapse") : t("Expand")}
          type="button"
        >
          {expanded ? <ChevronDown className="h-3 w-3" /> : <ChevronRight className="h-3 w-3" />}
        </button>
        <JsonTreeLabel kind={entry.kind} label={entry.key} />
        <span>{open}</span>
        {!expanded ? (
          <>
            <span className="network-muted"> {entry.container.summary} </span>
            <span>{close}</span>
            {trailingComma ? <span>,</span> : null}
          </>
        ) : null}
      </div>
      {expanded ? <JsonChildrenRows container={entry.container} ctx={ctx} depth={depth} path={entry.path} /> : null}
    </div>
  );
}

function JsonChildrenRows({
  container,
  ctx,
  depth,
  path
}: {
  container: LogBodyContainerInfo;
  ctx: TreeContext;
  depth: number;
  path: string;
}) {
  const batch = ctx.childrenCache.get(path);
  const close = container.array ? "]" : "}";

  if (!batch) {
    // 批次在途(Worker 往返/同步微任务),先渲染收尾符避免结构跳变。
    return (
      <div className="min-w-0 whitespace-pre-wrap break-words" style={{ paddingLeft: depth * 16 }}>
        <span className="inline-block w-4" />
        <span>{close}</span>
      </div>
    );
  }

  const { entries, hasMore, total } = batch;
  return (
    <>
      {entries.map((entry, index) => (
        <JsonEntryRow
          ctx={ctx}
          depth={depth + 1}
          entry={entry}
          key={entry.path}
          trailingComma={index < entries.length - 1 || hasMore}
        />
      ))}
      {hasMore ? (
        <div className="network-muted min-w-0 whitespace-pre-wrap break-words" style={{ paddingLeft: (depth + 1) * 16 }}>
          <span className="inline-block w-4" />
          <span>{container.array ? `... ${formatCompactNumber(Math.max(0, total - entries.length))}` : "..."}</span>
        </div>
      ) : null}
      <div className="min-w-0 whitespace-pre-wrap break-words" style={{ paddingLeft: depth * 16 }}>
        <span className="inline-block w-4" />
        <span>{close}</span>
      </div>
    </>
  );
}

function JsonTreeLabel({ kind, label }: { kind: "index" | "key"; label: string }) {
  return (
    <>
      <span className={kind === "index" ? "network-muted" : "text-[color:var(--network-accent)]"}>
        {kind === "index" ? label : JSON.stringify(label)}
      </span>
      <span>: </span>
    </>
  );
}

function JsonPrimitiveValue({
  onToggleString,
  primitive,
  stringOverride,
  stringPath
}: {
  onToggleString: (path: string) => void;
  primitive: NonNullable<LogBodyValueDescriptor["primitive"]>;
  stringOverride?: LogBodyStringResult;
  stringPath: string;
}) {
  const t = useAppText();
  if (primitive.kind === "string") {
    const serialized = stringOverride?.display ?? primitive.display;
    return (
      <span className="text-emerald-600 dark:text-emerald-300">
        {serialized}
        {primitive.truncated ? (
          <button
            className="network-muted ml-1 inline border-0 bg-transparent p-0 align-baseline text-caption font-semibold outline-none"
            onClick={() => onToggleString(stringPath)}
            title={t("展开长文本")}
            type="button"
          >
            {`… (+${primitive.fullLength - primitive.display.length})`}
          </button>
        ) : null}
        {stringOverride?.truncated ? <span className="network-muted"> ({t("truncated")})</span> : null}
      </span>
    );
  }
  if (primitive.kind === "number") {
    return <span className="text-blue-600 dark:text-blue-300">{primitive.display}</span>;
  }
  if (primitive.kind === "boolean") {
    return <span className="text-purple-600 dark:text-purple-300">{primitive.display}</span>;
  }
  if (primitive.kind === "null") {
    return <span className="network-muted">null</span>;
  }
  return <span>{primitive.display}</span>;
}

function HighlightedLine({ line, query }: { line: string; query: string }) {
  const normalized = query.trim().toLowerCase();
  if (line.length === 0 || !normalized) {
    return <div>{line || "\u00A0"}</div>;
  }
  const lower = line.toLowerCase();
  const segments: Array<{ text: string; match: boolean }> = [];
  let index = 0;
  while (index < line.length) {
    const found = lower.indexOf(normalized, index);
    if (found === -1) {
      if (index < line.length) {
        segments.push({ text: line.slice(index), match: false });
      }
      break;
    }
    if (found > index) {
      segments.push({ text: line.slice(index, found), match: false });
    }
    segments.push({ text: line.slice(found, found + normalized.length), match: true });
    index = found + normalized.length;
  }
  return (
    <div>
      {segments.map((segment, segmentIndex) =>
        segment.match ? (
          <mark className="network-search-mark" key={segmentIndex}>{segment.text}</mark>
        ) : (
          <span key={segmentIndex}>{segment.text}</span>
        )
      )}
    </div>
  );
}

export function NetworkKeyValueTable({ emptyLabel = "No values", rows }: { emptyLabel?: string; rows: Array<[string, string]> }) {
  const t = useAppText();

  if (rows.length === 0) {
    return <div className="network-kv-empty px-4 py-10 text-center text-body font-semibold">{t(emptyLabel)}</div>;
  }

  return (
    <div className="min-w-[520px]">
      <div className="network-kv-header grid h-9 grid-cols-[minmax(180px,0.9fr)_minmax(280px,1.6fr)] items-center border-b text-body font-bold">
        <div className="network-kv-key-head border-r px-3">{t("Key")}</div>
        <div className="px-3">{t("Value")}</div>
      </div>
      {rows.map(([key, value], index) => (
        <div
          className={cn(
            "network-kv-row grid min-h-9 grid-cols-[minmax(180px,0.9fr)_minmax(280px,1.6fr)] items-start text-body font-semibold",
            index % 2 === 0 ? "network-kv-row-even" : "network-kv-row-odd"
          )}
          key={`${key}-${index}`}
        >
          <div className="network-kv-key min-w-0 px-3 py-2">{key}</div>
          <div className="network-kv-value min-w-0 whitespace-pre-wrap break-words px-3 py-2">{value}</div>
        </div>
      ))}
    </div>
  );
}
