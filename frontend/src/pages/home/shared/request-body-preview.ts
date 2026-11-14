import type { RequestLogBody } from "@/contracts/app";
import { formatLogBodyView } from "./logs";

/** 详情页展示适配器的协议判定结果。
 *
 * 这是**容错读取器**,不是权威协议实现:网关侧 decoder 才是转换正确性
 * 的依据(方案 §4.1)。识别失败一律回退 raw,不做猜测性归一。 */
export type WireProtocol =
  | "openai_chat"
  | "openai_responses"
  | "anthropic"
  | "gemini"
  | "embeddings"
  | "unknown";

export type ParsedToolCall = {
  arguments: string;
  id?: string;
  name: string;
};

export type ParsedToolResult = {
  /** 关联的调用 id(协议缺失时为 undefined,由 name 关联) */
  callId?: string;
  content: string;
  isError: boolean;
  name?: string;
};

export type ConversationMessage = {
  content: string;
  index: number;
  /** 协议本地标识(Responses item id;其它协议无) */
  itemId?: string;
  /** 原始 content 数组(多模态 part 识别用;纯字符串消息无) */
  contentParts?: unknown[];
  /** 思考文本(anthropic thinking / gemini thought / openai reasoning_content) */
  reasoning?: string;
  /** tool 消息关联的调用 id */
  toolCallId?: string;
  /** 该消息里的工具调用(assistant 消息) */
  toolCalls?: ParsedToolCall[];
  role: string;
  /** 该消息里的工具结果(anthropic tool_result / gemini functionResponse) */
  toolResults?: ParsedToolResult[];
  raw: unknown;
};

/** 请求里声明的工具定义(tools 字段)。 */
export type PreviewTool = {
  description: string;
  name: string;
  parameters: unknown;
};

/** 会话级请求参数(统计格展示)。 */
export type PreviewParams = {
  maxTokens?: number;
  model?: string;
  stream?: boolean;
  toolChoice?: unknown;
};

export type ResponsePreviewSection = {
  kind: "content" | "reasoning" | "tool" | "refusal";
  name?: string;
  text: string;
};

export type RequestBodyPreview = {
  format: "conversation" | "response" | "raw";
  messages?: ConversationMessage[];
  params?: PreviewParams;
  parsed: boolean;
  protocol: WireProtocol;
  sections?: ResponsePreviewSection[];
  tools?: PreviewTool[];
  text: string;
};

/** 请求正文预览(按协议分支解析会话结构)。 */
export function requestBodyPreview(body: RequestLogBody | undefined): RequestBodyPreview {
  const raw = body?.text ?? "";
  if (!isPreviewable(body, raw)) return rawPreview(raw, "unknown");
  const parsed = formatLogBodyView(body).json;
  if (!isRecord(parsed)) return rawPreview(raw, "unknown");
  const protocol = detectProtocol(parsed);
  if (protocol === "embeddings") return rawPreview(raw, protocol);
  const messages = extractMessages(parsed, protocol);
  if (!messages?.length) return rawPreview(raw, protocol);
  return {
    format: "conversation",
    messages,
    params: previewParams(parsed),
    parsed: true,
    protocol,
    text: messages.map(({ role, content }) => `${role}\n${content}`).join("\n\n"),
    tools: previewTools(parsed, protocol),
  };
}

/** 响应正文预览(按协议分支解析内容/reasoning/工具)。 */
export function responseBodyPreview(body: RequestLogBody | undefined): RequestBodyPreview {
  const raw = body?.text ?? "";
  if (!isPreviewable(body, raw)) return rawPreview(raw, "unknown");
  const parsed = formatLogBodyView(body).json;
  if (!isRecord(parsed)) return rawPreview(raw, "unknown");
  const protocol = detectProtocol(parsed);
  const sections = extractResponseSections(parsed, protocol);
  return sections.length
    ? {
        format: "response",
        parsed: true,
        protocol,
        sections,
        text: sections.map((section) => section.text).filter(Boolean).join("\n\n"),
      }
    : rawPreview(raw, protocol);
}

function rawPreview(raw: string, protocol: WireProtocol): RequestBodyPreview {
  return { format: "raw", parsed: false, protocol, text: raw };
}

/** 协议判定:按判别字段而不是「像不像」—— 每个分支都要有独占特征。
 *  无法判定返回 unknown,由调用方回退 raw。 */
export function detectProtocol(root: Record<string, unknown>): WireProtocol {
  // Gemini:contents / candidates / systemInstruction / usageMetadata。
  if (Array.isArray(root.contents) || Array.isArray(root.candidates)) return "gemini";
  if (isRecord(root.systemInstruction) || isRecord(root.usageMetadata)) return "gemini";
  // Responses:object:"response" / 顶层 type:response.* / instructions /
  // input 为 Responses item 数组(带 function_call 等 type)。
  if (root.object === "response") return "openai_responses";
  if (typeof root.type === "string" && root.type.startsWith("response.")) return "openai_responses";
  if (root.instructions !== undefined) return "openai_responses";
  if (Array.isArray(root.output) && (root.input !== undefined || root.output.length > 0)) {
    return "openai_responses";
  }
  if (Array.isArray(root.input) && root.input.some((item) => isResponsesItem(item))) {
    return "openai_responses";
  }
  // Anthropic:顶层 type:message + content 数组,或 messages + 字符串 system,
  // 或消息内容里出现 anthropic 独占 block 类型,或响应体只有 content 数组
  // (无 choices/candidates —— 那些已被上面的分支排掉)。
  if (root.type === "message" && Array.isArray(root.content)) return "anthropic";
  if (Array.isArray(root.messages) && typeof root.system === "string") return "anthropic";
  if (Array.isArray(root.messages)) {
    const first = root.messages[0];
    if (isRecord(first) && Array.isArray(first.content)) {
      const block = first.content.find(isRecord);
      if (block && typeof block.type === "string" && ANTHROPIC_BLOCK_TYPES.has(block.type)) {
        return "anthropic";
      }
    }
    return "openai_chat";
  }
  if (Array.isArray(root.content) && root.content.some(isRecord)) {
    const block = root.content.find(isRecord)!;
    if (typeof block.type === "string" && ANTHROPIC_BLOCK_TYPES.has(block.type)) return "anthropic";
  }
  // Embeddings:input 存在且没有对话结构。
  if (root.input !== undefined && !Array.isArray(root.output)) return "embeddings";
  if (Array.isArray(root.data) && isRecord(root.data[0]) && "embedding" in root.data[0]) {
    return "embeddings";
  }
  return "unknown";
}

const ANTHROPIC_BLOCK_TYPES = new Set([
  "text",
  "image",
  "tool_use",
  "tool_result",
  "thinking",
  "redacted_thinking",
  "document",
]);

/** Responses item 类型(用于把 `input` 数组与普通 chat messages 区分开)。 */
const RESPONSES_ITEM_TYPES = new Set([
  "function_call",
  "function_call_output",
  "reasoning",
  "message",
  "item_reference",
  "computer_call",
  "computer_call_output",
  "web_search_call",
  "file_search_call",
]);

function isResponsesItem(value: unknown): boolean {
  if (!isRecord(value)) return false;
  const type = stringField(value.type);
  return type !== undefined && RESPONSES_ITEM_TYPES.has(type);
}

function extractMessages(
  root: Record<string, unknown>,
  protocol: WireProtocol,
): ConversationMessage[] | undefined {
  switch (protocol) {
    case "openai_chat":
      return chatMessages(root.messages);
    case "anthropic":
      return anthropicMessages(root);
    case "gemini":
      return geminiMessages(root);
    case "openai_responses":
      return responsesMessages(root);
    default:
      // unknown:仍尝试 messages(容错),但不声称具体协议。
      return Array.isArray(root.messages) ? chatMessages(root.messages) : undefined;
  }
}

function chatMessages(values: unknown): ConversationMessage[] {
  if (!Array.isArray(values)) return [];
  return values.flatMap((value, index) => {
    if (!isRecord(value)) return [];
    const role = stringField(value.role) ?? stringField(value.type) ?? `message ${index + 1}`;
    const toolCalls = parseChatToolCalls(value.tool_calls);
    const content = value.content ?? value.parts ?? value.text ?? value.input ?? "";
    const message: ConversationMessage = {
      content: contentText(content),
      contentParts: Array.isArray(content) ? content : undefined,
      index,
      raw: value,
      reasoning: stringField(value.reasoning_content) ?? stringField(value.reasoning),
      role,
    };
    if (toolCalls.length) message.toolCalls = toolCalls;
    const toolCallId = stringField(value.tool_call_id);
    if (toolCallId) message.toolCallId = toolCallId;
    const name = stringField(value.name);
    if (name && role === "tool") {
      message.toolResults = [{ callId: toolCallId, content: message.content, isError: false, name }];
    }
    return [message];
  });
}

function parseChatToolCalls(value: unknown): ParsedToolCall[] {
  if (!Array.isArray(value)) return [];
  return value.flatMap((item) => {
    if (!isRecord(item)) return [];
    const fn = isRecord(item.function) ? item.function : item;
    const name = stringField(fn.name);
    if (!name) return [];
    return [
      {
        arguments: stringField(fn.arguments) ?? "",
        id: stringField(item.id),
        name,
      },
    ];
  });
}

function anthropicMessages(root: Record<string, unknown>): ConversationMessage[] {
  const messages: ConversationMessage[] = [];
  const system = root.system;
  if (typeof system === "string" && system) {
    messages.push({ content: system, index: 0, raw: { role: "system", content: system }, role: "system" });
  } else if (Array.isArray(system)) {
    messages.push({
      content: contentText(system),
      index: 0,
      raw: { role: "system", content: system },
      role: "system",
    });
  }
  const offset = messages.length;
  const values = Array.isArray(root.messages) ? root.messages : [];
  values.forEach((value, index) => {
    if (!isRecord(value)) return;
    const role = stringField(value.role) ?? `message ${index + 1}`;
    const blocks = Array.isArray(value.content) ? value.content.filter(isRecord) : [];
    const message: ConversationMessage = {
      content: contentText(value.content),
      contentParts: Array.isArray(value.content) ? value.content : undefined,
      index: offset + index,
      raw: value,
      reasoning: anthropicReasoning(blocks),
      role,
    };
    const calls = blocks
      .filter((block) => block.type === "tool_use")
      .flatMap((block) => {
        const name = stringField(block.name);
        if (!name) return [];
        return [{ arguments: contentText(block.input ?? {}), id: stringField(block.id), name }];
      });
    if (calls.length) message.toolCalls = calls;
    const results = blocks
      .filter((block) => block.type === "tool_result")
      .map((block) => ({
        callId: stringField(block.tool_use_id),
        content: contentText(block.content ?? ""),
        isError: block.is_error === true,
      }));
    if (results.length) message.toolResults = results;
    messages.push(message);
  });
  return messages;
}

/** anthropic 消息内 thinking / redacted_thinking 块聚合为思考文本。 */
function anthropicReasoning(blocks: Record<string, unknown>[]): string | undefined {
  const parts = blocks.flatMap((block) => {
    if (block.type === "thinking") return [contentText(block.thinking ?? "")];
    if (block.type === "redacted_thinking") return ["[redacted thinking]"];
    return [];
  });
  const joined = parts.filter(Boolean).join("\n\n");
  return joined || undefined;
}

function geminiMessages(root: Record<string, unknown>): ConversationMessage[] {
  const messages: ConversationMessage[] = [];
  const system = root.systemInstruction;
  if (isRecord(system)) {
    messages.push({
      content: contentText(system.parts ?? ""),
      index: 0,
      raw: { role: "system", ...system },
      role: "system",
    });
  }
  const offset = messages.length;
  const contents = Array.isArray(root.contents) ? root.contents : [];
  contents.forEach((value, index) => {
    if (!isRecord(value)) return;
    const rawRole = stringField(value.role);
    const role = rawRole === "model" ? "assistant" : (rawRole ?? `message ${index + 1}`);
    const parts = Array.isArray(value.parts) ? value.parts.filter(isRecord) : [];
    const message: ConversationMessage = {
      content: contentText(value.parts),
      contentParts: Array.isArray(value.parts) ? value.parts : undefined,
      index: offset + index,
      raw: value,
      reasoning: geminiReasoning(parts),
      role,
    };
    const calls = parts
      .filter((part) => isRecord(part.functionCall))
      .flatMap((part) => {
        const call = part.functionCall as Record<string, unknown>;
        const name = stringField(call.name);
        if (!name) return [];
        return [{ arguments: contentText(call.args ?? {}), id: stringField(call.id), name }];
      });
    if (calls.length) message.toolCalls = calls;
    const results = parts
      .filter((part) => isRecord(part.functionResponse))
      .map((part) => {
        const response = part.functionResponse as Record<string, unknown>;
        return {
          callId: stringField(response.id),
          content: contentText(response.response ?? {}),
          isError: false,
          name: stringField(response.name),
        };
      });
    if (results.length) message.toolResults = results;
    messages.push(message);
  });
  return messages;
}

/** gemini parts 中 thought:true 的文本聚合为思考文本。 */
function geminiReasoning(parts: Record<string, unknown>[]): string | undefined {
  const joined = parts
    .filter((part) => part.thought === true && typeof part.text === "string")
    .map((part) => part.text as string)
    .filter(Boolean)
    .join("\n\n");
  return joined || undefined;
}

function responsesMessages(root: Record<string, unknown>): ConversationMessage[] {
  const messages: ConversationMessage[] = [];
  const instructions = root.instructions;
  if (typeof instructions === "string" && instructions) {
    messages.push({
      content: instructions,
      index: 0,
      raw: { role: "system", content: instructions },
      role: "system",
    });
  }
  const input = root.input;
  const items: unknown[] = Array.isArray(input)
    ? input
    : typeof input === "string"
      ? [{ role: "user", content: input }]
      : [];
  const offset = messages.length;
  items.forEach((value, index) => {
    if (typeof value === "string") {
      messages.push({ content: value, index: offset + index, raw: value, role: "user" });
      return;
    }
    if (!isRecord(value)) return;
    const type = stringField(value.type);
    if (type === "function_call") {
      const name = stringField(value.name) ?? "tool";
      const args = stringField(value.arguments) ?? "";
      messages.push({
        content: args,
        index: offset + index,
        itemId: stringField(value.id),
        raw: value,
        role: "assistant",
        toolCalls: [{ arguments: args, id: stringField(value.call_id) ?? stringField(value.id), name }],
      });
      return;
    }
    if (type === "function_call_output") {
      const output = contentText(value.output ?? "");
      messages.push({
        content: output,
        index: offset + index,
        itemId: stringField(value.id),
        raw: value,
        role: "tool",
        toolCallId: stringField(value.call_id),
        toolResults: [{ callId: stringField(value.call_id), content: output, isError: false }],
      });
      return;
    }
    const role = stringField(value.role) ?? (type === "reasoning" ? "reasoning" : `message ${index + 1}`);
    const content = value.content ?? value.text ?? value.summary ?? "";
    messages.push({
      content: contentText(content),
      contentParts: Array.isArray(content) ? content : undefined,
      index: offset + index,
      itemId: stringField(value.id),
      raw: value,
      role,
    });
  });
  return messages;
}

function extractResponseSections(
  root: Record<string, unknown>,
  protocol: WireProtocol,
): ResponsePreviewSection[] {
  switch (protocol) {
    case "anthropic":
      return anthropicResponseSections(root);
    case "gemini":
      return geminiResponseSections(root);
    case "openai_responses":
      return responsesResponseSections(root);
    default:
      return chatResponseSections(root);
  }
}

function chatResponseSections(root: Record<string, unknown>): ResponsePreviewSection[] {
  const sections: ResponsePreviewSection[] = [];
  const choices = Array.isArray(root.choices) ? root.choices : [];
  const first = isRecord(choices[0]) ? choices[0] : undefined;
  const message = isRecord(first?.message) ? first.message : undefined;
  const delta = isRecord(first?.delta) ? first.delta : undefined;
  const reasoning =
    message?.reasoning_content ??
    message?.reasoning ??
    delta?.reasoning_content ??
    delta?.reasoning ??
    root.reasoning_content ??
    root.thinking;
  if (reasoning !== undefined) sections.push({ kind: "reasoning", text: contentText(reasoning) });
  const content = message?.content ?? delta?.content ?? root.output_text;
  if (content !== undefined && content !== null && content !== "") {
    sections.push({ kind: "content", text: contentText(content) });
  }
  const refusal = message?.refusal ?? delta?.refusal;
  if (typeof refusal === "string" && refusal) sections.push({ kind: "refusal", text: refusal });
  const calls = message?.tool_calls ?? delta?.tool_calls ?? root.tool_calls;
  if (Array.isArray(calls)) {
    calls.forEach((call, index) => {
      if (!isRecord(call)) return;
      const fn = isRecord(call.function) ? call.function : call;
      sections.push({
        kind: "tool",
        name: stringField(fn.name) ?? `Tool ${index + 1}`,
        text: contentText(fn.arguments ?? fn.input ?? ""),
      });
    });
  }
  return sections;
}

function anthropicResponseSections(root: Record<string, unknown>): ResponsePreviewSection[] {
  const sections: ResponsePreviewSection[] = [];
  const content = Array.isArray(root.content) ? root.content.filter(isRecord) : [];
  for (const block of content) {
    switch (block.type) {
      case "thinking":
        sections.push({ kind: "reasoning", text: contentText(block.thinking ?? "") });
        break;
      case "redacted_thinking":
        sections.push({ kind: "reasoning", text: "[redacted thinking]" });
        break;
      case "text":
        sections.push({ kind: "content", text: contentText(block.text ?? "") });
        break;
      case "tool_use":
        sections.push({
          kind: "tool",
          name: stringField(block.name) ?? "tool_use",
          text: contentText(block.input ?? {}),
        });
        break;
      case "tool_result":
        sections.push({
          kind: "tool",
          name: stringField(block.tool_use_id) ?? "tool_result",
          text: contentText(block.content ?? ""),
        });
        break;
      default:
        break;
    }
  }
  return sections;
}

function geminiResponseSections(root: Record<string, unknown>): ResponsePreviewSection[] {
  const sections: ResponsePreviewSection[] = [];
  const candidates = Array.isArray(root.candidates) ? root.candidates : [];
  const first = isRecord(candidates[0]) ? candidates[0] : undefined;
  const content = isRecord(first?.content) ? first.content : undefined;
  const parts = Array.isArray(content?.parts) ? content.parts.filter(isRecord) : [];
  for (const part of parts) {
    if (part.thought === true && typeof part.text === "string") {
      sections.push({ kind: "reasoning", text: part.text });
    } else if (typeof part.text === "string") {
      sections.push({ kind: "content", text: part.text });
    }
    if (isRecord(part.functionCall)) {
      const call = part.functionCall as Record<string, unknown>;
      sections.push({
        kind: "tool",
        name: stringField(call.name) ?? "functionCall",
        text: contentText(call.args ?? {}),
      });
    }
  }
  return sections;
}

function responsesResponseSections(root: Record<string, unknown>): ResponsePreviewSection[] {
  const sections: ResponsePreviewSection[] = [];
  const output = Array.isArray(root.output) ? root.output.filter(isRecord) : [];
  for (const item of output) {
    const type = stringField(item.type);
    if (type === "reasoning") {
      sections.push({ kind: "reasoning", text: contentText(item.summary ?? item.content ?? "") });
    } else if (type === "function_call") {
      sections.push({
        kind: "tool",
        name: stringField(item.name) ?? "function_call",
        text: stringField(item.arguments) ?? "",
      });
    } else if (type === "message") {
      const parts = Array.isArray(item.content) ? item.content.filter(isRecord) : [];
      for (const part of parts) {
        if (part.type === "output_text") {
          sections.push({ kind: "content", text: contentText(part.text ?? "") });
        } else if (part.type === "refusal") {
          sections.push({ kind: "refusal", text: contentText(part.refusal ?? part.text ?? "") });
        }
      }
    }
  }
  // 流式增量形态(delta 直接挂在根上)。
  if (!sections.length && typeof root.output_text === "string" && root.output_text) {
    sections.push({ kind: "content", text: root.output_text });
  }
  return sections;
}

/** 预览可用性守卫:正文非空、非 base64、长度在帽内。帽取捕获帽(512KB)
 *  的 4 倍冗余 —— SSE 聚合/部分树恢复都在毫秒级,大正文不再一刀切回退
 *  原文(2026-10-25:此前 128KB 帽把典型 ZCode 会话正文(数百 KB)整个
 *  拒掉,详情页预览按钮消失)。截断正文放行:尽力展示已捕获部分。 */
const PREVIEW_TEXT_LIMIT = 2 * 1024 * 1024;

function isPreviewable(body: RequestLogBody | undefined, raw: string): boolean {
  return Boolean(raw) && raw.length <= PREVIEW_TEXT_LIMIT && body?.encoding !== "base64";
}

/** 请求 tools 声明(chat/responses/anthropic/gemini 四种形态)。 */
function previewTools(root: Record<string, unknown>, protocol: WireProtocol): PreviewTool[] | undefined {
  const values = Array.isArray(root.tools) ? root.tools : [];
  const tools: PreviewTool[] = [];
  for (const value of values) {
    if (!isRecord(value)) continue;
    if (protocol === "gemini") {
      // gemini: tools[] 内是 functionDeclarations 数组。
      const decls = Array.isArray(value.functionDeclarations) ? value.functionDeclarations : [];
      for (const decl of decls) {
        if (!isRecord(decl)) continue;
        const name = stringField(decl.name);
        if (!name) continue;
        tools.push({ description: stringField(decl.description) ?? "", name, parameters: decl.parameters ?? {} });
      }
      continue;
    }
    // chat / responses:{type:"function", function:{...}};anthropic 顶层平铺。
    const fn = isRecord(value.function) ? value.function : value;
    const name = stringField(fn.name);
    if (!name) continue;
    tools.push({
      description: stringField(fn.description) ?? "",
      name,
      parameters: fn.parameters ?? fn.input_schema ?? {},
    });
  }
  return tools.length ? tools : undefined;
}

/** 会话级参数(model/max_tokens/stream/tool_choice;gemini 取 toolConfig)。 */
function previewParams(root: Record<string, unknown>): PreviewParams {
  const generationConfig = isRecord(root.generationConfig) ? root.generationConfig : {};
  const maxTokens =
    numberValue(root.max_tokens) ?? numberValue(root.max_completion_tokens) ??
    numberValue(root.maxTokens) ?? numberValue(generationConfig.maxOutputTokens);
  const stream = typeof root.stream === "boolean" ? root.stream : undefined;
  return {
    maxTokens,
    model: stringField(root.model),
    stream,
    toolChoice: root.tool_choice ?? (isRecord(root.toolConfig) ? root.toolConfig : undefined),
  };
}

function numberValue(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

export function contentText(content: unknown): string {
  if (typeof content === "string") return content;
  if (Array.isArray(content)) {
    return content
      .map((part) => {
        if (typeof part === "string") return part;
        if (!isRecord(part)) return JSON.stringify(part);
        if (typeof part.text === "string") return part.text;
        if (typeof part.thinking === "string") return part.thinking;
        return JSON.stringify(part, null, 2);
      })
      .join("\n");
  }
  return content == null ? "" : JSON.stringify(content, null, 2);
}

function stringField(value: unknown): string | undefined {
  return typeof value === "string" ? value : undefined;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
