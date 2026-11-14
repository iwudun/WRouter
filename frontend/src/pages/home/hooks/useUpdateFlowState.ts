// R7 split: update dialog flow — check/download/install + update status subscription.
import { useEffect, useRef, useState } from "react";
import { AppUpdateStatus, fallbackUpdateStatus } from "../shared/index";
// R8: UpdateActionBusy used to be re-inlined here as a string union; import the
// single definition from the dialog component.
import type { UpdateActionBusy } from "../components/update";

export type UpdateFlowStateOptions = {
  appInfoVersion: string;
  formatError: (error: unknown) => string;
  t: (value: string) => string;
};

export function useUpdateFlowState({ appInfoVersion, formatError, t }: UpdateFlowStateOptions) {
  const [updateActionBusy, setUpdateActionBusy] = useState<UpdateActionBusy>("");
  const [updateActionError, setUpdateActionError] = useState("");
  const [updateDialogOpen, setUpdateDialogOpen] = useState(false);
  const [updateDialogStatus, setUpdateDialogStatus] = useState<AppUpdateStatus>(fallbackUpdateStatus);
  const updateActionBusyRef = useRef(false);

  useEffect(() => {
    if (!window.wr) {
      return;
    }

    let disposed = false;
    void window.wr.getUpdateStatus()
      .then((status) => {
        if (!disposed) {
          setUpdateDialogStatus(status);
        }
      })
      .catch(() => {
        if (!disposed) {
          setUpdateDialogStatus(fallbackUpdateStatus);
        }
      });

    const unsubscribe = window.wr.onUpdateStatusChanged((status) => {
      if (!disposed) {
        setUpdateDialogStatus(status);
      }
    });

    return () => {
      disposed = true;
      unsubscribe();
    };
  }, [appInfoVersion]);

  function openUpdateDialog() {
    setUpdateDialogOpen(true);
    setUpdateActionError("");
    void checkForAppUpdate();
  }

  function openSidebarUpdateDialog() {
    setUpdateDialogOpen(true);
    setUpdateActionError("");
    if (updateDialogStatus.canDownload || updateDialogStatus.state === "available") {
      void downloadAppUpdate();
      return;
    }
    if (
      updateDialogStatus.canInstall ||
      updateDialogStatus.state === "checking" ||
      updateDialogStatus.state === "downloading" ||
      updateDialogStatus.state === "downloaded" ||
      updateDialogStatus.state === "installing"
    ) {
      return;
    }
    void checkForAppUpdate();
  }

  async function checkForAppUpdate() {
    if (updateActionBusyRef.current) {
      return;
    }
    if (!window.wr?.updateCheck) {
      setUpdateDialogStatus({
        ...fallbackUpdateStatus,
        currentVersion: appInfoVersion,
        lastError: t("Automatic updates are not available in this build."),
        state: "error"
      });
      return;
    }

    updateActionBusyRef.current = true;
    setUpdateActionBusy("check");
    setUpdateActionError("");
    try {
      setUpdateDialogStatus(await window.wr.updateCheck());
    } catch {
      // 20261102_01 P5-3（D2）：拿不到状态**不是错误** —— 本来就没有更新
      // 通道。回落到只读态,让对话框显示「自动更新暂不可用」,而不是弹一条
      // 红字错误(浏览器运行时 updateCheck 直接 reject)。
      setUpdateDialogStatus({
        ...fallbackUpdateStatus,
        currentVersion: appInfoVersion
      });
    } finally {
      updateActionBusyRef.current = false;
      setUpdateActionBusy("");
    }
  }

  async function downloadAppUpdate() {
    if (updateActionBusyRef.current || !window.wr?.updateDownload) {
      return;
    }

    updateActionBusyRef.current = true;
    setUpdateActionBusy("download");
    setUpdateActionError("");
    try {
      setUpdateDialogStatus(await window.wr.updateDownload());
    } catch (error) {
      setUpdateActionError(formatError(error));
    } finally {
      updateActionBusyRef.current = false;
      setUpdateActionBusy("");
    }
  }

  async function installAppUpdate() {
    if (updateActionBusyRef.current || !window.wr?.updateInstall) {
      return;
    }

    updateActionBusyRef.current = true;
    setUpdateActionBusy("install");
    setUpdateActionError("");
    try {
      await window.wr.updateInstall();
    } catch (error) {
      setUpdateActionError(formatError(error));
      updateActionBusyRef.current = false;
      setUpdateActionBusy("");
    }
  }

  return {
    updateActionBusy,
    updateActionError,
    setUpdateActionError,
    updateDialogOpen,
    setUpdateDialogOpen,
    updateDialogStatus,
    openUpdateDialog,
    openSidebarUpdateDialog,
    checkForAppUpdate,
    downloadAppUpdate,
    installAppUpdate
  };
}
