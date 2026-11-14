// Component tests for the call-logs view (调用日志), rendered directly with
// the zh copy: row projection, status chips with counters, expandable
// details, pagination state, and the empty state.
import { waitFor } from "@testing-library/react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { CallLogsView } from "@/pages/home/components/call-logs";
import { AppI18nContext, appCopy } from "@/pages/home/shared/i18n";
import { mockWindowWr } from "../helpers/mockWindowWr";
import type { CallLogEntry, CallLogListFilter, CallLogPage } from "@/contracts/app";

const okEntry: CallLogEntry = {
  apiKeyId: "key_1",
  apiKeyName: "Key One",
  baseModel: "m1",
  cacheReadTokens: 1205,
  cacheWriteTokens: 5,
  conversionChain: "OpenAI Compatible → OpenAI Responses",
  createdAt: "2026-08-15T15:42:23Z",
  durationMs: 1200,
  endReason: "completed",
  endpointPath: "/v1/chat/completions",
  firstByteMs: 137,
  id: 2,
  inputTokens: 4816,
  isStream: true,
  method: "POST",
  modelLabel: "Provider One/m1",
  ok: true,
  outputTokens: 2661,
  path: "/v1/chat/completions",
  protocol: "openai",
  provider: "p1",
  providerAttempts: ["p1"],
  reasoningTokens: 120,
  reasoningEffort: "high",
  status: "completed",
  requestId: "req_e7a7cdce",
  requestedModel: "p1/m1",
  // 2026-10-08 丰富化:网关结构化明细(等待时长/失败状态/错误/耗时)。
  retryAttempts: [
    { attempt: 1, delayMs: 30000, durationMs: 120.5, error: "rate limited", final: false, provider: "p1", model: "m1", statusCode: 429, waitSource: "retry-after" },
    { attempt: 2, delayMs: 0, durationMs: 1080, final: true, provider: "p1", model: "m1", statusCode: 200, upstreamRequest: { model: "m1" }, upstreamResponse: "{\"ok\":true}" }
  ],
  statusCode: 200,
  totalTokens: 7487,
  // 20261108_01 §13:解码/输出速度由后端统一计算,前端直接展示。
  decodeTps: 2503.29,
  outputTps: 1234.56
};

const rejectedEntry: CallLogEntry = {
  apiKeyId: undefined,
  apiKeyName: undefined,
  baseModel: "m9",
  cacheReadTokens: 0,
  cacheWriteTokens: 0,
  createdAt: "2026-08-15T15:39:51Z",
  durationMs: 3,
  endReason: "auth_failed",
  endpointPath: "/v1/chat/completions",
  error: "Unauthorized",
  id: 1,
  inputTokens: 0,
  isStream: false,
  method: "POST",
  modelLabel: "m9",
  ok: false,
  outputTokens: 0,
  path: "/v1/chat/completions",
  protocol: "openai",
  provider: undefined,
  providerAttempts: [],
  reasoningTokens: 0,
  status: "failed",
  requestId: "req_bcf07fe7",
  retryAttempts: [],
  statusCode: 401,
  totalTokens: 0
};

const fullPage: CallLogPage = {
  generatedAt: "2026-08-16T00:00:00Z",
  items: [okEntry, rejectedEntry],
  options: {
    credentials: [{ id: "key_1", name: "Key One" }],
    models: ["Provider One/m1", "m9"]
  },
  page: 1,
  pageSize: 25,
  statusCounts: { all: 2, canceled: 0, completed: 1, failed: 1, pending: 0, processing: 0 },
  total: 2,
  totalRows: 2,
  totalPages: 1,
  windowSize: 2000
};

const emptyPage: CallLogPage = {
  ...fullPage,
  items: [],
  options: { credentials: [], models: [] },
  statusCounts: { all: 0, canceled: 0, completed: 0, failed: 0, pending: 0, processing: 0 },
  total: 0,
  totalRows: 0
};

let root: Root | null = null;

function mountView({
  page = fullPage,
  filter = { page: 1, pageSize: 25, status: "all" } as CallLogListFilter,
  refreshLogs = () => {},
  updateFilter = () => {},
  onOpenDetail = () => {}
}: {
  page?: CallLogPage;
  filter?: CallLogListFilter;
  refreshLogs?: () => void | Promise<void>;
  updateFilter?: (patch: CallLogListFilter, resetPage?: boolean) => void;
  onOpenDetail?: (item: CallLogEntry) => void;
} = {}) {
  const container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  root.render(
    <AppI18nContext.Provider value={appCopy.zh}>
      <CallLogsView
        error=""
        filter={filter}
        loading={false}
        onOpenDetail={onOpenDetail}
        page={page}
        refreshLogs={refreshLogs}
        updateFilter={updateFilter}
      />
    </AppI18nContext.Provider>
  );
  return container;
}

async function waitForRender(container: HTMLElement) {
  await waitFor(() => {
    expect(container.textContent ?? "").not.toBe("");
  });
  return container;
}


function findRowByText(text: string, rootElement: ParentNode = document) {
  const row = Array.from(rootElement.querySelectorAll<HTMLElement>(".network-log-row"))
    .find((element) => element.textContent?.includes(text));
  expect(row, `row containing "${text}" not found`).toBeTruthy();
  return row!;
}

describe("CallLogsView", () => {
  afterEach(() => {
    root?.unmount();
    root = null;
    document.body.innerHTML = "";
    vi.restoreAllMocks();
  });

  it("列名按约定显示:缓存读/写、首字/总耗时、解码/输出速度单位小写", async () => {
    const container = await waitForRender(mountView());
    const text = container.textContent ?? "";
    expect(text).toContain("缓存读/写");
    expect(text).toContain("首字/总耗时");
    expect(text).not.toContain("缓存读/缓存写");
    expect(text).not.toContain("首字耗时/总耗时");
    // 2026-10-14:单位简写 tok/s 固定在表头,数据格只放数值。
    expect(text).toContain("(tok/s)");
    expect(text).not.toContain("2503tok/s");
    // 20261108_01 §13:数值来自后端 decodeTps / outputTps(前端不再二次计算)。
    expect(text).toContain("解码/输出速度");
    expect(text).toContain("2503");
    expect(text).toContain("1235");
    expect(text).not.toContain("tokens/s");
    expect(text).not.toContain("TOKENS/S");
  });

  it("渲染行数据：新列（请求方式/思考强度/合并输入输出/推理/命中率/首字总耗时）", async () => {
    const container = await waitForRender(mountView());
    const text = container.textContent ?? "";

    expect(text).toContain("Provider One/m1");
    expect(text).toContain("Key One");
    // 状态列显示五档状态文案(不再是 HTTP 码;码进 title tooltip)。
    expect(text).toContain("已完成");
    expect(text).toContain("失败");
    expect(text).not.toContain("401");
    // 请求方式列:流式 pill / 非流式文本。
    expect(text).toContain("流式");
    expect(text).toContain("非流式");
    // 思考强度徽章(high → 高)替代原模型列「流式」徽章。
    expect(text).toContain("高");
    // 输入/输出合并列 + 推理列(>0 才显示)。
    expect(text).toContain("4,816 / 2,661");
    expect(text).toContain("120");
    // 输入/输出(推理)合并列:推理 > 0 时带括号后缀。
    expect(text).toContain("4,816 / 2,661 (120)");
    // 缓存读/写列 + 缓存命中率列(非 Anthropic 只显示结果,两位小数)。
    expect(text).toContain("1,205 / 5");
    expect(text).toContain("25.02%");
    // 首字/总耗时:137ms / 1.2s。
    expect(text).toContain("137ms / 1.2s");
    // Rejected rows are part of the audit trail.
    expect(text).toContain("m9");
    expect(text).toContain("req_bcf07fe7");
  });

  it("Anthropic 行缓存命中率展示计算式(缓存读 ÷ 三段和),其余协议只显示结果", async () => {
    const anthropicEntry: CallLogEntry = {
      ...okEntry,
      id: 3,
      protocol: "anthropic",
      isStream: false,
      firstByteMs: undefined,
      inputTokens: 21000,
      outputTokens: 4400,
      reasoningTokens: 2600,
      cacheReadTokens: 32000,
      cacheWriteTokens: 8192,
      status: "completed"
    };
    const container = await waitForRender(mountView({
      page: { ...fullPage, items: [anthropicEntry] }
    }));
    const text = container.textContent ?? "";
    // 单元格只显示百分比(两位小数);计算式(32000 ÷ 三段和)移到 tooltip,
    // tooltip 公式用精确数,与结果可核对。
    expect(text).toContain("52.29%");
    expect(text).not.toContain("÷");
    const rateCell = Array.from(container.querySelectorAll(".network-log-row div"))
      .find((el) => (el.getAttribute("title") ?? "").includes("缓存命中率"));
    expect(rateCell?.getAttribute("title")).toBe("缓存命中率 = 32,000 ÷ (21,000 + 32,000 + 8,192) = 52.29%");
    // 缓存读/写列。
    expect(text).toContain("3.2万 / 8,192");
    // 输入/输出(推理)列。
    expect(text).toContain("2.1万 / 4,400 (2,600)");
  });

  it("耗时 ≥1min 显示「分+秒」形式,详情面板 Token 用量显示精确数", async () => {
    const longEntry: CallLogEntry = {
      ...okEntry,
      id: 4,
      durationMs: 72_400,
      firstByteMs: 2_350,
      inputTokens: 123_456,
      outputTokens: 7_890,
      reasoningTokens: 1_234,
      totalTokens: 132_580
    };
    const container = await waitForRender(mountView({
      page: { ...fullPage, items: [longEntry] }
    }));
    let text = container.textContent ?? "";
    // 耗时列:1m12s(非 1.2m);首字 2.4s 仍为秒。
    expect(text).toContain("1m12s");
    expect(text).not.toContain("1.2m");
    expect(text).toContain("2.4s");

    const row = findRowByText("req_e7a7cdce");
    row.click();
    await waitFor(() => {
      expect(text = container.textContent ?? "").toContain("Token 用量");
    });
    // 详情面板 Token 用量为精确数(千分位分组);条目行仍是约数(12.3万)。
    expect(text).toContain("输入123,456");
    expect(text).toContain("总计132,580");
    expect(text).toContain("12.3万 / 7,890");
    expect(container.querySelector(".network-detail > .grid")?.className).toContain("2xl:grid-cols-4");
    expect(container.querySelectorAll(".network-detail > .grid > section")).toHaveLength(4);
  });

  it("状态 chips 显示计数，点击失败 chip 触发 status 筛选", async () => {
    const updateFilter = vi.fn();
    const container = await waitForRender(mountView({ updateFilter }));
    const text = container.textContent ?? "";

    expect(text).toContain("全部");
    expect(text).toContain("等待中");
    expect(text).toContain("处理中");
    expect(text).toContain("已完成");
    expect(text).toContain("失败");
    expect(text).toContain("已取消");

    const failedChip = Array.from(container.querySelectorAll<HTMLButtonElement>("[data-active] button, button[aria-pressed]"))
      .find((element) => element.textContent?.includes("失败"));
    expect(failedChip).toBeTruthy();
    failedChip!.click();
    expect(updateFilter).toHaveBeenCalledWith({ status: "failed" });
  });

  it("在途行：显示处理中状态，展开时不请求正文详情", async () => {
    const getRequestLogDetail = vi.fn(() => Promise.resolve(undefined));
    mockWindowWr({ getRequestLogDetail });
    const inFlightEntry: CallLogEntry = {
      ...okEntry,
      id: -1,
      status: "processing",
      statusCode: 0,
      endReason: undefined,
      firstByteMs: undefined,
      durationMs: 420,
      inputTokens: 0,
      outputTokens: 0,
      totalTokens: 0,
      reasoningTokens: 0,
      cacheReadTokens: 0,
      cacheWriteTokens: 0,
      // 在途行尚无落库尝试明细(完成时一次性写入);继承 okEntry 的
      // retryAttempts 会失真。
      retryAttempts: []
    };
    const container = await waitForRender(mountView({
      page: { ...fullPage, items: [inFlightEntry] }
    }));
    await waitFor(() => {
      expect(container.textContent ?? "").toContain("处理中");
    });
    const row = findRowByText("req_e7a7cdce");
    row.click();
    // 2026-10-17:尝试表常驻 —— 在途行渲染 #1 活动行(处理中,无详情按钮);
    // 处理中冗余的行级「查看请求/响应详情」按钮不再渲染;表格下方的
    // 「请求进行中…」说明文字同步删除。
    await waitFor(() => {
      const text = container.textContent ?? "";
      expect(text).toContain("请求尝试");
      expect(text).toContain("原始请求");
      expect(text).toContain("处理中");
      expect(text).not.toContain("查看请求/响应详情");
      expect(text).not.toContain("请求进行中");
    });
    expect(getRequestLogDetail).not.toHaveBeenCalled();
  });

  it("在途行总耗时按登记时刻本地计时跳动(首字显示快照值)", async () => {
    const createdAt = new Date(Date.now() - 4_000).toISOString();
    const inFlightEntry: CallLogEntry = {
      ...okEntry,
      id: -9,
      status: "processing",
      createdAt,
      durationMs: 4_000,
      firstByteMs: 800
    };
    const container = await waitForRender(mountView({
      page: { ...fullPage, items: [inFlightEntry] }
    }));
    const durationText = () => {
      const row = Array.from(container.querySelectorAll(".network-log-row"))
        .find((element) => element.textContent?.includes("req_e7a7cdce"));
      return row?.children[10]?.textContent ?? "";
    };
    const before = durationText();
    expect(before).toMatch(/^800ms \/ 4\.\ds$/);
    // 快照冻结在取数瞬间;本地 1s tick 让总耗时继续走。
    await new Promise((resolve) => setTimeout(resolve, 1_300));
    const after = durationText();
    expect(after).not.toBe(before);
    expect(after).toMatch(/^800ms \/ 5\.\ds$/);
  }, 10_000);

  it("在途行观察到答案完成后冻结总耗时(answerEndMs)", async () => {
    const createdAt = new Date(Date.now() - 10_000).toISOString();
    const inFlightEntry: CallLogEntry = {
      ...okEntry,
      id: -10,
      status: "processing",
      createdAt,
      durationMs: 3_000,
      firstByteMs: 800,
      // 20261108_01 §4.1:答案已完成 → 计时冻结在该时刻。
      answerEndMs: 3_000
    };
    const container = await waitForRender(mountView({
      page: { ...fullPage, items: [inFlightEntry] }
    }));
    const durationText = () => {
      const row = Array.from(container.querySelectorAll(".network-log-row"))
        .find((element) => element.textContent?.includes("req_e7a7cdce"));
      return row?.children[10]?.textContent ?? "";
    };
    expect(durationText()).toMatch(/^800ms \/ 3\.0s$/);
    await new Promise((resolve) => setTimeout(resolve, 1_300));
    expect(durationText()).toMatch(/^800ms \/ 3\.0s$/);
  }, 10_000);

  it("密钥筛选 Select 以 id 作为 option value，选择后回传 id 而非显示名称", async () => {
    const updateFilter = vi.fn();
    const container = await waitForRender(mountView({ updateFilter }));
    const select = container.querySelector<HTMLSelectElement>('select[aria-label="筛选调用日志密钥"]');
    expect(select, 'credential select not found').toBeTruthy();

    const options = Array.from(select!.querySelectorAll("option")).map((option) => ({
      label: option.textContent,
      value: option.value
    }));
    expect(options).toEqual([
      { label: "全部密钥", value: "" },
      { label: "Key One", value: "key_1" }
    ]);

    // Simulate picking "Key One": the change must carry the stable id so the
    // backend api_key_id filter matches (regression: name was sent before).
    const nativeSetter = Object.getOwnPropertyDescriptor(window.HTMLSelectElement.prototype, "value")!.set!;
    nativeSetter.call(select, "key_1");
    select!.dispatchEvent(new Event("change", { bubbles: true }));
    expect(updateFilter).toHaveBeenCalledWith({ credential: "key_1" });
  });

  it("已选密钥 id 不在当前窗口时保留显示，不静默回退为全部密钥", async () => {
    const container = await waitForRender(mountView({
      filter: { page: 1, pageSize: 25, status: "all", credential: "key_stale" }
    }));
    const select = container.querySelector<HTMLSelectElement>('select[aria-label="筛选调用日志密钥"]');
    expect(select).toBeTruthy();
    expect(select!.value).toBe("key_stale");
  });

  it("点击行展开：请求信息 + Token 用量 + 错误明细", async () => {
    const container = await waitForRender(mountView());
    const row = findRowByText("req_e7a7cdce");
    row.click();
    await waitFor(() => {
      const text = container.textContent ?? "";
      expect(text).toContain("请求与路由");
      expect(text).toContain("Token 用量");
      expect(text).toContain("请求端点");
      expect(text).toContain("/v1/chat/completions");
      expect(text).toContain("转换链路");
      expect(text).toContain("OpenAI Compatible → OpenAI Responses");
      expect(text).toContain("结束原因");
      expect(text).toContain("completed");
      expect(text).toContain("p1/m1");
      expect(text).toContain("7,487");
      // Migrated cost/thinking/retry displays (method/path bar removed).
      expect(text).toContain("成本");
      expect(text).toContain("思考");
      // 2026-09-26:请求信息补充思考强度(原始枚举词)。
      expect(text).toContain("思考强度");
      expect(text).toContain("high");
      expect(text).toContain("请求尝试");
      // 2026-10-13:每行区分原始/重试请求,并新增「请求与响应」列。
      expect(text).toContain("类型");
      expect(text).toContain("原始请求");
      expect(text).toContain("重试请求");
      expect(text).toContain("请求与响应");
      expect(text).toContain("查看详情");
      expect(text).toContain("未捕获");
      // 2026-10-14:错误信息独立成列(不再挤在结果列)。
      expect(text).toContain("错误信息");
      // 2026-10-08 重试明细表:真实等待时长/失败状态/错误/耗时入列
      // (旧行为 status 列显示 provider id、delayMs 恒 0)。
      expect(text).toContain("HTTP 429");
      expect(text).toContain("rate limited");
      expect(text).toContain("30 s");
      expect(text).toContain("120.5 ms");
      expect(text).toContain("HTTP 200");
      // 2026-10-17:尝试表「供应商」列改「请求模型」(展示 attempt.model,
      // 旧行回落 provider);耗时/等待表头中文化;「尝试供应商」行删除。
      expect(text).toContain("请求模型");
      expect(text).toContain("尝试耗时");
      expect(text).not.toContain("尝试供应商");
    });
    // 点击失败行(无真实 attempt,如认证失败)后:统一尝试表合成一行
    // 承载错误(2026-10-14),不再有顶部红框。
    const rejectedRow = findRowByText("req_bcf07fe7");
    rejectedRow.click();
    await waitFor(() => {
      const text = container.textContent ?? "";
      expect(text).toContain("Unauthorized");
      expect(text).toContain("auth_failed");
      expect(text).toContain("未路由");
      expect(text).toContain("请求尝试");
      expect(text).toContain("HTTP 401");
      expect(text).toContain("查看详情");
    });
  });

  it("展开行不读取正文，仅显示独立详情入口", async () => {
    const getRequestLogDetail = vi.fn(() => Promise.resolve(undefined));
    const onOpenDetail = vi.fn();
    mockWindowWr({ getRequestLogDetail });
    const container = await waitForRender(mountView({ onOpenDetail }));
    const row = findRowByText("req_e7a7cdce");
    row.click();
    // 2026-10-13:详情入口移入「请求尝试」表的「请求与响应」列(有尝试表
    // 时不再渲染行级底部按钮)。
    await waitFor(() => {
      expect(container.textContent ?? "").toContain("查看详情");
    });
    expect(getRequestLogDetail).not.toHaveBeenCalled();
    const button = Array.from(container.querySelectorAll<HTMLButtonElement>("button"))
      .find((element) => element.textContent?.includes("查看详情"));
    button?.click();
    expect(onOpenDetail).toHaveBeenCalledWith(okEntry);
  });

  it("分页栏显示范围并禁用首末页按钮", async () => {
    const container = await waitForRender(mountView());
    const text = container.textContent ?? "";
    expect(text).toContain("1-2 / 2");
    expect(text).toContain("1 / 1");

    const buttons = Array.from(container.querySelectorAll<HTMLButtonElement>("button"));
    const prev = buttons.find((element) => element.getAttribute("aria-label") === "上一页");
    const next = buttons.find((element) => element.getAttribute("aria-label") === "下一页");
    expect(prev?.disabled).toBe(true);
    expect(next?.disabled).toBe(true);  });

  it("空状态显示暂无调用日志", async () => {
    const container = await waitForRender(mountView({ page: emptyPage }));
    await waitFor(() => {
      expect(container.textContent ?? "").toContain("暂无调用日志");
    });
  });

  it("点击刷新按钮触发刷新并显示旋转反馈", async () => {
    const refreshLogs = vi.fn(() => Promise.resolve());
    const container = await waitForRender(mountView({ refreshLogs }));
    const button = Array.from(container.querySelectorAll<HTMLButtonElement>("button"))
      .find((element) => element.getAttribute("aria-label") === "刷新调用日志");
    expect(button).toBeTruthy();
    button!.click();
    expect(refreshLogs).toHaveBeenCalled();
    await waitFor(() => {
      expect(button!.querySelector(".animate-spin")).not.toBeNull();
    });
  });

});
