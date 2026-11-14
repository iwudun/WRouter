import { useEffect, useRef, useState } from "react";
import { CircleAlert, Fingerprint, LoaderCircle, X } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Dialog,
  DialogBody,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle
} from "@/components/ui/dialog";
import { Label } from "@/components/ui/label";
import { cn } from "@/lib/utils";
import type {
  FingerprintAttribution,
  FingerprintDetectRequest,
  FingerprintDetectResponse,
  FingerprintDetectionResult,
  FingerprintEvidenceStrength,
  FingerprintProbeTier,
  GatewayProviderConfig
} from "@/contracts/app";
import { SelectControl, useAppText } from "../shared/index";

// 方案 20261030_01 F2:模型指纹检测弹窗。
//
// 2026-11-01:从供应商详情页迁到「模型指纹」独立页(用户要求检测入口集中
// 到指纹页)—— 供应商不再由详情页上下文注入,而是**弹窗内自选**;选中供应
// 商后模型清单与检测请求都跟随该供应商。
//
// 结构照 `provider-connectivity.tsx`:配置区 / 运行区 / 结果区 / inline 错误。
// **不弹 toast** —— 后端的拒绝理由(如跨套件不可比对)必须留在弹窗里让用户
// 看到完整上下文(方案 §3.6 第 5 条)。

/** 探针档位 → 探针条数(与后端 `ProbeTier::probe_count` 对齐)。 */
const PROBE_TIER_PROBES: Record<FingerprintProbeTier, number> = {
  quick: 3,
  standard: 12,
  full: 36
};

const PROBE_TIER_LABEL: Record<FingerprintProbeTier, string> = {
  quick: "Quick (3 probes)",
  standard: "Standard (12 probes)",
  full: "Full (36 probes)"
};

export function FingerprintDetectDialog({
  onClose,
  onDetect,
  open,
  providers
}: {
  onClose: () => void;
  onDetect: (request: FingerprintDetectRequest) => Promise<FingerprintDetectResponse>;
  open: boolean;
  providers: GatewayProviderConfig[];
}) {
  const t = useAppText();
  const [providerId, setProviderId] = useState("");
  const [selectedModels, setSelectedModels] = useState<string[]>([]);
  const [tier, setTier] = useState<FingerprintProbeTier>("standard");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [result, setResult] = useState<FingerprintDetectResponse>();

  const provider = providers.find((item) => item.id === providerId || item.name === providerId);
  const models = provider?.models ?? [];

  // Only reset on the open rising edge: `providers` is recomputed on every
  // render by the parent, so resetting on its identity would wipe the result
  // right after each detection.
  const prevOpenRef = useRef(false);
  useEffect(() => {
    if (open && !prevOpenRef.current) {
      const first = providers[0];
      const firstId = first?.id || first?.name || "";
      setProviderId(firstId);
      setSelectedModels(first?.models ?? []);
      setTier("standard");
      setResult(undefined);
      setError("");
    }
    prevOpenRef.current = open;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  // 切供应商:模型清单与结果一并重置(检测请求永远跟随当前选中供应商)。
  function changeProvider(nextId: string) {
    setProviderId(nextId);
    setSelectedModels(providers.find((item) => item.id === nextId || item.name === nextId)?.models ?? []);
    setResult(undefined);
  }

  function toggleModel(model: string) {
    setSelectedModels((current) =>
      current.includes(model)
        ? current.filter((item) => item !== model)
        : [...current, model]
    );
    setResult(undefined);
  }

  async function runDetection() {
    if (busy || !providerId || selectedModels.length === 0) {
      return;
    }
    setBusy(true);
    setError("");
    setResult(undefined);
    try {
      const response = await onDetect({
        modelNames: selectedModels,
        probeTier: tier,
        providerId
      });
      setResult(response);
    } catch (detectError) {
      setError(detectError instanceof Error ? detectError.message : String(detectError));
    } finally {
      setBusy(false);
    }
  }

  const estimatedProbes = PROBE_TIER_PROBES[tier] * selectedModels.length;
  const tierOptions = (["quick", "standard", "full"] as FingerprintProbeTier[]).map((value) => ({
    label: t(PROBE_TIER_LABEL[value]),
    value
  }));


  return (
    <Dialog onOpenChange={(nextOpen) => { if (!nextOpen) onClose(); }} open={open}>
      <DialogContent className="max-w-[640px]">
        <DialogHeader>
          <div className="flex min-w-0 items-center gap-2">
            <Fingerprint className="h-4 w-4 shrink-0" />
            <DialogTitle>{t("Detect model fingerprint")}</DialogTitle>
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
            <div className="rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2.5">
              <div className="flex items-start gap-2 text-body font-medium text-amber-900 dark:text-amber-100">
                <CircleAlert className="mt-0.5 h-3.5 w-3.5 shrink-0" />
                <span>
                  {t("This detection sends real model requests with your provider API key and may consume account balance.")}
                </span>
              </div>
            </div>

            {/* ---- 配置区 ---- */}
            <div className="grid gap-3 sm:grid-cols-2">
              <div className="rounded-md border border-border bg-background p-2">
                <div className="mb-1.5 text-caption font-medium text-muted-foreground">{t("Provider")}</div>
                <SelectControl
                  aria-label={t("Provider")}
                  onChange={changeProvider}
                  options={providers.map((item) => ({
                    label: item.name || item.id || "",
                    value: item.id || item.name || ""
                  }))}
                  value={providerId}
                />
              </div>
              <div className="rounded-md border border-border bg-background p-2 sm:col-span-2">
                <div className="mb-1.5 flex items-center justify-between gap-2">
                  <span className="text-caption font-medium text-muted-foreground">{t("Probe tier")}</span>
                  <span className="text-caption text-muted-foreground">
                    {t("Estimated probes")}: {estimatedProbes}
                  </span>
                </div>
                <SelectControl
                  aria-label={t("Probe tier")}
                  onChange={(value) => {
                    setTier(value as FingerprintProbeTier);
                    setResult(undefined);
                  }}
                  options={tierOptions}
                  value={tier}
                />
              </div>
            </div>

            <div className="rounded-md border border-border bg-background p-2">
              <div className="mb-2 flex min-w-0 flex-wrap items-center justify-between gap-2">
                <div className="min-w-0 truncate text-body font-semibold">{t("Models to detect")}</div>
                <div className="flex shrink-0 gap-1">
                  <Button
                    className="h-6 px-1.5 text-caption"
                    disabled={busy || models.length === 0}
                    onClick={() => { setSelectedModels(models); setResult(undefined); }}
                    type="button"
                    variant="outline"
                  >
                    {t("Select all")}
                  </Button>
                  <Button
                    className="h-6 px-1.5 text-caption"
                    disabled={busy || selectedModels.length === 0}
                    onClick={() => { setSelectedModels([]); setResult(undefined); }}
                    type="button"
                    variant="outline"
                  >
                    {t("Clear all")}
                  </Button>
                </div>
              </div>
              <div className="max-h-[160px] overflow-auto">
                <div className="grid grid-cols-1 gap-2">
                  {models.map((model) => {
                    const checked = selectedModels.includes(model);
                    return (
                      <Label
                        className={cn(
                          "flex min-h-8 min-w-0 cursor-pointer items-center gap-2 rounded-md border border-border bg-background px-2 py-1.5 text-left text-body transition-colors hover:bg-muted",
                          checked && "border-primary bg-accent"
                        )}
                        key={model}
                      >
                        <Checkbox checked={checked} disabled={busy} onCheckedChange={() => toggleModel(model)} />
                        <span className="min-w-0 flex-1 truncate font-mono text-caption" title={model}>{model}</span>
                      </Label>
                    );
                  })}
                </div>
              </div>
            </div>

            {/* ---- 运行区 ---- */}
            {busy ? (
              <div className="flex items-center gap-2 rounded-md border border-border bg-muted/20 px-3 py-2 text-body">
                <LoaderCircle className="h-4 w-4 animate-spin" />
                <span>
                  {t("Detecting")} · {t("Estimated probes")}: {estimatedProbes}
                </span>
              </div>
            ) : null}

            {/* ---- inline 错误(不弹 toast) ---- */}
            {error ? (
              <div className="rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2 text-caption text-destructive">
                {error}
              </div>
            ) : null}

            {/* ---- 结果区 ---- */}
            {result ? <FingerprintDetectionResults result={result} /> : null}
          </div>
        </DialogBody>
        <DialogFooter>
          <Button disabled={busy} onClick={onClose} type="button" variant="outline">
            {result ? t("Close") : t("Cancel")}
          </Button>
          <Button
            disabled={busy || selectedModels.length === 0}
            onClick={() => void runDetection()}
            type="button"
          >
            {busy ? <LoaderCircle className="h-4 w-4 animate-spin" /> : <Fingerprint className="h-4 w-4" />}
            {busy ? t("Detecting") : t("Start detection")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/** 两段结论 + 诊断行(方案 §3.6 结果区)。 */
export function FingerprintDetectionResults({
  result
}: {
  result: FingerprintDetectResponse;
}) {
  const t = useAppText();

  return (
    <div className="space-y-3">
      <div className="min-w-0 truncate text-body font-semibold">{t("Detection results")}</div>
      {result.results.map((item) => (
        <FingerprintDetectionResultCard key={item.model} result={item} />
      ))}
    </div>
  );
}

function FingerprintDetectionResultCard({
  result
}: {
  result: FingerprintDetectionResult;
}) {
  const t = useAppText();
  const primary = primaryAttribution(result);
  const strength = primary?.strength;

  return (
    <div className="rounded-md border border-border bg-muted/20">
      <div className="flex min-w-0 items-center justify-between gap-2 border-b border-border px-3 py-2">
        <div className="min-w-0 truncate font-mono text-body font-semibold" title={result.model}>
          {result.model}
        </div>
        {strength ? (
          <Badge variant={strengthVariant(strength)}>
            {t("Evidence strength")}: {strengthLabel(t, strength)}
          </Badge>
        ) : null}
      </div>

      <div className="space-y-2 p-2">
        <div className="grid gap-2">
          <FingerprintConclusionCard
            attribution={result.conclusions.global}
            label={t("Global candidate conclusion")}
          />
          <FingerprintConclusionCard
            attribution={result.conclusions.anchor}
            label={t("Provider anchor conclusion")}
          />
        </div>

        <div className="flex flex-wrap items-center gap-3 px-1 text-caption text-muted-foreground">
          <span>
            {t("Valid probes")}: {result.diagnostics.accepted}/{result.diagnostics.probes}
          </span>
          <span>
            {t("Gaps")}: {result.diagnostics.gaps}
          </span>
          <span>
            {t("Parse failures")}: {result.diagnostics.parseFailures}
          </span>
          <span>
            {t("Probe suite")}: {result.probeSuite}
          </span>
          <span>
            {t("Duration")}: {result.diagnostics.durationMs}ms
          </span>
        </div>

        {result.skipped.length > 0 ? (
          <div className="rounded-md border border-amber-500/40 bg-amber-500/10 px-2 py-1.5 text-caption text-amber-900 dark:text-amber-100">
            <div className="font-medium">{t("Skipped libraries")}</div>
            {result.skipped.map((item) => (
              <div className="mt-0.5" key={`${item.library}:${item.reason}`}>
                <span className="font-mono">{item.library}</span>: {item.reason}
              </div>
            ))}
          </div>
        ) : null}
      </div>
    </div>
  );
}

function FingerprintConclusionCard({
  attribution,
  label
}: {
  attribution?: FingerprintAttribution;
  label: string;
}) {
  const t = useAppText();

  if (!attribution) {
    return (
      <div className="rounded-md border border-dashed border-border bg-background/70 px-3 py-2">
        <div className="text-caption font-medium text-muted-foreground">{label}</div>
        <div className="mt-1 text-caption text-muted-foreground">{t("No conclusion for this library.")}</div>
      </div>
    );
  }

  if (attribution.error) {
    return (
      <div className="rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2">
        <div className="text-caption font-medium text-foreground">{label}</div>
        <div className="mt-1 text-caption text-destructive">{attribution.error}</div>
      </div>
    );
  }

  // 方案 X4:证据不足 → **不显示具体模型名**(候选列表也一并隐藏)。
  const insufficient = attribution.strength === "insufficient";
  const name = attribution.prediction_name ?? attribution.prediction ?? "";

  return (
    <div className="rounded-md border border-border bg-background px-3 py-2">
      <div className="flex min-w-0 items-center justify-between gap-2">
        <span className="min-w-0 truncate text-caption font-medium">{label}</span>
        <Badge variant={strengthVariant(attribution.strength)}>{strengthLabel(t, attribution.strength)}</Badge>
      </div>
      {insufficient ? (
        <div className="mt-1.5 text-caption text-muted-foreground">{t("Evidence insufficient to attribute")}</div>
      ) : (
        <>
          <div className="mt-1.5 truncate text-body font-semibold" title={name}>
            {t("Most likely model")}: {name}
          </div>
          <div className="mt-1.5 space-y-1">
            {attribution.results.slice(0, 5).map((candidate) => (
              <div className="flex items-center gap-2 text-caption" key={candidate.model}>
                <span className="min-w-0 flex-1 truncate font-mono" title={candidate.display_name || candidate.model}>
                  {candidate.display_name || candidate.model}
                </span>
                <span className="h-1.5 w-20 shrink-0 overflow-hidden rounded-full bg-muted">
                  <span
                    className="block h-full rounded-full bg-primary"
                    style={{ width: `${Math.max(0, Math.min(100, Math.round(candidate.probability * 100)))}%` }}
                  />
                </span>
                <span className="w-12 shrink-0 text-right font-mono">{candidate.probability.toFixed(3)}</span>
              </div>
            ))}
          </div>
        </>
      )}
    </div>
  );
}

/** 结论的主来源:全局库 / 锚点(按本次实际比对的库优先)。 */
export function primaryAttribution(result: FingerprintDetectionResult): FingerprintAttribution | undefined {
  const order: Array<"global" | "anchor"> = ["global", "anchor"];
  for (const key of order) {
    const attribution = result.conclusions[key];
    if (attribution && !attribution.error) {
      return attribution;
    }
  }
  return undefined;
}

function strengthVariant(strength: FingerprintEvidenceStrength): "danger" | "outline" | "success" | "warning" {
  if (strength === "strong") {
    return "success";
  }
  if (strength === "moderate") {
    return "warning";
  }
  if (strength === "insufficient") {
    return "danger";
  }
  return "outline";
}

function strengthLabel(t: (value: string) => string, strength: FingerprintEvidenceStrength): string {
  if (strength === "strong") {
    return t("Strong evidence");
  }
  if (strength === "moderate") {
    return t("Moderate evidence");
  }
  if (strength === "weak") {
    return t("Weak evidence");
  }
  return t("Insufficient evidence");
}
