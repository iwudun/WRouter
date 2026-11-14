// Regression tests for the connectivity check dialog (add/edit provider):
// 1. After a check completes, the parent (add-provider draft panel) recomputes
//    a fresh `models` array on every render — that re-render must NOT wipe the
//    just-displayed result (it used to flash and disappear).
// 2. Reopening the dialog resets the selection to all models and clears the
//    previous result.
import { waitFor } from "@testing-library/react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ProviderConnectivityCheckDialog } from "@/pages/home/components/providers/provider-connectivity";
import type { ProviderConnectivityCheckReport } from "@/pages/home/shared/types";

const passedReport: ProviderConnectivityCheckReport = {
  failed: [],
  passed: [
    {
      message: "ok",
      model: "model-a",
      protocols: [
        { endpoint: "https://example.test/v1/chat/completions", message: "ok", protocol: "openai_chat_completions", supported: true }
      ],
      supported: true
    }
  ],
  results: []
};

let root: Root | null = null;
let container: HTMLElement | null = null;

function mountDialog(models: string[], onCheck: (models: string[]) => Promise<ProviderConnectivityCheckReport>, open = true) {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  root.render(<ProviderConnectivityCheckDialog models={models} onCheck={onCheck} onClose={() => {}} open={open} />);
  return container;
}

async function clickButtonByText(text: string) {
  await waitFor(() => {
    const button = Array.from(container!.querySelectorAll<HTMLButtonElement>("button"))
      .find((element) => element.textContent?.trim() === text && !element.disabled);
    expect(button, `enabled button "${text}" not found`).toBeTruthy();
    button!.click();
  });
}

afterEach(() => {
  root?.unmount();
  root = null;
  container?.remove();
  container = null;
  document.body.innerHTML = "";
});

describe("ProviderConnectivityCheckDialog", () => {
  it("检测完成后父组件以新的 models 数组引用重渲染，结果不被清空", async () => {
    const onCheck = vi.fn(async () => passedReport);
    const models = ["model-a"];
    const c = mountDialog(models, onCheck);

    await clickButtonByText("Start check");
    await waitFor(() => {
      expect(c.textContent).toContain("Check results");
    });

    // 模拟添加供应商面板的每次重渲染：同内容、新数组引用。等待一个真实的
    // 调度窗口，确保旧缺陷（effect 清空结果）有机会暴露，避免竞态假阴性。
    root!.render(<ProviderConnectivityCheckDialog models={[...models]} onCheck={onCheck} onClose={() => {}} open={true} />);
    await new Promise((resolve) => setTimeout(resolve, 300));

    await waitFor(() => {
      expect(c.textContent).toContain("Check results");
      expect(c.textContent).toContain("model-a");
    });
    expect(onCheck).toHaveBeenCalledTimes(1);
  });

  it("关闭后重新打开对话框时，重新勾选全部模型并清空上一次结果", async () => {
    const onCheck = vi.fn(async () => passedReport);
    const models = ["model-a"];
    const c = mountDialog(models, onCheck);

    await clickButtonByText("Start check");
    await waitFor(() => {
      expect(c.textContent).toContain("Check results");
    });

    root!.render(<ProviderConnectivityCheckDialog models={models} onCheck={onCheck} onClose={() => {}} open={false} />);
    await waitFor(() => {
      expect(c.textContent).not.toContain("Check Availability");
    });
    root!.render(<ProviderConnectivityCheckDialog models={models} onCheck={onCheck} onClose={() => {}} open={true} />);

    await waitFor(() => {
      expect(c.textContent).not.toContain("Check results");
    });
    await clickButtonByText("Start check");
    await waitFor(() => {
      expect(onCheck).toHaveBeenCalledTimes(2);
    });
    expect(onCheck).toHaveBeenLastCalledWith(["model-a"]);
  });
});
