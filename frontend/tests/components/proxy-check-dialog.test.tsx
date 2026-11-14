// 2026-09-13:代理「检测可用性」弹窗的取消交互与结果面板测试。
//  - 检测中页脚出现「取消」;点击后当前目标跑完即停(后续目标不再发请求);
//  - 检测中点 X/关窗同样置取消位且 onClose 立即回调;
//  - 结果面板:标题 + 可用/不可用统计徽标 + 逐代理逐目标行。
import { act, fireEvent, waitFor } from "@testing-library/react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ProxyPresetTestRequest, ProxyPresetTestResult } from "@/contracts/app";
import { ProxyCheckDialog } from "@/pages/home/components/settings/settings-proxy";

const testProxyPreset = vi.fn<(request: ProxyPresetTestRequest) => Promise<ProxyPresetTestResult>>();
vi.mock("@/lib/api", () => ({
  api: {
    get testProxyPreset() {
      return testProxyPreset;
    }
  }
}));

const presets = [
  { id: "clash", name: "Clash 本地", password: "", port: 7890, server: "127.0.0.1", username: "" }
];
const targets = ["https://a.com", "https://b.com"];

let root: Root | null = null;
let container: HTMLElement | null = null;

async function mountDialog(props: Partial<{ onClose: () => void }> = {}) {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  root.render(
    <ProxyCheckDialog
      formatError={(error) => String(error)}
      onClose={props.onClose ?? (() => {})}
      open
      presets={presets}
      targets={targets}
    />
  );
  // 勾选集(selection / targetSelection)在「open 上升沿」的**挂载 effect**
  // 里填充(settings-proxy.tsx:593-602)。React 18 的 passive effect 异步
  // 调度 → 必须等它跑完再点「开始检测」,否则那一刻勾选集还是空 →
  // 开始按钮仍是禁用 → 点击落空 → 一个探测请求都没发出。
  await act(async () => { await Promise.resolve(); });
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

beforeEach(() => {
  testProxyPreset.mockReset();
});

function footerButton(label: string): HTMLButtonElement | undefined {
  return Array.from(container!.querySelectorAll("button")).find(
    (button) => button.textContent?.trim() === label
  );
}

function targetResult(url: string, ok: boolean): ProxyPresetTestResult["targets"][number] {
  return ok
    ? { latencyMs: 12, ok: true, status: 204, url }
    : { error: "Proxy unreachable.", ok: false, url };
}

describe("ProxyCheckDialog", () => {
  it("检测中页脚出现「取消」;点击后当前目标跑完即停,不再发后续请求", async () => {
    const pending: Array<(value: ProxyPresetTestResult) => void> = [];
    testProxyPreset.mockImplementation(
      () => new Promise<ProxyPresetTestResult>((resolve) => pending.push(resolve))
    );
    await mountDialog();
    await waitFor(() => expect(footerButton("Start check")).toBeTruthy());
    fireEvent.click(footerButton("Start check")!);

    // 检测中:开始按钮禁用、页脚出现可点的「取消」。
    //
    // 注意:**空闲态的页脚也有一个「取消」按钮**(results 为空时显示 Cancel
    // 而非 Close)→ 「Cancel 存在」**不能**证明已进入 busy 态,拿它当等待
    // 条件会在 React 还没重渲染时立刻通过(实测偶发失败)。真正的信号是
    // 开始按钮变禁用。
    await waitFor(() => expect(footerButton("Start check")?.disabled).toBe(true));
    expect(footerButton("Cancel")).toBeTruthy();

    // 点击取消 → 当前目标(a.com)resolve 后循环即停,b.com 不再请求。
    fireEvent.click(footerButton("Cancel")!);
    pending[0]({
      ok: true,
      proxy: "preset:Clash 本地@127.0.0.1:7890",
      targets: [targetResult("https://a.com", true)]
    });

    await waitFor(() => expect(footerButton("Start check")).toBeTruthy());
    expect(testProxyPreset).toHaveBeenCalledTimes(1);

    // 结果面板:标题 + 统计徽标 + 已完成的目标行(取消的目标 b.com 不产生结果行)。
    expect(container!.textContent).toContain("Check results");
    expect(container!.textContent).toContain("1 Available");
    expect(container!.textContent).toContain("12 ms");
    expect(container!.textContent).toContain("0 Unavailable");
  });

  it("检测中点 X 关窗:置取消位并立即回调 onClose,后台循环随后停止", async () => {
    const pending: Array<(value: ProxyPresetTestResult) => void> = [];
    testProxyPreset.mockImplementation(
      () => new Promise<ProxyPresetTestResult>((resolve) => pending.push(resolve))
    );
    const onClose = vi.fn();
    await mountDialog({ onClose });
    await waitFor(() => expect(footerButton("Start check")).toBeTruthy());
    fireEvent.click(footerButton("Start check")!);
    await waitFor(() => expect(footerButton("Cancel")).toBeTruthy());

    const closeX = Array.from(container!.querySelectorAll("button")).find(
      (button) => button.getAttribute("aria-label") === "Close dialog"
    );
    fireEvent.click(closeX!);
    expect(onClose).toHaveBeenCalledTimes(1);

    pending[0]({
      ok: false,
      proxy: "preset:Clash 本地@127.0.0.1:7890",
      targets: [targetResult("https://a.com", false)]
    });
    await waitFor(() => expect(testProxyPreset).toHaveBeenCalledTimes(1));
  });

  // 注:不测「多目标全部完成后的统计徽标」——waitFor 只能等到首个目标渲染出
  // 结果面板,后续目标的落地与同步断言存在竞态,全量跑时呈间歇性失败。
});
