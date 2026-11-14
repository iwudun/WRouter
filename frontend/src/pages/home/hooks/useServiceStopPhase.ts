import { useCallback, useEffect, useState } from "react";

import { api } from "@/lib/api";
import { queryClient } from "@/lib/queryClient";

import type { ServiceStopPhase } from "../components/service-stop";

type ServicePhase = ServiceStopPhase | "normal";

const STOP_CONFIRM_POLL_MS = 500;
const STOP_CONFIRM_TIMEOUT_MS = 30_000;

/**
 * 「服务停止」落地态(20261102_01 P6,2026-11-03 拆三阶段)。
 *
 * - `normal`:一切正常;
 * - `stopping`:停机请求已受理,进程尚未退出;
 * - `stopped`:后端不可达(已退出或正在最后收尾)。
 *
 * 受理 ≠ 退出:两者必须分开,否则页面会把「请求已受理」当成「进程已退出」。
 */
export function useServiceStopPhase() {
  const [servicePhase, setServicePhase] = useState<ServicePhase>("normal");
  const [serviceRestartHint, setServiceRestartHint] = useState("");

  const markStopped = useCallback(() => setServicePhase("stopped"), []);

  // 「正在停止」→ 轮询确认后端是否真的退出。判据:管理面**连不上**才算
  // 退出(后端在网关面排空期间仍在线);超时未确认也落到「已停止」。
  useEffect(() => {
    if (servicePhase !== "stopping") {
      return;
    }
    let cancelled = false;
    const deadline = Date.now() + STOP_CONFIRM_TIMEOUT_MS;
    const timer = window.setInterval(() => {
      void api
        .getRuntimeControl()
        .then((control) => {
          // 仍在线 → 后端还在收尾(顺序停机:网关面排空期间管理面仍在线)。
          // 顺带记下重启命令:其它标签页是被 SSE 通告叫停的,拿不到 stopService 回执。
          if (control?.restartHint) {
            setServiceRestartHint(control.restartHint);
          }
          if (Date.now() >= deadline && !cancelled) {
            cancelled = true;
            window.clearInterval(timer);
            setServicePhase("stopped");
          }
        })
        .catch(() => {
          // 连不上 → 后端已退出。
          if (cancelled) {
            return;
          }
          cancelled = true;
          window.clearInterval(timer);
          setServicePhase("stopped");
        });
    }, STOP_CONFIRM_POLL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [servicePhase]);

  function handleServiceStopping(restartHint?: string) {
    if (restartHint) {
      setServiceRestartHint(restartHint);
    }
    setServicePhase("stopping");
  }

  async function handleServiceRestored() {
    setServicePhase("normal");
    await queryClient.invalidateQueries();
  }

  return {
    servicePhase,
    serviceRestartHint,
    handleServiceStopping,
    handleServiceRestored,
    markStopped,
  };
}
