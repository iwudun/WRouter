// 模型指纹库独立页的前端测试(方案 20261030_01 F10;2026-11-01 从设置分区
// 升级为主导航独立视图)。
//
// 覆盖:
// 1. 采集向导的入库前置校验:X7「疑似同一模型」与「样本不足」两种后端拒绝
//    都必须**原文 inline 展示**(不弹 toast、不静默失败);
// 2. 自建库列表渲染(全局候选库 + 供应商锚点分组)+ 两个库区块**可折叠**;
// 3. 搜索框按模型名 / 家族 / 采集通道过滤条目。
import { waitFor } from "@testing-library/react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { FingerprintLibraryView } from "@/pages/home/components/model-fingerprints";
import type { FingerprintState } from "@/pages/home/hooks/useFingerprintState";
import type { FingerprintListResponse, GatewayProviderConfig } from "@/contracts/app";

const library: FingerprintListResponse = {
  entries: [
    {
      cvAccuracy: 0.963,
      family: "gpt",
      familyName: "GPT",
      id: "global:-:gpt-x",
      kind: "global",
      label: "gpt-x",
      modelName: "gpt-x",
      probeEnv: "env-1,env-2,env-3",
      probeSuite: "wrouter-v1",
      sampleCount: 12
    },
    {
      cvAccuracy: 0.91,
      id: "anchor:p1:anchor-y",
      kind: "anchor",
      label: "anchor-y",
      modelName: "anchor-y",
      probeEnv: "env-1,env-2",
      probeSuite: "wrouter-v1",
      providerId: "p1",
      sampleCount: 6
    }
  ]
};

const providers: GatewayProviderConfig[] = [
  { id: "p1", models: ["model-a", "model-b"], name: "Provider One" }
];

function makeState(overrides: Partial<FingerprintState> = {}): FingerprintState {
  return {
    busy: false,
    enroll: vi.fn(async () => ({
      entry: {
        id: "global:-:model-a",
        kind: "global" as const,
        label: "model-a",
        modelName: "model-a",
        probeSuite: "wrouter-v1" as const,
        sampleCount: 12
      },
      gaps: 0,
      probeSeed: "seed",
      probeTier: "standard" as const
    })),
    error: "",
    exportLibrary: vi.fn(async () => undefined),
    importLibrary: vi.fn(async () => undefined),
    library,
    loading: false,
    refresh: vi.fn(async () => undefined),
    removeEntry: vi.fn(async () => true),
    ...overrides
  };
}

let root: Root | null = null;
let container: HTMLElement | null = null;

async function mountPage(state: FingerprintState) {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  root.render(<FingerprintLibraryView providers={providers} state={state} />);
  // React 18 的 createRoot().render() 是并发的 —— 等首帧落地再断言。
  await waitFor(() => {
    expect(container!.querySelectorAll("section").length).toBeGreaterThan(0);
  });
  return container;
}

async function clickButtonByText(text: string) {
  await waitFor(() => {
    const button = Array.from(container!.querySelectorAll<HTMLButtonElement>("button"))
      .find((element) => {
        const label = element.textContent?.trim() ?? "";
        return (label === text || label.startsWith(text)) && !element.disabled;
      });
    expect(button, `enabled button "${text}" not found`).toBeTruthy();
    button!.click();
  });
}

function setInputValue(label: string, value: string) {
  const input = container!.querySelector<HTMLInputElement>(`input[aria-label="${label}"]`);
  expect(input, `input "${label}" not found`).toBeTruthy();
  const setter = Object.getOwnPropertyDescriptor(
    window.HTMLInputElement.prototype,
    "value"
  )?.set;
  setter?.call(input, value);
  input!.dispatchEvent(new Event("input", { bubbles: true }));
}

afterEach(() => {
  root?.unmount();
  root = null;
  container?.remove();
  container = null;
  document.body.innerHTML = "";
});

describe("FingerprintLibraryView", () => {
  it("自建库列表渲染:全局候选库 + 供应商锚点分组", async () => {
    const c = await mountPage(makeState());
    expect(c.textContent).toContain("Global candidate library");
    expect(c.textContent).toContain("gpt-x");
    expect(c.textContent).toContain("96%"); // 单探针准确率
    expect(c.textContent).toContain("Provider anchors");
    // 锚点按供应商分组折叠,默认收起 → 锚点条目不直接可见。
    expect(c.textContent).not.toContain("anchor-y");
    await clickButtonByText("Provider One");
    await waitFor(() => {
      expect(c.textContent).toContain("anchor-y");
    });
  });

  it("库区块可折叠:点击标题行收起 / 再展开内容", async () => {
    const c = await mountPage(makeState());
    const section = c.querySelectorAll("section")[0];
    const header = section.querySelector<HTMLButtonElement>("button[aria-expanded]");
    expect(header, "collapsible header not found").toBeTruthy();
    expect(header!.getAttribute("aria-expanded")).toBe("true");
    expect(section.textContent).toContain("gpt-x");

    header!.click();
    await waitFor(() => {
      expect(header!.getAttribute("aria-expanded")).toBe("false");
      expect(section.textContent).not.toContain("gpt-x");
    });

    header!.click();
    await waitFor(() => {
      expect(header!.getAttribute("aria-expanded")).toBe("true");
      expect(section.textContent).toContain("gpt-x");
    });
  });

  it("搜索框过滤条目:命中自建条目、无命中显示空态", async () => {
    const c = await mountPage(makeState());
    setInputValue("Search fingerprint entries", "gpt");
    await waitFor(() => {
      expect(c.textContent).toContain("gpt-x");
    });
    setInputValue("Search fingerprint entries", "no-such-model");
    await waitFor(() => {
      expect(c.textContent).toContain("No matching fingerprint entries");
    });
  });

  it("工具行「检测模型指纹」打开检测弹窗(检测入口已集中到本页)", async () => {
    const c = await mountPage(makeState());
    await clickButtonByText("Detect model fingerprint");
    await waitFor(() => {
      // 检测弹窗的供应商选择与运行按钮落地。
      expect(c.querySelector('select[aria-label="Provider"]')).toBeTruthy();
      expect(c.textContent).toContain("Start detection");
      // 默认选中第一个供应商。
      expect((c.querySelector('select[aria-label="Provider"]') as HTMLSelectElement).value).toBe("p1");
    });
  });

  it("采集向导:后端拒绝(X7 疑似同一模型)原文 inline 展示", async () => {
    const duplicateMessage =
      "疑似与既有条目 'gpt-x' 是同一个模型(重复收录会让 top-1 概率塌陷)。请改用别名,或直接复用既有条目。";
    const enroll = vi.fn(async () => {
      throw new Error(duplicateMessage);
    });
    const c = await mountPage(makeState({ enroll: enroll as never }));

    await clickButtonByText("Add fingerprint");
    await waitFor(() => {
      expect(c.textContent).toContain("Confirm and save");
    });
    setInputValue("Model name", "model-a");
    await clickButtonByText("Confirm and save");
    await waitFor(() => {
      expect(c.textContent).toContain(duplicateMessage);
    });
    expect(enroll).toHaveBeenCalledTimes(1);
  });

  it("采集向导:样本不足被拒时原文 inline 展示", async () => {
    const shortageMessage =
      "采集失败:没有一条探针拿到可用回答(上游拒答或严重截断)。";
    const enroll = vi.fn(async () => {
      throw new Error(shortageMessage);
    });
    const c = await mountPage(makeState({ enroll: enroll as never }));

    await clickButtonByText("Add fingerprint");
    await waitFor(() => {
      expect(c.textContent).toContain("Confirm and save");
    });
    setInputValue("Model name", "model-b");
    await clickButtonByText("Confirm and save");
    await waitFor(() => {
      expect(c.textContent).toContain(shortageMessage);
    });
  });

  it("导出有落地动作:触发下载并展示成功摘要", async () => {
    // jsdom 没有 Blob URL 实现 → stub 后断言下载链路被走到。
    const createObjectURL = vi.fn(() => "blob:mock");
    const revokeObjectURL = vi.fn();
    Object.defineProperty(URL, "createObjectURL", {
      value: createObjectURL,
      configurable: true
    });
    Object.defineProperty(URL, "revokeObjectURL", {
      value: revokeObjectURL,
      configurable: true
    });
    const exportLibrary = vi.fn(async () => ({
      ok: true,
      json: '{"schema":"robust-number-fingerprint-bank"}',
      modelCount: 2,
      sampleCount: 4,
      bankIncluded: true,
      probeSuite: "wrouter-v1" as const,
      reason: null
    }));
    const c = await mountPage(makeState({ exportLibrary: exportLibrary as never }));

    await clickButtonByText("Export");
    await waitFor(() => {
      expect(createObjectURL).toHaveBeenCalledTimes(1);
      expect(revokeObjectURL).toHaveBeenCalledTimes(1);
      expect(c.textContent).toContain("Fingerprint library exported.");
    });
    expect(c.textContent).toContain("Models: 2");
    expect(c.textContent).toContain("Samples: 4");
  });

  it("导入展示 imported/skipped 摘要(结果不再被丢弃)", async () => {
    const importLibrary = vi.fn(async () => ({
      ok: true,
      imported: [{ model: "m1", id: "global:-:m1" }],
      skipped: [{ model: "m2", reason: "duplicate" }],
      probeSuite: "wrouter-v1" as const
    }));
    const c = await mountPage(makeState({ importLibrary: importLibrary as never }));

    const input = Array.from(c.querySelectorAll<HTMLInputElement>('input[type="file"]'))[0];
    expect(input, "import file input not found").toBeTruthy();
    const file = new File(["{}"], "library.json", { type: "application/json" });
    Object.defineProperty(input, "files", { value: [file], configurable: true });
    input.dispatchEvent(new Event("change", { bubbles: true }));

    await waitFor(() => {
      expect(c.textContent).toContain("Fingerprint library imported.");
    });
    expect(c.textContent).toContain("Imported: 1");
    expect(c.textContent).toContain("Skipped: 1");
  });
});
