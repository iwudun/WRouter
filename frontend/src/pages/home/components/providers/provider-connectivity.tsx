import { useEffect, useRef, useState } from "react";
import { CircleAlert, LoaderCircle, ShieldCheck, X } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Dialog, DialogBody, DialogContent, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Label } from "@/components/ui/label";
import { cn } from "@/lib/utils";
import {
  AnimatedIconSwap, mergeProviderModelLists,
  ProviderConnectivityCheckReport, translateProbeProtocolMessage, translatedProviderProtocolLabel, useAppText,
} from "../../shared/index";

// F4.3: 从 providers-list.tsx 拆出 —— 连通性检查对话框与结果面板。

export function ProviderConnectivityCheckDialog({
  models,
  onCheck,
  onClose,
  open,
  providerName
}: {
  models: string[];
  onCheck: (models: string[]) => Promise<ProviderConnectivityCheckReport>;
  onClose: () => void;
  open: boolean;
  providerName?: string;
}) {
  const t = useAppText();
  const [busy, setBusy] = useState(false);
  const [checkModelSelection, setCheckModelSelection] = useState<string[]>([]);
  const [checkResult, setCheckResult] = useState<ProviderConnectivityCheckReport>();

  // Initialize only on the open rising edge. The add-provider panel recomputes
  // a fresh `models` array on every render, so resetting on `models` identity
  // would wipe the result right after each check (parent state updates from
  // the check itself trigger a re-render). In-dialog selection changes clear
  // the result explicitly in toggleCheckModel / select-all / clear-all.
  const prevOpenRef = useRef(false);
  useEffect(() => {
    if (open && !prevOpenRef.current) {
      setCheckModelSelection(models);
      setCheckResult(undefined);
    }
    prevOpenRef.current = open;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  function toggleCheckModel(model: string) {
    setCheckModelSelection((current) =>
      current.includes(model)
        ? current.filter((item) => item !== model)
        : mergeProviderModelLists(current, [model])
    );
    setCheckResult(undefined);
  }

  async function confirmCheck() {
    setBusy(true);
    try {
      setCheckResult(await onCheck(checkModelSelection));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog onOpenChange={(nextOpen) => { if (!nextOpen) onClose(); }} open={open}>
      <DialogContent className="max-w-[520px]">
        <DialogHeader>
          <div className="min-w-0">
            <DialogTitle>{t("Check Availability")}</DialogTitle>
          </div>
          <Button
            aria-label={t("Close dialog")}
            disabled={busy}
            onClick={onClose}
            size="iconSm"
            title={t("Close")}
            type="button"
            variant="ghost"
          >
            <X className="h-4 w-4" />
          </Button>
        </DialogHeader>
        <DialogBody>
          <div className="space-y-3">
            {providerName ? <div className="truncate text-title font-semibold text-foreground">{providerName}</div> : null}
            <div className="rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2.5">
              <div className="flex items-start gap-2 text-body font-medium text-amber-900 dark:text-amber-100">
                <CircleAlert className="mt-0.5 h-3.5 w-3.5 shrink-0" />
                <span>{t("This check sends real model requests with your provider API key and may consume account balance.")}</span>
              </div>
            </div>

            <div className="rounded-md border border-border bg-background p-2">
              <div className="mb-2 flex min-w-0 flex-wrap items-center justify-between gap-2">
                <div className="min-w-0 truncate text-body font-semibold">{t("Models to check")}</div>
                <div className="flex shrink-0 gap-1">
                  <Button className="h-6 px-1.5 text-caption" disabled={busy || models.length === 0} onClick={() => { setCheckModelSelection(models); setCheckResult(undefined); }} type="button" variant="outline">
                    {t("Select all")}
                  </Button>
                  <Button className="h-6 px-1.5 text-caption" disabled={busy || checkModelSelection.length === 0} onClick={() => { setCheckModelSelection([]); setCheckResult(undefined); }} type="button" variant="outline">
                    {t("Clear all")}
                  </Button>
                </div>
              </div>
              <div className="max-h-[180px] overflow-auto">
                <div className="grid grid-cols-1 gap-2">
                  {models.map((model) => {
                    const checked = checkModelSelection.includes(model);
                    return (
                      <Label
                        className={cn(
                          "flex min-h-8 min-w-0 cursor-pointer items-center gap-2 rounded-md border border-border bg-background px-2 py-1.5 text-left text-body transition-colors hover:bg-muted",
                          checked && "border-primary bg-accent"
                        )}
                        key={model}
                      >
                        <Checkbox checked={checked} disabled={busy} onCheckedChange={() => toggleCheckModel(model)} />
                        <span className="min-w-0 flex-1 truncate font-mono text-caption" title={model}>{model}</span>
                      </Label>
                    );
                  })}
                </div>
              </div>
            </div>

            {checkResult ? <ProviderConnectivityResultPanel result={checkResult} /> : null}
          </div>
        </DialogBody>
        <DialogFooter>
          <Button disabled={busy} onClick={onClose} type="button" variant="outline">
            {checkResult ? t("Close") : t("Cancel")}
          </Button>
          <Button disabled={busy || checkModelSelection.length === 0} onClick={() => void confirmCheck()} type="button">
            <AnimatedIconSwap iconKey={busy ? "checking" : "start"}>
              {busy ? <LoaderCircle className="h-4 w-4 animate-spin" /> : <ShieldCheck className="h-4 w-4" />}
            </AnimatedIconSwap>
            {t("Start check")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

export function ProviderConnectivityResultPanel({ result }: { result: ProviderConnectivityCheckReport }) {
  const t = useAppText();

  return (
    <div className="rounded-md border border-border bg-muted/20">
      <div className="flex min-w-0 items-center justify-between gap-2 border-b border-border px-3 py-2">
        <div className="min-w-0 truncate text-body font-semibold">{t("Check results")}</div>
        <div className="flex shrink-0 gap-1">
          <Badge variant={result.passed.length > 0 ? "success" : "outline"}>{result.passed.length} {t("Available")}</Badge>
          <Badge variant={result.failed.length > 0 ? "warning" : "outline"}>{result.failed.length} {t("Unavailable")}</Badge>
        </div>
      </div>
      <div className="max-h-[220px] overflow-auto p-2">
        <ProviderConnectivityResultGroup
          emptyLabel={t("No available models")}
          items={result.passed}
          label={t("Available models")}
          variant="success"
        />
        <ProviderConnectivityResultGroup
          className="mt-2"
          emptyLabel={t("No unavailable models")}
          items={result.failed}
          label={t("Unavailable models")}
          variant="warning"
        />
      </div>
    </div>
  );
}

function ProviderConnectivityResultGroup({
  className,
  emptyLabel,
  items,
  label,
  variant
}: {
  className?: string;
  emptyLabel: string;
  items: ProviderConnectivityCheckReport["results"];
  label: string;
  variant: "success" | "warning";
}) {
  const t = useAppText();

  return (
    <div className={className}>
      <div className="mb-1 min-w-0 text-caption font-medium uppercase tracking-wide text-muted-foreground">
        <span className="min-w-0 truncate">{label}</span>
      </div>
      {items.length === 0 ? (
        <div className="rounded-md border border-dashed border-border bg-background/70 px-2 py-2 text-center text-caption text-muted-foreground">{emptyLabel}</div>
      ) : (
        <div className="space-y-1.5">
          {items.map((item) => {
            const supportedProtocols = item.protocols.filter((protocol) => protocol.supported);
            const metricProtocol = item.protocols.find((protocol) => protocol.durationMs != null || protocol.usage != null);
            const latencyMs = metricProtocol?.durationMs;
            const usage = metricProtocol?.usage;
            // 20260906_01 §4.6:本次探测实际走的出口(direct/system/预设名)。
            const proxySource = item.protocols.find((protocol) => protocol.proxy)?.proxy;
            return (
              <div className="min-w-0 rounded-md border border-border bg-background px-2 py-1.5 text-caption" key={item.model}>
                <div className="flex min-w-0 items-center justify-between gap-2">
                  <span className="min-w-0 truncate font-mono font-semibold" title={item.model}>{item.model}</span>
                  <Badge variant={variant}>{item.supported ? t("Available") : t("Unavailable")}</Badge>
                </div>
                <div className="mt-1 truncate text-muted-foreground" title={translateProbeProtocolMessage(item.message, t)}>
                  {translateProbeProtocolMessage(item.message, t)}
                </div>
                {latencyMs != null || usage || proxySource ? (
                  <div className="mt-1 flex flex-wrap items-center gap-2 font-mono text-caption text-muted-foreground">
                    {latencyMs != null ? <span title={t("Latency")}>{latencyMs}ms</span> : null}
                    {usage ? <span title={t("Tokens consumed")}>↑{usage.prompt_tokens ?? 0} ↓{usage.completion_tokens ?? 0} {t("tokens")}</span> : null}
                    {proxySource ? <span title={t("Proxy source")}>{t("Proxy source")}: {proxySource}</span> : null}
                  </div>
                ) : null}
                {supportedProtocols.length > 0 ? (
                  <div className="mt-1 flex flex-wrap gap-1">
                    {supportedProtocols.map((protocol) => (
                      <Badge key={`${item.model}:${protocol.protocol}:${protocol.endpoint}`} variant="outline">
                        {translatedProviderProtocolLabel(protocol.protocol, t)}
                      </Badge>
                    ))}
                  </div>
                ) : null}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
