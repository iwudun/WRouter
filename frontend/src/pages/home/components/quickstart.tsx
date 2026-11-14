// 20261101_03:快速开始页 —— 应用默认第一页。三步接入指引:
// ①添加模型供应商 → ②创建路由密钥 → ③客户端指向网关。
// 范式与设置页分区一致:轻卡(rounded-md border bg-card)+ 裸排行,无玻璃。
import { type ReactNode, useRef, useState } from "react";
import { ArrowRight, Check, Copy, KeyRound, Layers3, ScrollText } from "lucide-react";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import { copyTextToClipboard, useAppText } from "../shared/index";

const quickstartContentWidthClassName = "mx-auto grid w-full max-w-[900px] grid-cols-1 gap-4";

export function QuickstartView({
  endpoint,
  onOpenApiKeys,
  onOpenCallLogs,
  onOpenProviders,
  running
}: {
  /** 网关回环地址(含协议,如 http://127.0.0.1:3460);未配置时为空串。 */
  endpoint: string;
  onOpenApiKeys: () => void;
  onOpenCallLogs: () => void;
  onOpenProviders: () => void;
  running: boolean;
}) {
  const t = useAppText();
  const [copied, setCopied] = useState(false);
  const copyResetTimer = useRef<number>();
  const baseUrl = endpoint.trim().replace(/\/+$/, "");

  async function copyEndpoint() {
    if (!baseUrl) {
      return;
    }
    await copyTextToClipboard(baseUrl);
    setCopied(true);
    if (copyResetTimer.current !== undefined) {
      window.clearTimeout(copyResetTimer.current);
    }
    copyResetTimer.current = window.setTimeout(() => setCopied(false), 1300);
  }

  return (
    <section className="h-full min-h-0 overflow-y-auto p-5 max-[720px]:p-3">
      <div className={quickstartContentWidthClassName}>
        <div className="grid gap-1">
          <h3 className="text-body-lg font-semibold text-foreground">{t("Quickstart")}</h3>
          <p className="text-caption leading-4 text-muted-foreground">
            {t("Connect your client in three steps: add a provider, create a routing key, then point your app at the gateway.")}
          </p>
        </div>

        <StepCard
          actionLabel={t("Open model management")}
          icon={Layers3}
          onAction={onOpenProviders}
          step={1}
          title={t("Add model providers")}
        >
          <GuideRow text={t("Go to Model management, click Add provider, and fill in the name, Base URL and API key of your upstream service.")} />
          <GuideRow text={t("Use Fetch models to pull the model list from the upstream in one click.")} />
          <GuideRow text={t("Saved providers are exposed by the gateway as provider/model pairs.")} />
        </StepCard>

        <StepCard
          actionLabel={t("Open routing keys")}
          icon={KeyRound}
          onAction={onOpenApiKeys}
          step={2}
          title={t("Create a routing key")}
        >
          <GuideRow text={t("In Routing keys, add a key; clients use it as their API key when calling WRouter.")} />
          <GuideRow text={t("Each key can restrict models, set token quotas, rate limits and expiry.")} />
        </StepCard>

        <StepCard
          step={3}
          title={t("Connect your client")}
        >
          <GuideRow text={t("Point your app at the gateway below and use the routing key as the API key (Authorization: Bearer).")} />
          <div className="mt-1 grid gap-1.5">
            <div className="flex min-w-0 items-center gap-2 rounded-md border border-border bg-muted/20 px-2.5 py-2">
              <span
                aria-hidden="true"
                className={cn("h-2 w-2 shrink-0 rounded-full", running ? "bg-emerald-500" : "bg-muted-foreground/45")}
              />
              <span className="shrink-0 text-body font-medium text-foreground">{t("Gateway endpoint")}</span>
              <span className="min-w-0 flex-1 truncate font-mono text-caption text-foreground" title={baseUrl}>
                {baseUrl || t("Not configured")}
              </span>
              <Button
                aria-label={t("Copy endpoint")}
                className="shrink-0"
                disabled={!baseUrl}
                onClick={() => void copyEndpoint()}
                size="iconSm"
                title={t("Copy endpoint")}
                type="button"
                variant="ghost"
              >
                {copied ? <Check className="h-3.5 w-3.5 text-emerald-600" /> : <Copy className="h-3.5 w-3.5" />}
              </Button>
            </div>
            <ProtocolRow label={t("OpenAI compatible")} path={`${baseUrl}/v1/chat/completions`} />
            <ProtocolRow label={t("Anthropic compatible")} path={`${baseUrl}/v1/messages`} />
            <ProtocolRow label="Responses" path={`${baseUrl}/v1/responses`} />
            <ProtocolRow label="Gemini" path={`${baseUrl}/v1beta/models/...:generateContent`} />
          </div>
        </StepCard>

        <div className="flex min-w-0 flex-wrap items-center gap-2 rounded-md border border-border bg-muted/20 px-3 py-2.5">
          <ScrollText aria-hidden="true" className="h-4 w-4 shrink-0 text-muted-foreground" />
          <span className="min-w-0 flex-1 text-caption leading-4 text-muted-foreground">
            {t("Once requests are flowing, watch them land in Call Logs and Usage stats.")}
          </span>
          <Button onClick={onOpenCallLogs} size="sm" type="button" variant="outline">
            {t("Open call logs")}
            <ArrowRight className="h-3.5 w-3.5" />
          </Button>
        </div>
      </div>
    </section>
  );
}

function StepCard({
  actionLabel,
  children,
  icon: Icon,
  onAction,
  step,
  title
}: {
  actionLabel?: string;
  children: ReactNode;
  icon?: typeof Layers3;
  onAction?: () => void;
  step: number;
  title: string;
}) {
  return (
    <div className="grid grid-cols-1 gap-2.5 rounded-md border border-border bg-card p-4">
      <div className="flex min-w-0 items-center gap-2">
        <span
          aria-hidden="true"
          className="flex h-6 w-6 shrink-0 items-center justify-center rounded-md bg-primary/10 text-caption font-semibold text-primary"
        >
          {step}
        </span>
        {Icon ? (
          <span aria-hidden="true" className="flex h-6 w-6 shrink-0 items-center justify-center rounded-md text-muted-foreground">
            <Icon className="h-3.5 w-3.5" />
          </span>
        ) : null}
        <h4 className="min-w-0 flex-1 text-body-lg font-semibold text-foreground">{title}</h4>
        {onAction && actionLabel ? (
          <Button className="shrink-0" onClick={onAction} size="sm" type="button" variant="outline">
            {actionLabel}
            <ArrowRight className="h-3.5 w-3.5" />
          </Button>
        ) : null}
      </div>
      <div className="grid gap-1.5 pl-8">{children}</div>
    </div>
  );
}

function GuideRow({ text }: { text: string }) {
  return (
    <div className="flex min-w-0 gap-2 text-body leading-5 text-muted-foreground">
      <span aria-hidden="true" className="mt-2 h-1 w-1 shrink-0 rounded-full bg-muted-foreground/50" />
      <span className="min-w-0">{text}</span>
    </div>
  );
}

function ProtocolRow({ label, path }: { label: string; path: string }) {
  return (
    <div className="flex min-w-0 items-center gap-2 rounded-md bg-muted/20 px-2.5 py-1.5">
      <span className="shrink-0 text-caption font-medium text-foreground">{label}</span>
      <span className="min-w-0 flex-1 truncate font-mono text-caption text-muted-foreground" title={path}>
        POST {path}
      </span>
    </div>
  );
}
