import { detectProtocol, type WireProtocol } from "../../shared/request-body-preview";

/** 由 wire 格式推断 API 端点路径。 */
const PROTOCOL_PATHS: Record<WireProtocol, string> = {
  anthropic: "/v1/messages",
  embeddings: "/v1/embeddings",
  gemini: "/v1beta/models:generateContent",
  openai_chat: "/v1/chat/completions",
  openai_responses: "/v1/responses",
  unknown: "/v1/chat/completions",
};

const SKIP_HEADERS = new Set([
  "accept-encoding",
  "connection",
  "content-length",
  "host",
  "transfer-encoding",
]);

export type CurlSource = {
  body?: unknown;
  headers?: Record<string, string | string[]>;
  /** gemini 请求体含 model 时按 path 参数拼接(gemini 端点形如
   *  /v1beta/models/{model}:generateContent)。 */
  inferModelPath?: boolean;
  /** 覆盖目标 origin(上游重放传 host 推导值;客户端重放缺省当前页面)。 */
  origin?: string;
  protocol?: WireProtocol;
};

/** 生成可重放的 curl 命令。headers 视为已脱敏(网关落库前已滤认证头),
 *  认证字段缺失时以 $WR_API_KEY 占位,便于直接填充使用;origin 缺省时
 *  以 https://<host> 占位(上游捕获不含请求头,host 未知)。 */
export function generateCurl(source: CurlSource): string {
  const body = typeof source.body === "string" ? safeJsonParse(source.body) : source.body;
  const protocol = source.protocol ?? (isRecord(body) ? detectProtocol(body) : "unknown");
  let path = PROTOCOL_PATHS[protocol];
  if (source.inferModelPath && isRecord(body) && typeof body.model === "string" && protocol === "gemini") {
    path = `/v1beta/models/${body.model}:generateContent`;
  }
  const origin = (source.origin ?? "https://<host>").replace(/\/+$/, "");
  const parts = [`curl '${origin}${path}'`];
  let hasAuth = false;
  for (const [name, value] of Object.entries(source.headers ?? {})) {
    if (SKIP_HEADERS.has(name.toLowerCase())) continue;
    const joined = Array.isArray(value) ? value.join(", ") : value;
    if (!joined) continue;
    if (name.toLowerCase() === "authorization" || name.toLowerCase().includes("api-key")) hasAuth = true;
    parts.push(`  -H '${name}: ${escapeShell(joined)}'`);
  }
  if (!hasAuth) {
    parts.push("  -H 'Authorization: Bearer $WR_API_KEY'");
  }
  if (source.body != null) {
    const text = typeof source.body === "string" ? source.body : stringify(source.body);
    if (text) parts.push(`  -d '${escapeShell(text)}'`);
  }
  return parts.join(" \\\n");
}

/** curl 的非流式变体(去掉 stream 字段)。 */
export function stripStreamField(curl: string): string {
  return curl
    .replace(/"stream"\s*:\s*(true|false)\s*,?\s*/g, "")
    .replace(/,\s*}/g, "}")
    .replace(/{\s*,/g, "{");
}

function escapeShell(value: string): string {
  return value.replace(/'/g, "'\\''");
}

function safeJsonParse(value: string): unknown {
  try {
    return JSON.parse(value);
  } catch {
    return undefined;
  }
}

function stringify(value: unknown): string {
  try {
    return JSON.stringify(value);
  } catch {
    return String(value);
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}
