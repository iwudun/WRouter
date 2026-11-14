// K6/K7 (方案 20260905_02;2026-09-06 四轮调整):API密钥多密钥区块组件测试
// (并入详情面板 API key 章节;健康态 stats 由面板查询后经 props 下发)。
//  - 历史数据兼容:挂载时为无名称存量密钥补默认名「密钥 N」;
//  - 徽章渲染 冷却中(剩余时间) / 正常 / 禁用;解冷却触发 stats 重取;
//  - 固定模式单启用走 setProviderFixedKey 单次写入;负载均衡自由启停;
//  - 密钥内联编辑(原 WR 方式):眼睛切明文、blur 保存、空值取消;
//  - 名称不可重名;删除/weight 提交走 setProviderAccounts。
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, waitFor } from "@testing-library/react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { GatewayProviderConfig, ProviderCredentialAccountStat, ProviderCredentialStats } from "@/contracts/app";
import { ProviderApiKeysSection } from "@/pages/home/components/providers/provider-api-keys";
import { mockWindowWr } from "../helpers/mockWindowWr";

const statsPayload: ProviderCredentialStats = {
  providers: [
    {
      providerId: "p1",
      accounts: [
        { accountId: "a1", cooldownRemainingMs: 45_000, cooldownReason: "HTTP 429", lastStatusCode: 429, state: "cooling" },
        { accountId: "a2", cooldownRemainingMs: 0, state: "ok" }
      ]
    }
  ]
};

function statsMap(payload: ProviderCredentialStats, providerId = "p1"): Map<string, ProviderCredentialAccountStat> {
  const map = new Map<string, ProviderCredentialAccountStat>();
  for (const stat of payload.providers.find((item) => item.providerId === providerId)?.accounts ?? []) {
    map.set(stat.accountId, stat);
  }
  return map;
}

const provider: GatewayProviderConfig = {
  accounts: [
    { api_key: "sk-main-plaintext", enabled: true, id: "a1", label: "Main", priority: 0, weight: 2 },
    { enabled: true, id: "a2", label: "Backup", priority: 0, weight: 1 },
    { enabled: false, id: "a3", label: "Off", priority: 0, weight: 1 }
  ],
  credentialStrategy: "first",
  id: "p1",
  models: ["model-a"],
  name: "P1"
};

function makeProps(config: GatewayProviderConfig = provider) {
  return {
    index: 0,
    notify: vi.fn(),
    onStatsRefetch: vi.fn(async () => {}),
    provider: config,
    setProviderAccounts: vi.fn(),
    setProviderFixedKey: vi.fn(),
    statsByAccountId: statsMap(statsPayload),
    statsFetchedAt: Date.now()
  };
}

let root: Root | null = null;
let container: HTMLElement | null = null;

function mountSection(props: ReturnType<typeof makeProps>) {
  mockWindowWr({
    clearCredentialCooldown: vi.fn(async () => ({ accountId: "a1", cleared: true, providerId: "p1" }))
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } });
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  root.render(
    <QueryClientProvider client={client}>
      <ProviderApiKeysSection {...props} />
    </QueryClientProvider>
  );
  return container;
}

function findButtonByAriaLabel(label: string, scope: HTMLElement = container!) {
  const button = Array.from(scope.querySelectorAll<HTMLButtonElement>("button")).find(
    (element) => element.getAttribute("aria-label") === label
  );
  expect(button, `button "${label}" not found`).toBeTruthy();
  return button!;
}

function findSwitch(checked: boolean, scope: HTMLElement = container!) {
  const toggle = Array.from(
    scope.querySelectorAll<HTMLInputElement>('input[role="switch"][title="Key status"]')
  ).find((element) => element.getAttribute("aria-checked") === String(checked));
  expect(toggle, `switch checked=${checked} not found`).toBeTruthy();
  return toggle!;
}

afterEach(() => {
  root?.unmount();
  root = null;
  container?.remove();
  container = null;
  document.body.innerHTML = "";
  vi.restoreAllMocks();
});

describe("ProviderApiKeysSection (K6/K7)", () => {
  it("历史数据兼容:挂载时为无名称的存量密钥补默认名「密钥 N」", async () => {
    const unlabeled: GatewayProviderConfig = {
      ...provider,
      accounts: [
        { api_key: "sk-1", enabled: true, id: "a1" },
        { enabled: true, id: "a2" },
        { enabled: false, id: "a3", label: "Backup" }
      ]
    };
    const props = makeProps(unlabeled);
    mountSection(props);
    await waitFor(() => {
      expect(props.setProviderAccounts).toHaveBeenCalled();
    });
    const filled = props.setProviderAccounts.mock.calls.at(-1)![1];
    // 测试环境语言为 en → 词条 "Key label" = "Key"。
    expect(filled.find((account: { id?: string }) => account.id === "a1").label).toBe("Key 1");
    expect(filled.find((account: { id?: string }) => account.id === "a2").label).toBe("Key 2");
    expect(filled.find((account: { id?: string }) => account.id === "a3").label).toBe("Backup");
    // (输入框回显依赖 provider prop 回流,单测 mock 不回流,不在此断言。)
  });

  it("徽章显示 冷却中(剩余时间+原因)/正常/禁用;stats 由 props 下发", async () => {
    const c = mountSection(makeProps());
    await waitFor(() => {
      expect(c.textContent).toContain("Cooling down");
      expect(c.textContent).toContain("Healthy");
    });
    // 冷却徽章带剩余时间与原因(title);禁用徽章取自配置而非 stats。
    // (状态列外层还有包裹 span,按 title 定位 Badge 本体。)
    const coolingBadge = Array.from(c.querySelectorAll("span")).find((element) =>
      element.getAttribute("title")?.includes("HTTP 429")
    );
    expect(coolingBadge?.textContent).toMatch(/Cooling down \d+:\d\d/);
    expect(c.textContent).toContain("Disabled");
  });

  it("「立即解冷却」调用 clearCredentialCooldown 并触发 stats 重取", async () => {
    const props = makeProps();
    const c = mountSection(props);

    await waitFor(() => {
      expect(c.textContent).toContain("Cooling down");
    });
    fireEvent.click(findButtonByAriaLabel("Clear cooldown"));

    await waitFor(() => {
      expect(props.notify).toHaveBeenCalledWith("Cooldown cleared");
    });
    expect(props.onStatsRefetch).toHaveBeenCalledTimes(1);
  });

  it("固定密钥模式单启用:走 setProviderFixedKey 单次写入(修双写覆盖);负载均衡自由启停", async () => {
    // 固定密钥(first):a3 开启 → 单次 setProviderFixedKey(a3)。
    const props = makeProps();
    const c = mountSection(props);
    await waitFor(() => {
      expect(c.querySelector('input[role="switch"]')).toBeTruthy();
    });
    fireEvent.click(findSwitch(false));
    expect(props.setProviderFixedKey).toHaveBeenCalledWith(0, "a3");

    // 负载均衡(balanced):开关只翻转本行(setProviderAccounts),不触碰固定记忆。
    const balancedProps = makeProps({ ...provider, credentialStrategy: "balanced" });
    const balancedView = mountSection(balancedProps);
    await waitFor(() => {
      expect(balancedView.querySelector('input[role="switch"]')).toBeTruthy();
    });
    fireEvent.click(findSwitch(false, balancedView));
    const balanced = balancedProps.setProviderAccounts.mock.calls.at(-1)![1];
    expect(balanced.find((account: { id?: string }) => account.id === "a3").enabled).toBe(true);
    expect(balanced.find((account: { id?: string }) => account.id === "a1").enabled).toBe(true);
    expect(balancedProps.setProviderFixedKey).not.toHaveBeenCalled();
  });

  it("密钥名称不能重名:冲突提交被拒并提示,配置不变", async () => {
    const props = makeProps();
    const c = mountSection(props);
    await waitFor(() => {
      expect(c.querySelector('input[aria-label="Key name"]')).toBeTruthy();
    });

    // a2(Backup)的名称改成 a1 的 "Main" → 回退 + 提示,不落配置。
    const callsBefore = props.setProviderAccounts.mock.calls.length;
    const labelInputs = Array.from(
      c.querySelectorAll<HTMLInputElement>('input[aria-label="Key name"]')
    );
    expect(labelInputs.length).toBe(3);
    fireEvent.change(labelInputs[1], { target: { value: "Main" } });
    fireEvent.blur(labelInputs[1]);
    expect(props.notify).toHaveBeenCalledWith("Key name already exists.");
    expect(props.setProviderAccounts.mock.calls.length).toBe(callsBefore);

    // 非重名正常提交。
    fireEvent.change(labelInputs[1], { target: { value: "Spare" } });
    fireEvent.blur(labelInputs[1]);
    const renamed = props.setProviderAccounts.mock.calls.at(-1)![1];
    expect(renamed.find((account: { id?: string }) => account.id === "a2").label).toBe("Spare");
  });

  it("密钥内联编辑(原 WR 方式):眼睛切明文,blur 保存,空值取消", async () => {
    const props = makeProps();
    const c = mountSection(props);
    await waitFor(() => {
      expect(c.querySelector('input[role="switch"]')).toBeTruthy();
    });

    // 默认 password 掩码;点眼睛切明文(type=text),再点回 password。
    const keyInput = c.querySelector<HTMLInputElement>('input[aria-label="API key Main"]');
    expect(keyInput?.getAttribute("type")).toBe("password");
    fireEvent.click(findButtonByAriaLabel("Show"));
    expect(keyInput?.getAttribute("type")).toBe("text");
    fireEvent.click(findButtonByAriaLabel("Hide"));
    expect(keyInput?.getAttribute("type")).toBe("password");

    // 内联输入新密钥 blur 保存。
    fireEvent.change(keyInput!, { target: { value: "sk-replaced" } });
    fireEvent.blur(keyInput!);
    const replaced = props.setProviderAccounts.mock.calls.at(-1)![1];
    expect(replaced.find((account: { id?: string }) => account.id === "a1").api_key).toBe("sk-replaced");

    // 空值 blur = 取消,不改配置。
    const callsBefore = props.setProviderAccounts.mock.calls.length;
    fireEvent.change(keyInput!, { target: { value: "" } });
    fireEvent.blur(keyInput!);
    expect(props.setProviderAccounts.mock.calls.length).toBe(callsBefore);
  });

  it("删除需确认:点删除先弹框,确认才移除;weight 提交走 setProviderAccounts;拖拽手柄存在", async () => {
    const props = makeProps();
    const c = mountSection(props);
    await waitFor(() => {
      expect(c.querySelector('input[role="switch"]')).toBeTruthy();
    });

    // 删除:点行内删除按钮 → 先弹确认框,配置不变。
    fireEvent.click(findButtonByAriaLabel("Remove key Main"));
    expect(props.setProviderAccounts).not.toHaveBeenCalled();
    const dialog = document.body;
    expect(dialog.textContent).toContain("Delete this API key from the provider?");
    // 确认(Dropbox portal 在 body 下,按文本找 Delete 按钮)→ a1 移除,数组只剩 a2/a3。
    const deleteButton = Array.from(dialog.querySelectorAll<HTMLButtonElement>("button"))
      .find((element) => element.textContent?.trim() === "Delete");
    expect(deleteButton, "dialog Delete button not found").toBeTruthy();
    fireEvent.click(deleteButton!);
    await waitFor(() => {
      expect(props.setProviderAccounts).toHaveBeenCalled();
    });
    const removed = props.setProviderAccounts.mock.calls.at(-1)![1];
    expect(removed.map((account: { id?: string }) => account.id)).toEqual(["a2", "a3"]);

    // weight:行内输入 blur 提交数字。
    const weightInput = container!.querySelector<HTMLInputElement>('input[aria-label="Weight Main"]');
    expect(weightInput).toBeTruthy();
    fireEvent.change(weightInput!, { target: { value: "7" } });
    fireEvent.blur(weightInput!);
    const weighted = props.setProviderAccounts.mock.calls.at(-1)![1];
    expect(weighted.find((account: { id?: string }) => account.id === "a1").weight).toBe(7);

    // 每行左侧有拖拽手柄。
    const grips = c.querySelectorAll('[aria-label="Drag to reorder"]');
    expect(grips.length).toBe(3);
  });
});
