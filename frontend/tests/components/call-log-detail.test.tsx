import { fireEvent, waitFor } from "@testing-library/react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { CallLogEntry } from "@/contracts/app";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { CallLogDetailView } from "@/pages/home/components/call-log-detail";
import { AppI18nContext, appCopy } from "@/pages/home/shared/i18n";
import { mockWindowWr } from "../helpers/mockWindowWr";

const item: CallLogEntry = {
  apiKeyName: "Key",
  baseModel: "gpt",
  cacheReadTokens: 0,
  cacheWriteTokens: 0,
  createdAt: new Date().toISOString(),
  durationMs: 1000,
  endpointPath: "/v1/chat/completions",
  firstByteMs: 100,
  id: 1,
  inputTokens: 5,
  isStream: true,
  method: "POST",
  modelLabel: "gpt",
  ok: true,
  outputTokens: 3,
  path: "/v1/chat/completions",
  protocol: "openai",
  providerAttempts: ["p"],
  reasoningTokens: 0,
  status: "completed",
  requestId: "req-1",
  retryAttempts: [],
  statusCode: 200,
  totalTokens: 8
};

let root: Root | undefined;
afterEach(() => {
  root?.unmount();
  root = undefined;
  document.body.innerHTML = "";
  vi.restoreAllMocks();
});

function mount(detail: unknown, selectedItem = item) {
  const container = document.createElement("div");
  document.body.appendChild(container);
  const getRequestLogDetail = vi.fn(() => Promise.resolve(detail));
  const getRequestLogTrace = vi.fn(({ offset = 0 }: { offset?: number }) => Promise.resolve({ items: offset === 0 ? [{ kind: "sse_event", payload: { ordinal: 0 } }] : [], total: 1 }));
  mockWindowWr({ getRequestLogDetail, getRequestLogTrace });
  root = createRoot(container);
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  root.render(<QueryClientProvider client={queryClient}><AppI18nContext.Provider value={appCopy.zh}><CallLogDetailView item={selectedItem} onBack={() => {}} /></AppI18nContext.Provider></QueryClientProvider>);
  return { container, getRequestLogDetail, getRequestLogTrace };
}

describe("CallLogDetailView", () => {
  it("loads body only in standalone view and exposes conversational preview with JSON modes", async () => {
    const { container } = mount({
      request_body_text: JSON.stringify({ messages: [{ role: "user", content: "hello" }] }),
      request_body_size_bytes: 49,
      request_body_truncated: false,
      response_body_text: '{"choices":[{"message":{"content":"world"}}]}',
      response_body_size_bytes: 47,
      response_body_truncated: false,
      response_headers: { "content-type": "application/json" }
    });
    await waitFor(() => expect(container.textContent).toContain("hello"));
    expect(container.textContent).not.toContain("world");
    expect(container.textContent).toContain("对话阅览");
    expect(container.textContent).toContain("JSON 原文");
    expect(container.textContent).not.toContain("会话");
    const responseTab = Array.from(container.querySelectorAll("[role=tab]")).find((tab) => tab.textContent === "响应");
    fireEvent.click(responseTab!);
    expect(container.textContent).toContain("world");
    expect(container.textContent).toContain("application/json");
  });

  it("searches messages and reveals long content and raw message JSON", async () => {
    const long = "long message ".repeat(80);
    const { container } = mount({
      request_body_text: JSON.stringify({ messages: [
        { role: "user", content: "find this" },
        { role: "assistant", content: long }
      ] }),
      response_body_text: "{}"
    });
    await waitFor(() => expect(container.textContent).toContain("find this"));
    const search = container.querySelector<HTMLInputElement>("input")!;
    const roleFilter = container.querySelector<HTMLSelectElement>("select")!;
    fireEvent.change(roleFilter, { target: { value: "assistant" } });
    expect(container.textContent).not.toContain("find this");
    fireEvent.change(roleFilter, { target: { value: "all" } });
    fireEvent.change(search, { target: { value: "not present" } });
    expect(container.textContent).toContain("无匹配消息");
    fireEvent.change(search, { target: { value: "find this" } });
    expect(container.textContent).toContain("find this");
    const rawButton = container.querySelector<HTMLButtonElement>('button[title="查看原始 JSON"]');
    fireEvent.click(rawButton!);
    expect(container.textContent).toContain('"role": "user"');
  });

  it("shows a missing-record state when the API has no detail", async () => {
    const { container } = mount(undefined);
    await waitFor(() => expect(container.textContent).toContain("未找到请求记录"));
  });

  it("loads captured wire events lazily in the attempts panel", async () => {
    const { container, getRequestLogTrace } = mount({ request_body_text: "{}" });
    await waitFor(() => expect(container.textContent).toContain("cURL 重放"));
    const attemptsTab = Array.from(container.querySelectorAll("[role=tab]")).find((tab) => tab.textContent === "请求尝试");
    fireEvent.click(attemptsTab!);
    await waitFor(() => expect(getRequestLogTrace).toHaveBeenCalled());
    await waitFor(() => expect(container.textContent).toContain("sse_event"));
  });

  it("does not request detail while the request is in flight", async () => {
    const { container, getRequestLogDetail } = mount(undefined, { ...item, status: "processing" });
    await waitFor(() => expect(container.textContent).toContain("请求进行中"));
    expect(getRequestLogDetail).not.toHaveBeenCalled();
  });

  it("shows response terminal + usage and drops the protocol/model badges from the toolbar (D5/D7)", async () => {
    const { container } = mount({
      request_body_text: JSON.stringify({ messages: [{ role: "user", content: "hi" }] }),
      response_body_text: JSON.stringify({
        choices: [{ message: { content: "world" }, finish_reason: "stop" }],
        usage: { prompt_tokens: 5, completion_tokens: 3, total_tokens: 8 }
      })
    });
    await waitFor(() => expect(container.querySelectorAll("[role=tab]").length).toBe(3));
    // 2026-10-18:工具栏徽章(协议「OpenAI Chat」/模型名)已删,与概览卡去重。
    await waitFor(() => expect(container.textContent).not.toContain("OpenAI Chat"));
    const responseTab = Array.from(container.querySelectorAll("[role=tab]")).find((tab) => tab.textContent === "响应");
    fireEvent.click(responseTab!);
    await waitFor(() => expect(container.textContent).toContain("world"));
    expect(container.textContent).toContain("已完成");
    expect(container.textContent).toContain("stop");
  });

  it("shows an incomplete terminal without fabricating completion (D7)", async () => {
    const { container } = mount({
      request_body_text: "{}",
      response_body_text: JSON.stringify({
        object: "response",
        status: "incomplete",
        incomplete_details: { reason: "max_output_tokens" },
        output: []
      })
    });
    await waitFor(() => expect(container.querySelectorAll("[role=tab]").length).toBe(3));
    const responseTab = Array.from(container.querySelectorAll("[role=tab]")).find((tab) => tab.textContent === "响应");
    fireEvent.click(responseTab!);
    await waitFor(() => expect(container.textContent).toContain("不完整"));
    expect(container.textContent).toContain("max_output_tokens");
    expect(container.textContent).not.toContain("已完成");
  });

  it("renders per-attempt upstream request/response when captured (D8)", async () => {
    const captured = {
      ...item,
      retryAttempts: [
        {
          attempt: 1,
          attemptId: "prov-test#1",
          delayMs: 0,
          durationMs: 12,
          final: true,
          provider: "prov-test",
          statusCode: 200,
          upstreamRequest: "{\"model\":\"m\"}",
          upstreamRequestHeaders: { "content-type": "application/json", authorization: "***" },
          upstreamResponse: "data: {\"choices\":[]}",
          upstreamResponseSize: 21,
          upstreamHeaders: { "content-type": "text/event-stream" },
          conversion: {
            strategy: "semantic" as const,
            diagnostics: [{ code: "dropped_field", path: "store", detail: "no equivalent" }]
          }
        }
      ]
    };
    const { container } = mount({ request_body_text: "{}" }, captured);
    await waitFor(() => expect(container.querySelectorAll("[role=tab]").length).toBe(3));
    const attemptsTab = Array.from(container.querySelectorAll("[role=tab]")).find((tab) => tab.textContent === "请求尝试");
    fireEvent.click(attemptsTab!);
    await waitFor(() => expect(container.textContent).toContain("prov-test#1"));
    // 2026-10-18:cURL 重放移入标题行;上游请求/响应改为 chevron 折叠卡。
    expect(container.textContent).toContain("cURL 重放");
    expect(container.textContent).toContain("上游请求");
    expect(container.textContent).toContain("上游响应");
    // 展开请求卡:左右分栏(请求头 | 请求正文)以 JSON 树展示,认证键打码。
    const requestToggle = Array.from(container.querySelectorAll("button")).find((button) => button.textContent?.includes("上游请求"));
    fireEvent.click(requestToggle!);
    await waitFor(() => expect(container.textContent).toContain("请求头"));
    expect(container.textContent).toContain("请求正文");
    expect(container.textContent).toContain("application/json");
    expect(container.textContent).toContain("***");
    // 展开响应卡:左右分栏(响应头 | 响应正文)。
    const responseToggle = Array.from(container.querySelectorAll("button")).find((button) => button.textContent?.includes("上游响应"));
    fireEvent.click(responseToggle!);
    await waitFor(() => expect(container.textContent).toContain("响应头"));
    expect(container.textContent).toContain("响应正文");
    expect(container.textContent).toContain("content-type");
    expect(container.textContent).toContain("转换诊断");
    expect(container.textContent).toContain("dropped_field");
  });

  it("marks uncaptured per-attempt data as unavailable instead of empty (D8)", async () => {
    const uncaptured = {
      ...item,
      retryAttempts: [
        { attempt: 1, delayMs: 0, durationMs: 9, final: true, provider: "prov-test", statusCode: 200 }
      ]
    };
    const { container } = mount({ request_body_text: "{}" }, uncaptured);
    await waitFor(() => expect(container.querySelectorAll("[role=tab]").length).toBe(3));
    const attemptsTab = Array.from(container.querySelectorAll("[role=tab]")).find((tab) => tab.textContent === "请求尝试");
    fireEvent.click(attemptsTab!);
    await waitFor(() => expect(container.textContent).toContain("未捕获上游请求"));
    expect(container.textContent).toContain("未捕获上游响应");
    // 无捕获时不渲染上游响应折叠卡(响应头/正文分栏随之缺席)。
    expect(container.textContent).not.toContain("上游响应头");
  });
});
