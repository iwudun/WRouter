// 模型指纹库(方案 20261030_01)。2026-11-01 从设置分区升级为主导航独立
// 视图(调用日志与路由规则之间),页面结构对齐 routing.tsx 的裸排语言:
//
// - 顶部工具行(裸排在星空背景上):搜索 / 刷新 / 导入导出 / 新增指纹;
// - 库内容是**可折叠区块**(全局候选库 / 供应商锚点 / 局限说明);
// - 内置参考库已下线,页面只呈现用户自建的两层库;
// - 采集向导从内联面板改为 Dialog(与「新增路由规则」同一交互范式)。
import { useMemo, useState, type ReactNode } from "react";
import { motion } from "motion/react";
import {
  ChevronDown, Database, Download, Fingerprint, LoaderCircle, Plus, Search, Trash2, Upload
} from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog, DialogBody, DialogContent, DialogFooter, DialogHeader, DialogTitle
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { cn } from "@/lib/utils";
import { api } from "@/lib/api";
import type {
  FingerprintEnrollRequest,
  FingerprintEnrollResponse,
  FingerprintExportResponse,
  FingerprintLibraryEntry,
  FingerprintProbeTier,
  GatewayProviderConfig
} from "@/contracts/app";
// 2026-11-01:检测入口集中到指纹页(原供应商详情页的「检测模型指纹」已移除)。
import { FingerprintDetectDialog } from "./fingerprint-detect-dialog";
import { useFingerprintState, type FingerprintState } from "../hooks/useFingerprintState";
import { RefreshButton, SelectControl, useAppText } from "../shared/index";

const PROBE_TIER_LABEL: Record<FingerprintProbeTier, string> = {
  quick: "Quick (3 probes)",
  standard: "Standard (12 probes)",
  full: "Full (36 probes)"
};

/** 触发浏览器下载(评审 P1-2:导出必须有落地动作,不能只拿结果不产出文件)。 */
export function downloadFingerprintExport(response: FingerprintExportResponse) {
  const blob = new Blob([response.json], { type: "application/json" });
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = `wrouter-model-fingerprints-${new Date().toISOString().slice(0, 10)}.json`;
  anchor.click();
  URL.revokeObjectURL(url);
}

/** 主导航入口(自持状态)。 */
export function ModelFingerprintsView({ providers }: { providers: GatewayProviderConfig[] }) {
  const state = useFingerprintState();
  return <FingerprintLibraryView providers={providers} state={state} />;
}

/** 指纹库视图(状态注入 → 可单测)。 */
export function FingerprintLibraryView({
  providers,
  state
}: {
  providers: GatewayProviderConfig[];
  state: FingerprintState;
}) {
  const t = useAppText();
  const [query, setQuery] = useState("");
  const [wizardOpen, setWizardOpen] = useState(false);
  // 2026-11-01:检测弹窗(供应商/模型/档位/比对库都在弹窗内选)。
  const [detectOpen, setDetectOpen] = useState(false);
  const [wizardDraft, setWizardDraft] = useState<{ label?: string; modelName?: string }>({});
  const [expandedAnchors, setExpandedAnchors] = useState<string[]>([]);
  // 导出 / 导入的结果摘要(inline 展示;评审 P1-2 之前结果被直接丢弃)。
  const [transferNote, setTransferNote] = useState<{ kind: "error" | "success"; text: string }>();

  async function handleExport() {
    setTransferNote(undefined);
    const response = await state.exportLibrary();
    if (!response) {
      return; // 失败时 state.error 已 inline 展示。
    }
    try {
      downloadFingerprintExport(response);
      setTransferNote({
        kind: "success",
        text: `${t("Fingerprint library exported.")} · ${t("Models")}: ${response.modelCount} · ${t("Samples")}: ${response.sampleCount}${response.reason ? ` · ${response.reason}` : ""}`
      });
    } catch (downloadError) {
      setTransferNote({
        kind: "error",
        text: downloadError instanceof Error ? downloadError.message : String(downloadError)
      });
    }
  }

  async function handleImport(file: File) {
    setTransferNote(undefined);
    const response = await state.importLibrary(await file.text());
    if (!response) {
      return; // 失败时 state.error 已 inline 展示。
    }
    const parts = [t("Fingerprint library imported.")];
    parts.push(`${t("Imported")}: ${response.imported.length}`);
    if (response.skipped.length > 0) {
      parts.push(`${t("Skipped")}: ${response.skipped.length}`);
    }
    setTransferNote({
      kind: "success",
      text: parts.join(" · ")
    });
  }

  const normalizedQuery = query.trim().toLowerCase();
  const entries = state.library?.entries ?? [];
  const globalEntries = entries.filter((entry) => entry.kind === "global" && entryMatches(entry, normalizedQuery));
  const anchorEntries = entries.filter((entry) => entry.kind === "anchor");
  const anchorsByProvider = useMemo(() => {
    const groups = new Map<string, FingerprintLibraryEntry[]>();
    for (const entry of anchorEntries) {
      if (!entryMatches(entry, normalizedQuery)) {
        continue;
      }
      const key = entry.providerId ?? "";
      const bucket = groups.get(key);
      if (bucket) {
        bucket.push(entry);
      } else {
        groups.set(key, [entry]);
      }
    }
    return groups;
  }, [anchorEntries, normalizedQuery]);

  function toggleAnchor(providerId: string) {
    setExpandedAnchors((current) =>
      current.includes(providerId)
        ? current.filter((item) => item !== providerId)
        : [...current, providerId]
    );
  }

  function openWizard(draft: { label?: string; modelName?: string } = {}) {
    setWizardDraft(draft);
    setWizardOpen(true);
  }

  return (
    <motion.div
      animate={{ opacity: 1 }}
      className="flex h-full min-h-0 min-w-0 flex-col gap-3"
      initial={{ opacity: 0 }}
      transition={{ duration: 0.15 }}
    >
      {/* 工具行裸排在星空背景上,区块卡片只保留库数据(对齐 routing.tsx)。 */}
      <div className="flex flex-wrap items-center gap-2">
        <div className="relative min-w-0 flex-1">
          <Search className="pointer-events-none absolute left-2.5 top-1/2 z-[1] h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
          <Input
            aria-label={t("Search fingerprint entries")}
            className="pl-8"
            onChange={(event) => setQuery(event.target.value)}
            placeholder={t("Search fingerprint entries")}
            value={query}
          />
        </div>
        <RefreshButton
          ariaLabel={t("Refresh fingerprint library")}
          loading={state.loading}
          onRefresh={state.refresh}
          title={t("Refresh")}
        />
        <Button
          className="h-9 gap-1.5 px-2.5"
          disabled={state.busy}
          onClick={() => void handleExport()}
          size="sm"
          type="button"
          variant="outline"
        >
          <Download className="h-3.5 w-3.5" />
          {t("Export")}
        </Button>
        <Label className="inline-flex h-9 cursor-pointer items-center gap-1.5 rounded-md border border-border bg-background px-2.5 text-body hover:bg-muted">
          <Upload className="h-3.5 w-3.5" />
          {t("Import file")}
          <input
            accept="application/json,.json"
            className="hidden"
            disabled={state.busy}
            onChange={(event) => {
              const file = event.target.files?.[0];
              event.target.value = "";
              if (!file) {
                return;
              }
              void handleImport(file);
            }}
            type="file"
          />
        </Label>
        <Button
          aria-label={t("Detect model fingerprint")}
          onClick={() => setDetectOpen(true)}
          type="button"
          variant="outline"
        >
          <Fingerprint className="h-4 w-4" />
          {t("Detect model fingerprint")}
        </Button>
        <Button aria-label={t("Add fingerprint")} onClick={() => openWizard()} type="button">
          <Plus className="h-4 w-4" />
          {t("Add fingerprint")}
        </Button>
      </div>

      {/* 导入导出摘要 + 全局错误:页面级 inline 提示条。 */}
      {transferNote ? (
        <div
          className={cn(
            "rounded-md border px-3 py-2 text-caption",
            transferNote.kind === "success"
              ? "border-emerald-200 bg-emerald-50 text-emerald-800 dark:border-emerald-900/60 dark:bg-emerald-950/30 dark:text-emerald-200"
              : "border-destructive/30 bg-destructive/5 text-destructive"
          )}
        >
          {transferNote.text}
        </div>
      ) : null}
      {state.error ? (
        <div className="rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2 text-caption text-destructive">
          {state.error}
        </div>
      ) : null}

      {/* 库内容:两个库区块 + 局限说明,全部可折叠;滚动由本视图自管。 */}
      <div className="min-h-0 flex-1 space-y-2 overflow-auto px-1 py-0.5 pb-2">
        <CollapsibleSection count={globalEntries.length} title={t("Global candidate library")}>
          {globalEntries.length === 0 ? (
            <SectionEmpty label={normalizedQuery ? t("No matching fingerprint entries") : t("No fingerprint entries yet.")} />
          ) : (
            <div className="divide-y divide-border/50">
              {globalEntries.map((entry) => (
                <FingerprintEntryRow entry={entry} key={entry.id} onDelete={state.removeEntry} />
              ))}
            </div>
          )}
        </CollapsibleSection>

        <CollapsibleSection count={anchorEntries.length} title={t("Provider anchors")}>
          {anchorsByProvider.size === 0 ? (
            <SectionEmpty label={normalizedQuery ? t("No matching fingerprint entries") : t("No fingerprint entries yet.")} />
          ) : (
            <div className="divide-y divide-border/50">
              {[...anchorsByProvider.entries()].map(([providerId, group]) => {
                const provider = providers.find((item) => item.id === providerId || item.name === providerId);
                const expanded = expandedAnchors.includes(providerId);
                return (
                  <div key={providerId}>
                    <button
                      className="flex w-full min-w-0 items-center gap-2 px-3 py-2 text-left text-body font-medium transition-colors hover:bg-muted/50"
                      onClick={() => toggleAnchor(providerId)}
                      type="button"
                    >
                      <ChevronDown
                        className={cn("h-3 w-3 shrink-0 text-muted-foreground transition-transform duration-150", !expanded && "-rotate-90")}
                      />
                      <span className="min-w-0 flex-1 truncate">{provider?.name || providerId || "-"}</span>
                      <span className="shrink-0 text-caption text-muted-foreground">{group.length}</span>
                    </button>
                    {expanded ? (
                      <div className="divide-y divide-border/50">
                        {group.map((entry) => (
                          <FingerprintEntryRow entry={entry} key={entry.id} onDelete={state.removeEntry} />
                        ))}
                      </div>
                    ) : null}
                  </div>
                );
              })}
            </div>
          )}
        </CollapsibleSection>

        {/* 局限说明:默认收起,不占主视线。 */}
        <CollapsibleSection count={0} defaultOpen={false} mutedCount title={t("Limitations")}>
          <ul className="grid gap-1.5 px-3 py-2.5 text-caption leading-4 text-muted-foreground">
            <li>· {t("Closed-set attribution: a model outside the library is attributed to the closest candidate.")}</li>
            <li>· {t("System prompts injected by the relay affect the result; avoid detecting from clients with custom system prompts.")}</li>
          </ul>
        </CollapsibleSection>
      </div>

      {/* 采集向导:独立页里改为 Dialog(与「新增路由规则」同一范式)。 */}
      {wizardOpen ? (
        <FingerprintEnrollWizard
          initialLabel={wizardDraft.label}
          initialModelName={wizardDraft.modelName}
          onClose={() => setWizardOpen(false)}
          onEnroll={state.enroll}
          providers={providers}
        />
      ) : null}

      {/* 检测弹窗:供应商与模型在弹窗内自选(检测入口已从供应商详情页集中到这里)。 */}
      {detectOpen ? (
        <FingerprintDetectDialog
          onClose={() => setDetectOpen(false)}
          onDetect={(request) => api.detectModelFingerprint(request)}
          open
          providers={providers}
        />
      ) : null}
    </motion.div>
  );
}

/** 条目搜索匹配(备注 / 模型名 / 家族 / 供应商)。 */
function entryMatches(entry: FingerprintLibraryEntry, query: string): boolean {
  if (!query) {
    return true;
  }
  return [entry.label, entry.modelName, entry.familyName, entry.family, entry.providerId]
    .filter(Boolean)
    .join(" ")
    .toLowerCase()
    .includes(query);
}


/** 可折叠区块:标题行(chevron + 标题 + 计数)常驻,内容行收进卡内。 */
function CollapsibleSection({
  badge,
  children,
  count,
  defaultOpen = true,
  mutedCount = false,
  title
}: {
  badge?: ReactNode;
  children: ReactNode;
  count?: number;
  defaultOpen?: boolean;
  /** 计数为 0 时不显示数字(局限说明等无条目语义的区块)。 */
  mutedCount?: boolean;
  title: string;
}) {
  const [open, setOpen] = useState(defaultOpen);
  return (
    <section className="overflow-hidden rounded-md border border-border/60 bg-background/60">
      <button
        aria-expanded={open}
        className="flex w-full min-w-0 items-center gap-2 px-3 py-2.5 text-left transition-colors hover:bg-muted/50"
        onClick={() => setOpen((current) => !current)}
        type="button"
      >
        <ChevronDown
          className={cn("h-3.5 w-3.5 shrink-0 text-muted-foreground transition-transform duration-150", !open && "-rotate-90")}
        />
        <span className="min-w-0 truncate text-body-lg font-semibold text-foreground">{title}</span>
        {badge}
        {count !== undefined && !mutedCount ? (
          <span className="shrink-0 text-caption text-muted-foreground">{count}</span>
        ) : null}
      </button>
      {open ? <div className="border-t border-border/50">{children}</div> : null}
    </section>
  );
}

function SectionEmpty({ label }: { label: string }) {
  return (
    <div className="px-3 py-4 text-center text-body text-muted-foreground">
      {label}
    </div>
  );
}

function FingerprintEntryRow({
  entry,
  onDelete
}: {
  entry: FingerprintLibraryEntry;
  onDelete: (id: string) => Promise<boolean>;
}) {
  const t = useAppText();
  return (
    <div className="flex min-w-0 flex-wrap items-center gap-2 px-3 py-2 text-caption">
      <span className="min-w-0 flex-1 truncate font-mono font-semibold" title={entry.label || entry.modelName}>
        {entry.label || entry.modelName}
      </span>
      <span className="shrink-0 text-muted-foreground">
        {t("Family")}: {entry.familyName || entry.family || "-"}
      </span>
      <span className="shrink-0 text-muted-foreground">
        {t("Samples")}: {entry.sampleCount}
      </span>
      <span className="shrink-0 text-muted-foreground">
        {t("Coverage environments")}: {entry.probeEnv ? entry.probeEnv.split(",").filter(Boolean).length : 0}
      </span>
      <span className="shrink-0 text-muted-foreground">
        {t("Single-probe accuracy")}: {entry.cvAccuracy == null ? "-" : `${Math.round(entry.cvAccuracy * 100)}%`}
      </span>
      <Button
        aria-label={`${t("Delete")} ${entry.label || entry.modelName}`}
        className="h-6 w-6 shrink-0 p-0 hover:bg-destructive/20 hover:text-destructive"
        onClick={() => void onDelete(entry.id)}
        size="iconSm"
        title={t("Delete")}
        type="button"
        variant="ghost"
      >
        <Trash2 className="h-3.5 w-3.5" />
      </Button>
    </div>
  );
}

function FingerprintEnrollWizard({
  initialLabel,
  initialModelName,
  onClose,
  onEnroll,
  providers
}: {
  initialLabel?: string;
  initialModelName?: string;
  onClose: () => void;
  onEnroll: (request: FingerprintEnrollRequest) => Promise<FingerprintEnrollResponse>;
  providers: GatewayProviderConfig[];
}) {
  const t = useAppText();
  const [providerId, setProviderId] = useState(providers[0]?.id || providers[0]?.name || "");
  const [modelName, setModelName] = useState(initialModelName ?? "");
  const [label, setLabel] = useState(initialLabel ?? "");
  const [family, setFamily] = useState("");
  const [kind, setKind] = useState<"global" | "anchor">("global");
  const [tier, setTier] = useState<FingerprintProbeTier>("standard");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [result, setResult] = useState<FingerprintEnrollResponse>();

  const provider = providers.find((item) => item.id === providerId || item.name === providerId);
  const providerModels = provider?.models ?? [];

  async function submit() {
    if (busy) {
      return;
    }
    if (!providerId) {
      setError(t("Select a provider first."));
      return;
    }
    if (!modelName.trim()) {
      setError(t("Select a model first."));
      return;
    }
    setBusy(true);
    setError("");
    try {
      setResult(
        await onEnroll({
          family: family.trim() || undefined,
          kind,
          label: label.trim() || modelName.trim(),
          modelName: modelName.trim(),
          probeTier: tier,
          providerId
        })
      );
    } catch (enrollError) {
      // X7 重复收录 / 样本不足 → 后端直接报错,原文展示(inline)。
      setError(enrollError instanceof Error ? enrollError.message : String(enrollError));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog onOpenChange={(open) => { if (!open) onClose(); }}>
      <DialogContent>
        <DialogHeader>
          <div className="flex min-w-0 items-center gap-2">
            <Fingerprint className="h-4 w-4 shrink-0" />
            <DialogTitle>{t("Add fingerprint")}</DialogTitle>
          </div>
        </DialogHeader>

        <DialogBody>
          <div className="grid gap-3 sm:grid-cols-2">
            <div className="grid gap-1.5">
              <span className="text-caption font-medium text-muted-foreground">{t("Provider")}</span>
              <SelectControl
                aria-label={t("Provider")}
                onChange={(value) => {
                  setProviderId(value);
                  setModelName("");
                }}
                options={providers.map((item) => ({
                  label: item.name || item.id || "",
                  value: item.id || item.name || ""
                }))}
                value={providerId}
              />
            </div>
            <div className="grid gap-1.5">
              <span className="text-caption font-medium text-muted-foreground">{t("Model name")}</span>
              <Input
                aria-label={t("Model name")}
                list="fingerprint-model-options"
                onChange={(event) => setModelName(event.target.value)}
                placeholder={providerModels[0] ?? ""}
                spellCheck={false}
                value={modelName}
              />
              <datalist id="fingerprint-model-options">
                {providerModels.map((model) => (
                  <option key={model} value={model} />
                ))}
              </datalist>
            </div>
            <div className="grid gap-1.5">
              <span className="text-caption font-medium text-muted-foreground">{t("Family")}</span>
              <Input
                aria-label={t("Family")}
                onChange={(event) => setFamily(event.target.value)}
                value={family}
              />
            </div>
            <div className="grid gap-1.5">
              <span className="text-caption font-medium text-muted-foreground">{t("Notes")}</span>
              <Input aria-label={t("Notes")} onChange={(event) => setLabel(event.target.value)} value={label} />
            </div>
            <div className="grid gap-1.5">
              <span className="text-caption font-medium text-muted-foreground">{t("Probe tier")}</span>
              <SelectControl
                aria-label={t("Probe tier")}
                onChange={(value) => setTier(value as FingerprintProbeTier)}
                options={(["quick", "standard", "full"] as FingerprintProbeTier[]).map((value) => ({
                  label: t(PROBE_TIER_LABEL[value]),
                  value
                }))}
                value={tier}
              />
            </div>
            <div className="grid gap-1.5">
              <span className="text-caption font-medium text-muted-foreground">{t("Global candidate library")}</span>
              <SelectControl
                aria-label={t("Global candidate library")}
                onChange={(value) => setKind(value as "global" | "anchor")}
                options={[
                  { label: t("Global candidate library"), value: "global" },
                  { label: t("Provider anchors"), value: "anchor" }
                ]}
                value={kind}
              />
            </div>
          </div>

          {error ? (
            <div className="mt-3 rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2 text-caption text-destructive">
              {error}
            </div>
          ) : null}

          {result ? (
            <div className="mt-3 rounded-md border border-emerald-200 bg-emerald-50 px-3 py-2 text-caption text-emerald-800 dark:border-emerald-900/60 dark:bg-emerald-950/30 dark:text-emerald-200">
              <div className="font-medium">{result.entry?.label || result.entry?.modelName}</div>
              <div className="mt-0.5">
                {t("Sample count")}: {result.entry?.sampleCount ?? 0} · {t("Gaps")}: {result.gaps} ·{" "}
                {t("Single-probe accuracy")}:{" "}
                {result.entry?.cvAccuracy == null ? "-" : `${Math.round(result.entry.cvAccuracy * 100)}%`}
              </div>
            </div>
          ) : null}
        </DialogBody>

        <DialogFooter>
          <Button disabled={busy} onClick={onClose} type="button" variant="outline">
            {t("Cancel")}
          </Button>
          <Button
            disabled={busy}
            onClick={() => void submit()}
            type="button"
          >
            {busy ? <LoaderCircle className="h-4 w-4 animate-spin" /> : <Database className="h-4 w-4" />}
            {t("Confirm and save")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
