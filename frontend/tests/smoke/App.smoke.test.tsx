import { waitFor } from "@testing-library/react";
import React, { StrictMode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import App from "@/pages/home/App";
import { mockWindowWr } from "../helpers/mockWindowWr";

/**
 * Mirrors the render-error boundary in src/pages/home/main.tsx so a render
 * failure surfaces as a visible #wr-error-boundary node instead of an opaque
 * test-framework error. Asserting this node stays absent is the direct
 * counterpart of the 40b717c black-screen regression (React never mounted).
 */
class SmokeErrorBoundary extends React.Component<
  { children: React.ReactNode },
  { error: Error | null }
> {
  constructor(props: { children: React.ReactNode }) {
    super(props);
    this.state = { error: null };
  }
  static getDerivedStateFromError(error: Error) {
    return { error };
  }
  componentDidCatch(error: Error, info: React.ErrorInfo) {
    console.error("[wr:fatal] React render error", error, info.componentStack);
  }
  render() {
    if (this.state.error) {
      return (
        <div id="wr-error-boundary">
          <h2>WRouter 渲染失败</h2>
          <pre>{this.state.error.stack || String(this.state.error)}</pre>
        </div>
      );
    }
    return this.props.children;
  }
}

let consoleErrorSpy: ReturnType<typeof vi.spyOn>;
let root: Root | null = null;

function mountApp() {
  const container = document.createElement("div");
  container.id = "root";
  document.body.appendChild(container);
  root = createRoot(container);
  root.render(
    <StrictMode>
      <SmokeErrorBoundary>
        <App />
      </SmokeErrorBoundary>
    </StrictMode>
  );
  return container;
}

async function waitForMounted(container: HTMLDivElement) {
  await waitFor(() => {
    // React has mounted: #root has children and the error boundary never appeared.
    expect(container.childNodes.length).toBeGreaterThan(0);
    expect(document.getElementById("wr-error-boundary")).toBeNull();
  });
}

function fatalConsoleErrors(): unknown[][] {
  return consoleErrorSpy.mock.calls.filter(
    (args: unknown[]) => typeof args[0] === "string" && args[0].includes("[wr:fatal]")
  );
}

describe("App 启动冒烟", () => {
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

  it("React 成功挂载：#root 有内容、无渲染错误边界、无 [wr:fatal] 日志、启动期 RPC 全部触发", async () => {
    const container = mountApp();
    await waitForMounted(container);

    expect(fatalConsoleErrors()).toHaveLength(0);

    const spies = (window as { wr?: Record<string, unknown> }).wr as Record<
      string,
      ReturnType<typeof vi.fn>
    >;
    // 启动期 RPC 由**挂载后的 effect** 触发(React Query 的 useQuery 在
    // passive effect 里取数),而 `waitForMounted` 只等 DOM 提交 —— DOM 有
    // 子节点不代表 effect 已经跑完。只等挂载就断言 RPC 会偶发失败
    // (全量并行、机器负载高时必现)→ 这里等 RPC **真的发生**。
    await waitFor(() => {
      // Bootstrap RPCs (App.tsx:381-400)
      expect(spies.getAppInfo).toHaveBeenCalled();
      expect(spies.getConfig).toHaveBeenCalled();
      // Polling + hooks fired on mount regardless of view
      expect(spies.getGatewayStatus).toHaveBeenCalled();
      expect(spies.getUpdateStatus).toHaveBeenCalled();
      // Event subscriptions must have been registered (and returned unsubs,
      // otherwise StrictMode's double effect run would have thrown)
      expect(spies.onOpenSettingsRequest).toHaveBeenCalled();
      expect(spies.onOpenUpdateRequest).toHaveBeenCalled();
      expect(spies.onUpdateStatusChanged).toHaveBeenCalled();
    });
    // 20261101_03:默认第一页是 quickstart(接入指引),启动期不拉用量 ——
    // 用量查询由 usage-stats 视图激活时触发(懒加载 + KeepAlive)。
    expect(spies.getUsageStats).not.toHaveBeenCalled();
  });

  it("默认启动即进入 quickstart 视图（MainLayout 侧边栏渲染）", async () => {
    const container = mountApp();
    await waitForMounted(container);

    // #primary-sidebar is only rendered by MainLayout (layout.tsx:212); its
    // presence proves the app lands on the main layout on startup.
    expect(document.getElementById("primary-sidebar")).not.toBeNull();
    expect(fatalConsoleErrors()).toHaveLength(0);
  });
});
