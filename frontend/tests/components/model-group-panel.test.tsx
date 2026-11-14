// 20260905_04 M6:聚合模型编辑面板组件测试。
// 2026-09-26 二轮:分模式头部 —— 新建=取消/添加/叉号(草稿),编辑=启用/已
// 禁用+删除(即时落库);名称失焦提交。
// 2026-09-26 三轮:名称 = 只读 WRouter 前缀 + 输入框;目标行单行化(手柄/
// 权重/状态点);添加收敛为 快捷添加(弹窗)+ 自定义添加(未保存行);优先级
// 不再有数字输入,由排列顺序决定。
// 2026-09-26 四轮:章节定名「路由策略/路由模型列表」;负载均衡权重恒显示;
// 重复路由模型选项级禁用;状态圆点恒显示(有效绿/失效琥珀);弹窗更名。
import { fireEvent, waitFor } from "@testing-library/react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { GatewayProviderConfig, ModelGroupConfig } from "@/contracts/app";
import { ModelGroupPanel } from "@/pages/home/components/providers/model-group-panel";

const providers = [
  { id: "p1", models: ["gpt-4o", "m1"], name: "Primary" } as GatewayProviderConfig,
  { id: "p2", models: ["gpt-4o"], name: "Secondary" } as GatewayProviderConfig
];

function makeDraft(overrides: Partial<ModelGroupConfig> = {}): ModelGroupConfig {
  return {
    enabled: true,
    id: "g1",
    name: "smart-router",
    strategy: "first",
    targets: [
      { model: "gpt-4o", providerId: "p1" },
      { model: "gpt-4o", providerId: "p2" }
    ],
    ...overrides
  };
}

function makeProps(draft: ModelGroupConfig = makeDraft()) {
  return {
    draft,
    error: "",
    groups: [draft],
    isEdit: true,
    onChange: vi.fn(),
    onClose: vi.fn(),
    onDelete: vi.fn(),
    onQuickAdd: vi.fn(),
    onAddTarget: vi.fn(),
    onRemoveTarget: vi.fn(),
    onReorderTargets: vi.fn(),
    onSubmit: vi.fn(async () => true),
    onToggleEnabled: vi.fn(),
    onUpdateTarget: vi.fn(),
    providers
  };
}

let root: Root | null = null;
let container: HTMLElement | null = null;

function mountPanel(props: ReturnType<typeof makeProps>) {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  root.render(<ModelGroupPanel {...props} />);
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

function queryByText(fragment: string): Element | undefined {
  return Array.from(container!.querySelectorAll("*")).find(
    (node) => node.children.length === 0 && node.textContent?.includes(fragment)
  );
}

function findButton(label: string): HTMLButtonElement | undefined {
  return Array.from(container!.querySelectorAll("button")).find(
    (button) => button.textContent?.trim() === label
  );
}

describe("ModelGroupPanel 编辑模式(即时落库)", () => {
  it("renders the draft name and target rows", async () => {
    mountPanel(makeProps());
    await waitFor(() => {
      const nameInput = container!.querySelector<HTMLInputElement>('input[aria-label="Group name"]');
      expect(nameInput?.value).toBe("smart-router");
      expect(container!.querySelectorAll('button[aria-label="Remove target"]').length).toBe(2);
    });
  });

  it("名称 = 只读 WRouter 前缀 + 可输入聚合名;章节为 路由策略/目标模型池", async () => {
    mountPanel(makeProps());
    await waitFor(() => {
      const prefix = container!.querySelector<HTMLInputElement>('input[readonly]');
      expect(prefix?.value).toBe("WRouter");
      expect(prefix?.disabled).toBe(false);
      expect(queryByText("Routing strategy")).toBeDefined();
      expect(queryByText("Target model pool")).toBeDefined();
      // 旧章节名不再出现
      expect(queryByText("Target models")).toBeUndefined();
      expect(queryByText("Strategy")).toBeUndefined();
      expect(queryByText("Aggregate strategy")).toBeUndefined();
      expect(queryByText("Routing targets")).toBeUndefined();
      expect(queryByText("Routing model list")).toBeUndefined();
    });
  });

  it("优先级无数字输入;负载均衡权重恒显示(文字+输入框),失焦提交", async () => {
    const props = makeProps();
    mountPanel(props);
    await waitFor(() => {
      expect(container!.querySelector('input[aria-label="Load balancing weight"]')).toBeDefined();
    });
    expect(queryByText("Load balancing weight")).toBeDefined();
    expect(container!.querySelector('input[aria-label="Priority"]')).toBeNull();
    const weightInput = container!.querySelector<HTMLInputElement>('input[aria-label="Load balancing weight"]')!;
    fireEvent.change(weightInput, { target: { value: "5" } });
    expect(props.onUpdateTarget).not.toHaveBeenCalled();
    fireEvent.blur(weightInput);
    expect(props.onUpdateTarget).toHaveBeenCalledWith(0, { weight: 5 });
  });

  it("重复路由模型在选项级禁用(选不出来,而非添加后被静默丢弃);跨供应商同名不受限", async () => {
    const draft = makeDraft({
      targets: [
        { model: "gpt-4o", providerId: "p1" },
        { model: "m1", providerId: "p1" },
        { model: "gpt-4o", providerId: "p2" }
      ]
    });
    mountPanel(makeProps(draft));
    await waitFor(() => {
      expect(container!.querySelectorAll('select[aria-label="Model"]').length).toBe(3);
    });
    const modelSelects = Array.from(container!.querySelectorAll<HTMLSelectElement>('select[aria-label="Model"]'));
    const optionOf = (select: HTMLSelectElement, value: string) =>
      Array.from(select.options).find((option) => option.value === value)!;
    // 行2(p1/m1):同供应商 p1 的 gpt-4o 已被行1占用 → 禁用;自身 m1 可选。
    expect(optionOf(modelSelects[1]!, "gpt-4o").disabled).toBe(true);
    expect(optionOf(modelSelects[1]!, "m1").disabled).toBe(false);
    // 行3(p2/gpt-4o):gpt-4o 被行1(p1)占用但供应商不同 → 不受限。
    expect(optionOf(modelSelects[2]!, "gpt-4o").disabled).toBe(false);
  });

  it("自定义添加:点击在列表底部追加未保存行,移除即回收", async () => {
    const props = makeProps();
    mountPanel(props);
    await waitFor(() => {
      expect(container!.querySelectorAll('button[aria-label="Remove target"]').length).toBe(2);
    });
    fireEvent.click(findButton("Custom add")!);
    expect(container!.querySelectorAll('button[aria-label="Remove target"]').length).toBe(3);
    fireEvent.click(findButton("Custom add")!);
    expect(container!.querySelectorAll('button[aria-label="Remove target"]').length).toBe(4);
    // 未保存行不入草稿:未触发 onAddTarget
    expect(props.onAddTarget).not.toHaveBeenCalled();
    // 逐行移除未保存行
    const removeButtons = () => Array.from(container!.querySelectorAll('button[aria-label="Remove target"]'));
    fireEvent.click(removeButtons()[3]!);
    fireEvent.click(removeButtons()[2]!);
    expect(container!.querySelectorAll('button[aria-label="Remove target"]').length).toBe(2);
  });

  it("快捷添加:弹窗带常驻说明,默认全选供应商,可取消勾选后只添加部分", async () => {
    const props = makeProps();
    mountPanel(props);
    await waitFor(() => {
      expect(findButton("Quick add")).toBeDefined();
    });
    fireEvent.click(findButton("Quick add")!);
    await waitFor(() => {
      expect(queryByText("Quick add target models")).toBeDefined();
    });
    // 功能说明常驻在「模型」章节上方
    expect(queryByText("Pick a model name and check the providers to add; one target will be added per checked provider.")).toBeDefined();
    // p1/p2 都提供 gpt-4o → 默认全选
    const modelSelect = container!.querySelector<HTMLSelectElement>('select[aria-label="Select model"]')!;
    fireEvent.change(modelSelect, { target: { value: "gpt-4o" } });
    await waitFor(() => {
      expect(container!.querySelectorAll('input[type="checkbox"]').length).toBe(2);
    });
    // 取消勾选第一个供应商(Primary)→ 只添加 Secondary
    fireEvent.click(container!.querySelectorAll<HTMLInputElement>('input[type="checkbox"]')[0]!);
    fireEvent.click(findButton("Add")!);
    expect(props.onQuickAdd).toHaveBeenCalledTimes(1);
    expect(props.onQuickAdd).toHaveBeenCalledWith([{ model: "gpt-4o", providerId: "p2" }]);
    expect(queryByText("Quick add target models")).toBeUndefined();
  });

  it("状态圆点恒显示:失效(供应商/模型被删)琥珀警示,有效绿色", async () => {
    const draft = makeDraft({
      targets: [
        { model: "m1", providerId: "ghost" },
        { model: "gone", providerId: "p1" },
        { model: "m1", providerId: "p1" }
      ]
    });
    mountPanel(makeProps(draft));
    await waitFor(() => {
      expect(container!.querySelector('span[title="Provider has been removed."]')).toBeDefined();
      expect(container!.querySelector('span[title="Model has been removed from the provider."]')).toBeDefined();
    });
    expect(container!.querySelectorAll('span[title="Provider has been removed."]').length).toBe(1);
    // 恒显:3 条目 3 个点,有效的那个是绿色"模型可用"。
    expect(container!.querySelectorAll('span[title="Model available."]').length).toBe(1);
  });

  it("头部 = 启用/已禁用 + 删除,无保存/取消按钮", async () => {
    mountPanel(makeProps());
    await waitFor(() => {
      expect(container!.querySelector('div[aria-label="Status"]')).toBeDefined();
      expect(container!.querySelector('button[aria-label="Delete model group"]')).toBeDefined();
    });
    // 2026-09-26:编辑模式标题展示统一调用前缀 WRouter/<聚合名>。
    expect(queryByText("WRouter/smart-router")).toBeDefined();
    expect(findButton("Save")).toBeUndefined();
    expect(findButton("Cancel")).toBeUndefined();
  });

  it("flags duplicate group names", async () => {
    const draft = makeDraft();
    const other: ModelGroupConfig = { ...makeDraft(), id: "g2", name: "Smart-Router" };
    const props = { ...makeProps(draft), groups: [other] };
    mountPanel(props);
    await waitFor(() => {
      expect(queryByText("Model group name already exists.")).toBeDefined();
    });
  });

  it("名称失焦提交(onChange 携带新名称)", async () => {
    const props = makeProps();
    mountPanel(props);
    await waitFor(() => {
      expect(container!.querySelector('input[aria-label="Group name"]')).toBeDefined();
    });
    const nameInput = container!.querySelector<HTMLInputElement>('input[aria-label="Group name"]')!;
    fireEvent.change(nameInput, { target: { value: "renamed-group" } });
    expect(props.onChange).not.toHaveBeenCalled();
    fireEvent.blur(nameInput);
    expect(props.onChange).toHaveBeenCalledWith({ name: "renamed-group" });
  });

  it("removes a target row and toggles group enabled via the pill toggle", async () => {
    const props = makeProps();
    mountPanel(props);
    await waitFor(() => {
      expect(container!.querySelectorAll('button[aria-label="Remove target"]').length).toBe(2);
    });
    fireEvent.click(container!.querySelectorAll('button[aria-label="Remove target"]')[1]);
    expect(props.onRemoveTarget).toHaveBeenCalledWith(1);
    const toggleGroup = container!.querySelector<HTMLDivElement>('div[aria-label="Status"]');
    expect(toggleGroup, "header status toggle not found").toBeTruthy();
    const disabledButton = Array.from(toggleGroup!.querySelectorAll("button"))
      .find((button) => button.textContent === "Disabled");
    expect(disabledButton).toBeTruthy();
    fireEvent.click(disabledButton!);
    expect(props.onToggleEnabled).toHaveBeenCalledWith(false);
    expect(props.onChange).not.toHaveBeenCalled();
  });

  it("shows the delete button only in edit mode", async () => {
    const editProps = makeProps();
    mountPanel(editProps);
    await waitFor(() => {
      expect(container!.querySelector('button[aria-label="Delete model group"]')).toBeDefined();
    });
    root!.unmount();
    container!.remove();
    const addProps = makeProps(makeDraft({ id: "new" }));
    addProps.isEdit = false;
    mountPanel(addProps);
    await waitFor(() => {
      expect(container!.querySelector('input[aria-label="Group name"]')).toBeDefined();
    });
    expect(container!.querySelector('button[aria-label="Delete model group"]')).toBeNull();
  });
});

describe("ModelGroupPanel 新建模式(草稿,对齐添加供应商)", () => {
  function makeAddProps() {
    const props = makeProps(makeDraft({ id: "new", name: "" }));
    props.isEdit = false;
    return props;
  }

  it("标题为添加聚合模型,头部 = 取消/添加/叉号,无启停与删除", async () => {
    mountPanel(makeAddProps());
    await waitFor(() => {
      expect(queryByText("Add model group")).toBeDefined();
      expect(findButton("Cancel")).toBeDefined();
      expect(findButton("Add")).toBeDefined();
      expect(container!.querySelector('button[aria-label="Close dialog"]')).toBeDefined();
    });
    expect(container!.querySelector('div[aria-label="Status"]')).toBeNull();
    expect(container!.querySelector('button[aria-label="Delete model group"]')).toBeNull();
  });

  it("添加按钮触发 onSubmit,取消/叉号触发 onClose", async () => {
    const props = makeAddProps();
    mountPanel(props);
    const add = await waitFor(() => {
      const button = findButton("Add");
      expect(button).toBeDefined();
      return button!;
    });
    fireEvent.click(add);
    await waitFor(() => expect(props.onSubmit).toHaveBeenCalledTimes(1));
    fireEvent.click(container!.querySelector('button[aria-label="Close dialog"]')!);
    expect(props.onClose).toHaveBeenCalledTimes(1);
    fireEvent.click(findButton("Cancel")!);
    expect(props.onClose).toHaveBeenCalledTimes(2);
  });

  it("新建模式名称逐键同步进草稿", async () => {
    const props = makeAddProps();
    mountPanel(props);
    await waitFor(() => {
      expect(container!.querySelector('input[aria-label="Group name"]')).toBeDefined();
    });
    const nameInput = container!.querySelector<HTMLInputElement>('input[aria-label="Group name"]')!;
    fireEvent.change(nameInput, { target: { value: "agg" } });
    expect(props.onChange).toHaveBeenCalledWith({ name: "agg" });
  });
});
