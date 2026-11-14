// 服务控制分区组件测试。
//
// 覆盖:
//  1. 运行时状态行(网关状态 / 网关地址 / 管理面地址)与动作按钮的呈现;
//  2. 「暂停服务」→ setGatewayEnabled(false);
//  3. 退出动作按后端自报的托管方式分支(systemd → 停止服务;embedded → 退出应用);
//  4. 「停止服务」**被受理** → 通知上层切「正在停止」落地页(受理 ≠ 已退出,
//     2026-11-03:回调由 onServiceStopped 更名为 onServiceStopping);
//  5. 落地页两个阶段(正在停止 / 已停止)的呈现与「重新检测」回调。
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, waitFor } from "@testing-library/react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { RuntimeControl } from "@/contracts/app";
import { ServiceControlSection } from "@/pages/home/components/settings/settings-service";
import { ServiceStopScreen } from "@/pages/home/components/service-stop";
import { mockWindowWr } from "../helpers/mockWindowWr";

function makeControl(overrides: Partial<RuntimeControl> = {}): RuntimeControl {
  return {
    canExit: true,
    canPause: true,
    exitAction: "stopService",
    gatewayEnabled: true,
    gatewayEndpoint: "http://127.0.0.1:3460",
    gatewayState: "running",
    managedBy: "systemd",
    managementEndpoint: "http://127.0.0.1:3462",
    restartHint: "systemctl --user start wrouter.service",
    ...overrides
  };
}

let root: Root | null = null;
let container: HTMLElement | null = null;

function mount(node: React.ReactNode) {
  const client = new QueryClient({
    defaultOptions: { queries: { gcTime: Infinity, retry: false } }
  });
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  root.render(<QueryClientProvider client={client}>{node}</QueryClientProvider>);
  return container;
}

function mountSection(onServiceStopping?: (restartHint: string) => void) {
  return mount(<ServiceControlSection onServiceStopping={onServiceStopping} />);
}

function buttonByText(scope: HTMLElement, text: string): HTMLButtonElement | undefined {
  return Array.from(scope.querySelectorAll<HTMLButtonElement>("button"))
    .find((element) => element.textContent?.trim() === text);
}

afterEach(() => {
  root?.unmount();
  root = null;
  container?.remove();
  container = null;
  document.body.innerHTML = "";
  delete window.wr;
});

describe("ServiceControlSection (20261102_01 P6)", () => {
  it("展示网关状态、网关地址与管理面地址", async () => {
    mockWindowWr({
      getRuntimeControl: vi.fn(async () => makeControl())
    });
    const c = mountSection();

    await waitFor(() => {
      expect(c.textContent).toContain("Service control");
    });
    // 等 runtimeControl 查询回填(地址来自后端自报)。
    await waitFor(() => {
      expect(c.textContent).toContain("http://127.0.0.1:3460");
    });
    expect(c.textContent).toContain("Running");
    expect(c.textContent).toContain("http://127.0.0.1:3462");
    expect(buttonByText(c, "Pause service")).toBeTruthy();
  });

  it("网关已暂停时展示「恢复服务」与「已暂停」", async () => {
    mockWindowWr({
      getRuntimeControl: vi.fn(async () => makeControl({ gatewayEnabled: false, gatewayState: "stopped" }))
    });
    const c = mountSection();

    await waitFor(() => {
      expect(c.textContent).toContain("Paused");
    });
    expect(buttonByText(c, "Resume service")).toBeTruthy();
    expect(buttonByText(c, "Pause service")).toBeUndefined();
  });

  it("点击「暂停服务」调用 setGatewayEnabled(false)", async () => {
    const setGatewayEnabled = vi.fn(async () => ({ enabled: false, state: "stopped" }));
    mockWindowWr({
      getRuntimeControl: vi.fn(async () => makeControl()),
      setGatewayEnabled
    });
    const c = mountSection();

    await waitFor(() => {
      expect(buttonByText(c, "Pause service")).toBeTruthy();
    });
    fireEvent.click(buttonByText(c, "Pause service")!);

    await waitFor(() => {
      expect(setGatewayEnabled).toHaveBeenCalledWith(false);
    });
  });

  it("systemd 托管展示「停止服务」而不是「退出应用」", async () => {
    mockWindowWr({
      getRuntimeControl: vi.fn(async () => makeControl({ managedBy: "systemd" }))
    });
    const c = mountSection();

    await waitFor(() => {
      expect(buttonByText(c, "Stop service")).toBeTruthy();
    });
    expect(buttonByText(c, "Exit app")).toBeUndefined();
  });

  it("embedded 内嵌展示「退出应用」而不是「停止服务」", async () => {
    mockWindowWr({
      getRuntimeControl: vi.fn(async () => makeControl({
        canExit: false,
        exitAction: "exitApp",
        managedBy: "embedded"
      }))
    });
    const c = mountSection();

    await waitFor(() => {
      expect(buttonByText(c, "Exit app")).toBeTruthy();
    });
    expect(buttonByText(c, "Stop service")).toBeUndefined();
  });

  it("「停止服务」需二次确认,确认后调用 stopService 并通知上层切「正在停止」态", async () => {
    const stopService = vi.fn(async () => ({
      restartHint: "systemctl --user start wrouter.service",
      stopping: true
    }));
    const onServiceStopping = vi.fn();
    mockWindowWr({
      getRuntimeControl: vi.fn(async () => makeControl()),
      stopService
    });
    const c = mountSection(onServiceStopping);

    await waitFor(() => {
      expect(buttonByText(c, "Stop service")).toBeTruthy();
    });
    fireEvent.click(buttonByText(c, "Stop service")!);

    // 未确认前不得调用后端。
    await waitFor(() => {
      expect(buttonByText(c, "Confirm")).toBeTruthy();
    });
    expect(stopService).not.toHaveBeenCalled();

    fireEvent.click(buttonByText(c, "Confirm")!);

    await waitFor(() => {
      expect(stopService).toHaveBeenCalledTimes(1);
    });
    // 回执只代表「已受理」→ 通知上层进入 stopping 态(不是「已停止」)。
    expect(onServiceStopping).toHaveBeenCalledWith("systemctl --user start wrouter.service");
  });

  it("stopService 失败时提示错误,不切停止态", async () => {
    const onServiceStopping = vi.fn();
    const notify = vi.fn();
    mockWindowWr({
      getRuntimeControl: vi.fn(async () => makeControl()),
      stopService: vi.fn(async () => {
        throw new Error("boom");
      })
    });
    const c = mount(
      <ServiceControlSection notify={notify} onServiceStopping={onServiceStopping} />
    );

    await waitFor(() => {
      expect(buttonByText(c, "Stop service")).toBeTruthy();
    });
    fireEvent.click(buttonByText(c, "Stop service")!);
    await waitFor(() => {
      expect(buttonByText(c, "Confirm")).toBeTruthy();
    });
    fireEvent.click(buttonByText(c, "Confirm")!);

    await waitFor(() => {
      expect(notify).toHaveBeenCalled();
    });
    expect(onServiceStopping).not.toHaveBeenCalled();
  });
});

describe("ServiceStopScreen (20261102_01 P6 / 2026-11-03 两阶段)", () => {
  it("「已停止」阶段展示重启命令与「重新检测」,成功后回调", async () => {
    const getRuntimeControl = vi.fn(async () => makeControl());
    mockWindowWr({ getRuntimeControl });
    const onRestored = vi.fn();
    const c = mount(
      <ServiceStopScreen onRestored={onRestored} phase="stopped" restartHint="systemctl --user start wrouter.service" />
    );

    await waitFor(() => {
      expect(c.textContent).toContain("Service stopped");
    });
    expect(c.textContent).toContain("systemctl --user start wrouter.service");

    fireEvent.click(buttonByText(c, "Re-check")!);
    await waitFor(() => {
      expect(getRuntimeControl).toHaveBeenCalled();
    });
    expect(onRestored).toHaveBeenCalledTimes(1);
  });

  it("「正在停止」阶段显示受理说明,且不提供「重新检测」按钮", async () => {
    mockWindowWr({ getRuntimeControl: vi.fn(async () => makeControl()) });
    const c = mount(<ServiceStopScreen onRestored={vi.fn()} phase="stopping" />);

    await waitFor(() => {
      expect(c.textContent).toContain("Stopping service");
    });
    expect(c.textContent).toContain("The stop request was accepted; waiting for the process to exit.");
    // 进程尚未退出 → 不能给「已停止」文案,也不提供「重新检测」。
    expect(c.textContent).not.toContain("Service stopped");
    expect(buttonByText(c, "Re-check")).toBeUndefined();
  });
});
