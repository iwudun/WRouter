// 20261017_01:添加模型弹窗「获取模型」面板测试。
//  - 入口可见性:仅新增模式 + 宿主传入 fetchModels 时渲染;
//  - 获取成功 → 面板(搜索/全选/清空/返回);已存在模型禁选 + 「Added」徽标;
//  - 单选确认走 onSave(单模型路径),多选确认走 onSaveMany(表单元数据值
//    作批量默认值)且按钮计数;
//  - 获取失败 → inline 红字,不进面板;Back 返回手填模式。
import { act, fireEvent, waitFor } from "@testing-library/react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ModelFormDialog, type ModelFormFields } from "@/pages/home/components/providers/provider-model-form";

let root: Root | null = null;
let container: HTMLElement | null = null;

async function mountDialog(overrides: {
  existingModels?: string[];
  fetchModels?: () => Promise<string[]>;
  mode?: "add" | "edit";
  onSave?: (name: string, fields: ModelFormFields) => void;
  onSaveMany?: (names: string[], fields: ModelFormFields) => void;
} = {}) {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  root.render(
    <ModelFormDialog
      existingModels={overrides.existingModels ?? []}
      fetchModels={overrides.fetchModels}
      mode={overrides.mode ?? "add"}
      onClose={() => undefined}
      onSave={overrides.onSave ?? (() => undefined)}
      onSaveMany={overrides.onSaveMany}
      open
    />
  );
  // 表单在「open 上升沿」的**挂载 effect** 里初始化字段
  // (provider-model-form.tsx:72-85 的 setName("") 等)。React 18 的 passive
  // effect 是异步调度的 → 挂载后必须等它跑完再交互,否则刚输入的值会被
  // 初始化 effect 重置掉(全量并行负载下偶发:Add 按钮一直保持禁用)。
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

function buttonWithText(text: string): HTMLButtonElement | undefined {
  return Array.from(container!.querySelectorAll("button")).find(
    (button) => button.textContent?.trim() === text
  );
}

// 本仓库 Checkbox 是原生 <input type="checkbox">(aria-checked,无 role)。
function checkboxes(): HTMLInputElement[] {
  return Array.from(container!.querySelectorAll<HTMLInputElement>("div.max-h-64 input[type=\"checkbox\"]"));
}

describe("ModelFormDialog 获取模型面板", () => {
  it("编辑模式或未传 fetchModels 时不显示入口;手填路径不变", async () => {
    await mountDialog({ mode: "edit" });
    await waitFor(() => {
      expect(container!.textContent).toContain("Edit model");
    });
    expect(buttonWithText("Fetch models")).toBeUndefined();

    root!.unmount();
    container!.remove();
    await mountDialog({ mode: "add" });
    await waitFor(() => {
      expect(container!.querySelector<HTMLInputElement>("input[placeholder='Model name']")).not.toBeNull();
    });
    expect(buttonWithText("Fetch models")).toBeUndefined();
    // 手填:输入名称后 Add 可用。
    const nameInput = container!.querySelector<HTMLInputElement>("input[placeholder='Model name']");
    fireEvent.change(nameInput!, { target: { value: "m1" } });
    await waitFor(() => {
      expect(buttonWithText("Add")?.disabled).toBe(false);
    });
  });

  it("获取成功进面板:已存在禁选 + Added 徽标;多选批量确认走 onSaveMany", async () => {
    const onSave = vi.fn();
    const onSaveMany = vi.fn();
    await mountDialog({
      existingModels: ["m1"],
      fetchModels: async () => ["m1", "m2", "m3"],
      onSave,
      onSaveMany
    });
    await waitFor(() => {
      expect(buttonWithText("Fetch models")).toBeDefined();
    });
    fireEvent.click(buttonWithText("Fetch models")!);
    await waitFor(() => {
      expect(container!.querySelector("input[placeholder='Search models']")).not.toBeNull();
    });
    // 三行,首行已存在禁选 + Added 徽标。
    await waitFor(() => {
      expect(checkboxes()).toHaveLength(3);
    });
    expect(checkboxes()[0]!.disabled).toBe(true);
    expect(container!.textContent).toContain("Added");

    // Add 在未选中时禁用。
    expect(buttonWithText("Add")?.disabled).toBe(true);

    // 勾选 m2/m3(索引 1、2)→ 按钮变批量计数。
    fireEvent.click(checkboxes()[1]!);
    fireEvent.click(checkboxes()[2]!);
    const bulk = buttonWithText("Add model (2)");
    expect(bulk).toBeDefined();
    expect(bulk!.disabled).toBe(false);

    // 确认前调元数据:上下文窗口 200000 作为批量默认值。
    const contextInput = container!.querySelector<HTMLInputElement>("input[type='number']");
    fireEvent.change(contextInput!, { target: { value: "200000" } });
    fireEvent.click(bulk!);

    await waitFor(() => {
      expect(onSaveMany).toHaveBeenCalledTimes(1);
    });
    expect(onSaveMany).toHaveBeenCalledWith(["m2", "m3"], {
      contextWindow: 200000,
      inputTypes: ["text"],
      maxOutputTokens: 128000
    });
    expect(onSave).not.toHaveBeenCalled();
  });

  it("单选确认走单模型 onSave;Select all 只选未存在项", async () => {
    const onSave = vi.fn();
    const onSaveMany = vi.fn();
    await mountDialog({
      existingModels: ["m1"],
      fetchModels: async () => ["m1", "m2", "m3"],
      onSave,
      onSaveMany
    });
    await waitFor(() => {
      expect(buttonWithText("Fetch models")).toBeDefined();
    });
    fireEvent.click(buttonWithText("Fetch models")!);
    await waitFor(() => {
      expect(checkboxes()).toHaveLength(3);
    });
    fireEvent.click(buttonWithText("Select all")!);
    // Select all 跳过已存在的 m1 → 2 个选中。
    expect(buttonWithText("Add model (2)")).toBeDefined();
    fireEvent.click(buttonWithText("Clear")!);
    expect(buttonWithText("Add")?.disabled).toBe(true);

    // 勾选单个 → 单模型路径。
    fireEvent.click(checkboxes()[2]!);
    await waitFor(() => {
      expect(buttonWithText("Add")?.disabled).toBe(false);
    });
    fireEvent.click(buttonWithText("Add")!);
    expect(onSave).toHaveBeenCalledTimes(1);
    expect(onSave).toHaveBeenCalledWith("m3", {
      contextWindow: 1000000,
      inputTypes: ["text"],
      maxOutputTokens: 128000
    });
    expect(onSaveMany).not.toHaveBeenCalled();
  });

  it("搜索过滤 + Back 返回手填模式", async () => {
    await mountDialog({ fetchModels: async () => ["m1", "m2"] });
    await waitFor(() => {
      expect(buttonWithText("Fetch models")).toBeDefined();
    });
    fireEvent.click(buttonWithText("Fetch models")!);
    await waitFor(() => {
      expect(checkboxes()).toHaveLength(2);
    });
    const search = container!.querySelector<HTMLInputElement>("input[placeholder='Search models']");
    fireEvent.change(search!, { target: { value: "m2" } });
    await waitFor(() => {
      expect(checkboxes()).toHaveLength(1);
    });
    expect(container!.textContent).not.toContain("m1");
    // 无匹配占位。
    fireEvent.change(search!, { target: { value: "zz" } });
    await waitFor(() => {
      expect(container!.textContent).toContain("No matching models");
    });

    fireEvent.click(buttonWithText("Back")!);
    await waitFor(() => {
      expect(container!.querySelector("input[placeholder='Model name']")).not.toBeNull();
    });
    expect(buttonWithText("Fetch models")).toBeDefined();
  });

  it("获取失败 inline 红字且不进面板", async () => {
    await mountDialog({ fetchModels: async () => {
      throw new Error("Endpoint returned HTTP 403.");
    } });
    await waitFor(() => {
      expect(buttonWithText("Fetch models")).toBeDefined();
    });
    fireEvent.click(buttonWithText("Fetch models")!);
    await waitFor(() => {
      expect(container!.textContent).toContain("Endpoint returned HTTP 403.");
    });
    // 仍处手填模式。
    expect(container!.querySelector("input[placeholder='Model name']")).not.toBeNull();
    expect(container!.querySelector("input[placeholder='Search models']")).toBeNull();
  });
});
