// 2026-10-11 统一多密钥形态:添加供应商页与详情页同一密钥管理。
//  - 「添加 API密钥」可连续加多把;固定密钥模式下首把启用、后续默认停用;
//  - 行内密钥 blur 提交进草稿;固定模式开启另一把 → 单启用 + pinned 记忆;
//  - 「检测可用性」判据:任一启用且已设密钥(与详情面板一致)。
import { fireEvent, waitFor } from "@testing-library/react";
import { useState, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AddProviderForm } from "@/pages/home/components/providers/providers-form";
import { createProviderDraft } from "@/pages/home/shared/providers";
import type { AddProviderDraft } from "@/pages/home/shared/types";
import type { ProxyPreset } from "@/contracts/app";

let root: Root | null = null;
let container: HTMLElement | null = null;

afterEach(() => {
  root?.unmount();
  root = null;
  container?.remove();
  container = null;
  document.body.innerHTML = "";
  vi.restoreAllMocks();
});

const proxyPresets: ProxyPreset[] = [];

type Tracked = { draft: AddProviderDraft };

// Harness 持草稿状态;tracked 引用同步最新值供断言。
function mountDraftFormTracked(applyInitial: (draft: AddProviderDraft) => AddProviderDraft) {
  const tracked: Tracked = { draft: applyInitial(createProviderDraft([])) };
  function Harness(): ReactNode {
    const [draft, setDraft] = useState<AddProviderDraft>(() => tracked.draft);
    return (
      <AddProviderForm
        draft={draft}
        error=""
        notify={vi.fn()}
        onAddModel={() => {}}
        onCheck={async () => {}}
        onEditModel={() => {}}
        onChange={(patch) => {
          setDraft((current) => {
            const next = { ...current, ...patch };
            tracked.draft = next;
            return next;
          });
        }}
        onOpenProxySettings={() => {}}
        onRemoveModel={() => {}}
        proxyPresets={proxyPresets}
      />
    );
  }
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  root.render(<Harness />);
  return { container: container!, root, tracked };
}

function findButtonByText(text: string, scope: HTMLElement) {
  const button = Array.from(scope.querySelectorAll<HTMLButtonElement>("button")).find(
    (element) => element.textContent?.trim() === text
  );
  expect(button, `button "${text}" not found`).toBeTruthy();
  return button!;
}

function keyRows(scope: HTMLElement) {
  return Array.from(scope.querySelectorAll<HTMLInputElement>('input[aria-label^="API key "]'));
}

function switches(scope: HTMLElement) {
  return Array.from(scope.querySelectorAll<HTMLInputElement>('input[role="switch"][title="Key status"]'));
}

async function waitMounted(scope: HTMLElement) {
  await waitFor(() => {
    expect(findButtonByText("Add API key", scope)).toBeTruthy();
  });
}

describe("添加供应商页多密钥管理(与详情页统一)", () => {
  it("「添加 API密钥」连续添加多把;固定密钥模式下首把启用、后续默认停用", async () => {
    const { container: c, tracked } = mountDraftFormTracked((draft) => ({ ...draft, name: "EveryAPI" }));
    await waitMounted(c);

    fireEvent.click(findButtonByText("Add API key", c));
    expect(tracked.draft.accounts).toHaveLength(1);
    expect(tracked.draft.accounts[0].enabled).not.toBe(false);
    expect(tracked.draft.accounts[0].label).toBe("Key 1");

    // 固定模式(默认 first)下第二把默认停用(单启用约束,填完再开启)。
    fireEvent.click(findButtonByText("Add API key", c));
    expect(tracked.draft.accounts).toHaveLength(2);
    expect(tracked.draft.accounts[1].enabled).toBe(false);
  });

  it("行内密钥 blur 提交进草稿;固定模式开启另一把 → 单启用 + pinned 记忆", async () => {
    const { container: c, tracked } = mountDraftFormTracked((draft) => ({ ...draft, name: "EveryAPI" }));
    await waitMounted(c);

    fireEvent.click(findButtonByText("Add API key", c));
    fireEvent.click(findButtonByText("Add API key", c));
    await waitFor(() => {
      expect(keyRows(c).length).toBe(2);
    });

    // 第一把填密钥,blur 提交。
    fireEvent.change(keyRows(c)[0], { target: { value: "sk-basic" } });
    fireEvent.blur(keyRows(c)[0]);
    expect(tracked.draft.accounts[0].api_key).toBe("sk-basic");

    // 开关第二把(默认停用 → 开启):单启用收敛到第二把并记 pinned。
    const secondToggle = switches(c)[1];
    expect(secondToggle.getAttribute("aria-checked")).toBe("false");
    fireEvent.click(secondToggle);
    expect(tracked.draft.accounts.map((account) => account.enabled)).toEqual([false, true]);
    expect(tracked.draft.pinnedAccountId).toBe(tracked.draft.accounts[1].id);
    expect(tracked.draft.credentialStrategy).toBe("first");
  });

  it("策略切负载均衡后启停自由,不再收敛单启用", async () => {
    const { container: c, tracked } = mountDraftFormTracked((draft) => ({ ...draft, name: "EveryAPI" }));
    await waitMounted(c);

    fireEvent.click(findButtonByText("Add API key", c));
    fireEvent.click(findButtonByText("Add API key", c));
    await waitFor(() => {
      expect(switches(c).length).toBe(2);
    });

    // 切到负载均衡。
    const strategySelect = c.querySelector<HTMLSelectElement>('select[aria-label="Key strategy"]');
    expect(strategySelect).toBeTruthy();
    fireEvent.change(strategySelect!, { target: { value: "balanced" } });
    expect(tracked.draft.credentialStrategy).toBe("balanced");

    // 开启第二把:不收敛第一把,也不写 pinned。
    fireEvent.click(switches(c)[1]);
    expect(tracked.draft.accounts.map((account) => account.enabled)).toEqual([true, true]);
    expect(tracked.draft.pinnedAccountId).toBeUndefined();
  });

  it("「检测可用性」判据与详情面板一致:任一启用且已设密钥才可用", async () => {
    const seeded = (apply: (draft: AddProviderDraft) => AddProviderDraft) =>
      mountDraftFormTracked((draft) =>
        apply({ ...draft, name: "EveryAPI", baseUrl: "https://api.example.com/v1", modelsText: "m1" })
      );

    // 无密钥:按钮禁用。
    const empty = seeded((draft) => draft);
    await waitMounted(empty.container);
    expect(findButtonByText("Check Availability", empty.container).disabled).toBe(true);
    empty.root!.unmount();
    empty.container.remove();

    // 一把启用且有明文:可用。
    const withKey = seeded((draft) => ({
      ...draft,
      accounts: [{ enabled: true, id: "a1", label: "Key 1", priority: 0, weight: 1, api_key: "sk-1" }]
    }));
    await waitMounted(withKey.container);
    expect(findButtonByText("Check Availability", withKey.container).disabled).toBe(false);
    withKey.root!.unmount();
    withKey.container.remove();

    // 有行但全停用:仍不可用。
    const allOff = seeded((draft) => ({
      ...draft,
      accounts: [{ enabled: false, id: "a1", label: "Key 1", priority: 0, weight: 1, api_key: "sk-1" }]
    }));
    await waitMounted(allOff.container);
    expect(findButtonByText("Check Availability", allOff.container).disabled).toBe(true);
  });
});
