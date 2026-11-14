// F2b.1: useGatewayEvents —— 管理面 SSE 订阅 hook。
//
// 职责：
//   1. 建立事件流（lib/gateway-events 的 fetch-stream 客户端），暴露连接状态
//      （connecting / live / reconnecting，供 UI 角标）；
//   2. F2b.2: 事件 → Query 失效（映射表 + 去抖/节流合并，F2b.6）；
//   3. F2b.5: 断连期间的降级轮询——全应用唯一定时刷新逻辑，每 5s 重取活跃
//      query，重连（live）后自愈停止，并由 onReconnected 全量刷新一次。
//
// 轮询退役（F2b.5）：各 hook 的 refetchInterval 已全部移除，
// shared/polling.ts 与 lib/refresh-intervals.ts 已删除——本 hook 内的
// DEGRADED_POLL_MS 是代码库中仅存的定时刷新。
import { useEffect, useRef, useState } from "react";
import { useQueryClient, type QueryClient } from "@tanstack/react-query";
import {
  createGatewayEventsClient, EventInvalidationCoalescer, type GatewayEventsClient, type GatewayEventsStatus
} from "@/lib/gateway-events";
import { hasBridge } from "@/lib/api";

const DEGRADED_POLL_MS = 5000;

function refetchActiveQueries(queryClient: QueryClient): void {
  void queryClient.refetchQueries({ type: "active", stale: true });
}

export function useGatewayEvents(
  options: { enabled?: boolean; onServiceStopping?: () => void } = {}
): { status: GatewayEventsStatus } {
  const queryClient = useQueryClient();
  const [status, setStatus] = useState<GatewayEventsStatus>("connecting");
  const clientRef = useRef<GatewayEventsClient | null>(null);
  // 回调走 ref:避免调用方每次渲染新建函数导致客户端被反复重建。
  const onServiceStoppingRef = useRef(options.onServiceStopping);
  useEffect(() => {
    onServiceStoppingRef.current = options.onServiceStopping;
  }, [options.onServiceStopping]);

  useEffect(() => {
    if (options.enabled === false || !hasBridge()) {
      return;
    }

    const coalescer = new EventInvalidationCoalescer((prefixes) => {
      for (const prefix of prefixes) {
        void queryClient.invalidateQueries({ queryKey: [...prefix] });
      }
    });

    let degradedPollTimer: number | undefined;
    const stopDegradedPoll = () => {
      if (degradedPollTimer !== undefined) {
        window.clearInterval(degradedPollTimer);
        degradedPollTimer = undefined;
      }
    };

    const client = createGatewayEventsClient({
      onEvent: (event) => coalescer.add(event.type),
      onStatus: (next) => {
        setStatus(next);
        if (next === "live") {
          stopDegradedPoll();
        } else if (degradedPollTimer === undefined) {
          // 断连降级：维持数据新鲜度（本文件头注释：唯一存留定时刷新）。
          degradedPollTimer = window.setInterval(() => {
            refetchActiveQueries(queryClient);
          }, DEGRADED_POLL_MS);
        }
      },
      onReconnected: () => refetchActiveQueries(queryClient),
      // 2026-11-03:后端开始停机 → 通知上层切「正在停止」态。
      onServiceStopping: () => onServiceStoppingRef.current?.()
    });
    clientRef.current = client;
    client.start();

    return () => {
      // 取消订阅 + 断掉自动重连(客户端 stop() 置位 disposed,不再重连)。
      client.stop();
      clientRef.current = null;
      stopDegradedPoll();
      coalescer.dispose();
    };
    // queryClient 实例稳定;enabled 由调用方控制。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [options.enabled === false, queryClient]);

  return { status };
}
