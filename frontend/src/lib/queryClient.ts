// F2a.1: 应用级 QueryClient。main.tsx 以 Provider 注入；App.smoke 等测试经由
// App 的内层组件结构自动获得同一实例。
//
// 全局默认（对齐旧轮询语义）：
//   - retry: false —— 旧 startVisiblePolling 失败即呈现错误态、下一轮自愈；
//   - refetchOnWindowFocus: true —— 保留 visibilitychange 回前台即刷新
//     （原 polling.ts 的 onVisibilityChange 行为）；
//   - staleTime: 0 —— 视图激活即拉取（原 effect-on-activate 语义），
//     轮询型查询靠 refetchInterval（间隔集中在 refresh-intervals.ts）。
import { QueryCache, QueryClient } from "@tanstack/react-query";

type QueryErrorBridge = (error: unknown) => void;

let errorBridge: QueryErrorBridge | null = null;

/**
 * F2a.1 错误事件桥：QueryCache 级错误转发给 App 的 feedback 组件
 * （App 挂载时注册 formatError 后的 setActionError），未注册时静默——
 * 各 hook 的 data.error 呈现仍由局部 query.error 承担。
 */
export function setQueryErrorBridge(bridge: QueryErrorBridge | null): void {
  errorBridge = bridge;
}

export function createAppQueryClient(): QueryClient {
  return new QueryClient({
    defaultOptions: {
      queries: {
        retry: false,
        refetchOnWindowFocus: true,
        staleTime: 0
      }
    },
    queryCache: new QueryCache({
      onError: (error) => errorBridge?.(error)
    })
  });
}

export const queryClient = createAppQueryClient();
