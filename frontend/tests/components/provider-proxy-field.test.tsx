// 方案 20260906_01 F3:供应商「网络代理」字段组件测试。
//  - 三档位(跟随全局默认/直连/自定义代理);
//  - 2026-09-13 二轮:两个选择框恒显示 —— 非自定义档位时预设框冻结
//    (disabled + 占位),选中自定义代理时可用且显示「名称 · host:port」;
//  - 验收 #5:预设列表为空时出现「去设置页配置代理」并可跳转;
//  - 引用失效(预设被删)时自动收敛到第一个可用预设,避免显示与落库不一致。
import { fireEvent, waitFor } from "@testing-library/react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ProviderProxyOverride, ProxyPreset } from "@/contracts/app";
import { ProviderProxyField } from "@/pages/home/components/providers/provider-proxy-field";

const presets: ProxyPreset[] = [
  { id: "clash", name: "Clash 本地", password: "", port: 7890, server: "127.0.0.1", username: "" },
  { id: "corp", name: "公司网关", password: "secret", port: 8080, server: "gw.corp", username: "u" }
];

function makeProps(
  overrides: Partial<{
    mode: ProviderProxyOverride["mode"];
    onOpenProxySettings: () => void;
    presetId: string;
    presets: ProxyPreset[];
  }> = {}
) {
  return {
    mode: "inherit" as ProviderProxyOverride["mode"],
    onChange: vi.fn(),
    onOpenProxySettings: vi.fn(),
    presetId: "",
    presets,
    ...overrides
  };
}

let root: Root | null = null;
let container: HTMLElement | null = null;

function mountField(props: ReturnType<typeof makeProps>) {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  root.render(<ProviderProxyField {...props} />);
  return container;
}

afterEach(() => {
  if (root) {
    root.unmount();
    root = null;
  }
  container?.remove();
  container = null;
  vi.restoreAllMocks();
});

function modeSelect(): HTMLSelectElement {
  const select = container!.querySelector<HTMLSelectElement>('select[aria-label="Network proxy"]');
  expect(select, "应渲染「网络代理」档位下拉").not.toBeNull();
  return select!;
}

function presetSelect(): HTMLSelectElement | null {
  return container!.querySelector<HTMLSelectElement>('select[aria-label="Custom proxies"]');
}

describe("ProviderProxyField", () => {
  it("渲染三档位;非自定义档位下预设框冻结显示占位(恒不消失)", async () => {
    mountField(makeProps());
    await waitFor(() => {
      const options = Array.from(modeSelect().options).map((option) => option.value);
      expect(options).toEqual(["inherit", "direct", "preset"]);
      // 2026-09-13 二轮:预设框恒显示,未选自定义代理时冻结 + 占位。
      const preset = presetSelect();
      expect(preset, "预设框不应消失").not.toBeNull();
      expect(preset!.disabled).toBe(true);
      expect(preset!.value).toBe("");
    });
  });

  it("选「自定义代理」后预设框启用,只显示名称与 host:port", async () => {
    const props = makeProps({ mode: "preset", presetId: "clash" });
    mountField(props);
    await waitFor(() => {
      const preset = presetSelect();
      expect(preset).not.toBeNull();
      expect(preset!.disabled).toBe(false);
    });
    const labels = Array.from(presetSelect()!.options)
      .filter((option) => option.value !== "")
      .map((option) => option.textContent);
    expect(labels).toEqual(["Clash 本地 (127.0.0.1:7890)", "公司网关 (gw.corp:8080)"]);
    // 决策 #10:凭据不得出现在任何展示面。
    expect(container!.textContent).not.toContain("secret");

    fireEvent.change(modeSelect(), { target: { value: "inherit" } });
    expect(props.onChange).toHaveBeenLastCalledWith("inherit", "");
  });

  it("选「直连」上抛 direct 且清空预设引用", async () => {
    const props = makeProps();
    mountField(props);
    await waitFor(() => expect(modeSelect()).toBeTruthy());
    fireEvent.change(modeSelect(), { target: { value: "direct" } });
    expect(props.onChange).toHaveBeenCalledWith("direct", "");
  });

  it("切到「自定义代理」时默认选中第一个预设(省一次点击)", async () => {
    const props = makeProps();
    mountField(props);
    await waitFor(() => expect(modeSelect()).toBeTruthy());
    fireEvent.change(modeSelect(), { target: { value: "preset" } });
    expect(props.onChange).toHaveBeenCalledWith("preset", "clash");
  });

  it("验收 #5(交互):预设列表为空时选「自定义代理」上抛空引用(调用方据此不落库)", async () => {
    const props = makeProps({ presets: [] });
    mountField(props);
    await waitFor(() => expect(modeSelect()).toBeTruthy());
    fireEvent.change(modeSelect(), { target: { value: "preset" } });
    expect(props.onChange).toHaveBeenCalledWith("preset", "");
  });

  it("验收 #5(空态):就地提示「还没有自定义代理」并可跳转设置页", async () => {
    // 组件受控:空态由父级回写的 mode="preset" 驱动,这里直接以该档位挂载。
    const props = makeProps({ mode: "preset", presets: [] });
    mountField(props);
    await waitFor(() => {
      const hint = Array.from(container!.querySelectorAll("*")).find(
        (node) => node.children.length === 0 && node.textContent === "No custom proxies yet."
      );
      expect(hint, "应就地提示还没有自定义代理").toBeTruthy();
    });

    const jump = Array.from(container!.querySelectorAll("button")).find(
      (button) => button.textContent === "Configure proxies in Settings"
    );
    expect(jump, "应提供跳转设置页的按钮").toBeTruthy();
    fireEvent.click(jump!);
    expect(props.onOpenProxySettings).toHaveBeenCalledTimes(1);
    // 预设框仍在(恒显示),但列表为空时冻结。
    expect(presetSelect()).not.toBeNull();
    expect(presetSelect()!.disabled).toBe(true);
  });

  it("引用失效(预设被删)时只做显示回退,不自作主张回写", async () => {
    // 归一由父级负责(normalizeProviderProxy → inherit)。字段若自行回写,会
    // 与父级的写回互相触发形成死循环 —— 删除被引用的预设时整页崩溃
    // (Maximum update depth exceeded),这里把「不回写」钉住。
    const props = makeProps({ mode: "preset", presetId: "gone" });
    mountField(props);
    await waitFor(() => expect(presetSelect()).not.toBeNull());
    expect(presetSelect()!.disabled).toBe(false);
    expect(props.onChange).not.toHaveBeenCalled();
  });

  it("预设为空且引用失效时不做收敛(保持在空态引导)", async () => {
    const props = makeProps({ mode: "preset", presetId: "gone", presets: [] });
    mountField(props);
    await waitFor(() => expect(container!.textContent).toContain("No custom proxies yet."));
    expect(props.onChange).not.toHaveBeenCalled();
  });
});
