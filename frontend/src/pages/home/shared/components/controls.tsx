import { useCallback, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import {
  Check,
  CircleHelp,
  Copy,
  RefreshCw,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { Card, CardContent, } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import { cn } from "@/lib/utils";
import type {
  AppConfig,
  GatewayStatus,
  UsageSeriesPoint,
  UsageStatsRange,
  UsageTotals,
} from "@/contracts/app";
import {
  translateOptions,
  useAppText,
} from "../i18n";
import { AnimatedIconSwap } from "./motion";
import {
  formatDuration,
} from "../network";
import {
  formatCompactNumber,
  formatPercent,
} from "../usage";


import { metricToneBar, normalizeProviderModelSelector } from "../common";
import { routeTargetOptions } from "../providers";

export function Field({
  action,
  children,
  className,
  label,
  labelClassName,
  labelHint
}: {
  action?: React.ReactNode;
  children: React.ReactNode;
  className?: string;
  label: string;
  /** 覆盖默认的表单小标签样式(缺省 = 大写 caption);供应商详情页的
   *  章节小标题用它升级为设置页「主题」同款 body-lg 加粗(2026-09-13)。 */
  labelClassName?: string;
  labelHint?: React.ReactNode;
}) {
  const labelRow = (
    <span
      className={cn(
        "flex min-w-0 items-center gap-1",
        labelClassName ?? "text-caption font-medium uppercase tracking-wide text-muted-foreground"
      )}
    >
      <span className="block truncate">{label}</span>
      {labelHint}
    </span>
  );
  // 带 action 时根节点改用 <div>：按钮不能嵌进 <label>（无效 HTML，且点击会
  // 穿透聚焦到被标注的输入框）。无 action 时保持原 <label> 结构与点击聚焦行为，
  // 现有调用零影响。header 行布局对齐详情面板「模型列表」那一行的范式。
  if (action) {
    return (
      <div className={cn("block min-w-0 space-y-1", className)}>
        <div className="flex min-w-0 items-center justify-between gap-2">
          {labelRow}
          <div className="shrink-0">{action}</div>
        </div>
        {children}
      </div>
    );
  }
  return (
    <Label className={cn("block min-w-0 space-y-1", className)}>
      {labelRow}
      {children}
    </Label>
  );
}

// Small question-mark affordance next to a field label. Shows ``children`` as a
// hover/focus bubble (same pattern as routing.tsx's built-in rule hint).
// ``children`` may contain "\n" line breaks — each line renders on its own row.
export function FieldLabelHint({ children, className }: { children: React.ReactNode; className?: string }) {
  const lines = typeof children === "string" ? children.split("\n") : undefined;
  const triggerRef = useRef<HTMLSpanElement>(null);
  const [position, setPosition] = useState<{ left: number; top: number }>();
  const panelWidth = 300;
  const viewportMargin = 8;

  // 浮层走 fixed + portal 逃出表单/卡片的 overflow 容器;位置按触发图标
  // 实测算,并夹在视口内(AGENTS:绝对定位会被祖先裁掉)。
  const show = () => {
    const rect = triggerRef.current?.getBoundingClientRect();
    if (!rect) {
      return;
    }
    setPosition({
      left: Math.max(viewportMargin, Math.min(rect.left, window.innerWidth - panelWidth - viewportMargin)),
      top: rect.bottom + 6
    });
  };
  const hide = () => setPosition(undefined);

  useEffect(() => {
    if (!position) {
      return;
    }
    const close = () => setPosition(undefined);
    window.addEventListener("scroll", close, true);
    window.addEventListener("resize", close);
    return () => {
      window.removeEventListener("scroll", close, true);
      window.removeEventListener("resize", close);
    };
  }, [position]);

  return (
    <span
      aria-label={typeof children === "string" ? children : undefined}
      className={cn(
        "relative inline-flex h-4 w-4 shrink-0 items-center justify-center rounded-full text-muted-foreground outline-none focus-visible:ring-2 focus-visible:ring-ring/30",
        className
      )}
      onBlur={hide}
      onFocus={show}
      onMouseEnter={show}
      onMouseLeave={hide}
      ref={triggerRef}
      tabIndex={0}
    >
      <CircleHelp className="h-3.5 w-3.5" aria-hidden="true" />
      {position
        ? createPortal(
            <span
              className="pointer-events-none fixed z-[80] w-[300px] pl-2"
              style={{
                left: position.left,
                maxHeight: Math.max(80, window.innerHeight - position.top - viewportMargin),
                overflowY: "auto",
                top: position.top
              }}
            >
              <span className="block rounded-md border border-border bg-popover px-2.5 py-2 text-left text-caption font-medium normal-case leading-4 text-popover-foreground shadow-card">
                {lines ? lines.map((line, index) => <span className="block" key={index}>{line || "\u00A0"}</span>) : children}
              </span>
            </span>,
            document.body
          )
        : null}
    </span>
  );
}

export function SelectControl({
  className,
  onChange,
  options,
  value,
  ...rest
}: Omit<React.ComponentProps<"select">, "onChange" | "value"> & {
  onChange: (value: string) => void;
  options?: Array<{ disabled?: boolean; label: string; value: string }>;
  value: string;
}) {
  return <Select {...rest} className={className} onValueChange={onChange} options={options} value={value} />;
}

export function RouteTargetControl({
  className,
  modelOptions,
  onChange,
  value
}: {
  className?: string;
  modelOptions: Array<{ label: string; value: string }>;
  onChange: (value: string) => void;
  value: string;
}) {
  const t = useAppText();
  const normalizedValue = normalizeProviderModelSelector(value);

  if (modelOptions.length === 0) {
    return <Input className={className} onChange={(event) => onChange(event.target.value)} value={normalizedValue} />;
  }

  const options = routeTargetOptions(modelOptions, normalizedValue);
  return <SelectControl className={className} onChange={onChange} options={translateOptions(options, t)} value={normalizedValue} />;
}

export function TextAreaControl({
  className,
  minHeight,
  onChange,
  value
}: {
  className?: string;
  minHeight: number;
  onChange: (value: string) => void;
  value: string;
}) {
  const responsiveMinHeight = `min(${minHeight}px, max(132px, calc(100dvh - 220px)))`;

  return (
    <Textarea
      className={cn(
        "min-h-0",
        className
      )}
      onChange={(event) => onChange(event.target.value)}
      spellCheck={false}
      style={{ minHeight: responsiveMinHeight }}
      value={value}
    />
  );
}

export function Toggle({ checked, disabled = false, onChange, size, title }: { checked: boolean; disabled?: boolean; onChange: (checked: boolean) => void; size?: "default" | "sm"; title?: string }) {
  return <Switch checked={checked} disabled={disabled} onCheckedChange={onChange} size={size} title={title} />;
}

export type MetricTone = "amber" | "blue" | "indigo" | "rose" | "slate" | "teal";

export function MetricCard({ className, label, title, tone, value }: { className?: string; label: string; title?: string; tone: MetricTone; value: string }) {
  return (
    <Card className={cn("flex h-full min-h-0 min-w-0 flex-col overflow-hidden", className)}>
      <div className={cn("h-1", metricToneBar(tone))} />
      <CardContent className="flex min-h-[88px] flex-1 flex-col justify-center">
        <div className="min-w-0">
          <div className="truncate text-caption font-medium text-muted-foreground" title={title}>{label}</div>
          <div className="mt-1 truncate text-display font-semibold" title={title}>{value}</div>
        </div>
      </CardContent>
    </Card>
  );
}

export type SystemStatusTone = "error" | "idle" | "ok" | "warn";

export type SystemStatusPoint = {
  dateLabel: string;
  point: UsageSeriesPoint;
  tone: SystemStatusTone;
};

export function usageStatusTone(point: Pick<UsageTotals, "requestCount" | "successRate">): SystemStatusTone {
  if (point.requestCount <= 0) return "idle";
  if (point.successRate >= 0.995) return "ok";
  if (point.successRate >= 0.98) return "warn";
  return "error";
}

export function formatStatusBucketDate(bucket: string, range: UsageStatsRange): string {
  const parsed = parseStatusBucketDate(bucket);
  if (!parsed) {
    return bucket;
  }
  const dateOptions: Intl.DateTimeFormatOptions = range === "today" || range === "24h"
    ? { day: "2-digit", hour: "2-digit", hour12: false, month: "2-digit" }
    : { day: "2-digit", month: "2-digit" };
  return new Intl.DateTimeFormat(undefined, dateOptions).format(parsed);
}

export function parseStatusBucketDate(bucket: string): Date | undefined {
  if (/^\d{4}-\d{1,2}-\d{1,2}T\d{1,2}:\d{2}/.test(bucket)) {
    const isoDate = new Date(bucket);
    if (Number.isFinite(isoDate.getTime())) {
      return isoDate;
    }
  }
  const match = bucket.match(/^(\d{4})-(\d{1,2})-(\d{1,2})(?:\s+(\d{1,2})(?::00)?)?$/);
  if (!match) {
    return undefined;
  }
  const [, year, month, day, hour] = match;
  return new Date(Number(year), Number(month) - 1, Number(day), hour === undefined ? 0 : Number(hour), 0, 0, 0);
}

export function systemStatusPointTooltip(segment: SystemStatusPoint, t: (value: string) => string): string {
  return [
    segment.dateLabel,
    `${t("Requests")}: ${formatCompactNumber(segment.point.requestCount)}`,
    `${t("Success rate")}: ${formatPercent(segment.point.successRate)}`,
    `${t("Failed requests")}: ${formatCompactNumber(segment.point.errorCount)}`,
    `${t("Duration")}: ${formatDuration(segment.point.avgDurationMs)}`
  ].join("\n");
}

export function GatewayStatusPill({ className, state }: { className?: string; state: GatewayStatus["state"] }) {  const t = useAppText();
  const running = state === "running";
  const label = running ? t("running") : t("not running");
  return (
    <span
      aria-label={label}
      className={cn(
        "inline-flex h-5 shrink-0 items-center gap-1.5 rounded-full border px-2 text-caption font-medium",
        running
          ? "border-emerald-500/25 bg-emerald-500/10 text-emerald-600"
          : "border-border bg-muted/40 text-muted-foreground",
        className
      )}
      title={label}
    >
      <span
        aria-hidden="true"
        className={cn(
          "h-1.5 w-1.5 shrink-0 rounded-full",
          running ? "bg-emerald-500 shadow-[0_0_0_3px_rgba(16,185,129,0.16)]" : "bg-muted-foreground/45"
        )}
      />
      <span className="truncate">{label}</span>
    </span>
  );
}

// Refresh feedback for data views: the local backend answers in a few
// milliseconds, so a plain loading flag never renders a visible frame. The
// icon spins for a guaranteed minimum, then flashes a green check once the
// fetch settles.
const REFRESH_MIN_SPIN_MS = 450;
const REFRESH_CHECK_MS = 650;

export function RefreshButton({
  ariaLabel,
  loading = false,
  onRefresh,
  title
}: {
  ariaLabel: string;
  loading?: boolean;
  onRefresh: () => void | Promise<void>;
  title?: string;
}) {
  const [phase, setPhase] = useState<"idle" | "spinning" | "done">("idle");
  const timersRef = useRef<number[]>([]);

  useEffect(() => {
    const timers = timersRef.current;
    return () => {
      timers.forEach((timer) => window.clearTimeout(timer));
    };
  }, []);

  const handleClick = useCallback(() => {
    if (phase !== "idle") {
      return;
    }
    setPhase("spinning");
    const startedAt = performance.now();
    const settle = () => {
      const remaining = Math.max(0, REFRESH_MIN_SPIN_MS - (performance.now() - startedAt));
      timersRef.current.push(window.setTimeout(() => setPhase("done"), remaining));
      timersRef.current.push(window.setTimeout(() => setPhase("idle"), remaining + REFRESH_CHECK_MS));
    };
    const result = onRefresh();
    if (result && typeof result.then === "function") {
      void result.then(settle, settle);
    } else {
      settle();
    }
  }, [onRefresh, phase]);

  const spinning = phase === "spinning" || loading;

  return (
    <button
      aria-label={ariaLabel}
      className="network-control-button flex h-7 w-7 items-center justify-center rounded-md border outline-none transition-transform active:scale-90 focus-visible:ring-2 focus-visible:ring-ring/30"
      disabled={phase === "spinning"}
      onClick={handleClick}
      title={title}
      type="button"
    >
      <AnimatedIconSwap className="h-3.5 w-3.5" iconKey={phase === "done" ? "done" : spinning ? "spinning" : "idle"}>
        {phase === "done" ? (
          <Check className="h-3.5 w-3.5 text-emerald-500" />
        ) : (
          <RefreshCw className={cn("h-3.5 w-3.5", spinning && "animate-spin")} />
        )}
      </AnimatedIconSwap>
    </button>
  );
}

export function EndpointInfoRow({
  copied,
  label,
  meta,
  value,
  onCopy
}: {
  copied: boolean;
  label: string;
  meta?: string;
  value: string;
  onCopy: (value: string) => void;
}) {
  const t = useAppText();

  return (
    <div className="grid grid-cols-[76px_minmax(0,1fr)_24px] items-center gap-2 text-body">
      <span className="text-right text-muted-foreground">{label ? `${label}:` : null}</span>
      <span className="min-w-0 truncate font-medium text-foreground">
        {value}
        {meta ? <span className="ml-1 text-caption font-normal text-muted-foreground">({meta})</span> : null}
      </span>
      <Button
        aria-label={copied ? t("Copied") : t("Copy")}
        className={cn(
          "flex h-6 w-6 shrink-0 items-center justify-center rounded-md border outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring/25",
          copied
            ? "border-emerald-500/25 bg-emerald-500/10 text-emerald-600"
            : "border-transparent bg-transparent text-muted-foreground hover:bg-muted hover:text-foreground"
        )}
        title={copied ? t("Copied") : t("Copy")}
        type="button"
        unstyled
        onMouseDown={(event) => event.stopPropagation()}
        onClick={(event) => {
          event.stopPropagation();
          onCopy(value);
        }}
      >
        <AnimatedIconSwap iconKey={copied ? "copied" : "copy"}>
          {copied ? <Check className="h-3.5 w-3.5" /> : <Copy className="h-3.5 w-3.5" />}
        </AnimatedIconSwap>
      </Button>
    </div>
  );
}

export function loopbackEndpointFromStatus(endpoint: string, config: AppConfig): string {
  try {
    const parsed = new URL(endpoint);
    return `http://127.0.0.1:${parsed.port || config.gateway.port}`;
  } catch {
    return `http://127.0.0.1:${config.gateway.port}`;
  }
}

export function gatewayBaseUrlFromEndpoint(endpoint: string): string {
  try {
    const parsed = new URL(endpoint);
    parsed.pathname = appendGatewayBasePath(parsed.pathname);
    parsed.search = "";
    parsed.hash = "";
    return parsed.toString().replace(/\/$/, "");
  } catch {
    return `${endpoint.replace(/\/+$/, "")}/v1`;
  }
}

function appendGatewayBasePath(pathname: string): string {
  const normalized = pathname.replace(/\/+$/, "");
  if (!normalized || normalized === "/") {
    return "/v1";
  }
  if (normalized.endsWith("/v1")) {
    return normalized;
  }
  return `${normalized}/v1`;
}

