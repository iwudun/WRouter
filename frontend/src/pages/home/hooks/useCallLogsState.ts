// Call-log page (调用日志) state: filter + query-driven refresh, mirroring
// useLogsState.
//
// F2a.3/F2a.5: 迁移到 TanStack Query——keepPreviousData 消除切页/筛选闪烁
// （旧页面停留直到新页返回）。刷新驱动：call-logs 打开时的 log-appended
// 事件失效（F2b.2）+ SSE 断连期降级轮询；返回形状与迁移前一致，
// 视图组件零改动。请求层统一走 lib/api（F2a.6）。
import { useCallback, useEffect, useState } from "react";
import { keepPreviousData, useQuery, useQueryClient } from "@tanstack/react-query";
import type { CallLogListFilter, CallLogPage } from "@/contracts/app";
import { ViewId, createEmptyCallLogPage } from "../shared/index";
import { api, hasBridge } from "@/lib/api";
import { queryKeys } from "@/lib/queryKeys";

export type CallLogsStateOptions = {
  activeView: ViewId;
  formatError: (error: unknown) => string;
};

export function useCallLogsState({ activeView, formatError }: CallLogsStateOptions) {
  const queryClient = useQueryClient();
  const [manualLoading, setManualLoading] = useState(false);
  const [callLogFilter, setCallLogFilter] = useState<CallLogListFilter>({
    page: 1,
    pageSize: 25,
    status: "all"
  });

  const callLogsQuery = useQuery({
    queryKey: queryKeys.callLogs(callLogFilter),
    queryFn: () => (hasBridge() ? api.getCallLogs(callLogFilter) : createEmptyCallLogPage(callLogFilter)),
    enabled: activeView === "call-logs",
    placeholderData: keepPreviousData,
    retry: false
  });

  const callLogPage: CallLogPage = callLogsQuery.data ?? createEmptyCallPageSafe(callLogFilter);
  const callLogError = callLogsQuery.error ? formatError(callLogsQuery.error) : "";
  // 加载态语义对齐迁移前：手动刷新/筛选切换显示 spinner；后台 5s 轮询不闪。
  const callLogLoading =
    manualLoading ||
    (callLogsQuery.isFetching && (callLogsQuery.isLoading || callLogsQuery.isPlaceholderData));

  const refreshCallLogs = useCallback((showLoading = true) => {
    if (showLoading) {
      setManualLoading(true);
    }
    return queryClient
      .refetchQueries({ queryKey: queryKeys.callLogs(callLogFilter) })
      .finally(() => setManualLoading(false));
  }, [callLogFilter, queryClient]);

  // 视图离开时清掉手动刷新态，避免下次进入残留 spinner。
  useEffect(() => {
    if (activeView !== "call-logs") {
      setManualLoading(false);
    }
  }, [activeView]);

  const updateCallLogFilter = useCallback((patch: CallLogListFilter, resetPage = true) => {
    setCallLogFilter((current) => ({
      ...current,
      ...patch,
      page: resetPage ? 1 : patch.page ?? current.page
    }));
  }, []);

  return {
    callLogError,
    callLogFilter,
    callLogLoading,
    callLogPage,
    refreshCallLogs,
    updateCallLogFilter
  };
}

function createEmptyCallPageSafe(filter: CallLogListFilter): CallLogPage {
  return createEmptyCallLogPage(filter);
}
