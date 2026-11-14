import type { RequestLogBody } from "@/contracts/app";
import type { RequestLogTraceItem } from "@/contracts/app";
import { formatLogBodyView } from "./logs";
import { detectProtocol } from "./request-body-preview";

/** 响应终态(D7):正常结束 / 不完整 / 失败 / 未知。
 *
 * 「未知」是真实状态:没有 terminal 证据时不得显示为完成。 */
export type ResponseTerminal = "complete" | "incomplete" | "failed" | "unknown";

export type ResponseUsage = {
  cacheRead?: number;
  cacheWrite?: number;
  input?: number;
  output?: number;
  reasoning?: number;
  total?: number;
};

export type ResponseSummary = {
  finishReason?: string;
  /** 不完整原因(如 upstream_stream_incomplete)。 */
  incompleteReason?: string;
  protocol: string;
  terminal: ResponseTerminal;
  usage?: ResponseUsage;
};

/** 从响应正文 + 已捕获 trace 推导终态与 usage。
 *
 * usage 只来自实际捕获的数据;正文不可解析时返回空 summary(调用方显示
 * 「未捕获」而不是 0)。 */
export function responseSummary(
  body: RequestLogBody | undefined,
  traceItems: RequestLogTraceItem[] = [],
): ResponseSummary {
  const raw = body?.text ?? "";
  const parsed = raw && body?.encoding !== "base64" ? formatLogBodyView(body).json : undefined;
  const root = isRecord(parsed) ? parsed : undefined;
  const protocol = root ? detectProtocol(root) : "unknown";
  const fromBody = root ? summaryFromJson(root, protocol) : undefined;
  const fromTrace = summaryFromTrace(traceItems);
  return {
    finishReason: fromBody?.finishReason ?? fromTrace.finishReason,
    incompleteReason: fromTrace.incompleteReason ?? fromBody?.incompleteReason,
    protocol,
    terminal: fromTrace.terminal !== "unknown" ? fromTrace.terminal : (fromBody?.terminal ?? "unknown"),
    usage: fromBody?.usage ?? fromTrace.usage,
  };
}

function summaryFromJson(root: Record<string, unknown>, protocol: string): ResponseSummary {
  const usage = usageFromJson(root, protocol);
  const { finishReason, incompleteReason } = finishFromJson(root, protocol);
  let terminal: ResponseTerminal = "unknown";
  if (incompleteReason) terminal = "incomplete";
  else if (root.error !== undefined || root.type === "error" || root.type === "response.failed") {
    terminal = "failed";
  } else if (root.status === "incomplete") terminal = "incomplete";
  else if (root.status === "failed") terminal = "failed";
  else if (root.status === "completed") terminal = "complete";
  else if (finishReason) {
    // Gemini 的 finishReason 里只有 STOP/MAX_TOKENS 是正常结束;
    // SAFETY/RECITATION 等是内容被截断,必须显示为不完整而不是完成。
    const isGemini = protocol === "gemini";
    terminal = isGemini && finishReason !== "STOP" && finishReason !== "MAX_TOKENS"
      ? "incomplete"
      : "complete";
  }
  return { finishReason, incompleteReason, protocol, terminal, usage };
}

function finishFromJson(
  root: Record<string, unknown>,
  protocol: string,
): { finishReason?: string; incompleteReason?: string } {
  const choice = Array.isArray(root.choices) ? root.choices[0] : undefined;
  const finishReason =
    (isRecord(choice) ? stringField(choice.finish_reason) : undefined) ??
    stringField(root.stop_reason) ??
    (isRecord(root.delta) ? stringField(root.delta.stop_reason) : undefined);
  const candidate = Array.isArray(root.candidates) ? root.candidates[0] : undefined;
  const geminiFinish = isRecord(candidate) ? stringField(candidate.finishReason) : undefined;
  const incompleteDetails = isRecord(root.incomplete_details) ? root.incomplete_details : undefined;
  const incompleteReason = incompleteDetails ? stringField(incompleteDetails.reason) : undefined;
  return {
    finishReason: finishReason ?? geminiFinish,
    incompleteReason: incompleteReason ?? (protocol === "openai_responses" && root.status === "incomplete" ? "incomplete" : undefined),
  };
}

function usageFromJson(root: Record<string, unknown>, protocol: string): ResponseUsage | undefined {
  const raw =
    (isRecord(root.usage) ? root.usage : undefined) ??
    (isRecord(root.usageMetadata) ? root.usageMetadata : undefined) ??
    (isRecord(root.response) && isRecord(root.response.usage) ? root.response.usage : undefined);
  if (!raw) return undefined;
  const usage: ResponseUsage = {
    input: numberField(raw, ["prompt_tokens", "input_tokens", "promptTokenCount"]),
    output: numberField(raw, ["completion_tokens", "output_tokens", "candidatesTokenCount"]),
    total: numberField(raw, ["total_tokens", "totalTokenCount"]),
    reasoning: numberField(raw, ["reasoning_tokens", "thoughtsTokenCount"]) ??
      numberField(isRecord(raw.completion_tokens_details) ? raw.completion_tokens_details : {}, ["reasoning_tokens"]),
    cacheRead: numberField(raw, ["cache_read_input_tokens", "cacheReadInputTokens", "cached_tokens"]),
    cacheWrite: numberField(raw, ["cache_creation_input_tokens", "cacheCreationInputTokens"]),
  };
  const hasAny = Object.values(usage).some((value) => typeof value === "number");
  if (!hasAny) return undefined;
  void protocol;
  return usage;
}

/** 从已捕获 trace 推导终态与 usage(流式场景:正文是原始 SSE,JSON 解析
 *  不了,但 trace 里有逐事件 payload)。 */
function summaryFromTrace(items: RequestLogTraceItem[]): ResponseSummary {
  let terminal: ResponseTerminal = "unknown";
  let finishReason: string | undefined;
  let incompleteReason: string | undefined;
  let usage: ResponseUsage | undefined;
  for (const item of items) {
    const payload = item.payload;
    if (!isRecord(payload)) continue;
    const type = stringField(payload.type) ?? item.kind;
    if (type === "response.completed" || type === "message_stop") terminal = "complete";
    if (type === "response.incomplete") {
      terminal = "incomplete";
      const details = isRecord(payload.response) && isRecord(payload.response.incomplete_details)
        ? payload.response.incomplete_details
        : undefined;
      incompleteReason = (details ? stringField(details.reason) : undefined) ?? "incomplete";
    }
    if (type === "response.failed" || type === "error") terminal = "failed";
    if (type === "message_delta") {
      const delta = isRecord(payload.delta) ? payload.delta : undefined;
      finishReason = (delta ? stringField(delta.stop_reason) : undefined) ?? finishReason;
    }
    const choices = Array.isArray(payload.choices) ? payload.choices : [];
    for (const choice of choices) {
      if (!isRecord(choice)) continue;
      const reason = stringField(choice.finish_reason);
      if (reason) {
        finishReason = reason;
        terminal = "complete";
      }
    }
    const candidates = Array.isArray(payload.candidates) ? payload.candidates : [];
    for (const candidate of candidates) {
      if (!isRecord(candidate)) continue;
      const reason = stringField(candidate.finishReason);
      if (reason) {
        finishReason = reason;
        if (reason !== "STOP" && reason !== "MAX_TOKENS") terminal = "incomplete";
        else terminal = "complete";
      }
    }
    const found = usageFromJson(payload, "unknown") ??
      (isRecord(payload.response) ? usageFromJson(payload.response, "unknown") : undefined);
    if (found) usage = found;
  }
  return { finishReason, incompleteReason, protocol: "unknown", terminal, usage };
}

function numberField(source: Record<string, unknown>, keys: string[]): number | undefined {
  for (const key of keys) {
    const value = source[key];
    if (typeof value === "number" && Number.isFinite(value)) return value;
  }
  return undefined;
}

function stringField(value: unknown): string | undefined {
  return typeof value === "string" && value ? value : undefined;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
