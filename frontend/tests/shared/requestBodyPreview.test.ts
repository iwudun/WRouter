import { describe, expect, it } from "vitest";
import type { RequestLogBody } from "@/contracts/app";
import { formatLogBodyView, parseLogStreamPayloads, parsePartialJson } from "@/pages/home/shared/logs";
import { detectProtocol, requestBodyPreview, responseBodyPreview } from "@/pages/home/shared/request-body-preview";
import { responseSummary } from "@/pages/home/shared/response-summary";

function body(text: string, extra: Partial<RequestLogBody> = {}): RequestLogBody {
  return { encoding: "utf8", sizeBytes: text.length, text, truncated: false, ...extra };
}

describe("request/response body previews", () => {
  it("formats OpenAI conversation messages without changing the raw JSON path", () => {
    const parsed = requestBodyPreview(body(JSON.stringify({ model: "m", messages: [
      { role: "system", content: "rules" },
      { role: "user", content: [{ type: "text", text: "hello" }] }
    ] })));
    expect(parsed.format).toBe("conversation");
    expect(parsed.protocol).toBe("openai_chat");
    expect(parsed.text).toContain("system\nrules");
    expect(parsed.text).toContain("user\nhello");
  });

  it("accepts Gemini contents/parts and Anthropic-style messages", () => {
    const gemini = requestBodyPreview(body('{"contents":[{"role":"user","parts":[{"text":"hi"}]}]}'));
    expect(gemini.text).toContain("user\nhi");
    expect(gemini.protocol).toBe("gemini");
    expect(gemini.messages?.[0]).toMatchObject({ role: "user", content: "hi" });
    expect(responseBodyPreview(body('{"content":[{"type":"text","text":"answer"}]}')).text).toBe("answer");
    expect(responseBodyPreview(body('{"candidates":[{"content":{"parts":[{"text":"gemini"}]}}]}')).text).toBe("gemini");
  });

  it("separates response reasoning, content, and tool calls into readable sections", () => {
    const preview = responseBodyPreview(body(JSON.stringify({ choices: [{ message: {
      reasoning_content: "thinking",
      content: "answer",
      tool_calls: [{ function: { name: "lookup", arguments: JSON.stringify({ q: 1 }) } }]
    } }] })));
    expect(preview.sections?.map(({ kind, name, text }) => [kind, name, text])).toEqual([
      ["reasoning", undefined, "thinking"],
      ["content", undefined, "answer"],
      ["tool", "lookup", JSON.stringify({ q: 1 })]
    ]);
  });

  it("aggregates SSE and extracts OpenAI response content", () => {
    const stream = body('data: {"choices":[{"delta":{"content":"hello "}}]}\ndata: {"choices":[{"delta":{"content":"world"}}]}\ndata: [DONE]');
    expect(responseBodyPreview(stream).text).toBe("hello world");
  });

  it("parses upstreams that pack SSE events onto a single line without separators", () => {
    // 部分上游(如 agnes)事件间无换行:按行解析整行失败,须回落括号平衡扫描。
    const packed = 'data: {"choices":[{"delta":{"reasoning_content":"thin"}}]}'
      + 'data: {"choices":[{"delta":{"content":"hello"}}]}'
      + 'data: {"choices":[{"delta":{},"finish_reason":"stop"}]}'
      + 'data: [DONE]';
    expect(parseLogStreamPayloads(packed)).toHaveLength(3);
    const preview = responseBodyPreview(body(packed));
    expect(preview.format).toBe("response");
    expect(preview.sections?.map(({ kind, text }) => [kind, text])).toEqual([
      ["reasoning", "thin"],
      ["content", "hello"]
    ]);
    const view = formatLogBodyView(body(packed));
    expect(view.json).toMatchObject({
      choices: [{ delta: { content: "hello", reasoning_content: "thin" }, finish_reason: "stop" }]
    });
    expect(responseSummary(body(packed)).terminal).toBe("complete");
  });

  it("skips non-JSON data payloads instead of producing garbage entries", () => {
    expect(parseLogStreamPayloads('data: not json\ndata: {"a":1}')).toEqual([{ a: 1 }]);
    expect(parseLogStreamPayloads("event: ping\ndata: {broken")).toEqual([]);
  });

  it("preserves unknown, invalid, unparseable, and base64 bodies as raw text", () => {
    const unknown = '{"metadata":{"value":1}}';
    expect(requestBodyPreview(body(unknown))).toEqual({ format: "raw", parsed: false, protocol: "unknown", text: unknown });
    // 完全不可解析(空 messages,部分恢复也无消息)→ raw。
    expect(requestBodyPreview(body('{"messages": [')).format).toBe("raw");
    expect(requestBodyPreview(body('{"messages":[]}', { encoding: "base64" })).format).toBe("raw");
    // 2026-10-25:截断正文不再一刀切回 raw —— 部分树恢复后尽力出预览
    // (见下方 "partial JSON recovery" 描述块)。
  });
});

describe("protocol detection (D5)", () => {
  it("distinguishes the four conversation dialects by exclusive fields", () => {
    expect(detectProtocol({ contents: [] })).toBe("gemini");
    expect(detectProtocol({ usageMetadata: {} })).toBe("gemini");
    expect(detectProtocol({ object: "response", output: [] })).toBe("openai_responses");
    expect(detectProtocol({ type: "response.output_text.delta", delta: "x" })).toBe("openai_responses");
    expect(detectProtocol({ type: "message", content: [{ type: "text", text: "x" }] })).toBe("anthropic");
    expect(detectProtocol({ system: "rules", messages: [] })).toBe("anthropic");
    expect(detectProtocol({ messages: [{ role: "user", content: "hi" }] })).toBe("openai_chat");
  });

  it("recognizes embeddings and leaves genuinely unknown bodies alone", () => {
    expect(detectProtocol({ input: "hello", model: "m" })).toBe("embeddings");
    expect(detectProtocol({ data: [{ embedding: [0.1] }] })).toBe("embeddings");
    expect(detectProtocol({ metadata: { value: 1 } })).toBe("unknown");
  });

  it("does not claim Anthropic for OpenAI chat messages with array content", () => {
    // content 数组但 block 类型不属于 anthropic 独占集合 → 仍是 chat。
    expect(detectProtocol({ messages: [{ role: "user", content: [{ type: "input_text", text: "x" }] }] })).toBe("openai_chat");
  });
});

describe("protocol-specific request parsing (D6)", () => {
  it("links Anthropic tool_use and tool_result across messages", () => {
    const preview = requestBodyPreview(body(JSON.stringify({
      system: "be brief",
      messages: [
        { role: "assistant", content: [{ type: "tool_use", id: "toolu_1", name: "get_weather", input: { loc: "SF" } }] },
        { role: "user", content: [{ type: "tool_result", tool_use_id: "toolu_1", content: "sunny", is_error: false }] }
      ]
    })));
    expect(preview.protocol).toBe("anthropic");
    expect(preview.messages?.[0]).toMatchObject({ role: "system", content: "be brief" });
    expect(preview.messages?.[1].toolCalls).toEqual([
      { arguments: JSON.stringify({ loc: "SF" }, null, 2), id: "toolu_1", name: "get_weather" }
    ]);
    expect(preview.messages?.[2].toolResults).toEqual([
      { callId: "toolu_1", content: "sunny", isError: false }
    ]);
  });

  it("links Gemini functionCall and functionResponse and maps model→assistant", () => {
    const preview = requestBodyPreview(body(JSON.stringify({
      systemInstruction: { parts: [{ text: "sys" }] },
      contents: [
        { role: "model", parts: [{ functionCall: { name: "lookup", args: { q: 1 } } }] },
        { role: "user", parts: [{ functionResponse: { name: "lookup", response: { ok: true } } }] }
      ]
    })));
    expect(preview.protocol).toBe("gemini");
    expect(preview.messages?.[0]).toMatchObject({ role: "system", content: "sys" });
    expect(preview.messages?.[1]).toMatchObject({ role: "assistant" });
    expect(preview.messages?.[1].toolCalls?.[0]).toMatchObject({ name: "lookup" });
    expect(preview.messages?.[2].toolResults?.[0]).toMatchObject({ name: "lookup" });
  });

  it("keeps Responses item ids and function_call linkage", () => {
    const preview = requestBodyPreview(body(JSON.stringify({
      instructions: "sys",
      input: [
        { type: "function_call", id: "fc_1", call_id: "call_1", name: "lookup", arguments: '{"q":1}' },
        { type: "function_call_output", id: "fco_1", call_id: "call_1", output: "done" }
      ]
    })));
    expect(preview.protocol).toBe("openai_responses");
    expect(preview.messages?.[0]).toMatchObject({ role: "system", content: "sys" });
    expect(preview.messages?.[1]).toMatchObject({ itemId: "fc_1", role: "assistant" });
    expect(preview.messages?.[1].toolCalls?.[0]).toMatchObject({ id: "call_1", name: "lookup" });
    expect(preview.messages?.[2]).toMatchObject({ itemId: "fco_1", role: "tool", toolCallId: "call_1" });
  });

  it("keeps OpenAI chat tool_call_id linkage", () => {
    const preview = requestBodyPreview(body(JSON.stringify({
      messages: [
        { role: "assistant", content: "", tool_calls: [{ id: "call_9", function: { name: "lookup", arguments: '{"q":1}' } }] },
        { role: "tool", tool_call_id: "call_9", name: "lookup", content: "ok" }
      ]
    })));
    expect(preview.messages?.[0].toolCalls?.[0]).toMatchObject({ id: "call_9", name: "lookup" });
    expect(preview.messages?.[1]).toMatchObject({ role: "tool", toolCallId: "call_9" });
    expect(preview.messages?.[1].toolResults?.[0]).toMatchObject({ callId: "call_9", name: "lookup" });
  });
});

describe("response terminal state and usage (D7)", () => {
  it("reports complete + usage from a chat response", () => {
    const summary = responseSummary(body(JSON.stringify({
      choices: [{ message: { content: "hi" }, finish_reason: "stop" }],
      usage: { prompt_tokens: 5, completion_tokens: 2, total_tokens: 7 }
    })));
    expect(summary.terminal).toBe("complete");
    expect(summary.finishReason).toBe("stop");
    expect(summary.usage).toMatchObject({ input: 5, output: 2, total: 7 });
  });

  it("reads Responses incomplete_details as incomplete, never complete", () => {
    const summary = responseSummary(body(JSON.stringify({
      object: "response",
      status: "incomplete",
      incomplete_details: { reason: "max_output_tokens" },
      output: []
    })));
    expect(summary.terminal).toBe("incomplete");
    expect(summary.incompleteReason).toBe("max_output_tokens");
  });

  it("reads Anthropic usage and stop_reason", () => {
    const summary = responseSummary(body(JSON.stringify({
      type: "message",
      content: [{ type: "text", text: "hi" }],
      stop_reason: "end_turn",
      usage: { input_tokens: 4, output_tokens: 3, cache_read_input_tokens: 1 }
    })));
    expect(summary.terminal).toBe("complete");
    expect(summary.finishReason).toBe("end_turn");
    expect(summary.usage).toMatchObject({ input: 4, output: 3, cacheRead: 1 });
  });

  it("marks gemini non-STOP finish as incomplete", () => {
    const summary = responseSummary(body(JSON.stringify({
      candidates: [{ content: { parts: [{ text: "x" }] }, finishReason: "SAFETY" }]
    })));
    expect(summary.terminal).toBe("incomplete");
    expect(summary.finishReason).toBe("SAFETY");
  });

  it("derives terminal state from captured trace when the body is raw SSE", () => {
    const summary = responseSummary(body("data: {}\n\n"), [
      { kind: "sse_event", payload: { choices: [{ delta: { content: "x" }, finish_reason: null }] } },
      { kind: "sse_event", payload: { choices: [{ delta: {}, finish_reason: "stop" }] } },
      { kind: "sse_event", payload: { usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 } } }
    ]);
    expect(summary.terminal).toBe("complete");
    expect(summary.finishReason).toBe("stop");
    expect(summary.usage).toMatchObject({ input: 1, output: 1 });
  });

  it("reports unknown terminal when neither body nor trace proves completion", () => {
    const summary = responseSummary(body("data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n"));
    expect(summary.terminal).toBe("unknown");
    expect(summary.usage).toBeUndefined();
  });

  it("never fabricates zero usage for an uncaptured body", () => {
    expect(responseSummary(undefined).usage).toBeUndefined();
    expect(responseSummary(body("not json at all")).usage).toBeUndefined();
  });
});

describe("partial JSON recovery for truncated captures (2026-10-25)", () => {
  it("recovers a tree from JSON truncated mid-string, dropping the unfinished entry", () => {
    expect(parsePartialJson('{"a":1,"b":"trunc')).toEqual({ a: 1 });
  });

  it("keeps scalars whose token ends exactly at the cut", () => {
    // 数字/字面量没有闭合定界符,eof 处无法区分「完结」与「被截断」,
    // 按已解析前缀保留(字符串有闭合引号可判,中途截断则丢弃)。
    expect(parsePartialJson("[1,2,3")).toEqual([1, 2, 3]);
    expect(parsePartialJson('{"outer":[{"x":1},{"y":2')).toEqual({ outer: [{ x: 1 }, { y: 2 }] });
    expect(parsePartialJson('{"a":{"b":{"c":1')).toEqual({ a: { b: { c: 1 } } });
  });

  it("drops tokens cut mid-token (strings, keys, literals, exponent fragments)", () => {
    expect(parsePartialJson('{"a":1,"ke')).toEqual({ a: 1 });
    expect(parsePartialJson("[true, fals")).toEqual([true]);
    expect(parsePartialJson('{"a":1,"b":1e')).toEqual({ a: 1 });
  });

  it("decodes escapes exactly like JSON.parse", () => {
    expect(parsePartialJson('{"a":"line1\nline2","b":"\u4e2d","c":"q\\"uote"}')).toEqual({
      a: "line1\nline2",
      b: "中",
      c: 'q"uote'
    });
  });

  it("accepts complete JSON identically to JSON.parse", () => {
    expect(parsePartialJson('{"a":[1,{"b":null}],"c":true}')).toEqual({ a: [1, { b: null }], c: true });
    expect(parsePartialJson("[]")).toEqual([]);
  });

  it("refuses non-JSON and malformed text instead of fabricating a tree", () => {
    expect(parsePartialJson("not json at all")).toBeUndefined();
    expect(parsePartialJson("<html>error page</html>")).toBeUndefined();
    expect(parsePartialJson('{"a":1} trailing')).toBeUndefined();
    expect(parsePartialJson("{,}")).toBeUndefined();
  });

  it("feeds the recovered tree into formatLogBodyView and the conversation preview", () => {
    const truncated = body('{"model":"m","messages":[{"role":"user","content":"hi"},{"role":"user","content":"cut', {
      sizeBytes: 4096,
      truncated: true
    });
    const view = formatLogBodyView(truncated);
    // 尾消息的 content 键被截断丢弃,但消息本体(仅 role)保留 —— 表示会话
    // 在此继续、文本被截,比整条丢弃更有信息量。
    expect(view.json).toEqual({ model: "m", messages: [{ role: "user", content: "hi" }, { role: "user" }] });
    expect(view.text).toContain('"model": "m"');
    const preview = requestBodyPreview(truncated);
    expect(preview.format).toBe("conversation");
    expect(preview.messages).toHaveLength(2);
  });

  it("keeps previews available for large and truncated bodies (guard relaxed from 128KB)", () => {
    // 200KB 正文:此前被 128KB 帽整条拒掉,预览按钮消失。
    const big = body(JSON.stringify({ model: "m", messages: [{ role: "user", content: "x".repeat(200 * 1024) }] }));
    expect(big.text.length).toBeGreaterThan(128 * 1024);
    expect(requestBodyPreview(big).format).toBe("conversation");
    // 2MB 以上仍回退 raw,防超大正文拖垮主线程。
    const huge = body(JSON.stringify({ model: "m", messages: [{ role: "user", content: "x".repeat(3 * 1024 * 1024) }] }));
    expect(requestBodyPreview(huge).format).toBe("raw");
  });

  it("parses standard multi-line SSE captures (block separators preserved since 2026-10-25)", () => {
    const sse = 'data: {"choices":[{"delta":{"content":"He"}}]}\n\n'
      + 'data: {"choices":[{"delta":{"content":"llo"}}]}\n\n'
      + 'data: [DONE]\n\n';
    const payloads = parseLogStreamPayloads(sse);
    expect(payloads).toHaveLength(2);
    expect(responseBodyPreview(body(sse)).text).toBe("Hello");
  });
});
