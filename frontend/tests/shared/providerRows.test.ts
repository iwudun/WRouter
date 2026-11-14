import { describe, expect, it } from "vitest";
import type { GatewayProviderConfig } from "@/contracts/app";
import { providerListRowEqual } from "@/pages/home/components/providers/providers-list";
import { providerModelRowEqual } from "@/pages/home/components/providers/provider-detail-panel";
import { providerIsEnabled } from "@/pages/home/shared/providers";

/**
 * F4.3 行级 memo 佐证:comparator 决定 SSE config-changed 失效 → refetch 后
 * 哪些行可以跳过重渲染。这里逐字段钉住比较面 —— 展示面(名称/图标/启停/选中)
 * 未变的行必须命中 memo,展示面变化的行必须重渲染。
 */

function makeProvider(patch: Partial<GatewayProviderConfig> = {}): GatewayProviderConfig {
  return {
    name: "DeepSeek",
    api_base_url: "https://api.deepseek.com",
    api_key: "sk-test",
    models: ["deepseek-chat"],
    enabled: true,
    ...patch
  } as GatewayProviderConfig;
}

function makeRow(provider: GatewayProviderConfig, patch: Record<string, unknown> = {}) {
  return {
    dragging: false,
    index: 0,
    modelCount: 1,
    provider,
    proxyLabel: "跟随全局默认",
    selected: false,
    showDragHandle: true,
    onDragStart: () => {},
    onSelect: () => {},
    ...patch
  };
}

describe("providerListRowEqual(侧栏行 memo)", () => {
  it("同一 provider 引用且选中态不变时命中 memo", () => {
    const provider = makeProvider();
    const row = makeRow(provider);
    expect(providerListRowEqual(row, { ...row, onSelect: () => {} })).toBe(true);
  });

  it("不同引用但展示面(名称/图标/启停)等价时命中 memo", () => {
    const row = makeRow(makeProvider());
    const refetched = { ...row, provider: makeProvider() };
    expect(providerListRowEqual(row, refetched)).toBe(true);
  });

  it("名称、启停或选中态变化时重渲染", () => {
    const row = makeRow(makeProvider());
    expect(providerListRowEqual(row, { ...row, provider: makeProvider({ name: "改名" }) })).toBe(false);
    expect(providerListRowEqual(row, { ...row, provider: makeProvider({ enabled: false }) })).toBe(false);
    expect(providerListRowEqual(row, { ...row, selected: true })).toBe(false);
    expect(providerIsEnabled(makeProvider({ enabled: false }))).toBe(false);
  });

  it("拖拽重排后 index 或 dragging 变化时重渲染(2026-09-13 拖拽排序)", () => {
    // index 在比较面:onSelect 闭包与选中态都依赖位置;dragging 在比较面:
    // 被拖行要挂 z-10 + 阴影。
    const row = makeRow(makeProvider());
    expect(providerListRowEqual(row, { ...row, index: 2 })).toBe(false);
    expect(providerListRowEqual(row, { ...row, dragging: true })).toBe(false);
  });

  it("代理来源变化时重渲染(20260906_01 F3)", () => {
    // provider 引用没变(改的是全局预设名),但摘要文案变了 —— 不比较就会
    // 让行内徽标/tooltip 停留在旧出口。
    const row = makeRow(makeProvider());
    expect(providerListRowEqual(row, { ...row, proxyLabel: "Clash 本地" })).toBe(false);
    expect(providerListRowEqual(row, { ...row, proxyOverride: "direct" as const })).toBe(false);
    expect(providerListRowEqual(row, { ...row, proxyOverride: "preset" as const, proxyLabel: "Clash 本地" })).toBe(false);
  });

  it("模型数变化时重渲染(行内「N 模型」小字)", () => {
    // provider 引用没变是不可能的(加模型必然换对象),但 comparator 按
    // 名称/图标/启停短路会漏掉模型数,这里钉住 modelCount 必须参与比较。
    const row = makeRow(makeProvider());
    expect(providerListRowEqual(row, { ...row, modelCount: 2 })).toBe(false);
  });
});

describe("providerModelRowEqual(详情面板模型行 memo)", () => {
  it("同一 provider 引用且模型名不变时命中 memo", () => {
    const provider = makeProvider();
    const row = { index: 0, model: "deepseek-chat", notify: () => {}, onEditModel: () => {}, provider, removeModel: () => {} };
    expect(providerModelRowEqual(row, { ...row, notify: () => {} })).toBe(true);
  });

  it("仅其他 provider 的字段变化(名称/模型集/输入类型映射)时重渲染", () => {
    const row = { index: 0, model: "deepseek-chat", notify: () => {}, onEditModel: () => {}, provider: makeProvider(), removeModel: () => {} };
    expect(providerModelRowEqual(row, { ...row, provider: makeProvider({ name: "改名" }) })).toBe(false);
    expect(providerModelRowEqual(row, { ...row, provider: makeProvider({ models: ["other"] }) })).toBe(false);
    expect(providerModelRowEqual(row, { ...row, provider: makeProvider({ modelInputTypes: { "deepseek-chat": ["image"] } }) })).toBe(false);
    expect(providerModelRowEqual(row, { ...row, model: "other-model" })).toBe(false);
  });

  it("上下文窗口/输入类型 map 引用变化时重渲染(2026-09-10 视觉徽标)", () => {
    // 行内渲染「N K/M」与「视觉」徽标,两个 map 都在比较面上:同一次 refetch
    // 中被触碰的 map 换了引用就必须重绘,这里是引用比较语义的钉子。
    const row = { index: 0, model: "deepseek-chat", notify: () => {}, onEditModel: () => {}, provider: makeProvider({ modelContextWindow: { "deepseek-chat": 1000000 }, modelInputTypes: { "deepseek-chat": ["image"] } }), removeModel: () => {} };
    const sameMaps = { ...row, provider: row.provider };
    const refetched = { ...row, provider: makeProvider({ modelContextWindow: { "deepseek-chat": 1000000 }, modelInputTypes: { "deepseek-chat": ["image"] } }) };
    expect(providerModelRowEqual(row, sameMaps)).toBe(true);
    expect(providerModelRowEqual(row, refetched)).toBe(false);
  });
});
