// F2a.8: Query 迁移行为测试。
//  - useCallLogsState keepPreviousData（切页/筛选不闪烁、不丢当前页）
//  - useConfigState persistConfig 乐观回滚（显式动作失败恢复动作前草稿）
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, renderHook, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { CallLogPage } from "@/contracts/app";
import { useCallLogsState } from "@/pages/home/hooks/useCallLogsState";
import { useConfigState } from "@/pages/home/hooks/useConfigState";
import { fallbackConfig } from "@/pages/home/shared/fallbacks";
import { mockWindowWr } from "../helpers/mockWindowWr";

function makePage(page: number): CallLogPage {
  return {
    generatedAt: new Date().toISOString(),
    items: [],
    options: { credentials: [], models: [] },
    page,
    pageSize: 25,
    statusCounts: { all: 0, canceled: 0, completed: 0, failed: 0, pending: 0, processing: 0 },
    total: 0,
    totalRows: 0,
    totalPages: 1,
    windowSize: 2000
  };
}

function makeWrapper() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: Infinity } }
  });
  const wrapper = ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={client}>{children}</QueryClientProvider>
  );
  return { client, wrapper };
}

afterEach(() => {
  vi.restoreAllMocks();
});

describe("useCallLogsState keepPreviousData (F2a.3)", () => {
  it("筛选切换期间保留上一页数据，不闪空页", async () => {
    const { wr } = mockWindowWr();
    let resolvePending: ((page: CallLogPage) => void) | undefined;
    let callCount = 0;
    wr.getCallLogs = vi.fn(() => {
      callCount += 1;
      if (callCount === 1) {
        return Promise.resolve(makePage(1));
      }
      return new Promise<CallLogPage>((resolve) => {
        resolvePending = resolve;
      });
    });

    const { wrapper } = makeWrapper();
    const { result } = renderHook(
      () => useCallLogsState({ activeView: "call-logs", formatError: (e) => String(e) }),
      { wrapper }
    );

    await waitFor(() => expect(result.current.callLogPage.page).toBe(1));

    act(() => {
      result.current.updateCallLogFilter({ status: "failed" });
    });

    // 新查询在途：keepPreviousData 让旧页数据保持可见（迁移前行为：旧页 +
    // loading；迁移后：旧页 + isPlaceholderData，均无空页闪烁）。
    expect(result.current.callLogPage.page).toBe(1);

    await act(async () => {
      resolvePending?.(makePage(1));
      await Promise.resolve();
    });
    expect(result.current.callLogPage.page).toBe(1);
  });

  it("手动刷新驱动 loading（showLoading=true）", async () => {
    const { wr } = mockWindowWr();
    wr.getCallLogs = vi.fn(() => Promise.resolve(makePage(1)));
    const { wrapper } = makeWrapper();
    const { result } = renderHook(
      () => useCallLogsState({ activeView: "call-logs", formatError: (e) => String(e) }),
      { wrapper }
    );
    await waitFor(() => expect(result.current.callLogPage.page).toBe(1));
    await act(async () => {
      await result.current.refreshCallLogs(true);
    });
    expect(result.current.callLogLoading).toBe(false);
  });
});

describe("useConfigState persistConfig 乐观回滚 (F2a.2)", () => {
  it("保存失败时草稿回滚到动作前快照", async () => {
    const { wr } = mockWindowWr();
    wr.getConfig = vi.fn(() => Promise.resolve(fallbackConfig));
    wr.saveConfig = vi.fn(() => Promise.reject(new Error("boom: backend down")));

    const { wrapper } = makeWrapper();
    const harness = renderHook(
      () => useConfigState({
        formatError: (error) => `formatted:${String(error)}`,
        setActionError: () => undefined,
        t: (value) => value
      }),
      { wrapper }
    );

    await waitFor(() => expect(harness.result.current.configLoaded).toBe(true));

    const before = harness.result.current.draftConfig;

    let persisted: boolean | undefined;
    await act(async () => {
      const next = harness.result.current.buildConfigUpdate((config) => {
        config.theme = "dark";
        return config;
      });
      persisted = await harness.result.current.persistConfig(next, () => undefined);
    });

    expect(persisted).toBe(false);
    // 回滚：草稿引用恢复到动作前对象（未确认的动作变更被撤销）
    expect(harness.result.current.draftConfig).toBe(before);
  });
});
