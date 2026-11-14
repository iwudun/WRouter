import type { RequestLogBody } from "@/contracts/app";

import { parseLogJson, parsePartialJson } from "./partial-json";
import { aggregateLogStreamPayloads, aggregateParsedStreamJson } from "./stream-aggregate";

export function formatLogDateTime(value: string): string {
  const date = new Date(value);
  if (!Number.isFinite(date.getTime())) {
    return "";
  }
  const year = date.getFullYear();
  const month = String(date.getMonth() + 1).padStart(2, "0");
  const day = String(date.getDate()).padStart(2, "0");
  const hours = String(date.getHours()).padStart(2, "0");
  const minutes = String(date.getMinutes()).padStart(2, "0");
  const seconds = String(date.getSeconds()).padStart(2, "0");
  return `${year}-${month}-${day} ${hours}:${minutes}:${seconds}`;
}

export type FormattedLogBody = {
  json?: unknown;
  text: string;
};

export function formatLogBodyView(body: RequestLogBody | undefined): FormattedLogBody {
  if (!body || (!body.text && body.sizeBytes === 0)) {
    return { text: "No body" };
  }
  if (body.encoding === "base64") {
    return { text: body.text || "No body" };
  }

  const text = body.text || "";
  const json = parseLogJson(text);
  if (json !== undefined) {
    const normalizedJson = aggregateParsedStreamJson(json);
    return { json: normalizedJson, text: JSON.stringify(normalizedJson, null, 2) };
  }

  const streamPayloads = parseLogStreamPayloads(text);
  if (streamPayloads.length > 0) {
    const streamedJson = aggregateLogStreamPayloads(streamPayloads);
    return { json: streamedJson, text: JSON.stringify(streamedJson, null, 2) };
  }

  // 捕获截断的 JSON(超 512KB 捕获帽)整体 parse 必败;此前直接回落纯文本,
  // 详情页正文退化为一大坨无结构字符串。这里做「部分树恢复」:丢弃截断
  // 处未完成的值,按已收集条目闭合容器 —— 正文重新可结构化浏览。
  const partialJson = parsePartialJson(text);
  if (partialJson !== undefined) {
    return { json: partialJson, text: JSON.stringify(partialJson, null, 2) };
  }

  return { text: text || "No body" };
}

export function parseLogStreamPayloads(value: string): unknown[] {
  const payloads: unknown[] = [];
  for (const rawLine of value.split(/\r?\n/)) {
    const line = rawLine.trim();
    const payload = line.startsWith("data:")
      ? line.slice(5).trim()
      : line.startsWith("{") || line.startsWith("[")
        ? line
        : "";
    if (!payload || payload === "[DONE]") {
      continue;
    }
    const parsed = parseLogJson(payload);
    if (parsed !== undefined) {
      payloads.push(parsed);
    }
  }
  if (payloads.length === 0) {
    // 2026-10-18:部分上游把 SSE 事件连排在同一行(事件间无换行),按行
    // 解析会整行 JSON.parse 失败而全军覆没 —— 回落括号平衡扫描,与换行无关。
    return extractInlineStreamPayloads(value);
  }
  return payloads;
}

/** 单行连排 SSE 的兜底扫描上限(捕获正文本身有截断帽,此处再兜底)。 */
const STREAM_PAYLOAD_SCAN_CAP = 10000;

function extractInlineStreamPayloads(value: string): unknown[] {
  if (!value.includes("data:")) {
    return [];
  }
  const payloads: unknown[] = [];
  let from = value.indexOf("data:");
  while (from !== -1 && payloads.length < STREAM_PAYLOAD_SCAN_CAP) {
    let cursor = from + 5;
    while (cursor < value.length && /\s/.test(value[cursor])) {
      cursor++;
    }
    if (value.startsWith("[DONE]", cursor)) {
      from = value.indexOf("data:", cursor);
      continue;
    }
    const ch = value[cursor];
    if (ch !== "{" && ch !== "[") {
      from = value.indexOf("data:", cursor);
      continue;
    }
    const end = scanBalancedJson(value, cursor);
    if (end === -1) {
      break;
    }
    const parsed = parseLogJson(value.slice(cursor, end));
    if (parsed !== undefined) {
      payloads.push(parsed);
    }
    from = value.indexOf("data:", end);
  }
  return payloads;
}

/** 从 start 起做括号配平(字符串/转义感知),返回闭合括号之后的下标;
 *  不配平返回 -1。 */
function scanBalancedJson(value: string, start: number): number {
  let depth = 0;
  let inString = false;
  let escaped = false;
  for (let i = start; i < value.length; i++) {
    const ch = value[i];
    if (inString) {
      if (escaped) {
        escaped = false;
      } else if (ch === "\\") {
        escaped = true;
      } else if (ch === '"') {
        inString = false;
      }
      continue;
    }
    if (ch === '"') {
      inString = true;
    } else if (ch === "{" || ch === "[") {
      depth++;
    } else if (ch === "}" || ch === "]") {
      depth--;
      if (depth === 0) {
        return i + 1;
      }
    }
  }
  return -1;
}
