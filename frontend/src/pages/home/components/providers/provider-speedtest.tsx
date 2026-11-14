import { useEffect, useMemo, useRef, useState } from "react";
import { CircleAlert, LoaderCircle, Plus, X, Zap } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Dialog, DialogBody, DialogContent, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";
import { api } from "@/lib/api";
import type { GatewayEndpointSpeedTestResult, ProviderProxyOverride } from "@/contracts/app";
import { useAppText } from "../../shared/index";

// 「管理与测速」弹窗：对当前基础 URL（及用户临时添加的对比地址）测网络往返
// 延迟。轻量匿名 GET、不带 Key、不计费（区别于「检测可用性」）。候选列表为
// 会话级，关闭后不持久化。交互与色阶如下。

/** 测速总超时（秒）；与后端 speedtest.rs 的默认值一致。 */
const SPEED_TEST_TIMEOUT_SECS = 10;

type Entry = { id: string; url: string };

const randomId = () => `ep_${Math.random().toString(36).slice(2, 9)}`;

function normalizeUrl(url: string): string {
  return url.trim().replace(/\/+$/, "");
}

/** 延迟色阶：<300 绿 / <500 黄 / <800 橙 / 其余红。 */
function latencyTone(latency: number): string {
  if (latency < 300) return "text-emerald-600 dark:text-emerald-400";
  if (latency < 500) return "text-yellow-600 dark:text-yellow-400";
  if (latency < 800) return "text-orange-600 dark:text-orange-400";
  return "text-red-600 dark:text-red-400";
}

export function ProviderSpeedTestDialog({
  baseUrl,
  onApply,
  onClose,
  open,
  proxy
}: {
  baseUrl: string;
  onApply: (url: string) => void;
  onClose: () => void;
  open: boolean;
  /** 20260906_01 §4.6:按该供应商的代理测速(缺省 = 回落全局默认),
   *  否则测出的延迟与真实转发出口不一致。 */
  proxy?: ProviderProxyOverride;
}) {
  const t = useAppText();
  const [entries, setEntries] = useState<Entry[]>([]);
  const [customUrl, setCustomUrl] = useState("");
  const [addError, setAddError] = useState<string | null>(null);
  const [results, setResults] = useState<Map<string, GatewayEndpointSpeedTestResult>>(() => new Map());
  const [selected, setSelected] = useState("");
  const [testing, setTesting] = useState(false);
  const [lastError, setLastError] = useState<string | null>(null);
  const [autoSelect, setAutoSelect] = useState(false);

  // 仅在 open 上升沿种子化（镜像 provider-connectivity 的 prevOpenRef 范式），
  // 避免父组件重渲染时用新 baseUrl 冲掉测速结果。
  const prevOpenRef = useRef(false);
  useEffect(() => {
    if (open && !prevOpenRef.current) {
      const seed = normalizeUrl(baseUrl);
      setEntries(seed ? [{ id: randomId(), url: seed }] : []);
      setSelected(seed);
      setResults(new Map());
      setCustomUrl("");
      setAddError(null);
      setLastError(null);
      setTesting(false);
    }
    prevOpenRef.current = open;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  function handleAdd() {
    const candidate = customUrl.trim();
    if (!candidate) {
      setAddError(t("Enter a valid URL"));
      return;
    }
    let parsed: URL;
    try {
      parsed = new URL(candidate);
    } catch {
      setAddError(t("Enter a valid URL"));
      return;
    }
    if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
      setAddError(t("Only http and https URLs are supported."));
      return;
    }
    const sanitized = normalizeUrl(parsed.toString());
    if (entries.some((entry) => entry.url === sanitized)) {
      setAddError(t("URL already added"));
      return;
    }
    setAddError(null);
    setLastError(null);
    setEntries((prev) => [...prev, { id: randomId(), url: sanitized }]);
    setCustomUrl("");
  }

  function handleRemove(entry: Entry) {
    setLastError(null);
    setEntries((prev) => prev.filter((item) => item.id !== entry.id));
    setResults((prev) => {
      const next = new Map(prev);
      next.delete(entry.url);
      return next;
    });
    setSelected((current) => (current === entry.url ? "" : current));
  }

  async function runSpeedTest() {
    const urls = entries.map((entry) => entry.url);
    if (urls.length === 0) {
      setLastError(t("No endpoints yet"));
      return;
    }
    setTesting(true);
    setLastError(null);
    setResults(new Map());
    try {
      const list = await api.testApiEndpoints({ proxy, timeoutSecs: SPEED_TEST_TIMEOUT_SECS, urls });
      const map = new Map<string, GatewayEndpointSpeedTestResult>();
      for (const item of list) {
        map.set(normalizeUrl(item.url), item);
      }
      setResults(map);
      if (autoSelect) {
        const fastest = list
          .filter((item): item is GatewayEndpointSpeedTestResult & { latency: number } => typeof item.latency === "number")
          .sort((a, b) => a.latency - b.latency)[0];
        if (fastest) {
          setSelected(normalizeUrl(fastest.url));
        }
      }
    } catch (error) {
      setLastError(error instanceof Error ? error.message : String(error));
    } finally {
      setTesting(false);
    }
  }

  function handleApply() {
    if (!selected) return;
    onApply(selected);
    onClose();
  }

  // 按延迟升序（已测），未测/失败排最后。
  const sortedEntries = useMemo(() => {
    return entries.slice().sort((a, b) => {
      const la = results.get(a.url)?.latency ?? Number.POSITIVE_INFINITY;
      const lb = results.get(b.url)?.latency ?? Number.POSITIVE_INFINITY;
      if (la === lb) return a.url.localeCompare(b.url);
      return la - lb;
    });
  }, [entries, results]);

  const fastestUrl = useMemo(() => {
    let best: string | null = null;
    let bestLatency = Number.POSITIVE_INFINITY;
    for (const entry of entries) {
      const latency = results.get(entry.url)?.latency;
      if (typeof latency === "number" && latency < bestLatency) {
        bestLatency = latency;
        best = entry.url;
      }
    }
    return best;
  }, [entries, results]);

  return (
    <Dialog onOpenChange={(nextOpen) => { if (!nextOpen) onClose(); }} open={open}>
      <DialogContent className="max-w-[560px]">
        <DialogHeader>
          <div className="min-w-0">
            <DialogTitle>{t("Manage & speed test")}</DialogTitle>
          </div>
          <Button
            aria-label={t("Close dialog")}
            disabled={testing}
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
            <div className="rounded-md border border-border bg-muted/30 px-3 py-2 text-caption text-muted-foreground">
              {t("Speed test sends a lightweight request and does not consume balance.")}
            </div>

            <div className="flex min-w-0 flex-wrap items-center justify-between gap-2">
              <div className="text-caption text-muted-foreground">
                {entries.length} {t("Endpoints")}
              </div>
              <div className="flex shrink-0 items-center gap-3">
                <label className="flex cursor-pointer items-center gap-1.5 text-caption text-muted-foreground">
                  <Checkbox checked={autoSelect} disabled={testing} onCheckedChange={(checked) => setAutoSelect(checked)} />
                  {t("Auto-select fastest")}
                </label>
                <Button
                  className="h-7 gap-1 px-2 text-caption"
                  disabled={testing || entries.length === 0}
                  onClick={() => void runSpeedTest()}
                  size="sm"
                  type="button"
                  variant="outline"
                >
                  {testing ? <LoaderCircle className="h-3.5 w-3.5 animate-spin" /> : <Zap className="h-3.5 w-3.5" />}
                  {testing ? t("Testing speed") : t("Speed test")}
                </Button>
              </div>
            </div>

            <div className="space-y-1.5">
              <div className="flex gap-2">
                <Input
                  onChange={(event) => setCustomUrl(event.target.value)}
                  onKeyDown={(event) => {
                    if (event.key === "Enter") {
                      event.preventDefault();
                      handleAdd();
                    }
                  }}
                  placeholder={t("Add endpoint to compare")}
                  spellCheck={false}
                  type="text"
                  value={customUrl}
                />
                <Button aria-label={t("Add")} onClick={handleAdd} size="iconSm" type="button" variant="outline">
                  <Plus className="h-4 w-4" />
                </Button>
              </div>
              {addError ? (
                <div className="flex items-center gap-1.5 text-caption text-destructive">
                  <CircleAlert className="h-3 w-3 shrink-0" />
                  {addError}
                </div>
              ) : null}
            </div>

            {entries.length === 0 ? (
              <div className="rounded-md border border-dashed border-border bg-muted/20 px-3 py-6 text-center text-body text-muted-foreground">
                {t("No endpoints yet")}
              </div>
            ) : (
              <div className="space-y-1.5">
                {sortedEntries.map((entry) => {
                  const result = results.get(entry.url);
                  const latency = result?.latency ?? null;
                  const isSelected = selected === entry.url;
                  const isFastest = fastestUrl != null && fastestUrl === entry.url;
                  return (
                    <div
                      className={cn(
                        "group flex cursor-pointer items-center justify-between gap-2 rounded-md border px-3 py-2 transition-colors",
                        isSelected ? "border-primary bg-accent" : "border-border bg-background hover:bg-muted"
                      )}
                      key={entry.id}
                      onClick={() => setSelected(entry.url)}
                    >
                      <div className="flex min-w-0 flex-1 items-center gap-2">
                        <span className={cn("h-1.5 w-1.5 shrink-0 rounded-full", isSelected ? "bg-primary" : "bg-muted-foreground/40")} />
                        <span className="min-w-0 flex-1 truncate text-body" title={entry.url}>{entry.url}</span>
                        {isFastest && latency != null ? <Badge variant="success">{t("Fastest")}</Badge> : null}
                      </div>
                      <div className="flex shrink-0 items-center gap-2">
                        {latency != null ? (
                          <span className={cn("font-mono text-body font-medium", latencyTone(latency))}>{latency}ms</span>
                        ) : testing ? (
                          <LoaderCircle className="h-4 w-4 animate-spin text-muted-foreground" />
                        ) : result?.error ? (
                          <span className="text-caption text-muted-foreground" title={result.error}>{t("Failed")}</span>
                        ) : (
                          <span className="text-caption text-muted-foreground">—</span>
                        )}
                        <button
                          aria-label={`${t("Remove")} ${entry.url}`}
                          className="text-muted-foreground opacity-0 transition-opacity hover:text-destructive focus-visible:opacity-100 group-hover:opacity-100"
                          onClick={(event) => {
                            event.stopPropagation();
                            handleRemove(entry);
                          }}
                          title={t("Remove")}
                          type="button"
                        >
                          <X className="h-4 w-4" />
                        </button>
                      </div>
                    </div>
                  );
                })}
              </div>
            )}

            {lastError ? (
              <div className="flex items-center gap-1.5 text-caption text-destructive">
                <CircleAlert className="h-3 w-3 shrink-0" />
                {lastError}
              </div>
            ) : null}
          </div>
        </DialogBody>
        <DialogFooter>
          <Button disabled={testing} onClick={onClose} type="button" variant="outline">
            {t("Close")}
          </Button>
          <Button disabled={!selected || testing} onClick={handleApply} type="button">
            {t("Use this URL")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
