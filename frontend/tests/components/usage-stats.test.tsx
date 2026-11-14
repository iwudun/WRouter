// Smoke tests for the usage-stats view (rendered directly, bypassing App-level
// lazy/AnimatePresence timing).
//
// 20261102_01 改版:总览卡 2×2(Token/请求 + 缓存命中率/输出速度)、活跃度热力图
// 带卡片容器、原「Token 消耗分布 / 模型调用分布」两张卡合并为一张「用量趋势」
// (Token/请求 指标 tab + 面积/柱状图标 tab + 右侧模型调用排行榜)。
import { waitFor } from "@testing-library/react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";
import { UsageStatsView } from "@/pages/home/components/dashboard/usage-stats";
import { AppI18nContext } from "@/pages/home/shared/i18n";
import { appCopy } from "@/pages/home/shared/i18n";
import type { UsageStatsSnapshot } from "@/contracts/app";

// jsdom reports zero-sized layout, so the ChartFrame render-prop never fires.
// Stub layout + ResizeObserver so recharts actually mounts.
class ResizeObserverStub {
  observe() {}
  unobserve() {}
  disconnect() {}
}

beforeAll(() => {
  Object.defineProperty(globalThis, "ResizeObserver", { value: ResizeObserverStub, writable: true, configurable: true });
  Object.defineProperty(Element.prototype, "getBoundingClientRect", {
    value() {
      return { x: 0, y: 0, top: 0, left: 0, right: 800, bottom: 320, width: 800, height: 320, toJSON: () => ({}) } as DOMRect;
    },
    writable: true,
    configurable: true
  });
});

const richUsageSnapshot: UsageStatsSnapshot = {
  clientModels: [
    // 20261104_01 阶段三 · B-1a:后端 clientModels 是 (client, model, provider)
    // 三元组维度行,label = "{client} / {供应商/模型名}"。
    {
      caption: "Claude Code / provider-a/model-a",
      client: "Claude Code",
      key: "claude-cli/2.1.123provider-amodel-a",
      label: "Claude Code / provider-a/model-a",
      model: "model-a",
      provider: "provider-a",
      requestCount: 6,
      errorCount: 1,
      inputTokens: 600,
      outputTokens: 300,
      totalTokens: 900,
      costUsd: 0.02,
      avgDurationMs: 100,
      cacheRatio: 0.3333,
      cacheTokens: 200,
      avgFirstByteMs: 10,
      avgDecodeTps: 0,
      successRate: 0.833,
      maxShare: 1
    },
    {
      caption: "Claude Code / provider-a/model-b",
      client: "Claude Code",
      key: "claude-cli/2.1.123provider-amodel-b",
      label: "Claude Code / provider-a/model-b",
      model: "model-b",
      provider: "provider-a",
      requestCount: 2,
      errorCount: 0,
      inputTokens: 100,
      outputTokens: 0,
      totalTokens: 100,
      costUsd: 0.005,
      avgDurationMs: 90,
      cacheRatio: 0,
      cacheTokens: 0,
      avgFirstByteMs: 8,
      avgDecodeTps: 0,
      successRate: 1,
      maxShare: 0.5
    },
    {
      caption: "curl / provider-b/model-c",
      client: "curl",
      key: "curl/8.5.0provider-bmodel-c",
      label: "curl / provider-b/model-c",
      model: "model-c",
      provider: "provider-b",
      requestCount: 4,
      errorCount: 0,
      inputTokens: 400,
      outputTokens: 0,
      totalTokens: 400,
      costUsd: 0.001,
      avgDurationMs: 70,
      cacheRatio: 0,
      cacheTokens: 0,
      avgFirstByteMs: 5,
      avgDecodeTps: 0,
      successRate: 1,
      maxShare: 0.4
    }
  ],
  generatedAt: new Date().toISOString(),
  modelSeries: [
    {
      model: "model-a",
      points: [
        { bucket: "2026-08-08T00:00:00+00:00", label: "08-08", requestCount: 3, errorCount: 0, inputTokens: 300, outputTokens: 150, totalTokens: 450, costUsd: 0.01, avgDurationMs: 120, cacheRatio: 0, cacheTokens: 0, avgFirstByteMs: 0, avgDecodeTps: 0, successRate: 1 },
        { bucket: "2026-08-09T00:00:00+00:00", label: "08-09", requestCount: 5, errorCount: 1, inputTokens: 500, outputTokens: 250, totalTokens: 750, costUsd: 0.02, avgDurationMs: 130, cacheRatio: 0, cacheTokens: 0, avgFirstByteMs: 0, avgDecodeTps: 0, successRate: 0.8 }
      ]
    },
    {
      model: "model-b",
      points: [
        { bucket: "2026-08-08T00:00:00+00:00", label: "08-08", requestCount: 1, errorCount: 0, inputTokens: 100, outputTokens: 50, totalTokens: 150, costUsd: 0.005, avgDurationMs: 90, cacheRatio: 0, cacheTokens: 0, avgFirstByteMs: 0, avgDecodeTps: 0, successRate: 1 },
        { bucket: "2026-08-09T00:00:00+00:00", label: "08-09", requestCount: 1, errorCount: 0, inputTokens: 100, outputTokens: 50, totalTokens: 150, costUsd: 0.005, avgDurationMs: 95, cacheRatio: 0, cacheTokens: 0, avgFirstByteMs: 0, avgDecodeTps: 0, successRate: 1 }
      ]
    },
    {
      // 少调用但 Token 巨大:用于验证「Token 排行」与「请求排行」顺序确实不同。
      model: "model-c",
      points: [
        { bucket: "2026-08-08T00:00:00+00:00", label: "08-08", requestCount: 1, errorCount: 0, inputTokens: 5000, outputTokens: 0, totalTokens: 5000, costUsd: 0.001, avgDurationMs: 80, cacheRatio: 0, cacheTokens: 0, avgFirstByteMs: 0, avgDecodeTps: 0, successRate: 1 }
      ]
    }
  ],
  models: [
    { caption: "model-a", key: "model-a", label: "model-a", model: "model-a", provider: "provider-1", requestCount: 8, errorCount: 1, inputTokens: 800, outputTokens: 400, totalTokens: 1200, costUsd: 0.03, avgDurationMs: 125, cacheRatio: 0.25, cacheTokens: 200, avgFirstByteMs: 0, avgDecodeTps: 0, successRate: 0.875, maxShare: 1 },
    { caption: "model-b", key: "model-b", label: "model-b", model: "model-b", provider: "provider-1", requestCount: 2, errorCount: 0, inputTokens: 200, outputTokens: 100, totalTokens: 300, costUsd: 0.01, avgDurationMs: 92, cacheRatio: 0, cacheTokens: 0, avgFirstByteMs: 0, avgDecodeTps: 0, successRate: 1, maxShare: 0.25 },
    { caption: "model-c", key: "model-c", label: "model-c", model: "model-c", provider: "provider-1", requestCount: 1, errorCount: 0, inputTokens: 5000, outputTokens: 0, totalTokens: 5000, costUsd: 0.001, avgDurationMs: 80, cacheRatio: 0, cacheTokens: 0, avgFirstByteMs: 0, avgDecodeTps: 0, successRate: 1, maxShare: 1 }
  ],
  providerModels: [],
  range: "7d",
  recentRequests: [],
  series: [
    { bucket: "2026-08-08T00:00:00+00:00", label: "08-08", requestCount: 4, errorCount: 0, inputTokens: 400, outputTokens: 200, totalTokens: 600, costUsd: 0.015, avgDurationMs: 112, cacheRatio: 0, cacheTokens: 0, avgFirstByteMs: 0, avgDecodeTps: 0, successRate: 1 },
    { bucket: "2026-08-09T00:00:00+00:00", label: "08-09", requestCount: 6, errorCount: 1, inputTokens: 600, outputTokens: 300, totalTokens: 900, costUsd: 0.025, avgDurationMs: 124, cacheRatio: 0, cacheTokens: 0, avgFirstByteMs: 0, avgDecodeTps: 0, successRate: 0.833 }
  ],
  totals: { avgDurationMs: 119, cacheRatio: 0.2, cacheTokens: 200, costUsd: 0.04, errorCount: 1, inputTokens: 1000, outputTokens: 500, requestCount: 10, avgFirstByteMs: 0, avgDecodeTps: 0, successRate: 0.9, totalTokens: 1500 }
};

let root: Root | null = null;

function mountView(overrides: {
  client?: string;
  clientOptions?: string[];
  model?: string;
  modelOptions?: string[];
  setClient?: (value: string) => void;
  setModel?: (value: string) => void;
  stats?: UsageStatsSnapshot;
} = {}) {
  const {
    client = "",
    clientOptions = ["Claude Code", "curl"],
    model = "",
    modelOptions = ["provider-a/model-a", "provider-a/model-b"],
    setClient = () => {},
    setModel = () => {},
    stats = richUsageSnapshot
  } = overrides;
  const container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  root.render(
    <AppI18nContext.Provider value={appCopy.zh}>
      <UsageStatsView
        activityModelSeries={richUsageSnapshot.modelSeries}
        activitySeries={richUsageSnapshot.series}
        apiKeys={[{ createdAt: "2026-01-01", id: "key_1", key: "sk-test" }]}
        client={client}
        clientOptions={clientOptions}
        credential=""
        gatewayStatus={{
          endpoint: "http://127.0.0.1:3460",
          generatedConfigFile: "test",
          networkEndpoints: [],
          state: "running"
        }}
        loading={false}
        model={model}
        modelOptions={modelOptions}
        range="7d"
        refreshUsageStats={() => Promise.resolve()}
        setClient={setClient}
        setCredential={() => {}}
        setModel={setModel}
        setRange={() => {}}
        stats={stats}
      />
    </AppI18nContext.Provider>
  );
  return container;
}

async function waitForRender(container: HTMLElement, selector?: string) {
  await waitFor(() => {
    expect(container.textContent ?? "").not.toBe("");
    if (selector) {
      expect(container.querySelector(selector)).not.toBeNull();
    }
  });
  return container;
}

function clickButtonByText(text: string, rootElement: ParentNode = document) {
  const button = Array.from(rootElement.querySelectorAll<HTMLButtonElement>("button"))
    .find((element) => element.textContent?.trim() === text);
  expect(button, `button "${text}" not found`).toBeTruthy();
  button!.click();
}

describe("UsageStatsView", () => {
  beforeEach(() => {
    // jsdom default navigator.language is en-US; force zh so the view renders
    // with the zh copy ("使用统计" page labels).
  });

  afterEach(() => {
    root?.unmount();
    root = null;
    document.body.innerHTML = "";
  });

  it("(1) 总览卡为 2×2：上排 Token/请求，下排缓存命中率/解码速度", async () => {
    const container = await waitForRender(mountView(), '[data-module="usage-trend"]');
    const overview = container.querySelector<HTMLElement>('[data-module="overview"]');
    expect(overview).not.toBeNull();

    const metrics = Array.from(overview!.querySelectorAll<HTMLElement>(".usage-stats-metric"));
    expect(metrics).toHaveLength(4);
    // 顺序即视觉顺序:上排 Token/请求,下排 缓存命中率/解码速度。
    expect(metrics.map((cell) => cell.querySelector("div")?.textContent)).toEqual(["Token", "请求", "缓存命中率", "解码速度"]);
    // (20261102) 健康度 5 行 → 4 行:面板高度 340 → 284,总览卡同步压缩。
    expect(overview!.style.height).toBe("284px");
    expect(container.querySelector<HTMLElement>('[data-module="health"]')!.style.height).toBe("284px");

    // Token 说明行:输入/输出/缓存。
    expect(metrics[0].textContent).toContain("输入");
    expect(metrics[0].textContent).toContain("输出");
    expect(metrics[0].textContent).toContain("缓存");
    // 请求说明行:失败次数 + 成功率(失败为红色语义,数据里有 1 次失败)。
    expect(metrics[1].textContent).toContain("失败");
    expect(metrics[1].textContent).toContain("成功率");
    expect(metrics[1].querySelector(".text-destructive")).not.toBeNull();
    // 缓存命中率 = 20%,带进度条。
    expect(overview!.textContent).toContain("20%");
    expect(metrics[2].querySelector('[class*="rounded-full"]')).not.toBeNull();
    // 输出速度:avgDecodeTps=0 → 占位短横 + 无可计时提示。
    expect(metrics[3].textContent).toContain("—");
    expect(metrics[3].textContent).toContain("没有可测量的解码样本");

    // 旧 8 格指标(压缩节省/响应缓存节省/总请求数等)已随 2×2 改版下线。
    expect(overview!.textContent).not.toContain("压缩节省");
    expect(overview!.textContent).not.toContain("总请求数");
  });

  it("(1b) 阶段四 C-8:总览卡展示首字延迟与吞吐的 P50 / P95", async () => {
    const snapshot: UsageStatsSnapshot = {
      ...richUsageSnapshot,
      totals: {
        ...richUsageSnapshot.totals,
        avgFirstByteMs: 120,
        avgDecodeTps: 48,
        p50FirstByteMs: 110,
        p95FirstByteMs: 2400,
        p50DecodeTps: 45,
        p95DecodeTps: 120
      }
    };
    const container = await waitForRender(mountView({ stats: snapshot }), '[data-module="overview"]');
    const overview = container.querySelector<HTMLElement>('[data-module="overview"]');
    expect(overview).not.toBeNull();
    // 卡头右侧:首字延迟分位数(均值被长尾污染时看 P95)。
    expect(overview!.textContent).toContain("首字延迟");
    expect(overview!.textContent).toContain("P50");
    expect(overview!.textContent).toContain("P95");
    // 输出速度块:均值仍是主数值,分位数在说明行。
    const metrics = Array.from(overview!.querySelectorAll<HTMLElement>(".usage-stats-metric"));
    expect(metrics[3].textContent).toContain("48");
    expect(metrics[3].textContent).toContain("45");
    expect(metrics[3].textContent).toContain("120");
  });

  it("(2) 活跃度热力图带卡片容器（边框+背景），网格与图例保留", async () => {
    const container = await waitForRender(mountView(), '[data-module="activity"]');
    const activity = container.querySelector<HTMLElement>('[data-module="activity"]');
    expect(activity).not.toBeNull();
    // 卡片样式:圆角 + 边框 + 卡片底色(此前是裸容器,与内容混在一起)。
    expect(activity!.className).toContain("rounded-xl");
    expect(activity!.className).toContain("border");
    expect(activity!.className).toContain("bg-card");
    expect(activity!.textContent).toContain("活跃度");
    expect(activity!.textContent).toContain("最近 30 天");
    expect(activity!.querySelector('[role="img"]')).not.toBeNull();
    expect(activity!.querySelectorAll(".overview-activity-cell").length).toBeGreaterThan(0);
  });

  it("(2b) 活跃度浮层走 fixed + portal，不被滚动容器裁剪", async () => {
    const container = await waitForRender(mountView(), '[data-module="activity"]');
    const cell = container.querySelector<HTMLElement>(".overview-activity-cell.cursor-pointer");
    expect(cell).not.toBeNull();
    cell!.dispatchEvent(new MouseEvent("mouseover", { bubbles: true }));
    await waitFor(() => {
      const tooltip = Array.from(document.body.children).find(
        (element) => element.tagName === "SPAN" && element.className.includes("fixed") && element.className.includes("z-30")
      );
      expect(tooltip).toBeTruthy();
      expect(tooltip!.parentElement).toBe(document.body);
    });
  });

  it("(3) 合并用量趋势卡：Token/请求指标 tab + 面积/柱状图标 tab + 右侧模型排行", async () => {
    const container = await waitForRender(mountView(), '[data-module="usage-trend"]');
    const trend = container.querySelector<HTMLElement>('[data-module="usage-trend"]');
    expect(trend).not.toBeNull();
    expect(trend!.textContent).toContain("用量趋势");
    // 默认 Token tab 选中(aria-pressed)。
    const tokenTab = Array.from(trend!.querySelectorAll<HTMLButtonElement>("button"))
      .find((button) => button.textContent?.trim() === "Token");
    expect(tokenTab?.getAttribute("aria-pressed")).toBe("true");
    // 图表默认面积图。
    await waitFor(() => expect(trend!.querySelector(".recharts-area")).not.toBeNull());

    // 右侧排行榜:模型名 + 当前指标数值 + 命中率说明行;默认 Token 指标。
    // 排行榜标题现在位于卡头标题行(不在排行榜容器内)。
    const ranking = trend!.querySelector<HTMLElement>('[data-module="model-ranking"]');
    expect(ranking).not.toBeNull();
    // 20261106:原「模型 Token 排行」标题已换成「模型/客户端」维度 tab →
    // 排行维度改由 aria-label 表达(可见标题不再存在)。
    expect(ranking!.getAttribute("aria-label")).toBe("模型 Token 排行");
    expect(ranking!.textContent).toContain("model-a");
    expect(ranking!.textContent).toContain("model-b");
    expect(ranking!.textContent).toContain("缓存命中率");
    // 费用不再显示(20261102)。
    expect(ranking!.textContent).not.toContain("$");
    // 卡头行内不再有旧的小字号标题(caption)残留在排行榜实体里。
    expect(ranking!.querySelector("div")?.textContent).not.toContain("排行");

    // 旧的两张分析卡已合并下线。
    expect(container.querySelector('[data-module="token-consumption"]')).toBeNull();
    expect(container.querySelector('[data-module="model-calls"]')).toBeNull();
  });

  it("指标 tab 切换图表内容：Token ↔ 请求 均保持图表挂载", async () => {
    const container = await waitForRender(mountView(), '[data-module="usage-trend"]');
    const trend = container.querySelector<HTMLElement>('[data-module="usage-trend"]')!;
    const requestTab = Array.from(trend.querySelectorAll<HTMLButtonElement>("button"))
      .find((button) => button.textContent?.trim() === "请求");
    expect(requestTab).toBeTruthy();

    requestTab!.click();
    await waitFor(() => expect(requestTab!.getAttribute("aria-pressed")).toBe("true"));
    expect(trend.querySelector(".recharts-area")).not.toBeNull();

    clickButtonByText("Token", trend);
    await waitFor(() => expect(trend.querySelector(".recharts-area")).not.toBeNull());
  });

  it("排行榜随指标切换排序：Token 降序 ↔ 请求降序，且不设条数上限", async () => {
    const container = await waitForRender(mountView(), '[data-module="usage-trend"]');
    const trend = container.querySelector<HTMLElement>('[data-module="usage-trend"]')!;
    const ranking = trend.querySelector<HTMLElement>('[data-module="model-ranking"]')!;
    const names = () => Array.from(ranking.querySelectorAll<HTMLElement>("span[title]")).map((el) => el.textContent);

    // 默认 Token 指标:按 totalTokens 降序(model-c 5000 > model-a 1200 > model-b 300)。
    expect(ranking.getAttribute("aria-label")).toBe("模型 Token 排行");
    expect(names()).toEqual(["model-c", "model-a", "model-b"]);
    // 全部 3 个模型都进排行(不 slice 上限)。
    expect(names()).toHaveLength(3);

    // 切到请求:按 requestCount 降序(model-a 8 > model-b 2 > model-c 1)。
    clickButtonByText("请求", trend);
    await waitFor(() => expect(names()).toEqual(["model-a", "model-b", "model-c"]));
    expect(ranking.getAttribute("aria-label")).toBe("模型调用排行");
    // 费用列已移除;说明行保留另一维度 + 命中率。
    expect(ranking.textContent).not.toContain("$");
    expect(ranking.textContent).toContain("缓存命中率");
  });

  it("面积/柱状图标 tab 切换图型", async () => {
    const container = await waitForRender(mountView(), '[data-module="usage-trend"]');
    const trend = container.querySelector<HTMLElement>('[data-module="usage-trend"]')!;

    const barButton = trend.querySelector<HTMLButtonElement>('button[aria-label="柱状图"]');
    expect(barButton).not.toBeNull();
    barButton!.click();
    await waitFor(() => expect(trend.querySelector(".recharts-bar")).not.toBeNull());

    const areaButton = trend.querySelector<HTMLButtonElement>('button[aria-label="面积图"]');
    expect(areaButton).not.toBeNull();
    areaButton!.click();
    await waitFor(() => expect(trend.querySelector(".recharts-area")).not.toBeNull());
  });

  it("工具栏提供模型筛选下拉：选项来自 modelOptions，受控回显，选中回调携带完整 label", async () => {
    const container = await waitForRender(mountView(), ".overview-toolbar");

    // 三个筛选控件：时间段分段钮 + 路由密钥 + 模型。
    const modelSelect = container.querySelector<HTMLSelectElement>('select[aria-label="模型"]');
    expect(modelSelect).not.toBeNull();
    const credentialSelect = container.querySelector<HTMLSelectElement>('select[aria-label="路由密钥"]');
    expect(credentialSelect).not.toBeNull();

    // 选项 = "全部模型" + modelOptions（"供应商/模型名" label）。
    const optionLabels = Array.from(modelSelect!.querySelectorAll("option")).map((option) => option.textContent);
    expect(optionLabels).toEqual(["全部模型", "provider-a/model-a", "provider-a/model-b"]);

    // 默认选中"全部模型"（空值）。
    expect(modelSelect!.value).toBe("");

    // 受控回显：mount 即带选中值。
    root?.unmount();
    document.body.innerHTML = "";
    const preset = await waitForRender(mountView({ model: "provider-a/model-b" }), ".overview-toolbar");
    expect(preset.querySelector<HTMLSelectElement>('select[aria-label="模型"]')!.value).toBe("provider-a/model-b");

    // 选中回调携带完整 label（后端按 label 匹配）。
    let selected: string | undefined;
    root?.unmount();
    document.body.innerHTML = "";
    const interactive = await waitForRender(mountView({ setModel: (value) => { selected = value; } }), ".overview-toolbar");
    const select = interactive.querySelector<HTMLSelectElement>('select[aria-label="模型"]')!;
    const valueSetter = Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value")!.set!;
    valueSetter.call(select, "provider-a/model-a");
    select.dispatchEvent(new Event("change", { bubbles: true }));
    expect(selected).toBe("provider-a/model-a");
  });

  // 输出速度块的口径 tooltip:说明推理 Token 已被扣除(旧文案说的是
  // "Output tokens",与新口径不符)。
  it("输出速度 tooltip 说明推理 Token 已扣除", async () => {
    const container = await waitForRender(mountView(), ".overview-toolbar");

    const overview = container.querySelector<HTMLElement>('[data-module="overview"]')!;
    const metrics = Array.from(overview.querySelectorAll<HTMLElement>(".usage-stats-metric"));
    expect(metrics).toHaveLength(4);
    const speedTitle = metrics[3].getAttribute("title") ?? "";
    expect(speedTitle).toMatch(/思考 Token|reasoning tokens/i);
  });

  // 20261104_01 阶段三 · B-1a + 20261106(B-1b):客户端用量不再单独成卡,
  // 改由「用量趋势」卡右侧排行列承载(卡头「模型/客户端」tab 切维度);
  // 行数据仍按**纯 client** 归并 —— 同一客户端的多模型行合成一条。
  it("「客户端」维度 tab:排行按纯 client 归并(同一客户端的多模型行合成一条)", async () => {
    const container = await waitForRender(mountView(), '[data-module="usage-trend"]');
    const trend = container.querySelector<HTMLElement>('[data-module="usage-trend"]')!;
    // 独立「客户端用量」卡已下线。
    expect(container.querySelector('[data-module="client-usage"]')).toBeNull();

    // 卡头的「模型/客户端」tab:默认「模型」选中。
    const modelsTab = trend.querySelector<HTMLButtonElement>('[data-module="ranking-dimension-models"]');
    const clientsTab = trend.querySelector<HTMLButtonElement>('[data-module="ranking-dimension-clients"]');
    expect(modelsTab?.getAttribute("aria-pressed")).toBe("true");
    expect(clientsTab?.getAttribute("aria-pressed")).toBe("false");
    // 归一口径说明挂在客户端 tab 上(悬浮提示)。
    expect(clientsTab?.getAttribute("title") ?? "").toContain("User-Agent");

    clientsTab!.click();
    await waitFor(() => expect(trend.querySelector('[data-module="client-ranking"]')).not.toBeNull());

    const ranking = trend.querySelector<HTMLElement>('[data-module="client-ranking"]')!;
    const names = () => Array.from(ranking.querySelectorAll<HTMLElement>("[data-ranking-name]"))
      .map((row) => row.getAttribute("data-ranking-name"));
    // 两个调用方(claude-cli 两行归并成一条 + curl 一条)。
    expect(names()).toEqual(["Claude Code", "curl"]);
    // claude-cli:请求 6+2=8、Token 900+100=1000、输入 700、缓存 200 →
    // 命中率 200/700(与后端 cache_ratio 口径一致:cache/input)。
    expect(ranking.textContent).toContain("Claude Code");
    expect(ranking.textContent).toContain("1,000");
    expect(ranking.textContent).toContain("缓存命中率");
    expect(ranking.textContent).toContain("29%");
    // 用到的模型(展示名)挪到悬浮提示(窄列里放不下)。
    const title = ranking.querySelector<HTMLElement>('[data-ranking-name="Claude Code"] span[title]')?.getAttribute("title") ?? "";
    expect(title).toContain("provider-a/model-a");
    expect(title).toContain("provider-a/model-b");
    // curl:无缓存 → 命中率 0%。
    expect(ranking.textContent).toContain("0%");
  });

  it("工具栏提供调用方筛选下拉：选项是纯 client 串(去重),受控回显 + 回调", async () => {
    const container = await waitForRender(mountView(), ".overview-toolbar");
    const clientSelect = container.querySelector<HTMLSelectElement>('select[aria-label="客户端"]');
    expect(clientSelect).not.toBeNull();
    // 选项 = "全部客户端" + 去重后的纯 client(不是 "{client} / {模型}")。
    // 20261106(B-1b):client 是**归一 id**(产品名),不再是原始 UA。
    const optionLabels = Array.from(clientSelect!.querySelectorAll("option")).map((option) => option.textContent);
    expect(optionLabels).toEqual(["全部客户端", "Claude Code", "curl"]);
    expect(clientSelect!.value).toBe("");

    // 受控回显。
    root?.unmount();
    document.body.innerHTML = "";
    const preset = await waitForRender(mountView({ client: "curl" }), ".overview-toolbar");
    expect(preset.querySelector<HTMLSelectElement>('select[aria-label="客户端"]')!.value).toBe("curl");

    // 选中回调携带纯 client 串(后端按归一 id 匹配)。
    let selected: string | undefined;
    root?.unmount();
    document.body.innerHTML = "";
    const interactive = await waitForRender(mountView({ setClient: (value) => { selected = value; } }), ".overview-toolbar");
    const select = interactive.querySelector<HTMLSelectElement>('select[aria-label="客户端"]')!;
    const valueSetter = Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value")!.set!;
    valueSetter.call(select, "Claude Code");
    select.dispatchEvent(new Event("change", { bubbles: true }));
    expect(selected).toBe("Claude Code");
  });

  it("客户端排行随指标 tab 切换排序：Token 降序 ↔ 请求降序", async () => {
    const container = await waitForRender(mountView(), '[data-module="usage-trend"]');
    const trend = container.querySelector<HTMLElement>('[data-module="usage-trend"]')!;
    clickButtonByText("客户端", trend);
    await waitFor(() => expect(trend.querySelector('[data-module="client-ranking"]')).not.toBeNull());
    const ranking = trend.querySelector<HTMLElement>('[data-module="client-ranking"]')!;
    const names = () => Array.from(ranking.querySelectorAll<HTMLElement>("[data-ranking-name]"))
      .map((row) => row.getAttribute("data-ranking-name"));
    const primary = (client: string) => ranking
      .querySelector<HTMLElement>(`[data-ranking-name="${client}"] [data-ranking-value]`)
      ?.textContent ?? "";

    // 默认 Token 指标:Claude Code 1000 > curl 400。
    expect(names()).toEqual(["Claude Code", "curl"]);
    expect(primary("Claude Code")).toBe("1,000");
    expect(primary("curl")).toBe("400");

    // 切到请求指标:主数值换成请求数(Token 降到说明行)。
    clickButtonByText("请求", trend);
    await waitFor(() => expect(primary("Claude Code")).toBe("8"));
    expect(primary("curl")).toBe("4");
    expect(names()).toEqual(["Claude Code", "curl"]);
  });
});
