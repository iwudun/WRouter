import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { AnimatePresence, motion } from "motion/react";
import { Check, ChevronDown, CircleAlert, Copy, KeyRound, Pencil, Plus, Power, Search, Trash2, X } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { Dialog, DialogBody, DialogContent, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { cn } from "@/lib/utils";
import { api, hasBridge } from "@/lib/api";
import {
  AddApiKeyDraft, AnimatedDisclosure, AnimatedIconSwap, AnimatedListItem, apiKeyExpirationOptions, ApiKeyExpirationPreset,
  apiKeyExpirationStatus, ApiKeyLimitDraftRow, ApiKeyLimitMetric, apiKeyLimitMetricOptions, ApiKeyListItem, apiKeyMatchesQuery,
  copyTextToClipboard, createApiKeyLimitDraftRow, disclosureSpringTransition, EndpointInfoRow, Field,
  formatApiKeyExpiration, formatCompactTokenCount, formatQuotaWindowDuration, gatewayBaseUrlFromEndpoint, limitWindowOptions,
  LimitWindowPreset, loopbackEndpointFromStatus, quotaWindowOptions, quotaWindowPresetFromMs, QuotaWindowChoice, relativeTimeUnitLabel,
  relativeTimeValue, SelectControl, translateOptions,
  useAppText
} from "../shared/index";
import type { ApiKeyLimitConfig } from "@/contracts/app";
import type { AppConfig, GatewayStatus } from "@/contracts/app";

// Legacy keys migrated from plain strings predate createdAt tracking (epoch 0);
// anything before 2001 is treated as "unknown" rather than "56 years ago".
const MEANINGFUL_CREATED_AT_MS = 978307200_000;

// All content columns are flexible (fr) so they grow together and stay in
// the same width band on wide windows; only the action column stays fixed so
// its three icon buttons never wrap. The key column keeps the largest share
// (masked value + copy button), name slightly above the rest.
const KEY_LIST_GRID = "grid-cols-[minmax(110px,1fr)_minmax(88px,0.8fr)_minmax(210px,1.3fr)_minmax(120px,0.9fr)_minmax(100px,0.85fr)_minmax(88px,0.85fr)_minmax(88px,0.85fr)_minmax(88px,0.85fr)_104px]";

type ApiKeyStatusFilter = "all" | "disabled" | "enabled" | "expired";

const apiKeyStatusFilterOptions: Array<{ label: string; value: ApiKeyStatusFilter }> = [
  { label: "All", value: "all" },
  { label: "Enabled", value: "enabled" },
  { label: "Disabled", value: "disabled" },
  { label: "Expired", value: "expired" }
];

function matchesStatusFilter(apiKey: ApiKeyListItem, filter: ApiKeyStatusFilter): boolean {
  if (filter === "all") {
    return true;
  }
  if (filter === "enabled") {
    return apiKey.enabled;
  }
  if (filter === "disabled") {
    return !apiKey.enabled;
  }
  return apiKeyExpirationStatus(apiKey.expiresAt) === "expired";
}

export function ApiKeysView({
  addApiKey,
  apiKeys,
  config,
  editApiKey,
  endpoint,
  error,
  gatewayStatus,
  notify,
  removeApiKey,
  toggleApiKey
}: {
  addApiKey: () => void;
  apiKeys: ApiKeyListItem[];
  config: AppConfig;
  editApiKey: (index: number) => void;
  endpoint: string;
  error: string;
  gatewayStatus: GatewayStatus;
  notify: (message: string) => void;
  removeApiKey: (index: number) => void;
  toggleApiKey: (index: number) => void;
}) {
  const t = useAppText();
  const [query, setQuery] = useState("");
  const [statusFilter, setStatusFilter] = useState<ApiKeyStatusFilter>("all");
  const [deleteTarget, setDeleteTarget] = useState<ApiKeyListItem>();
  const [lastUsed, setLastUsed] = useState<Record<string, string>>({});
  const normalizedQuery = query.trim().toLowerCase();
  const visibleApiKeys = useMemo(
    () => apiKeys.filter((apiKey) => apiKeyMatchesQuery(apiKey, normalizedQuery) && matchesStatusFilter(apiKey, statusFilter)),
    [apiKeys, normalizedQuery, statusFilter]
  );
  // Per-chip counters follow the search query (same semantics as the call
  // logs chips) so the counts always describe the searched key set.
  const statusCounts = useMemo(() => {
    const searched = apiKeys.filter((apiKey) => apiKeyMatchesQuery(apiKey, normalizedQuery));
    const counts: Record<ApiKeyStatusFilter, number> = { all: searched.length, disabled: 0, enabled: 0, expired: 0 };
    for (const apiKey of searched) {
      if (apiKey.enabled) {
        counts.enabled += 1;
      } else {
        counts.disabled += 1;
      }
      if (apiKeyExpirationStatus(apiKey.expiresAt) === "expired") {
        counts.expired += 1;
      }
    }
    return counts;
  }, [apiKeys, normalizedQuery]);

  // "Last used" comes from the request-log aggregation RPC; refresh when the
  // key set changes (create/delete). Optional-chained so an older preload
  // without the RPC degrades to "—" instead of throwing.
  useEffect(() => {
    if (!hasBridge()) {
      return;
    }
    let cancelled = false;
    void api.getApiKeyLastUsed()
      .then((map) => {
        if (!cancelled && map) {
          setLastUsed(map);
        }
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [apiKeys.length]);

  async function copyApiKey(apiKey: ApiKeyListItem) {
    await copyTextToClipboard(apiKey.keyValue);
    notify(t("Copied routing key"));
  }

  return (
    <motion.div
      animate={{ opacity: 1 }}
      className="flex h-full min-h-0 min-w-0 flex-col gap-3"
      initial={{ opacity: 0 }}
      transition={{ duration: 0.15 }}
    >
      <GatewayEndpointCard config={config} endpoint={endpoint} gatewayStatus={gatewayStatus} />
      {/* 工具行裸排在星空背景上，卡片只保留密钥列表数据区 */}
      <div className="flex flex-wrap items-center gap-2">
        <div className="relative min-w-0 flex-1">
          <Search className="pointer-events-none absolute left-2.5 top-1/2 z-[1] h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
          <Input
            aria-label={t("Search routing keys")}
            className="pl-8"
            onChange={(event) => setQuery(event.target.value)}
            placeholder={t("Search routing keys")}
            value={query}
          />
        </div>
        <div aria-label={t("Status")} className="overview-segmented flex shrink-0" role="group">
          {apiKeyStatusFilterOptions.map((option) => (
            <Button
              aria-pressed={statusFilter === option.value}
              className={cn(
                "overview-segmented-item h-7 gap-1.5 px-2.5 text-caption font-medium text-muted-foreground hover:text-foreground",
                statusFilter === option.value && "text-foreground"
              )}
              data-active={statusFilter === option.value}
              key={option.value}
              onClick={() => setStatusFilter(option.value)}
              type="button"
              unstyled
            >
              {t(option.label)}
              <span className="network-count rounded-full px-1.5 py-px text-micro font-bold">
                {statusCounts[option.value]}
              </span>
            </Button>
          ))}
        </div>
        <Button aria-label={t("Add routing key")} onClick={addApiKey} title={t("Add routing key")} type="button">
          <Plus className="h-4 w-4" />
          {t("Add")}
        </Button>
      </div>
      {/* 20261104_01:恢复整版卡壳 —— 此前为「透出背景」把密钥列表摊在页面上
          (无边框、无卡片底色),现在重新落回有边框的卡片容器;表头是滚动时的
          浮层,行由发丝线分隔。 */}
      <Card className="flex min-h-0 min-w-0 flex-1 flex-col" data-module="api-keys-card">
        <CardContent className="flex min-h-0 flex-1 flex-col overflow-auto p-0">
          {error ? <div className="m-4 rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-body text-destructive flex items-start gap-2"><CircleAlert className="h-3.5 w-3.5 shrink-0 mt-0.5" /><span>{error}</span></div> : null}
          {apiKeys.length === 0 ? (
            <div className="flex min-h-0 flex-1 flex-col items-center justify-center gap-2 text-center">
              <KeyRound className="h-8 w-8 text-muted-foreground/40" />
              <div className="text-body text-muted-foreground">{t("No routing keys configured")}</div>
              <div className="text-caption text-muted-foreground/60">{t("Click Add to create one")}</div>
            </div>
          ) : null}
          {apiKeys.length > 0 && visibleApiKeys.length === 0 ? (
            <div className="flex min-h-0 flex-1 flex-col items-center justify-center gap-2 text-center text-body text-muted-foreground">
              {t("No matching routing keys")}
            </div>
          ) : null}
          {visibleApiKeys.length > 0 ? (
            <div className="min-h-0 flex-1 overflow-auto">
              <div className="min-w-[1120px] pb-2">
                <div className={cn("sticky top-0 z-10 grid h-9 items-center gap-3 border-b border-border/60 bg-card/95 px-4 text-body-lg font-semibold uppercase tracking-wide text-muted-foreground backdrop-blur-md", KEY_LIST_GRID)}>
                  <div className="truncate text-center">{t("Name")}</div>
                  <div className="truncate text-center">{t("Status")}</div>
                  <div className="truncate text-center">{t("Secret")}</div>
                  <div className="truncate text-center">{t("Quota")}</div>
                  <div className="truncate text-center">{t("Models")}</div>
                  <div className="truncate text-center">{t("Created")}</div>
                  <div className="truncate text-center">{t("Last used")}</div>
                  <div className="truncate text-center">{t("Expires")}</div>
                  <div aria-hidden="true" />
                </div>
                <div className="divide-y divide-border/60">
                  <AnimatePresence initial={false}>
                  {visibleApiKeys.map((apiKey) => (
                    <AnimatedListItem
                      className={cn("grid h-[46px] items-center gap-3 px-4 transition-colors hover:bg-muted/35", KEY_LIST_GRID)}
                      key={`${apiKey.keyValue}-${apiKey.index}`}
                    >
                      <div className="flex min-w-0 justify-center">
                        <div className="min-w-0 truncate text-data font-semibold" title={apiKey.name}>{apiKey.name}</div>
                      </div>
                      <div className="flex min-w-0 justify-center">
                        <span
                          className={cn(
                            "rounded-full px-2 py-0.5 text-data font-semibold",
                            apiKey.enabled ? "network-state-pill-completed-soft" : "network-service-muted"
                          )}
                        >
                          {apiKey.enabled ? t("Enabled") : t("Disabled")}
                        </span>
                      </div>
                      <div className="min-w-0">
                        <div className="flex min-w-0 items-center justify-center gap-1.5 text-data font-semibold" title={apiKey.masked}>
                          <span className="min-w-0 truncate font-mono">{apiKey.masked}</span>
                          <Button
                            className="shrink-0"
                            aria-label={t("Copy routing key")}
                            onClick={() => void copyApiKey(apiKey)}
                            size="iconSm"
                            title={t("Copy routing key")}
                            type="button"
                            variant="ghost"
                          >
                            <Copy className="h-3.5 w-3.5" />
                          </Button>
                        </div>
                      </div>
                      <QuotaCell limits={apiKey.limits} />
                      <ModelsCell models={apiKey.models} />
                      <CreatedAtCell value={apiKey.createdAt} />
                      <LastUsedCell value={lastUsed[apiKey.key.id]} />
                      <ExpirationCell apiKey={apiKey} />
                      <div className="flex items-center justify-end gap-1">
                        <Button
                          aria-label={apiKey.enabled ? t("Disable routing key") : t("Enable routing key")}
                          className={cn(apiKey.enabled ? "text-muted-foreground" : "text-emerald-600 dark:text-emerald-400")}
                          onClick={() => toggleApiKey(apiKey.index)}
                          size="iconSm"
                          title={apiKey.enabled ? t("Disable routing key") : t("Enable routing key")}
                          type="button"
                          variant="ghost"
                        >
                          <Power className="h-3.5 w-3.5" />
                        </Button>
                        <Button aria-label={t("Edit routing key")} onClick={() => editApiKey(apiKey.index)} size="iconSm" title={t("Edit routing key")} type="button" variant="ghost">
                          <Pencil className="h-3.5 w-3.5" />
                        </Button>
                        <Button aria-label={t("Remove routing key")} onClick={() => setDeleteTarget(apiKey)} size="iconSm" title={t("Remove routing key")} type="button" variant="ghost">
                          <Trash2 className="h-3.5 w-3.5" />
                        </Button>
                      </div>
                    </AnimatedListItem>
                  ))}
                  </AnimatePresence>
                </div>
              </div>
            </div>
          ) : null}
        </CardContent>
      </Card>
      {deleteTarget ? (
        <DeleteRoutingKeyDialog
          item={deleteTarget}
          onClose={() => setDeleteTarget(undefined)}
          onConfirm={() => {
            removeApiKey(deleteTarget.index);
            setDeleteTarget(undefined);
          }}
        />
      ) : null}
    </motion.div>
  );
}

function quotaWindowLabelKey(limits: ApiKeyLimitConfig): string {
  const preset = quotaWindowPresetFromMs(limits.quotaWindowMs);
  if (preset) return `per ${preset}`;
  const custom = formatQuotaWindowDuration(limits.quotaWindowMs);
  return custom ? `per ${custom}` : "per day";
}

function QuotaCell({ limits }: { limits?: ApiKeyLimitConfig }) {
  const t = useAppText();
  if (!limits?.maxTokens) {
    return <div className="min-w-0 truncate text-center text-data text-muted-foreground">{t("Unlimited")}</div>;
  }
  const compact = formatCompactTokenCount(limits.maxTokens);
  const windowLabel = quotaWindowLabelKey(limits);
  return (
    <div className="flex min-w-0 justify-center">
      <Badge title={`${compact} ${t("tokens")} ${t(windowLabel)}`} variant="outline">
        <span className="font-semibold">{compact}</span>
        <span className="text-muted-foreground">{t("tokens")}</span>
        <span className="text-muted-foreground">{t(windowLabel)}</span>
      </Badge>
    </div>
  );
}

function CreatedAtCell({ value }: { value?: string }) {
  const t = useAppText();
  const time = value ? new Date(value).getTime() : NaN;
  if (!Number.isFinite(time) || time < MEANINGFUL_CREATED_AT_MS) {
    return <div className="truncate text-center text-data text-muted-foreground/50">-</div>;
  }
  const relative = relativeTimeValue(value);
  const label = relative === "now" ? t("just now") : `${relative.value} ${t(relativeTimeUnitLabel(relative.unit, relative.value))}`;
  return <div className="truncate text-center text-data text-muted-foreground" title={new Date(time).toLocaleString()}>{label}</div>;
}

function LastUsedCell({ value }: { value?: string }) {
  const t = useAppText();
  const time = value ? new Date(value).getTime() : NaN;
  if (!Number.isFinite(time) || time < MEANINGFUL_CREATED_AT_MS) {
    return <div className="truncate text-center text-data text-muted-foreground/50">-</div>;
  }
  const relative = relativeTimeValue(value);
  const label = relative === "now" ? t("just now") : `${relative.value} ${t(relativeTimeUnitLabel(relative.unit, relative.value))}`;
  return <div className="truncate text-center text-data text-muted-foreground" title={new Date(time).toLocaleString()}>{label}</div>;
}

function ModelsCell({ models }: { models: string[] }) {
  const t = useAppText();
  if (!models.length) {
    return <div className="min-w-0 truncate text-center text-data text-muted-foreground">{t("All models")}</div>;
  }
  return (
    <div className="flex min-w-0 justify-center">
      <Badge title={models.join("\n")} variant="outline">
        {models.length} {models.length === 1 ? t("model") : t("models")}
      </Badge>
    </div>
  );
}

function ExpirationCell({ apiKey }: { apiKey: ApiKeyListItem }) {
  const t = useAppText();
  const status = apiKeyExpirationStatus(apiKey.expiresAt);
  const expirationText = t(formatApiKeyExpiration(apiKey));
  if (status === "expired") {
    return (
      <div className="flex min-w-0 justify-center">
        <span className="network-state-pill-error-soft rounded-full px-2 py-0.5 text-data font-semibold" title={expirationText}>
          {t("Expired")}
        </span>
      </div>
    );
  }
  if (status === "expiring") {
    return (
      <div className="flex min-w-0 justify-center">
        <span className="network-state-pill-active-soft rounded-full px-2 py-0.5 text-data font-semibold" title={expirationText}>
          {expirationText}
        </span>
      </div>
    );
  }
  return (
    <div className="min-w-0 truncate text-center text-data text-muted-foreground" title={expirationText}>
      {expirationText}
    </div>
  );
}

function DeleteRoutingKeyDialog({
  item,
  onClose,
  onConfirm
}: {
  item: ApiKeyListItem;
  onClose: () => void;
  onConfirm: () => void;
}) {
  const t = useAppText();

  return (
    <Dialog onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-[520px]">
        <DialogHeader>
          <div className="min-w-0">
            <DialogTitle>{t("Delete Routing Key")}</DialogTitle>
          </div>
          <Button aria-label={t("Close dialog")} onClick={onClose} size="iconSm" title={t("Close")} type="button" variant="ghost">
            <X className="h-4 w-4" />
          </Button>
        </DialogHeader>

        <DialogBody>
          <div className="rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2.5">
            <div className="flex items-start gap-2 text-body font-medium text-destructive">
              <CircleAlert className="mt-0.5 h-3.5 w-3.5 shrink-0" />
              <span>{t("Delete this routing key? Clients using it will immediately lose access.")}</span>
            </div>
            <div className="mt-2 space-y-1 text-caption text-muted-foreground">
              <div className="truncate" title={item.name}>
                <span className="font-medium text-foreground">{t("Name")}:</span> {item.name}
              </div>
              <div className="truncate font-mono" title={item.masked}>
                <span className="font-medium text-foreground">{t("Secret")}:</span> {item.masked}
              </div>
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

function GatewayEndpointCard({
  config,
  endpoint,
  gatewayStatus
}: {
  config: AppConfig;
  endpoint: string;
  gatewayStatus: GatewayStatus;
}) {
  const t = useAppText();
  const [copiedKey, setCopiedKey] = useState("");
  const copyResetTimer = useRef<number>();
  const running = gatewayStatus.state === "running";
  const statusLabel = running ? t("running") : t("not running");
  const value = endpoint.trim() || t("Not configured");
  const loopbackEndpoint = loopbackEndpointFromStatus(value, config);
  const loopbackBaseUrl = gatewayBaseUrlFromEndpoint(loopbackEndpoint);
  const networkEndpoints = running ? gatewayStatus.networkEndpoints ?? [] : [];

  async function copyEndpoint(valueToCopy: string, key: string) {
    await copyTextToClipboard(valueToCopy);
    setCopiedKey(key);
    if (copyResetTimer.current) {
      window.clearTimeout(copyResetTimer.current);
    }
    copyResetTimer.current = window.setTimeout(() => setCopiedKey(""), 1300);
  }

  return (
    <Card className="shrink-0">
      <CardHeader className="flex-row items-center justify-between gap-2 px-4 pb-2.5 pt-3">
        <div className="flex min-w-0 items-center gap-2">
          <span
            aria-hidden="true"
            className={cn(
              "h-2.5 w-2.5 shrink-0 rounded-full",
              running ? "bg-emerald-500 shadow-[0_0_0_3px_rgba(16,185,129,0.16)]" : "bg-muted-foreground/45"
            )}
          />
          <span className="shrink-0 text-body font-semibold">{t("Gateway endpoint")}</span>
          <span className="min-w-0 truncate font-mono text-body text-muted-foreground" title={loopbackBaseUrl}>
            {loopbackBaseUrl}
          </span>
          <Button
            aria-label={t("Copy endpoint")}
            className="shrink-0"
            onClick={() => void copyEndpoint(loopbackBaseUrl, "endpoint")}
            size="iconSm"
            title={t("Copy endpoint")}
            type="button"
            variant="ghost"
          >
            <AnimatedIconSwap iconKey={copiedKey === "endpoint" ? "copied" : "copy"}>
              {copiedKey === "endpoint" ? <Check className="h-3.5 w-3.5 text-emerald-600" /> : <Copy className="h-3.5 w-3.5" />}
            </AnimatedIconSwap>
          </Button>
        </div>
        <span className={cn("shrink-0 text-caption", running ? "text-emerald-600 dark:text-emerald-400" : "text-muted-foreground")}>{statusLabel}</span>
      </CardHeader>
      {networkEndpoints.length ? (
        <CardContent className="space-y-1.5 px-4 pb-3 pt-0">
          {networkEndpoints.map((entry, index) => {
            const baseUrl = gatewayBaseUrlFromEndpoint(entry.endpoint);
            return (
              <EndpointInfoRow
                copied={copiedKey === `network:${entry.interfaceName}:${entry.address}`}
                key={`${entry.interfaceName}-${entry.address}`}
                label={index === 0 ? t("Network") : ""}
                meta={entry.interfaceName}
                value={baseUrl}
                onCopy={(valueToCopy) => void copyEndpoint(valueToCopy, `network:${entry.interfaceName}:${entry.address}`)}
              />
            );
          })}
        </CardContent>
      ) : null}
    </Card>
  );
}

export function AddApiKeyDialog({
  canSubmit,
  draft,
  error,
  modelOptions,
  onChange,
  onClose,
  onSubmit
}: {
  canSubmit: boolean;
  draft: AddApiKeyDraft;
  error: string;
  modelOptions: string[];
  onChange: (patch: Partial<AddApiKeyDraft>) => void;
  onClose: () => void;
  onSubmit: () => void;
}) {
  const t = useAppText();

  return (
    <Dialog onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-[600px]">
        <DialogHeader>
          <div className="min-w-0">
            <DialogTitle>{t("Add Routing Key")}</DialogTitle>
          </div>
          <Button aria-label={t("Close dialog")} onClick={onClose} size="iconSm" title={t("Close")} type="button" variant="ghost">
            <X className="h-4 w-4" />
          </Button>
        </DialogHeader>

        <DialogBody>
          <motion.div layout transition={disclosureSpringTransition}>
            <ApiKeyFormFields draft={draft} modelOptions={modelOptions} onChange={onChange} />
          </motion.div>

          {error ? <div className="mt-3 rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-body text-destructive flex items-start gap-2"><CircleAlert className="h-3.5 w-3.5 shrink-0 mt-0.5" /><span>{error}</span></div> : null}
        </DialogBody>

        <DialogFooter>
          <Button onClick={onClose} type="button" variant="outline">
            {t("Cancel")}
          </Button>
          <Button disabled={!canSubmit} onClick={onSubmit} type="button">
            <Plus className="h-4 w-4" />
            {t("Add")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

export function ApiKeyCreatedDialog({
  apiKeyName,
  apiKeyValue,
  onClose
}: {
  apiKeyName: string;
  apiKeyValue: string;
  onClose: () => void;
}) {
  const t = useAppText();
  const [copied, setCopied] = useState(false);

  async function copyApiKey() {
    await copyTextToClipboard(apiKeyValue);
    setCopied(true);
  }

  return (
    <Dialog onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-[560px]">
        <DialogHeader>
          <div className="min-w-0">
            <DialogTitle>{t("Routing key created")}</DialogTitle>
          </div>
          <Button aria-label={t("Close dialog")} onClick={onClose} size="iconSm" title={t("Close")} type="button" variant="ghost">
            <X className="h-4 w-4" />
          </Button>
        </DialogHeader>

        <DialogBody>
          <div className="flex items-start gap-3 rounded-md border border-emerald-500/25 bg-emerald-500/10 px-3 py-3 text-body text-emerald-700">
            <span className="mt-0.5 flex h-5 w-5 shrink-0 items-center justify-center rounded-full bg-emerald-500 text-white">
              <Check className="h-3.5 w-3.5" />
            </span>
            <div className="min-w-0">
              <div className="font-semibold">{t("Routing key created")}</div>
              <div className="mt-0.5 text-emerald-700/80">{t("Copy this key now. It may not be shown again.")}</div>
            </div>
          </div>

          <div className="mt-4 space-y-2">
            <div className="text-caption font-medium uppercase tracking-wide text-muted-foreground">{apiKeyName || t("Routing key")}</div>
            <div className="flex min-w-0 items-center gap-2 rounded-md border border-border bg-muted/30 p-2">
              <code className="min-w-0 flex-1 overflow-x-auto whitespace-nowrap rounded bg-background px-2 py-1.5 font-mono text-body text-foreground">
                {apiKeyValue}
              </code>
              <Button
                aria-label={copied ? t("Copied") : t("Copy routing key")}
                className="shrink-0"
                onClick={() => void copyApiKey()}
                title={copied ? t("Copied") : t("Copy routing key")}
                type="button"
                variant={copied ? "secondary" : "default"}
              >
                <AnimatedIconSwap iconKey={copied ? "copied" : "copy"}>
                  {copied ? <Check className="h-4 w-4" /> : <Copy className="h-4 w-4" />}
                </AnimatedIconSwap>
                {copied ? t("Copied") : t("Copy")}
              </Button>
            </div>
          </div>
        </DialogBody>

        <DialogFooter>
          <Button onClick={onClose} type="button">
            {t("Done")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

export function EditApiKeyDialog({
  canSubmit,
  draft,
  error,
  keyId,
  modelOptions,
  onChange,
  onClose,
  onSubmit
}: {
  canSubmit: boolean;
  draft: AddApiKeyDraft;
  error: string;
  keyId: string;
  modelOptions: string[];
  onChange: (patch: Partial<AddApiKeyDraft>) => void;
  onClose: () => void;
  onSubmit: () => void;
}) {
  const t = useAppText();

  return (
    <Dialog onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-[600px]">
        <DialogHeader>
          <div className="min-w-0">
            <DialogTitle>{t("Edit Routing Key")}</DialogTitle>
          </div>
          <Button aria-label={t("Close dialog")} onClick={onClose} size="iconSm" title={t("Close")} type="button" variant="ghost">
            <X className="h-4 w-4" />
          </Button>
        </DialogHeader>

        <DialogBody>
          <ApiKeyFormFields defaultAdvancedOpen draft={draft} modelOptions={modelOptions} onChange={onChange} />

          {error ? <div className="mt-3 rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-body text-destructive flex items-start gap-2"><CircleAlert className="h-3.5 w-3.5 shrink-0 mt-0.5" /><span>{error}</span></div> : null}
        </DialogBody>

        <DialogFooter>
          <Button onClick={onClose} type="button" variant="outline">
            {t("Cancel")}
          </Button>
          <Button disabled={!canSubmit} onClick={onSubmit} type="button">
            <Check className="h-4 w-4" />
            {t("Save")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function DialogSection({ children, description, title }: { children: ReactNode; description: string; title: string }) {
  return (
    <section className="space-y-3">
      <div>
        <div className="text-title font-semibold">{title}</div>
        <div className="mt-0.5 text-caption text-muted-foreground">{description}</div>
      </div>
      {children}
    </section>
  );
}

function ApiKeyFormFields({
  defaultAdvancedOpen = false,
  draft,
  modelOptions,
  onChange
}: {
  defaultAdvancedOpen?: boolean;
  draft: AddApiKeyDraft;
  modelOptions: string[];
  onChange: (patch: Partial<AddApiKeyDraft>) => void;
}) {
  const t = useAppText();

  return (
    <div className="space-y-5">
      <DialogSection description={t("Set the basic key information")} title={t("Basic info")}>
        <Field label={t("Name")}>
          <Input
            autoFocus={!defaultAdvancedOpen}
            placeholder={t("Enter a name for this routing key")}
            value={draft.name}
            onChange={(event) => onChange({ name: event.target.value })}
          />
        </Field>
        <label className="flex items-center justify-between gap-3 rounded-md border border-border bg-muted/20 px-3 py-3">
          <span className="min-w-0">
            <span className="block text-data font-medium">{t("Enabled")}</span>
            <span className="mt-0.5 block text-caption text-muted-foreground">{t("Disabled keys are rejected with 403 by the gateway.")}</span>
          </span>
          <Switch checked={draft.enabled} onCheckedChange={(enabled) => onChange({ enabled })} />
        </label>
      </DialogSection>

      <DialogSection description={t("Set when this key expires")} title={t("Expiration")}>
        <div aria-label={t("Expiration")} className="overview-segmented flex" role="group">
          {apiKeyExpirationOptions.map((option) => (
            <Button
              aria-pressed={draft.expirationPreset === option.value}
              className={cn(
                "overview-segmented-item h-7 px-2.5 text-caption font-medium text-muted-foreground hover:text-foreground",
                draft.expirationPreset === option.value && "text-foreground"
              )}
              data-active={draft.expirationPreset === option.value}
              key={option.value}
              onClick={() => onChange({ expirationPreset: option.value as ApiKeyExpirationPreset })}
              type="button"
              unstyled
            >
              {t(option.label)}
            </Button>
          ))}
        </div>
        <AnimatePresence initial={false}>
          {draft.expirationPreset === "custom" ? (
            <AnimatedDisclosure key="expiration-custom">
              <Field label={t("Expires at")}>
                <Input type="datetime-local" value={draft.expiresAt} onChange={(event) => onChange({ expiresAt: event.target.value })} />
              </Field>
            </AnimatedDisclosure>
          ) : null}
        </AnimatePresence>
      </DialogSection>

      <DialogSection description={t("Set the token quota for this key")} title={t("Quota settings")}>
        <label className="flex items-center justify-between gap-3 rounded-md border border-border bg-muted/20 px-3 py-3">
          <span className="min-w-0">
            <span className="block text-data font-medium">{t("Unlimited quota")}</span>
            <span className="mt-0.5 block text-caption text-muted-foreground">{t("Enable unlimited token quota for this routing key")}</span>
          </span>
          <Switch checked={draft.quotaUnlimited} onCheckedChange={(quotaUnlimited) => onChange({ quotaUnlimited })} />
        </label>
        <AnimatePresence initial={false}>
          {draft.quotaUnlimited ? null : (
            <AnimatedDisclosure key="quota-fields">
              <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
                <Field label={t("Token quota")}>
                  <Input
                    min={1}
                    placeholder={t("Enter the token quota amount")}
                    type="number"
                    value={draft.quotaTokens}
                    onChange={(event) => onChange({ quotaTokens: event.target.value })}
                  />
                </Field>
                <Field label={t("Statistics window")}>
                  <SelectControl
                    value={draft.quotaWindow}
                    onChange={(value) => onChange({
                      quotaWindow: value as QuotaWindowChoice,
                      quotaWindowCustomMs: value === "custom" ? draft.quotaWindowCustomMs : undefined
                    })}
                    options={[
                      ...translateOptions(quotaWindowOptions, t),
                      // 原窗口值不在已知档位内 → 显示为可回选的 custom 项,保存时原样写回
                      ...(draft.quotaWindow === "custom"
                        ? [{ label: `${t("Custom")} (${formatQuotaWindowDuration(draft.quotaWindowCustomMs)})`, value: "custom" }]
                        : [])
                    ]}
                  />
                </Field>
              </div>
            </AnimatedDisclosure>
          )}
        </AnimatePresence>
      </DialogSection>

      <ApiKeyAdvancedSettings defaultOpen={defaultAdvancedOpen} draft={draft} modelOptions={modelOptions} onChange={onChange} />
    </div>
  );
}

function ApiKeyModelRestrictions({
  draft,
  modelOptions,
  onChange
}: {
  draft: AddApiKeyDraft;
  modelOptions: string[];
  onChange: (patch: Partial<AddApiKeyDraft>) => void;
}) {
  const t = useAppText();
  const toggleModel = (id: string, checked: boolean) => {
    onChange({
      models: checked ? [...draft.models, id] : draft.models.filter((model) => model !== id)
    });
  };
  // Keep selections that no longer exist in the configured model list visible
  // (e.g. a provider model was renamed) so saving never silently drops them.
  const orphaned = draft.models.filter((model) => !modelOptions.includes(model));

  return (
    <div className="space-y-1.5">
      <div className="flex items-center justify-between gap-2">
        <div className="min-w-0 text-caption font-medium text-muted-foreground">{t("Model restrictions")}</div>
        {draft.models.length ? (
          <Button className="h-6 px-2 text-caption" onClick={() => onChange({ models: [] })} size="sm" type="button" variant="ghost">
            {t("Clear selection")}
          </Button>
        ) : null}
      </div>
      <div className="text-caption text-muted-foreground">{t("Leave empty to allow all models")}</div>
      <div className="max-h-40 space-y-1 overflow-auto rounded-md border border-border p-2">
        {modelOptions.map((id) => (
          <label className="flex cursor-pointer items-center gap-2 rounded px-1 py-0.5 hover:bg-muted/40" key={id}>
            <Checkbox checked={draft.models.includes(id)} onCheckedChange={(checked) => toggleModel(id, checked)} />
            <span className="min-w-0 truncate font-mono text-caption" title={id}>{id}</span>
          </label>
        ))}
        {orphaned.map((id) => (
          <label className="flex cursor-pointer items-center gap-2 rounded px-1 py-0.5 hover:bg-muted/40" key={id}>
            <Checkbox checked onCheckedChange={(checked) => toggleModel(id, checked)} />
            <span className="min-w-0 truncate font-mono text-caption text-amber-600" title={id}>{id}</span>
          </label>
        ))}
      </div>
    </div>
  );
}

function ApiKeyAdvancedSettings({
  defaultOpen = false,
  draft,
  modelOptions,
  onChange
}: {
  defaultOpen?: boolean;
  draft: AddApiKeyDraft;
  modelOptions: string[];
  onChange: (patch: Partial<AddApiKeyDraft>) => void;
}) {
  const t = useAppText();
  const limitMetricOptions = translateOptions(apiKeyLimitMetricOptions, t);
  const limitWindowSelectOptions = translateOptions(limitWindowOptions, t);
  const [advancedOpen, setAdvancedOpen] = useState(defaultOpen);
  const updateLimitRow = (id: string, patch: Partial<ApiKeyLimitDraftRow>) => {
    onChange({
      limitRows: draft.limitRows.map((row) => (row.id === id ? { ...row, ...patch } : row))
    });
  };
  const addLimitRow = () => {
    const row = createApiKeyLimitDraftRow();
    if (row) {
      onChange({ limitRows: [...draft.limitRows, row] });
    }
  };
  const removeLimitRow = (id: string) => {
    onChange({ limitRows: draft.limitRows.filter((row) => row.id !== id) });
  };

  return (
    <div className="overflow-hidden rounded-md border border-border bg-background">
      <Button
        aria-expanded={advancedOpen}
        className="flex h-11 w-full items-center justify-between gap-3 px-3 text-left text-data font-medium transition-colors hover:bg-muted/40"
        onClick={() => setAdvancedOpen((value) => !value)}
        type="button"
        unstyled
      >
        <span className="min-w-0 truncate">{t("Advanced settings")}</span>
        <ChevronDown className={cn("h-4 w-4 shrink-0 text-muted-foreground transition-transform", advancedOpen && "rotate-180")} />
      </Button>
      <AnimatePresence initial={false}>
        {advancedOpen ? (
          <AnimatedDisclosure key="api-key-advanced">
            <div className="space-y-3 border-t border-border p-3">
              <ApiKeyModelRestrictions draft={draft} modelOptions={modelOptions} onChange={onChange} />
              <div className="space-y-2">
              <div className="text-caption text-muted-foreground">{t("Rate limits are enforced per minute, hour, or day.")}</div>
              {draft.limitRows.length === 0 ? (
                <div className="rounded-md border border-dashed border-border bg-muted/20 px-3 py-5 text-center text-body text-muted-foreground">
                  {t("No limits configured")}
                </div>
              ) : null}
              <AnimatePresence initial={false}>
                {draft.limitRows.map((row) => (
                  <AnimatedListItem className="grid grid-cols-[minmax(110px,0.9fr)_126px_minmax(0,1fr)_28px] gap-2" key={row.id}>
                    <SelectControl
                      value={row.metric}
                      onChange={(metric) => updateLimitRow(row.id, { metric: metric as ApiKeyLimitMetric })}
                      options={limitMetricOptions}
                    />
                    <SelectControl
                      value={row.window}
                      onChange={(window) => updateLimitRow(row.id, { window: window as LimitWindowPreset })}
                      options={limitWindowSelectOptions}
                    />
                    <Input type="number" value={row.value} onChange={(event) => updateLimitRow(row.id, { value: event.target.value })} />
                    <Button aria-label={t("Remove limit")} onClick={() => removeLimitRow(row.id)} size="iconSm" title={t("Remove limit")} type="button" variant="ghost">
                      <Trash2 className="h-3.5 w-3.5" />
                    </Button>
                  </AnimatedListItem>
                ))}
              </AnimatePresence>
              <Button onClick={addLimitRow} size="sm" type="button" variant="outline">
                <Plus className="h-3.5 w-3.5" />
                {t("Add limit")}
              </Button>
              </div>
            </div>
          </AnimatedDisclosure>
        ) : null}
      </AnimatePresence>
    </div>
  );
}
