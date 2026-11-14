import { CircleAlert, Trash2, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogBody, DialogContent, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { GatewayProviderConfig, providerBaseUrl, useAppText } from "../../shared/index";

// F4.3: 从 providers-list.tsx 拆出 —— 删除 provider 确认对话框。

export function DeleteProviderDialog({
  onClose,
  onConfirm,
  provider
}: {
  onClose: () => void;
  onConfirm: () => void;
  provider: GatewayProviderConfig;
}) {
  const t = useAppText();
  const name = provider.name || t("Unnamed provider");
  const baseUrl = providerBaseUrl(provider);

  return (
    <Dialog onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-[520px]">
        <DialogHeader>
          <div className="min-w-0">
            <DialogTitle>{t("Delete Provider")}</DialogTitle>
          </div>
          <Button aria-label={t("Close dialog")} onClick={onClose} size="iconSm" title={t("Close")} type="button" variant="ghost">
            <X className="h-4 w-4" />
          </Button>
        </DialogHeader>

        <DialogBody>
          <div className="rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2.5">
            <div className="flex items-start gap-2 text-body font-medium text-destructive">
              <CircleAlert className="mt-0.5 h-3.5 w-3.5 shrink-0" />
              <span>{t("Delete this provider from the configuration?")}</span>
            </div>
            <div className="mt-2 space-y-1 text-caption text-muted-foreground">
              <div className="truncate">
                <span className="font-medium text-foreground">{t("Name")}:</span> {name}
              </div>
              <div className="truncate" title={baseUrl}>
                <span className="font-medium text-foreground">{t("Request URL")}:</span> {baseUrl || t("Not set")}
              </div>
              <div>{t("This action is applied immediately to the draft config and will auto-save with other changes.")}</div>
            </div>
          </div>
        </DialogBody>

        <DialogFooter>
          <Button autoFocus onClick={onClose} type="button" variant="outline">
            {t("Cancel")}
          </Button>
          <Button onClick={onConfirm} type="button" variant="destructive">
            <Trash2 className="h-4 w-4" />
            {t("Delete")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
