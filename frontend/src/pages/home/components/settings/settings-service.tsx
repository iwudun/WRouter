// 20261102_01 P6:服务控制分区 —— 暂停 / 恢复 / 退出服务。
//
// 通用能力:所有运行时(Tauri 内嵌 / 独立进程 / systemd)走同一套管理 RPC,
// 前端**不按运行时硬编码分支** —— 能力由后端自报(`getRuntimeControl`)。
//
// 语义要点(与后端 service_control.rs 对齐):
// - 暂停 = 关闭网关面(3460)监听 → 客户端 connection refused;管理面(3462)
//   不受影响,页面保持可用,可随时恢复;
// - 停止服务 = 后端进程退出(响应后约 1.2s)→ 页面必须在此之前切到
//   「正在停止」落地页,否则后续 API 调用会失败;
//   注意 `stopService` 的回执只代表**请求已受理**,**不代表进程已退出**
//   (2026-11-03)——所以回调叫 `onServiceStopping`,不是 `onServiceStopped`;
// - 桌面壳内嵌模式(`managedBy=embedded`)后端与壳同进程 → 只能关窗口
//   (exitAction=exitApp),页面不得停后端。
import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { CircleAlert, LoaderCircle, LogOut, Pause, Play } from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogBody,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle
} from "@/components/ui/dialog";
import { api, hasBridge } from "@/lib/api";
import { queryKeys } from "@/lib/queryKeys";
import { closeWindow } from "@/lib/tauri-window";
import { useAppErrorText, useAppText } from "../../shared/index";
import type { GatewayControlState, RuntimeControl } from "@/contracts/app";

const settingsServiceContentWidthClassName = "mx-auto w-full max-w-[900px]";

type ExitMode = "exitApp" | "stopService";

/**
 * 网关状态文案。
 *
 * 暂停语义 = 关掉 3460 监听 → 后端回报 `gatewayEnabled=false` + `state=stopped`;
 * 两者任一成立都按「已暂停」呈现。
 */
function gatewayStateLabel(
  state: GatewayControlState,
  enabled: boolean,
  t: (value: string) => string
): string {
  if (!enabled || state === "stopped") {
    return t("Paused");
  }
  if (state === "starting") {
    return t("Gateway starting");
  }
  if (state === "error") {
    return t("Gateway error");
  }
  return t("Running");
}

export function ServiceControlSection({
  notify,
  onServiceStopping,
  t: tProp
}: {
  /** toast 提示(错误提示走 App 的 LightToast)。 */
  notify?: (message: string) => void;
  /**
   * 「停止服务」**已被后端受理** → 通知上层切到「正在停止」落地页。
   *
   * 注意:受理 ≠ 进程已退出。上层据此进入 stopping 态(取消 SSE 订阅与
   * 自动重连),再自行确认进程是否真的退出。
   */
  onServiceStopping?: (restartHint: string) => void;
  /** 文本翻译(默认取 i18n 上下文;设置页沿用 copy.text 风格传入)。 */
  t?: (value: string) => string;
}) {
  const contextT = useAppText();
  const t = tProp ?? contextT;
  const formatError = useAppErrorText();
  const queryClient = useQueryClient();
  const [busy, setBusy] = useState<"" | "exit" | "toggle">("");
  const [confirmOpen, setConfirmOpen] = useState(false);

  // 后端自报运行时能力(托管方式 / 可用动作 / 网关启停态)。
  const runtimeControlQuery = useQuery({
    queryKey: queryKeys.runtimeControl,
    queryFn: () => api.getRuntimeControl(),
    enabled: hasBridge(),
    retry: false
  });
  const control: RuntimeControl | undefined = runtimeControlQuery.data;

  // 内嵌模式(后端跑在桌面壳进程内)→ 只能关窗口;其余由后端停进程。
  const exitMode: ExitMode = control?.managedBy === "embedded" || control?.exitAction === "exitApp"
    ? "exitApp"
    : "stopService";
  const showExit = Boolean(control) && (exitMode === "exitApp" || Boolean(control?.canExit));
  const gatewayEnabled = control?.gatewayEnabled ?? true;

  function reportError(error: unknown) {
    notify?.(formatError(error));
  }

  async function toggleGateway() {
    if (busy) {
      return;
    }
    setBusy("toggle");
    try {
      await api.setGatewayEnabled(!gatewayEnabled);
      // 网关启停态变了 → 运行状态与自报能力都要重取。
      await queryClient.invalidateQueries({ queryKey: queryKeys.runtimeControl });
      await queryClient.invalidateQueries({ queryKey: queryKeys.gatewayStatus });
    } catch (error) {
      reportError(error);
    } finally {
      setBusy("");
    }
  }

  async function confirmExit() {
    if (busy) {
      return;
    }
    setBusy("exit");
    try {
      if (exitMode === "exitApp") {
        // 桌面壳内嵌:关窗口即可,后端随壳退出(不用新造壳命令)。
        await closeWindow();
        return;
      }
      const result = await api.stopService();
      // 后端约 1.2s 后退出进程 → 立刻切「正在停止」落地页,
      // 否则后续 API 调用会失败。
      // 回执只代表**已受理**;进程是否真的退出由上层继续确认。
      onServiceStopping?.(result?.restartHint ?? control?.restartHint ?? "");
    } catch (error) {
      reportError(error);
    } finally {
      setBusy("");
      setConfirmOpen(false);
    }
  }

  const controlError = runtimeControlQuery.error;

  return (
    <section className={settingsServiceContentWidthClassName + " grid grid-cols-1 gap-3"}>
      <div>
        <h4 className="text-body-lg font-semibold text-foreground">{t("Service control")}</h4>
        <p className="mt-0.5 text-caption leading-4 text-muted-foreground">
          {t("Pause the gateway without stopping the management panel; clients cannot reach the gateway while it is paused.")}
        </p>
      </div>

      <div className="grid grid-cols-1 gap-3 rounded-md border border-border bg-card p-3">
        <div className="grid gap-1 rounded-md bg-muted/20 px-2 py-2 sm:grid-cols-[160px_minmax(0,1fr)] sm:items-center">
          <div className="text-caption font-medium text-muted-foreground">{t("Gateway status")}</div>
          <div className="min-w-0 break-all text-caption text-foreground">
            {control ? gatewayStateLabel(control.gatewayState, control.gatewayEnabled, t) : "-"}
          </div>
        </div>
        <div className="grid gap-1 rounded-md bg-muted/20 px-2 py-2 sm:grid-cols-[160px_minmax(0,1fr)] sm:items-center">
          <div className="text-caption font-medium text-muted-foreground">{t("Gateway address")}</div>
          <div className="min-w-0 break-all font-mono text-caption text-foreground">
            {control?.gatewayEndpoint || "-"}
          </div>
        </div>
        <div className="grid gap-1 rounded-md bg-muted/20 px-2 py-2 sm:grid-cols-[160px_minmax(0,1fr)] sm:items-center">
          <div className="text-caption font-medium text-muted-foreground">{t("Management address")}</div>
          <div className="min-w-0 break-all font-mono text-caption text-foreground">
            {control?.managementEndpoint || "-"}
          </div>
        </div>
        {control?.gatewayError ? (
          <div className="rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2 text-caption text-destructive">
            {control.gatewayError}
          </div>
        ) : null}
        {controlError ? (
          <div className="rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2 text-caption text-destructive">
            {t("Service control is unavailable")}
          </div>
        ) : null}

        <div className="flex flex-wrap items-center gap-2">
          {control?.canPause ? (
            <Button disabled={busy !== ""} onClick={toggleGateway} size="sm" type="button" variant="outline">
              {busy === "toggle" ? <LoaderCircle className="h-4 w-4 animate-spin" /> : gatewayEnabled ? <Pause className="h-4 w-4" /> : <Play className="h-4 w-4" />}
              {gatewayEnabled ? t("Pause service") : t("Resume service")}
            </Button>
          ) : null}
          {showExit ? (
            <Button disabled={busy !== ""} onClick={() => setConfirmOpen(true)} size="sm" type="button" variant="destructive">
              {busy === "exit" ? <LoaderCircle className="h-4 w-4 animate-spin" /> : <LogOut className="h-4 w-4" />}
              {exitMode === "exitApp" ? t("Exit app") : t("Stop service")}
            </Button>
          ) : null}
        </div>
        <p className="text-caption leading-4 text-muted-foreground">
          {t("Clients cannot call the gateway while paused")}
        </p>
      </div>

      <Dialog onOpenChange={(open) => !open && setConfirmOpen(false)} open={confirmOpen}>
        <DialogContent className="max-w-[520px]">
          <DialogHeader>
            <div className="min-w-0">
              <DialogTitle>
                {exitMode === "exitApp" ? t("Exit app") : t("Stop service")}
              </DialogTitle>
            </div>
          </DialogHeader>

          <DialogBody>
            <div className="rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2.5">
              <div className="flex items-start gap-2 text-body font-medium text-destructive">
                <CircleAlert className="mt-0.5 h-3.5 w-3.5 shrink-0" />
                <span>
                  {exitMode === "exitApp"
                    ? t("Closing the window stops the backend service as well.")
                    : t("Stop the service now? The gateway stops and clients are disconnected.")}
                </span>
              </div>
              {exitMode === "stopService" && control?.restartHint ? (
                <div className="mt-2 text-caption text-muted-foreground">
                  <span className="font-medium text-foreground">{t("Restart command")}:</span>{" "}
                  <span className="break-all font-mono">{control.restartHint}</span>
                </div>
              ) : null}
            </div>
          </DialogBody>

          <DialogFooter>
            <Button autoFocus onClick={() => setConfirmOpen(false)} type="button" variant="outline">
              {t("Cancel")}
            </Button>
            <Button onClick={confirmExit} type="button" variant="destructive">
              <LogOut className="h-4 w-4" />
              {t("Confirm")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </section>
  );
}
