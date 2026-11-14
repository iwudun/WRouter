import type { CallLogEntry, RequestLogBody } from "@/contracts/app";

import { formatExactNumber } from "../../shared";

export type DetailTab = "request" | "response" | "attempts";
export type RequestBodyMode = "conversation" | "json";
export type ResponseBodyMode = "preview" | "json";

export type { CallLogEntry };

export function pageOrigin(): string {
  return /^https?:/.test(window.location.origin) ? window.location.origin : "";
}

export function safeJson(text?: string): unknown {
  if (!text) return undefined;
  try {
    return JSON.parse(text);
  } catch {
    return undefined;
  }
}

/** 概览卡(2026-10-17 合并原「请求概览 + Token 用量」两卡):标题行 = 状态
 *  + 转换链徽章;内容(2026-10-18 重排)= 标签在上/值在下的迷你格,路由 |
 *  性能 | Token 三组以细分隔线区隔,宽屏一行放下,窄域 flex-wrap。 */
export function renderCaptured(value: unknown): string {
  if (typeof value === "string") return value;
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

export function exactOrDash(value: number | undefined, locale: Intl.LocalesArgument): string {
  return value == null ? "-" : formatExactNumber(value, locale);
}

export function toRequestLogBody(text?: string, sizeBytes?: number, truncated?: boolean, contentTypeValue?: string): RequestLogBody | undefined {
  if (typeof text !== "string" || !text) return undefined;
  return { contentType: contentTypeValue, encoding: "utf8", sizeBytes: sizeBytes ?? text.length, text, truncated: Boolean(truncated) };
}

export function contentType(headers?: Record<string, string | string[]>): string | undefined {
  const entry = Object.entries(headers ?? {}).find(([name]) => name.toLowerCase() === "content-type")?.[1];
  return Array.isArray(entry) ? entry[0] : entry;
}
