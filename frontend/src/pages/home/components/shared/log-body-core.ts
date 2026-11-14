// F4.1/F4.2 — log-json-panel 正文解析的纯逻辑层。
//
// 该模块同时被 Worker(live 形态)与主线程同步回退(jsdom/无 Worker 环境)使用,
// 不允许导入 React 或触碰 DOM。主线程只接收"虚拟化后的节点树"(按路径懒取,
// 每批 ≤ 80 条),5MB 级 payload 的 JSON.parse/聚合/pretty stringify/行过滤全部
// 留在调用方之外(Worker)执行 —— G7 验收基线。
import type { RequestLogBody } from "@/contracts/app";

import {
  formatLogBodyView,
  isJsonContainer,
  jsonChildPath
} from "../../shared/logs";

export const logBodyChildEntryLimit = 80;
const logBodyLongStringLimit = 240;
const logBodyFullStringCap = 1024 * 1024;
const logBodyFilterLineCap = 2000;
const logBodyAutoExpandTextLimit = 160 * 1024;
// 2026-10-18 详情页 JSON 层级默认展开:阈值
// 放宽到 400 条目;超过仍回落根级展开,超大 payload 的手动「全部展开」另受
// expandAll 容器上限(2000)保护。
const logBodyAutoExpandEntryLimit = 400;
const logBodyExpandAllContainerCap = 2000;

export type LogBodyPrimitiveKind = "string" | "number" | "boolean" | "null" | "other";

export type LogBodyPrimitiveInfo = {
  display: string;
  kind: LogBodyPrimitiveKind;
  /** display 相对完整序列化串被截断(仅 string);完整串可经 stringAt 取回 */
  truncated: boolean;
  /** string:完整 JSON.stringify 后的长度(供 "(+N)" 标注);其余 0 */
  fullLength: number;
};

export type LogBodyContainerInfo = {
  array: boolean;
  count: number;
  /** 折叠态预览文本,与旧实现同形:Array(n) / Object(n) / Object(80+) */
  summary: string;
};

export type LogBodyValueDescriptor = {
  container: LogBodyContainerInfo | null;
  primitive: LogBodyPrimitiveInfo | null;
};

export type LogBodyNodeEntry = {
  path: string;
  key: string;
  kind: "index" | "key";
} & LogBodyValueDescriptor;

export type LogBodyChildrenBatch = {
  entries: LogBodyNodeEntry[];
  total: number;
  hasMore: boolean;
};

export type LogBodyViewMeta = {
  text: string;
  /** json 解析成功时为根节点描述(容器或原始值);非 JSON 正文为 null */
  root: LogBodyValueDescriptor | null;
  /** 小文档自动展开根节点(镜像旧 createInitialVisibleJsonPaths 语义) */
  initialExpanded: boolean;
};

export type LogBodyExpandAllResult = {
  containerPaths: string[];
  batches: Record<string, LogBodyChildrenBatch>;
};

export type LogBodyStringResult = {
  display: string;
  truncated: boolean;
};

export type LogBodyFilterResult = {
  lines: string[];
  matched: boolean;
  totalMatched: number;
  capped: boolean;
};

export type LogBodyView = LogBodyViewMeta & { json?: unknown };

export function createLogBodyView(body: RequestLogBody | undefined): LogBodyView {
  const base = formatLogBodyView(body);
  return {
    json: base.json,
    text: base.text,
    root: base.json !== undefined ? describeValue(base.json) : null,
    initialExpanded: computeInitialExpanded(base)
  };
}

function computeInitialExpanded(base: { json?: unknown; text: string }): boolean {
  if (!isJsonContainer(base.json)) {
    return false;
  }
  if (base.text.length > logBodyAutoExpandTextLimit) {
    return false;
  }
  return countAllEntries(base.json) <= logBodyAutoExpandEntryLimit;
}

export function describeValue(value: unknown): LogBodyValueDescriptor {
  if (isJsonContainer(value)) {
    const array = Array.isArray(value);
    const count = countAllEntries(value);
    const summary = array
      ? `Array(${value.length})`
      : count > logBodyChildEntryLimit
        ? `Object(${logBodyChildEntryLimit}+)`
        : `Object(${count})`;
    return { container: { array, count, summary }, primitive: null };
  }
  return { container: null, primitive: describePrimitive(value) };
}

function describePrimitive(value: unknown): LogBodyPrimitiveInfo {
  if (typeof value === "string") {
    const serialized = JSON.stringify(value);
    const truncated = serialized.length > logBodyLongStringLimit;
    return {
      display: truncated ? serialized.slice(0, logBodyLongStringLimit) : serialized,
      kind: "string",
      truncated,
      fullLength: serialized.length
    };
  }
  if (typeof value === "number") {
    return { display: String(value), kind: "number", truncated: false, fullLength: 0 };
  }
  if (typeof value === "boolean") {
    return { display: String(value), kind: "boolean", truncated: false, fullLength: 0 };
  }
  if (value === null) {
    return { display: "null", kind: "null", truncated: false, fullLength: 0 };
  }
  return { display: String(value), kind: "other", truncated: false, fullLength: 0 };
}

function countAllEntries(value: Record<string, unknown> | unknown[]): number {
  if (Array.isArray(value)) {
    return value.length;
  }
  let count = 0;
  for (const key in value) {
    if (Object.prototype.hasOwnProperty.call(value, key)) {
      count += 1;
    }
  }
  return count;
}

function containerEntries(
  value: Record<string, unknown> | unknown[],
  limit: number
): Array<[string, unknown]> {
  if (Array.isArray(value)) {
    const count = Math.min(value.length, limit);
    const entries: Array<[string, unknown]> = [];
    for (let index = 0; index < count; index += 1) {
      entries.push([String(index), value[index]]);
    }
    return entries;
  }
  const entries: Array<[string, unknown]> = [];
  for (const key in value) {
    if (!Object.prototype.hasOwnProperty.call(value, key)) {
      continue;
    }
    entries.push([key, value[key]]);
    if (entries.length >= limit) {
      break;
    }
  }
  return entries;
}

function resolveJsonPath(root: unknown, path: string): unknown {
  let current = root;
  for (const rawSegment of path.split("/").slice(1)) {
    if (!isJsonContainer(current)) {
      return undefined;
    }
    const segment = decodeURIComponent(rawSegment);
    current = Array.isArray(current)
      ? current[Number(segment)]
      : (current as Record<string, unknown>)[segment];
  }
  return current;
}

export function childrenBatchAt(json: unknown, path: string, limit = logBodyChildEntryLimit): LogBodyChildrenBatch {
  const value = resolveJsonPath(json, path);
  if (!isJsonContainer(value)) {
    return { entries: [], total: 0, hasMore: false };
  }
  const total = countAllEntries(value);
  const entries = containerEntries(value, limit).map(([key, child]) => ({
    path: jsonChildPath(path, key),
    key,
    kind: Array.isArray(value) ? ("index" as const) : ("key" as const),
    ...describeValue(child)
  }));
  return { entries, total, hasMore: total > entries.length };
}

export function expandAllPaths(
  json: unknown,
  limit = logBodyChildEntryLimit,
  containerCap = logBodyExpandAllContainerCap
): LogBodyExpandAllResult {
  const containerPaths: string[] = [];
  const batches: Record<string, LogBodyChildrenBatch> = {};
  if (!isJsonContainer(json)) {
    return { containerPaths, batches };
  }
  const stack: Array<{ path: string; value: unknown }> = [{ path: "$", value: json }];
  while (stack.length > 0 && containerPaths.length < containerCap) {
    const { path, value: current } = stack.pop()!;
    if (!isJsonContainer(current)) {
      continue;
    }
    containerPaths.push(path);
    const batch = childrenBatchAt(json, path, limit);
    batches[path] = batch;
    for (const entry of batch.entries) {
      if (containerPaths.length >= containerCap) {
        break;
      }
      if (entry.container) {
        stack.push({ path: entry.path, value: resolveJsonPath(json, entry.path) });
      }
    }
  }
  return { containerPaths, batches };
}

export function stringAt(json: unknown, path: string): LogBodyStringResult {
  const value = resolveJsonPath(json, path);
  if (typeof value !== "string") {
    return { display: describePrimitive(value).display, truncated: false };
  }
  const serialized = JSON.stringify(value);
  if (serialized.length > logBodyFullStringCap) {
    return {
      display: `${serialized.slice(0, logBodyFullStringCap)}…`,
      truncated: true
    };
  }
  return { display: serialized, truncated: false };
}

export function filterLogBodyLines(
  text: string,
  query: string,
  cache?: { lines: string[] | null; lowerLines: string[] | null }
): LogBodyFilterResult {
  const normalized = query.trim().toLowerCase();
  let lines: string[];
  let lowerLines: string[];
  if (cache?.lines) {
    lines = cache.lines;
    lowerLines = cache.lowerLines ?? (cache.lowerLines = lines.map((line) => line.toLowerCase()));
  } else {
    lines = text.split(/\r?\n/);
    lowerLines = normalized ? lines.map((line) => line.toLowerCase()) : [];
    if (cache) {
      cache.lines = lines;
      cache.lowerLines = lowerLines;
    }
  }
  if (!normalized) {
    return {
      lines: lines.slice(0, logBodyFilterLineCap),
      matched: true,
      totalMatched: lines.length,
      capped: lines.length > logBodyFilterLineCap
    };
  }
  const matched: string[] = [];
  for (let index = 0; index < lowerLines.length; index += 1) {
    if (lowerLines[index].includes(normalized)) {
      matched.push(lines[index]);
      if (matched.length >= logBodyFilterLineCap) {
        break;
      }
    }
  }
  const hasMore = matched.length >= logBodyFilterLineCap;
  return { lines: matched, matched: matched.length > 0, totalMatched: matched.length, capped: hasMore };
}

/**
 * Worker 与同步回退共用的按 key 缓存(LRU)。持有解析后的 json、格式化文本与
 * 懒生成的行数组;主线程形态只在同步回退里实例化(live 形态整体在 Worker 内)。
 */
type LogBodyCacheItem = {
  view: LogBodyView;
  filterCache: { lines: string[] | null; lowerLines: string[] | null };
};

export class LogBodyCache {
  private items = new Map<string, LogBodyCacheItem>();

  constructor(private readonly limit: number) {}

  format(key: string, body: RequestLogBody | null | undefined): LogBodyView {
    const existing = this.items.get(key);
    if (existing) {
      this.items.delete(key);
      this.items.set(key, existing);
      return existing.view;
    }
    const item: LogBodyCacheItem = {
      view: createLogBodyView(body ?? undefined),
      filterCache: { lines: null, lowerLines: null }
    };
    this.items.set(key, item);
    while (this.items.size > this.limit) {
      const oldest = this.items.keys().next().value;
      if (oldest === undefined) {
        break;
      }
      this.items.delete(oldest);
    }
    return item.view;
  }

  children(key: string, path: string): LogBodyChildrenBatch {
    const item = this.items.get(key);
    if (!item) {
      return { entries: [], total: 0, hasMore: false };
    }
    return childrenBatchAt(item.view.json, path);
  }

  expandAll(key: string): LogBodyExpandAllResult {
    const item = this.items.get(key);
    if (!item) {
      return { containerPaths: [], batches: {} };
    }
    return expandAllPaths(item.view.json);
  }

  stringAt(key: string, path: string): LogBodyStringResult {
    const item = this.items.get(key);
    if (!item) {
      return { display: "", truncated: false };
    }
    return stringAt(item.view.json, path);
  }

  filter(key: string, query: string): LogBodyFilterResult {
    const item = this.items.get(key);
    if (!item) {
      return { lines: [], matched: false, totalMatched: 0, capped: false };
    }
    return filterLogBodyLines(item.view.text, query, item.filterCache);
  }

  forget(key: string): void {
    this.items.delete(key);
  }
}
