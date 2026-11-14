import { useEffect, useMemo, useRef, useState } from "react";
import { Check, CircleAlert, Copy, Layers3, LoaderCircle, Pencil, Plus, ShieldCheck, Trash2, Zap } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import type { ProxyPreset, ProviderModelInputType } from "@/contracts/app";
import {
  AddProviderDraft, Field, FieldLabelHint, ProviderKeyMode,
  copyTextToClipboard,
  createProviderAccount,
  enabledAccountIds,
  enforceSingleEnabledKey,
  formatContextWindow,
  GatewayProviderProbeResult,
  mergeProviderModelLists, nextProviderAccountLabel, providerBaseUrlHintKey, providerFullUrlHintKey, providerKeyMode, providerProtocolOptions,
  probeProxyPayload, proxyOverrideFromSelection,
  providerProbeHasSupportedProtocol, SelectControl, splitLines, translateOptions,
  useAppText,
} from "../../shared/index";
import { ProviderApiKeysEditor } from "./provider-api-keys";
import {
  ProviderAdvancedSettings,
  joinAdvancedSummary,
  providerFingerprintSummary,
  providerProxySummary
} from "./provider-advanced-settings";
import { ProviderClientFingerprintField } from "./provider-client-fingerprint-field";
import { ProviderProxyField } from "./provider-proxy-field";
import { ProviderSpeedTestDialog } from "./provider-speedtest";

export function AddProviderForm({
  connectivityLoading = false,
  connectivityProbe,
  contextWindows,
  displayNames,
  inputTypes,
  draft,
  error,
  notify,
  onAddModel,
  onCheck,
  onEditModel,
  onChange,
  onOpenProxySettings,
  onRemoveModel,
  proxyPresets
}: {
  connectivityLoading?: boolean;
  connectivityProbe?: GatewayProviderProbeResult;
  contextWindows?: Record<string, number>;
  displayNames?: Record<string, string>;
  /** 草稿期模型的输入类型记录(「视觉」徽标数据源);与落库 map 同构。 */
  inputTypes?: Record<string, ProviderModelInputType[]>;
  draft: AddProviderDraft;
  error: string;
  notify?: (message: string) => void;
  onAddModel: () => void;
  onCheck?: () => Promise<unknown>;
  onEditModel: (model: string) => void;
  onChange: (patch: Partial<AddProviderDraft>, resetProbe?: boolean) => void;
  /** 20260906_01 验收 #5:预设列表为空时跳转设置页代理分区。 */
  onOpenProxySettings: () => void;
  onRemoveModel: (model: string) => void;
  proxyPresets: ProxyPreset[];
}) {
  const t = useAppText();
  const [speedTestOpen, setSpeedTestOpen] = useState(false);
  const protocolOptions = useMemo(() => translateOptions(providerProtocolOptions, t), [t]);
  const configuredModels = mergeProviderModelLists(draft.selectedModels, splitLines(draft.modelsText));
  // 2026-10-11 统一多密钥形态:密钥判据与详情面板一致(任一启用且已设密钥)。
  const hasUsableKey = draft.accounts.some((account) => account.enabled !== false && Boolean(account.api_key));
  const hasConnectivityCheckInputs = Boolean(
    draft.baseUrl.trim() &&
    (hasUsableKey || draft.apiKey.trim()) &&
    configuredModels.length > 0
  );
  // 密钥两模式(与详情面板同一两态):固定密钥 = first(单启用);负载均衡 = balanced。
  const keyMode = providerKeyMode(draft.credentialStrategy);
  const keyModeOptions = useMemo(
    () => [
      { label: t("Fixed key"), value: "fixed" },
      { label: t("Load balancing"), value: "balanced" }
    ],
    [t]
  );
  // 20261017:高级设置折叠区(网络代理/客户端指纹)收起态摘要 —— 草稿
  // 默认值即全默认,通常为空串不显示;展开内容恒收起(新增场景用不到)。
  const advancedSummary = joinAdvancedSummary([
    providerProxySummary(t, draft.proxyMode, draft.proxyPresetId, proxyPresets),
    providerFingerprintSummary(t, draft.clientFingerprintMode, draft.clientFingerprintPresetId)
  ]);

  /** 「添加 API密钥」:与详情面板 addApiKey 同规则 —— 首把恒启用;固定
   *  密钥模式下已有密钥时新行默认停用(单启用约束,填完再开启)。 */
  function addDraftApiKey() {
    const accounts = draft.accounts;
    const defaultLabel = nextProviderAccountLabel(accounts, t("Key label"));
    const usedIds = accounts.map((account) => account.id ?? "").filter(Boolean);
    const account = { ...createProviderAccount(usedIds), label: defaultLabel };
    const isFirstKey = accounts.length === 0;
    onChange(
      { accounts: [...accounts, !isFirstKey && keyMode === "fixed" ? { ...account, enabled: false } : account] },
      true
    );
  }

  /** 策略切换(镜像 setProviderCredentialMode 的草稿版):切回固定密钥时
   *  恢复固定侧记忆(pinned 仍指向列表中的账号则用它,否则首个启用/首把)
   *  并收敛为单启用;切负载均衡只改策略,启用集合保持现状。 */
  function changeKeyMode(mode: ProviderKeyMode) {
    if (mode === keyMode) {
      return;
    }
    if (mode === "fixed") {
      const target = draft.pinnedAccountId && draft.accounts.some((account) => account.id === draft.pinnedAccountId)
        ? draft.pinnedAccountId
        : enabledAccountIds(draft.accounts)[0] ?? draft.accounts[0]?.id;
      onChange(
        {
          ...(target ? { accounts: enforceSingleEnabledKey(draft.accounts, target), pinnedAccountId: target } : {}),
          credentialStrategy: "first"
        },
        true
      );
      return;
    }
    onChange({ credentialStrategy: "balanced" }, true);
  }

  /** 固定密钥模式开启一把:单启用 + pinned 记忆同步(单次写入,镜像
   *  setProviderFixedKey)。 */
  function pickDraftFixedKey(accountId: string) {
    if (!draft.accounts.some((account) => account.id === accountId)) {
      return;
    }
    onChange(
      {
        accounts: enforceSingleEnabledKey(draft.accounts, accountId),
        pinnedAccountId: accountId
      },
      true
    );
  }

  return (
    <>
      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
        <Field className="sm:col-span-2" label={t("Provider name")}>
          <Input onChange={(event) => onChange({ name: event.target.value })} placeholder={t("Fill in provider name")} value={draft.name} />
        </Field>
        <Field
          action={
            <div className="flex shrink-0 items-center gap-2">
              {/* 20261014_01 §3.2:URL 模式 TAB(切换不清空文本,语义切换;
                  选中态即草稿 urlMode)。表单三种 API 格式均非 Gemini 家族,
                  后端 endpoint() 的 family 守卫兜底手改/导入配置。 */}
              <div
                aria-label={t("URL mode")}
                className="grid shrink-0 grid-cols-2 gap-0.5 rounded-lg border border-border/60 bg-muted p-0.5"
                role="tablist"
              >
                {([["base", "Base URL"], ["full", "Full URL"]] as const).map(([mode, label]) => (
                  <button
                    aria-selected={draft.urlMode === mode}
                    className={`rounded-md px-2 py-0.5 text-caption font-semibold transition-colors ${draft.urlMode === mode ? "bg-primary text-primary-foreground shadow-sm" : "text-muted-foreground hover:text-foreground"}`}
                    key={mode}
                    onClick={() => onChange({ urlMode: mode })}
                    role="tab"
                    type="button"
                  >
                    {t(label)}
                  </button>
                ))}
              </div>
              <Button
                className="h-7 gap-1 px-2 text-caption"
                onClick={() => setSpeedTestOpen(true)}
                size="sm"
                title={t("Manage & speed test")}
                type="button"
                variant="outline"
              >
                <Zap className="h-3.5 w-3.5" />
                {t("Manage & speed test")}
              </Button>
            </div>
          }
          className="sm:col-span-2"
          label={t("Request URL")}
          labelHint={
            <FieldLabelHint>
              {t(draft.urlMode === "full" ? providerFullUrlHintKey : providerBaseUrlHintKey)}
            </FieldLabelHint>
          }
        >
          <Input
            value={draft.baseUrl}
            onChange={(event) => onChange({ baseUrl: event.target.value }, true)}
            placeholder={draft.urlMode === "full" ? "https://gw.example.com/llm/claude" : "https://api.example.com/v1"}
            spellCheck={false}
          />
        </Field>
        <Field className="sm:col-span-2" label={t("API format")}>
          <SelectControl
            aria-label={t("API format")}
            onChange={(value) => onChange({ protocol: value as AddProviderDraft["protocol"] }, true)}
            options={protocolOptions}
            value={draft.protocol}
          />
        </Field>
        {/* API密钥章节(2026-10-11 与详情页统一):标题行 = 策略(固定密钥/
            负载均衡)+ 添加按钮;多密钥列表交互与详情面板同一编辑器。 */}
        <Field
          action={
            <div className="flex shrink-0 items-center gap-2">
              <SelectControl
                aria-label={t("Key strategy")}
                className="w-[132px]"
                onChange={(value) => changeKeyMode(value as ProviderKeyMode)}
                options={keyModeOptions}
                value={keyMode}
              />
              <Button
                className="h-7 gap-1 px-2 text-caption"
                onClick={addDraftApiKey}
                size="sm"
                title={t("Add API key")}
                type="button"
                variant="outline"
              >
                <Plus className="h-3.5 w-3.5" />
                {t("Add API key")}
              </Button>
            </div>
          }
          className="sm:col-span-2"
          label={t("API key")}
        >
          <ProviderApiKeysEditor
            accounts={draft.accounts}
            fixedMode={keyMode === "fixed"}
            notify={notify ?? (() => {})}
            onAccountsChange={(accounts) => onChange({ accounts }, true)}
            onFixedKeyPick={pickDraftFixedKey}
          />
        </Field>
        {/* 20261017:高级设置折叠区(网络代理/客户端指纹;API 密钥之后,
            恒收起,有配置时标题行带摘要)。自定义请求头是详情页字段,表单
            无此编辑器。草稿态代理仍随探针载荷发送(20260906_01 §4.9b)。 */}
        <ProviderAdvancedSettings
          className="sm:col-span-2"
          contentClassName="space-y-3"
          summary={advancedSummary}
        >
          <ProviderProxyField
            mode={draft.proxyMode}
            onChange={(mode, presetId) => onChange({ proxyMode: mode, proxyPresetId: presetId })}
            onOpenProxySettings={onOpenProxySettings}
            presetId={draft.proxyPresetId}
            presets={proxyPresets}
          />
          <ProviderClientFingerprintField
            mode={draft.clientFingerprintMode}
            onChange={(mode, presetId) =>
              onChange({ clientFingerprintMode: mode, clientFingerprintPresetId: presetId })
            }
            presetId={draft.clientFingerprintPresetId}
            protocol={draft.protocol}
          />
        </ProviderAdvancedSettings>
        <div className="min-w-0 space-y-1 sm:col-span-2">
          <div className="flex min-w-0 items-center justify-between gap-2">
            <span className="block truncate text-caption font-medium uppercase tracking-wide text-muted-foreground">{t("Models list")}</span>
            <div className="flex shrink-0 items-center gap-2">
              <Button
                className="h-7 gap-1 px-2 text-caption"
                disabled={connectivityLoading || !onCheck || !hasConnectivityCheckInputs}
                onClick={() => void onCheck?.()}
                size="sm"
                title={t("Check Availability")}
                type="button"
                variant="outline"
              >
                <ShieldCheck className="h-3.5 w-3.5" />
                {t("Check Availability")}
              </Button>
              <Button
                aria-label={t("Add model")}
                className="h-7 gap-1 px-2 text-caption"
                onClick={onAddModel}
                size="sm"
                title={t("Add model")}
                type="button"
                variant="outline"
              >
                <Plus className="h-3.5 w-3.5" />
                {t("Add model")}
              </Button>
            </div>
          </div>
          {configuredModels.length === 0 ? (
            <div className="flex items-center gap-2 rounded-md border border-dashed border-border bg-muted/20 px-3 py-3 text-body text-muted-foreground">
              <Layers3 className="h-4 w-4 shrink-0 opacity-60" />
              <span>{t("No models yet. Click Add model to add one.")}</span>
            </div>
          ) : null}
          {configuredModels.length > 0 ? (
            <div className="overflow-hidden rounded-md border border-border/60 bg-background/60">
              <div className="divide-y divide-border/50">
                {configuredModels.map((model) => {
                  // 2026-09-10:与详情面板同范式 —— 直接展示全称「供应商/模型名」
                  // + 名称右侧复制按钮;别名不再单独成行。
                  const fullName = `${draft.name.trim() || t("Unnamed")}/${model}`;
                  const supportsVision = (inputTypes?.[model] ?? []).includes("image");
                  const contextWindow = contextWindows?.[model];
                  return (
                    <DraftModelRow
                      contextWindow={contextWindow}
                      fullName={fullName}
                      key={model}
                      model={model}
                      supportsVision={supportsVision}
                      onCopy={() => void copyTextToClipboard(fullName)}
                      onEdit={() => onEditModel(model)}
                      onRemove={() => onRemoveModel(model)}
                    />
                  );
                })}
              </div>
            </div>
          ) : null}
        </div>
        <div className="sm:col-span-2 flex min-w-0 flex-wrap items-center gap-2 text-body text-muted-foreground">
          {connectivityLoading ? (
            <span className="inline-flex items-center gap-1.5">
              <LoaderCircle className="h-3.5 w-3.5 animate-spin" />
              {t("Checking availability")}
            </span>
          ) : providerProbeHasSupportedProtocol(connectivityProbe) ? (
            <span className="inline-flex items-center gap-1.5 text-foreground">
              <Check className="h-3.5 w-3.5" />
              {t("Availability verified")}
            </span>
          ) : hasConnectivityCheckInputs ? (
            <span>{t("Click Check Availability to verify each model with a real request.")}</span>
          ) : draft.baseUrl.trim() || hasUsableKey || draft.apiKey.trim() || splitLines(draft.modelsText).length > 0 || draft.selectedModels.length > 0 ? (
            <span>{t("Enter API endpoint, API key, and at least one model to enable the availability check.")}</span>
          ) : null}
        </div>
      </div>

      {error ? <div className="mt-3 flex items-start gap-2 rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-body text-destructive"><CircleAlert className="mt-0.5 h-3.5 w-3.5 shrink-0" /><span>{error}</span></div> : null}

      <ProviderSpeedTestDialog
        baseUrl={draft.baseUrl}
        onApply={(url) => onChange({ baseUrl: url }, true)}
        onClose={() => setSpeedTestOpen(false)}
        open={speedTestOpen}
        proxy={probeProxyPayload(proxyOverrideFromSelection(draft.proxyMode, draft.proxyPresetId))}
      />
    </>
  );
}

// 草稿态模型行(2026-09-10):全称「供应商/模型名」一行 + 紧随的复制按钮 +
// 视觉/上下文窗口徽标,与详情面板 ProviderModelRow 同一交互范式。复制成功
// 反馈走行内图标切换(Copy→Check,1.5s 复位),不弹 toast。
function DraftModelRow({
  contextWindow,
  fullName,
  model,
  supportsVision,
  onCopy,
  onEdit,
  onRemove
}: {
  contextWindow?: number;
  fullName: string;
  model: string;
  supportsVision: boolean;
  onCopy: () => void;
  onEdit: () => void;
  onRemove: () => void;
}) {
  const t = useAppText();
  const [copied, setCopied] = useState(false);
  const copiedTimer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(copiedTimer.current), []);

  function copyFullName() {
    onCopy();
    setCopied(true);
    window.clearTimeout(copiedTimer.current);
    copiedTimer.current = window.setTimeout(() => setCopied(false), 1500);
  }

  return (
    <div className="flex min-h-[46px] items-center gap-2 px-3 py-2">
      <div className="flex min-w-0 flex-1 items-center gap-1">
        <span className="min-w-0 truncate font-mono text-body" title={fullName}>
          {fullName}
        </span>
        <Button
          aria-label={copied ? t("Copied") : `${t("Copy model name")} ${fullName}`}
          className="h-6 w-6 shrink-0 p-0 text-muted-foreground hover:text-foreground"
          onClick={copyFullName}
          size="iconSm"
          title={copied ? t("Copied") : t("Copy model name")}
          type="button"
          variant="ghost"
        >
          {copied ? <Check className="h-3.5 w-3.5 text-emerald-500" /> : <Copy className="h-3.5 w-3.5" />}
        </Button>
      </div>
      {supportsVision ? (
        <span className="shrink-0 rounded bg-primary/10 px-1.5 py-0.5 text-caption font-medium leading-none text-primary">{t("Vision")}</span>
      ) : null}
      {contextWindow ? (
        <span className="shrink-0 rounded bg-muted px-1.5 py-0.5 text-caption font-medium leading-none text-muted-foreground">{formatContextWindow(contextWindow)}</span>
      ) : null}
      <div className="flex shrink-0 items-center gap-0.5">
        <Button
          aria-label={`${t("Edit model")} ${model}`}
          className="h-7 w-7 p-0"
          onClick={onEdit}
          size="iconSm"
          title={t("Edit model")}
          type="button"
          variant="ghost"
        >
          <Pencil className="h-3.5 w-3.5" />
        </Button>
        <Button
          aria-label={`${t("Remove")} ${model}`}
          className="h-7 w-7 p-0 hover:bg-destructive/20 hover:text-destructive"
          onClick={onRemove}
          size="iconSm"
          title={t("Remove model")}
          type="button"
          variant="ghost"
        >
          <Trash2 className="h-3.5 w-3.5" />
        </Button>
      </div>
    </div>
  );
}
