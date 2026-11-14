// 「服务停止」全屏落地态 —— 20261102_01 P6 引入,2026-11-03 拆成两阶段。
//
// 两个阶段必须分开(2026-11-03 修正):
// - `stopping`:**停机请求已受理**,但后端进程**尚未退出**。此阶段只能显示
//   「正在停止」—— 把「请求已受理」当成「进程已退出」是错的;
// - `stopped`:后端已不可达(进程已退出,或正在最后收尾)→ 提示手动重启。
//
// 浏览器**无法自行拉起**一个已退出的进程 —— 只能提示用户手动重启
// (systemd 下给出 restartHint),再点「重新检测」回到管理面。
import { useState } from "react";
import { LoaderCircle, RefreshCw, ServerCrash } from "lucide-react";
import { Button } from "@/components/ui/button";
import { api } from "@/lib/api";
import { useAppErrorText, useAppText } from "../shared/index";

/** 停机落地态的两个阶段。 */
export type ServiceStopPhase = "stopping" | "stopped";

export function ServiceStopScreen({
  onRestored,
  restartHint,
  phase = "stopped"
}: {
  /** 重新检测到后端已恢复 → 回到正常界面。 */
  onRestored: () => void;
  /** 后端给出的重启命令(如 systemctl --user start wrouter.service)。 */
  restartHint?: string;
  /** 当前阶段:停机进行中 / 已停止。 */
  phase?: ServiceStopPhase;
}) {
  const t = useAppText();
  const formatError = useAppErrorText();
  const [checking, setChecking] = useState(false);
  const [error, setError] = useState("");
  const stopping = phase === "stopping";

  async function recheck() {
    if (checking) {
      return;
    }
    setChecking(true);
    setError("");
    try {
      await api.getRuntimeControl();
      onRestored();
    } catch (checkError) {
      setError(formatError(checkError));
    } finally {
      setChecking(false);
    }
  }

  return (
    <div className="flex h-full min-h-0 w-full items-center justify-center overflow-y-auto bg-background p-6 text-foreground">
      <div className="mx-auto w-full max-w-[560px]">
        <div className="rounded-md border border-border bg-card p-5">
          <div className="flex items-start gap-3">
            <span
              className={
                stopping
                  ? "flex h-9 w-9 shrink-0 items-center justify-center rounded-md bg-muted text-muted-foreground"
                  : "flex h-9 w-9 shrink-0 items-center justify-center rounded-md bg-destructive/10 text-destructive"
              }
            >
              {stopping ? (
                <LoaderCircle className="h-4 w-4 animate-spin" />
              ) : (
                <ServerCrash className="h-4 w-4" />
              )}
            </span>
            <div className="min-w-0 flex-1">
              <h1 className="text-title font-semibold">
                {stopping ? t("Stopping service") : t("Service stopped")}
              </h1>
              <p className="mt-1 text-caption leading-4 text-muted-foreground">
                {stopping
                  ? t("The stop request was accepted; waiting for the process to exit.")
                  : t("The browser cannot start a stopped process. Restart the service manually, then re-check.")}
              </p>
            </div>
          </div>

          {stopping ? null : restartHint ? (
            <div className="mt-4 grid gap-1 rounded-md bg-muted/20 px-3 py-2">
              <div className="text-caption font-medium text-muted-foreground">{t("Restart command")}</div>
              <div className="break-all font-mono text-caption text-foreground">{restartHint}</div>
            </div>
          ) : null}

          {error ? (
            <div className="mt-4 rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2 text-caption text-destructive">
              {error}
            </div>
          ) : null}

          {stopping ? null : (
            <div className="mt-4">
              <Button disabled={checking} onClick={recheck} size="sm" type="button" variant="outline">
                {checking ? <LoaderCircle className="h-4 w-4 animate-spin" /> : <RefreshCw className="h-4 w-4" />}
                {t("Re-check")}
              </Button>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
