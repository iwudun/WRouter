// 20261104_01:三页卡片壳恢复 + 左下角设置区横线。
//
// 此前为「透出背景」把 模型管理 / 路由密钥 / 调用日志 的数据区摊在页面底色上
// (去边框 + 透明),现在恢复成有边框、有卡片底色的容器;侧栏左下角设置区
// 补一道横线(border-top)。
import { waitFor } from "@testing-library/react";
import { StrictMode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import App from "@/pages/home/App";
import { mockWindowWr } from "../helpers/mockWindowWr";

let consoleErrorSpy: ReturnType<typeof vi.spyOn>;
let root: Root | null = null;

function mountApp() {
  const container = document.createElement("div");
  container.id = "root";
  document.body.appendChild(container);
  root = createRoot(container);
  root.render(
    <StrictMode>
      <App />
    </StrictMode>
  );
  return container;
}

async function waitForMounted(container: HTMLElement) {
  await waitFor(() => {
    expect(container.childNodes.length).toBeGreaterThan(0);
  });
}

/** 点侧栏导航项(按视图 id,不看文案 —— 文案随语言变)。 */
async function openView(view: string) {
  const button = document.querySelector<HTMLButtonElement>(`[data-nav="${view}"]`);
  expect(button, `sidebar item "${view}" not found`).toBeTruthy();
  button!.click();
  await waitFor(() => {
    expect(button!.className).toContain("bg-card");
  });
}

/** 断言该容器是有边框 + 卡片底色的卡片壳(不是透明摊在页面上)。 */
function expectCardShell(container: ParentNode, module: string) {
  const card = container.querySelector(`[data-module="${module}"]`);
  expect(card, `[data-module="${module}"] not found`).not.toBeNull();
  // 按**类名 token** 判定:子串匹配会把 `border-0` / `bg-card/20` 误判成
  // 「有边框 + 卡片底色」,那样证伪实验就假绿了。
  const tokens = card!.className.split(/\s+/);
  expect(tokens).toContain("border");
  expect(tokens).toContain("bg-card");
}

/**
 * 断言卡片边框**显式指定了颜色**(20261104_02)。
 *
 * 为什么单靠 `expect(tokens).toContain("border")` 不够:Tailwind v4 的裸
 * `border` 只输出 border-style/width,**不输出 border-color**,颜色落到 CSS
 * 初始值 currentColor;而 Card 自带 text-card-foreground(暗色 #ededf1 近白),
 * 于是描边渲染成明显的白边。所以必须额外要求 `border-border` 这个颜色类。
 */
function expectCardBorderColor(container: ParentNode, module: string) {
  const card = container.querySelector(`[data-module="${module}"]`);
  expect(card, `[data-module="${module}"] not found`).not.toBeNull();
  const tokens = card!.className.split(/\s+/);
  expect(tokens).toContain("border-border");
}

describe("UI 卡片壳恢复(20261104_01)", () => {
  beforeEach(() => {
    consoleErrorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    mockWindowWr();
  });

  afterEach(() => {
    consoleErrorSpy.mockRestore();
    root?.unmount();
    root = null;
    document.getElementById("root")?.remove();
    delete window.wr;
  });

it("左下角设置区上方有一道横线(border-top)", async () => {
    const container = mountApp();
    await waitForMounted(container);
    const block = container.querySelector('[data-module="sidebar-settings"]');
    expect(block).not.toBeNull();
    expect(block!.className.split(/\s+/)).toContain("border-t");
  });

  it("路由密钥页:密钥列表回到有边框的卡片壳", async () => {
    const container = mountApp();
    await waitForMounted(container);
    await openView("api-keys");
    await waitFor(() => {
      expect(container.querySelector('[data-module="api-keys-card"]')).not.toBeNull();
    });
    expectCardShell(container, "api-keys-card");
    expectCardBorderColor(container, "api-keys-card");
  });

  it("调用日志页:日志表回到有边框的卡片壳", async () => {
    const container = mountApp();
    await waitForMounted(container);
    await openView("call-logs");
    await waitFor(() => {
      expect(container.querySelector('[data-module="call-logs-card"]')).not.toBeNull();
    });
    expectCardShell(container, "call-logs-card");
    expectCardBorderColor(container, "call-logs-card");
  });

  it("模型管理页:master-detail 回到有边框的卡片壳", async () => {
    const container = mountApp();
    await waitForMounted(container);
    await openView("providers");
    await waitFor(() => {
      expect(container.querySelector('[data-module="providers-card"]')).not.toBeNull();
    });
    expectCardShell(container, "providers-card");
    expectCardBorderColor(container, "providers-card");
  });
});
