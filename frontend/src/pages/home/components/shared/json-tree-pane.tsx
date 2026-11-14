// 尝试卡内嵌的轻量 JSON 树面板(同步、无 Worker):与 LogJsonPanel 同一套
// 视觉口径(accent 键名/着色基元),但不走 log-body 服务 —— 尝试正文典型
// 体量小,不值得每卡一条 Worker 通道;超大字符串就地截断可展开。
// 解析失败时依次回落:单行连排 SSE 聚合(home/shared/logs 扫描器)→ 原文 pre。
// 2026-10-18 二轮:展开状态改受控 —— 「全部展开/全部折叠」合并为单枚按钮
// 移入宿主卡标题行,面板内不再有独立工具栏行。
import { useMemo, useState } from "react";
import { useAppText } from "../../shared";
import { formatLogBodyView } from "../../shared/logs";

const STRING_DISPLAY_LIMIT = 240;
// 「全部展开」容器上限(对齐 log-body 的 expandAll 容器帽,防无界 DOM)。
const EXPAND_ALL_CONTAINER_CAP = 2000;

type Container = Record<string, unknown> | unknown[];
type ContainerEntry = { key: string; index: boolean; value: unknown };

export function JsonTreePane({ className, expanded, onExpandedChange, text }: {
  className?: string;
  /** 展开路径集(受控,键为 \u001f 连接的路径段;根为 "");宿主卡标题行的
   *  单枚收展按钮持有此状态。 */
  expanded: Set<string>;
  onExpandedChange: (next: Set<string>) => void;
  text: string;
}) {
  const parsed = useMemo(() => parsePaneValue(text), [text]);

  if (!parsed.ok) {
    return (
      <pre className={`network-code overflow-auto whitespace-pre-wrap break-words font-mono text-caption leading-5 ${className ?? ""}`}>{text}</pre>
    );
  }

  const togglePath = (path: string) => {
    const next = new Set(expanded);
    if (next.has(path)) {
      next.delete(path);
    } else {
      next.add(path);
    }
    onExpandedChange(next);
  };

  return (
    <div className={`flex min-h-0 flex-col ${className ?? ""}`}>
      <div className="network-code min-h-0 flex-1 overflow-auto p-2 font-mono text-caption leading-5">
        <ContainerRow
          depth={0}
          expanded={expanded}
          index={false}
          label={null}
          path=""
          togglePath={togglePath}
          value={parsed.value}
        />
      </div>
    </div>
  );
}

/** 全部容器路径(含根 "";顺序 = 先根后子):宿主据此刻「全部展开」目标集、
 *  判定当前是否已全展开。解析失败(含空串)返回 []。 */
export function jsonTreeContainerPaths(text: string): string[] {
  const parsed = parsePaneValue(text);
  if (!parsed.ok) return [];
  const paths: string[] = [];
  const walk = (value: unknown, path: string) => {
    if (paths.length >= EXPAND_ALL_CONTAINER_CAP || !isContainer(value)) return;
    paths.push(path);
    for (const entry of containerEntries(value)) walk(entry.value, childPath(path, entry.key));
  };
  walk(parsed.value, "");
  return paths;
}

/** 与 LogJsonPanel 的 log-body 服务同口径:完整 JSON → SSE 流聚合 →
 *  截断 JSON 部分树恢复(formatLogBodyView 兜底链),再失败按不可解析处理。 */
function parsePaneValue(text: string): { ok: true; value: unknown } | { ok: false } {
  if (!text) return { ok: false };
  // 逐尝试的上游响应正文常是 SSE 文本:聚合成 {…, streamed_data} 再上树;
  // 截断的请求正文走部分树恢复。
  const view = formatLogBodyView({ encoding: "utf8", sizeBytes: text.length, text, truncated: false });
  if (view.json !== undefined) {
    return { ok: true, value: view.json };
  }
  return { ok: false };
}

/** 容器行(label 为 null 时是根,不渲染键名;isLast 控制收尾逗号)。 */
function ContainerRow({ depth, expanded, index, isLast = true, label, path, togglePath, value }: {
  depth: number;
  expanded: Set<string>;
  index: boolean;
  isLast?: boolean;
  label: string | null;
  path: string;
  togglePath: (path: string) => void;
  value: unknown;
}) {
  const t = useAppText();
  if (!isContainer(value)) return null;
  const isOpen = expanded.has(path);
  const entries = containerEntries(value);
  const open = Array.isArray(value) ? "[" : "{";
  const close = Array.isArray(value) ? "]" : "}";

  if (entries.length === 0) {
    return (
      <div className="min-w-0 whitespace-pre-wrap break-words" style={{ paddingLeft: depth * 14 }}>
        <span className="inline-block w-4" />
        {label != null ? <TreeKey index={index} label={label} /> : null}
        <span>{open}{close}</span>
        {!isLast ? <span>,</span> : null}
      </div>
    );
  }

  return (
    <div className="min-w-0">
      <div className="min-w-0 whitespace-pre-wrap break-words" style={{ paddingLeft: depth * 14 }}>
        <CollapseChevron isOpen={isOpen} onToggle={togglePath} path={path} title={isOpen ? t("Collapse") : t("Expand")} />
        {label != null ? <TreeKey index={index} label={label} /> : null}
        <span>{open}</span>
        {!isOpen ? (
          <>
            <span className="network-muted"> {containerSummary(value)} </span>
            <span>{close}</span>
            {!isLast ? <span>,</span> : null}
          </>
        ) : null}
      </div>
      {isOpen ? (
        <>
          {entries.map((entry, entryIndex) => (
            <ValueRow
              depth={depth + 1}
              entry={entry}
              expanded={expanded}
              isLast={entryIndex === entries.length - 1}
              key={`${entry.index ? "#" : ":"}${entry.key}`}
              path={childPath(path, entry.key)}
              togglePath={togglePath}
            />
          ))}
          <div className="min-w-0 whitespace-pre-wrap break-words" style={{ paddingLeft: depth * 14 }}>
            <span>{close}</span>
            {!isLast ? <span>,</span> : null}
          </div>
        </>
      ) : null}
    </div>
  );
}

/** 条目行:基元直出,容器递归 ContainerRow(尾逗号下传)。 */
function ValueRow({ depth, entry, expanded, isLast, path, togglePath }: {
  depth: number;
  entry: ContainerEntry;
  expanded: Set<string>;
  isLast: boolean;
  path: string;
  togglePath: (path: string) => void;
}) {
  if (!isContainer(entry.value)) {
    return (
      <div className="min-w-0 whitespace-pre-wrap break-words" style={{ paddingLeft: depth * 14 }}>
        <TreeKey index={entry.index} label={entry.key} />
        <PrimitiveValue value={entry.value} />
        {!isLast ? <span>,</span> : null}
      </div>
    );
  }
  return (
    <div className="min-w-0">
      <ContainerRow
        depth={depth}
        expanded={expanded}
        index={entry.index}
        isLast={isLast}
        label={entry.key}
        path={path}
        togglePath={togglePath}
        value={entry.value}
      />
    </div>
  );
}

function CollapseChevron({ isOpen, onToggle, path, title }: {
  isOpen: boolean;
  onToggle: (path: string) => void;
  path: string;
  title: string;
}) {
  return (
    <button
      aria-expanded={isOpen}
      aria-label={title}
      className="mr-1 inline-flex h-4 w-4 items-center justify-center rounded border border-border/60 align-[-2px] text-muted-foreground outline-none hover:text-foreground"
      onClick={() => onToggle(path)}
      title={title}
      type="button"
    >
      <span className="text-micro leading-none">{isOpen ? "▼" : "▶"}</span>
    </button>
  );
}

function TreeKey({ index = false, label }: { index?: boolean; label: string }) {
  if (index) {
    return (
      <>
        <span className="network-muted">{label}</span>
        <span>: </span>
      </>
    );
  }
  return (
    <>
      <span className="text-[color:var(--network-accent)]">{JSON.stringify(label)}</span>
      <span>: </span>
    </>
  );
}

/** 基元值:长字符串就地截断,点击 (+N) 展开完整内容。 */
function PrimitiveValue({ value }: { value: unknown }) {
  if (value === null) return <span className="network-muted">null</span>;
  if (typeof value === "string") return <StringValue value={value} />;
  if (typeof value === "number") return <span className="text-blue-600 dark:text-blue-300">{String(value)}</span>;
  if (typeof value === "boolean") return <span className="text-purple-600 dark:text-purple-300">{String(value)}</span>;
  return <span>{String(value)}</span>;
}

function StringValue({ value }: { value: string }) {
  const serialized = JSON.stringify(value);
  const [expanded, setExpanded] = useState(false);
  if (serialized.length <= STRING_DISPLAY_LIMIT || expanded) {
    return <span className="text-emerald-600 dark:text-emerald-300">{serialized}</span>;
  }
  return (
    <span className="text-emerald-600 dark:text-emerald-300">
      {serialized.slice(0, STRING_DISPLAY_LIMIT)}
      <button
        className="network-muted ml-1 inline border-0 bg-transparent p-0 align-baseline text-caption font-semibold outline-none"
        onClick={() => setExpanded(true)}
        title="展开长文本"
        type="button"
      >
        {`… (+${serialized.length - STRING_DISPLAY_LIMIT})`}
      </button>
    </span>
  );
}

function isContainer(value: unknown): value is Container {
  return typeof value === "object" && value !== null;
}

function containerEntries(value: Container): ContainerEntry[] {
  return Array.isArray(value)
    ? value.map((item, index) => ({ key: String(index), index: true, value: item }))
    : Object.entries(value).map(([key, item]) => ({ key, index: false, value: item }));
}

function childPath(parent: string, key: string): string {
  return parent ? `${parent}\u001f${key}` : key;
}

function containerSummary(value: Container): string {
  return Array.isArray(value) ? `Array(${value.length})` : `Object(${Object.keys(value).length})`;
}
