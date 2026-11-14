import { memo, useEffect, useMemo, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Check, Copy, Pencil, Plus, ShieldCheck, Trash2, Zap } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { api, hasBridge } from "@/lib/api";
import { queryKeys } from "@/lib/queryKeys";
import { cn } from "@/lib/utils";
import {
  Field, FieldLabelHint, GatewayProviderConfig, GatewayProviderProtocol, isProviderNameDuplicate, isReservedProviderName,
  ProviderClientFingerprintOverride,
  ProviderConnectivityCheckReport,
  ProviderCredentialAccountStat, ProviderCredentialConfig, ProviderKeyMode,
  ProviderProxyOverride, ProviderUrlMode, ProxyPreset,
  copyTextToClipboard,
  formatContextWindow,
  RefreshButton, createProviderAccount, nextProviderAccountLabel,
  providerApiKey, providerBaseUrl, providerBaseUrlHintKey, providerCredentialAccounts, providerDisplayIcon, providerIsEnabled, providerFullUrlHintKey, providerKeyMode,
  providerModelSupportsVision,
  providerProtocolOptions,
  clientFingerprintOverrideFromSelection, clientFingerprintSelectionFromOverride,
  normalizeProviderProxy, probeProxyPayload, proxyOverrideFromSelection, proxySelectionFromOverride,
  SelectControl, toProviderProtocol, useAppText,
} from "../../shared/index";
import { ProviderPresetIcon } from "./providers-deep-link";
import { ProviderClientFingerprintField } from "./provider-client-fingerprint-field";
import { ProviderConnectivityCheckDialog } from "./provider-connectivity";
import { ProviderHeadersEditor } from "./provider-headers-editor";
import { ProviderApiKeysSection } from "./provider-api-keys";
import {
  ProviderAdvancedSettings,
  joinAdvancedSummary,
  providerFingerprintSummary,
  providerProxySummary
} from "./provider-advanced-settings";
import { ProviderProxyField } from "./provider-proxy-field";
import { ProviderSpeedTestDialog } from "./provider-speedtest";

// F4.3: 从 providers-list.tsx 拆出 —— 右侧详情表单 + 行级 memo 的模型行 +
// 启停状态徽标。

export function ProviderDetailPanel({
  addModel,
  checkProviderConnectivity,
  index,
  notify,
  onEditModel,
  onOpenProxySettings,
  provider,
  providers,
  removeModel,
  removeProvider,
  migrateProviderLegacyKey,
  setProviderBaseUrl,
  setProviderUrlMode,
  setProviderClientFingerprint,
  setProviderHeaders,
  setProviderCredentialMode,
  setProviderEnabled,
  setProviderName,
  setProviderFixedKey,
  setProviderProtocol,
  setProviderProxy,
  setProviderAccounts,
  proxyPresets
}: {
  addModel: () => void;
  checkProviderConnectivity: (providerIndex: number, models?: string[]) => Promise<ProviderConnectivityCheckReport>;
  index: number;
  notify: (message: string) => void;
  onEditModel: (provider: GatewayProviderConfig, providerIndex: number, model: string) => void;
  /** 20260906_01 验收 #5:预设列表为空时跳转设置页代理分区。 */
  onOpenProxySettings: () => void;
  provider: GatewayProviderConfig;
  providers: Array<{ provider: GatewayProviderConfig; index: number }>;
  removeModel: (providerIndex: number, model: string) => void;
  removeProvider: (index: number) => void;
  migrateProviderLegacyKey: (providerIndex: number) => void;
  setProviderBaseUrl: (providerIndex: number, baseUrl: string) => void;
  /** 20261014_01:URL 模式切换(full 落键,base 摘键)。 */
  setProviderUrlMode: (providerIndex: number, urlMode: ProviderUrlMode) => void;
  /** 20261016_03:客户端指纹切换(undefined = 关闭,删掉键)。 */
  setProviderClientFingerprint: (
    providerIndex: number,
    fingerprint: ProviderClientFingerprintOverride | undefined
  ) => void;
  /** 20261016_03:自定义请求头(undefined = 摘键)。 */
  setProviderHeaders: (providerIndex: number, headers: Record<string, string> | undefined) => void;
  setProviderCredentialMode: (providerIndex: number, mode: ProviderKeyMode) => void;
  setProviderEnabled: (providerIndex: number, enabled: boolean) => void;
  setProviderName: (providerIndex: number, name: string) => void;
  setProviderFixedKey: (providerIndex: number, accountId: string) => void;
  setProviderProtocol: (providerIndex: number, protocol: GatewayProviderProtocol) => void;
  /** 20260906_01 §4.9b:undefined = 跟随全局默认(删掉 proxy 键)。 */
  setProviderProxy: (providerIndex: number, proxy: ProviderProxyOverride | undefined) => void;
  setProviderAccounts: (providerIndex: number, accounts: ProviderCredentialConfig[]) => void;
  proxyPresets: ProxyPreset[];
}) {
  const t = useAppText();
  const enabled = providerIsEnabled(provider);
  const baseUrl = providerBaseUrl(provider);
  const apiKey = providerApiKey(provider);
  const protocol = toProviderProtocol(provider.type) ?? toProviderProtocol(provider.provider) ?? "openai_chat_completions";
  const accounts = providerCredentialAccounts(provider);
  const keyMode = providerKeyMode(provider.credentialStrategy);
  // 检测可用性判据:任一启用且已设密钥(2026-09-06 第五轮起面板只渲染密钥
  // 列表,legacy 单密钥在打开面板时自动迁入列表)。
  const hasUsableKey = accounts.some((account) => account.enabled !== false && Boolean(account.api_key));
  // K7 健康态查询上提到面板(刷新按钮在标题行「添加 API密钥」右侧);
  // 无密钥行时无凭据池可查。
  const providerId = provider.id || provider.name;
  const statsQuery = useQuery({
    queryKey: queryKeys.providerCredentialStats(providerId),
    queryFn: () => api.getProviderCredentialStats({ providerId }),
    enabled: hasBridge() && Boolean(providerId) && accounts.length > 0,
    retry: false
  });
  const statsByAccountId = useMemo(() => {
    const map = new Map<string, ProviderCredentialAccountStat>();
    const entry = statsQuery.data?.providers.find((item) => item.providerId === providerId);
    for (const stat of entry?.accounts ?? []) {
      map.set(stat.accountId, stat);
    }
    return map;
  }, [statsQuery.data, providerId]);
  const [nameDraft, setNameDraft] = useState(provider.name);
  const [baseUrlDraft, setBaseUrlDraft] = useState(baseUrl);
  // 20260906_01 §4.9b:代理档位本地态(与 name/baseUrl 草稿同款回读)。
  // 回读必须先经 normalizeProviderProxy 归一:悬空预设引用(预设刚被删)要
  // 直接落成 inherit,否则「字段收敛 → 写回 → 新 provider → 又回读到悬空值」
  // 会形成写回死循环。
  const [proxyDraft, setProxyDraft] = useState(() =>
    proxySelectionFromOverride(normalizeProviderProxy(provider.proxy, proxyPresets))
  );
  // 20261016_03:客户端指纹档位本地态(与代理草稿同款回读;内建预设无悬空
  // 引用形态 —— 未知 id 后端归 off,这里原样回读显示,保存时后端门禁兜底)。
  const [fingerprintDraft, setFingerprintDraft] = useState(() =>
    clientFingerprintSelectionFromOverride(provider.clientFingerprint)
  );
  const [checkOpen, setCheckOpen] = useState(false);
  const [speedTestOpen, setSpeedTestOpen] = useState(false);

  useEffect(() => {
    setNameDraft(provider.name);
    setBaseUrlDraft(baseUrl);
    setProxyDraft(proxySelectionFromOverride(normalizeProviderProxy(provider.proxy, proxyPresets)));
    setFingerprintDraft(clientFingerprintSelectionFromOverride(provider.clientFingerprint));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [provider]);

  function changeProxy(mode: ProviderProxyOverride["mode"], presetId: string) {
    setProxyDraft({ mode, presetId });
    // 选了「自定义代理」但还没有任何预设:没有合法覆盖可写,只保留本地档位
    // 以显示「去设置页配置代理」的空态引导(验收 #5)。
    if (mode === "preset" && proxyPresets.length === 0) {
      return;
    }
    setProviderProxy(index, proxyOverrideFromSelection(mode, presetId));
  }

  function changeClientFingerprint(mode: "off" | "passthrough" | "preset", presetId: string) {
    setFingerprintDraft({ clientFingerprintMode: mode, clientFingerprintPresetId: presetId });
    setProviderClientFingerprint(index, clientFingerprintOverrideFromSelection(mode, presetId));
  }

  // 20261017:高级设置折叠区(网络代理/客户端指纹/自定义请求头)。
  // 「有非默认配置」= 折叠区默认展开(已配置的事实不藏进折叠);收起态
  // 标题行只摘要非默认项。
  const advancedHeaderCount = Object.keys(provider.headers ?? {}).length;
  const advancedConfigured =
    proxyDraft.mode !== "inherit" ||
    fingerprintDraft.clientFingerprintMode !== "off" ||
    advancedHeaderCount > 0;
  const advancedSummary = joinAdvancedSummary([
    providerProxySummary(t, proxyDraft.mode, proxyDraft.presetId, proxyPresets),
    providerFingerprintSummary(
      t,
      fingerprintDraft.clientFingerprintMode,
      fingerprintDraft.clientFingerprintPresetId
    ),
    advancedHeaderCount > 0 ? `${t("Headers")} ×${advancedHeaderCount}` : ""
  ]);

  // 2026-09-06 第五轮:存量 legacy 单密钥自动迁入列表(单次配置写入),
  // 面板对所有供应商统一渲染密钥列表。2026-10-11:条件放宽为「供应商级
  // 仍有密钥」——池已在但 secrets 残留影子键的存量供应商也在打开时清掉
  // (migrateProviderLegacyKey 内部分流:有池只清键,无池建行 + 清键)。
  useEffect(() => {
    if (apiKey.trim()) {
      migrateProviderLegacyKey(index);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [accounts.length, apiKey, index]);

  const protocolOptions = useMemo(
    () => providerProtocolOptions.map((option) => ({ label: t(option.label), value: option.value })),
    [t]
  );

  // 密钥两模式(2026-09-06):固定密钥 = first(单启用);负载均衡 = balanced。
  const keyModeOptions = useMemo(
    () => [
      { label: t("Fixed key"), value: "fixed" },
      { label: t("Load balancing"), value: "balanced" }
    ],
    [t]
  );

  /** 内容区顶部「供应商名称」输入框:失焦/回车提交(2026-09-26 从头部
   *  编辑按钮迁移而来,校验与原编辑路径一致)。 */
  function saveName() {
    const trimmed = nameDraft.trim();
    if (!trimmed || trimmed === provider.name) {
      setNameDraft(provider.name);
      return;
    }
    if (isProviderNameDuplicate(providers.map((item) => item.provider), trimmed, index)) {
      notify(t("Provider name already exists."));
      setNameDraft(provider.name);
      return;
    }
    // 名称是选择器别名,改名也不得占用聚合命名空间(2026-09-15)。
    if (isReservedProviderName(trimmed)) {
      notify(t("This provider name is reserved."));
      setNameDraft(provider.name);
      return;
    }
    setProviderName(index, trimmed);
  }

  // 「添加 API密钥」追加一把新密钥行——首把密钥恒启用;已有密钥时固定
  // 密钥模式下新行默认停用(单启用约束,填完再开启),负载均衡默认启用
  // 入池。新行带默认名称「密钥 N」(N 取第一个未占用号)。
  function addApiKey() {
    const defaultLabel = nextProviderAccountLabel(accounts, t("Key label"));
    const usedIds = accounts.map((account) => account.id ?? "").filter(Boolean);
    const account = { ...createProviderAccount(usedIds), label: defaultLabel };
    const isFirstKey = accounts.length === 0;
    setProviderAccounts(index, [
      ...accounts,
      !isFirstKey && keyMode === "fixed" ? { ...account, enabled: false } : account
    ]);
  }

  // 2026-09-13 二轮:章节小标题与本页「管理与测速」按钮文字同款同大小
  // (caption 字号、正常大小写、前景色)。
  const sectionLabelClassName = "text-caption font-medium text-foreground";

  return (
    <div className="flex h-full min-h-0 min-w-0 flex-col">
      {/* 2026-09-10:头部行内层与下方内容列同 max-w 居中,全屏时左右边缘
          对齐中间内容区域,而不是拉伸到面板两端。 */}
      <div className="h-12 shrink-0 border-b border-border/60 px-4">
        <div className="mx-auto flex h-full w-full max-w-[860px] items-center gap-2">
        {/* 2026-09-26:标题 = 图标 + 供应商名 + 「供应商」徽章(对齐聚合面板);
            名称编辑统一收进内容区顶部的「供应商名称」输入框,头部编辑按钮移除。
            头部图标用条目族(品牌预设图标,Box 兜底),与侧栏行一致。 */}
        <div className="flex min-w-0 flex-1 items-center gap-2">
          <ProviderPresetIcon className="h-7 w-7 shrink-0 rounded-md" iconUrl={providerDisplayIcon(provider)} />
          <span className="truncate text-body-lg font-semibold">{provider.name || t("Unnamed")}</span>
          <Badge className="shrink-0" variant="secondary">{t("Provider")}</Badge>
        </div>
        <EnabledToggle enabled={enabled} onToggle={() => setProviderEnabled(index, !enabled)} />
        <Button
          aria-label={`${t("Remove")} ${provider.name || t("provider")}`}
          className="bg-muted/50 text-foreground/85 hover:bg-destructive/20 hover:text-destructive"
          onClick={() => removeProvider(index)}
          size="iconSm"
          title={t("Remove provider")}
          type="button"
          variant="ghost"
        >
          <Trash2 className="h-3.5 w-3.5" />
        </Button>
        </div>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto p-4">
        {/* 2026-09-10:内容列 720 → 860,全屏下表格/密钥行更舒展。 */}
        <div className="mx-auto max-w-[860px] space-y-4">
          {/* 2026-09-26:供应商名称统一在内容区修改(与添加供应商表单同款字段)。 */}
          <Field label={t("Provider name")}>
            <Input
              aria-label={t("Provider name")}
              onBlur={saveName}
              onChange={(event) => setNameDraft(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter") {
                  event.preventDefault();
                  saveName();
                } else if (event.key === "Escape") {
                  event.preventDefault();
                  setNameDraft(provider.name);
                }
              }}
              placeholder={t("Fill in provider name")}
              spellCheck={false}
              value={nameDraft}
            />
          </Field>

          <Field
            action={
              <div className="flex shrink-0 items-center gap-2">
                {/* 20261014_01 §3.2:URL 模式 TAB(详情页与表单同一范式;
                    切换即时落库,base 摘键)。 */}
                <div
                  aria-label={t("URL mode")}
                  className="grid shrink-0 grid-cols-2 gap-0.5 rounded-lg border border-border/60 bg-muted p-0.5"
                  role="tablist"
                >
                  {([["base", "Base URL"], ["full", "Full URL"]] as const).map(([mode, label]) => (
                    <button
                      aria-selected={(provider.urlMode ?? "base") === mode}
                      className={`rounded-md px-2 py-0.5 text-caption font-semibold transition-colors ${(provider.urlMode ?? "base") === mode ? "bg-primary text-primary-foreground shadow-sm" : "text-muted-foreground hover:text-foreground"}`}
                      key={mode}
                      onClick={() => setProviderUrlMode(index, mode)}
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
            label={t("Request URL")}
            labelClassName={sectionLabelClassName}
            labelHint={
              <FieldLabelHint>
                {/* 20261014_01:详情面板只读展示该供应商的 URL 模式语义;
                    模式切换走编辑弹窗(表单 TAB)。 */}
                {t(provider.urlMode === "full" ? providerFullUrlHintKey : providerBaseUrlHintKey)}
              </FieldLabelHint>
            }
          >
            <Input
              aria-label={t("Request URL")}
              onBlur={() => {
                if (baseUrlDraft.trim() !== baseUrl) {
                  setProviderBaseUrl(index, baseUrlDraft);
                }
              }}
              onChange={(event) => setBaseUrlDraft(event.target.value)}
              placeholder={provider.urlMode === "full" ? "https://gw.example.com/llm/claude" : "https://api.example.com/v1"}
              spellCheck={false}
              value={baseUrlDraft}
            />
          </Field>

          <Field label={t("API format")} labelClassName={sectionLabelClassName}>
            <SelectControl
              aria-label={t("API format")}
              onChange={(value) => setProviderProtocol(index, value as GatewayProviderProtocol)}
              options={protocolOptions}
              value={protocol}
            />
          </Field>

          {/* API密钥章节(2026-09-06 第五轮起统一渲染密钥列表;存量 legacy
              单密钥由上方 effect 自动迁入)。标题行 = 策略(固定密钥/负载
              均衡两模式)+ 添加按钮 + 刷新按钮。 */}
          <Field
            action={
              <div className="flex shrink-0 items-center gap-2">
                <SelectControl
                  aria-label={t("Key strategy")}
                  className="w-[132px]"
                  onChange={(value) => setProviderCredentialMode(index, value as ProviderKeyMode)}
                  options={keyModeOptions}
                  value={keyMode}
                />
                <Button
                  className="h-7 gap-1 px-2 text-caption"
                  onClick={addApiKey}
                  size="sm"
                  title={t("Add API key")}
                  type="button"
                  variant="outline"
                >
                  <Plus className="h-3.5 w-3.5" />
                  {t("Add API key")}
                </Button>
                {accounts.length > 0 ? (
                  <RefreshButton
                    ariaLabel={t("Refresh key status")}
                    loading={statsQuery.isFetching}
                    onRefresh={async () => { await statsQuery.refetch(); }}
                    title={t("Refresh key status")}
                  />
                ) : null}
              </div>
            }
            label={t("API key")}
            labelClassName={sectionLabelClassName}
          >
            <ProviderApiKeysSection
              index={index}
              notify={notify}
              provider={provider}
              setProviderAccounts={setProviderAccounts}
              setProviderFixedKey={setProviderFixedKey}
              statsByAccountId={statsByAccountId}
              statsFetchedAt={statsQuery.dataUpdatedAt}
              onStatsRefetch={async () => { await statsQuery.refetch(); }}
            />
          </Field>

          {/* 20261017:高级设置折叠区(网络代理/客户端指纹/自定义请求头;
              API 密钥之后)。有非默认配置时默认展开,收起态标题行只摘要
              非默认项。三字段组件/交互原样保留,仅收纳进折叠层。 */}
          <ProviderAdvancedSettings
            contentClassName="space-y-4"
            defaultOpen={advancedConfigured}
            summary={advancedSummary}
          >
            <ProviderProxyField
              labelClassName={sectionLabelClassName}
              mode={proxyDraft.mode}
              onChange={changeProxy}
              onOpenProxySettings={onOpenProxySettings}
              presetId={proxyDraft.presetId}
              presets={proxyPresets}
            />

            <ProviderClientFingerprintField
              labelClassName={sectionLabelClassName}
              mode={fingerprintDraft.clientFingerprintMode}
              onChange={changeClientFingerprint}
              presetId={fingerprintDraft.clientFingerprintPresetId}
              protocol={protocol}
            />

            <ProviderHeadersEditor
              headers={provider.headers}
              labelClassName={sectionLabelClassName}
              onChange={(next) => setProviderHeaders(index, next)}
            />
          </ProviderAdvancedSettings>

          <div className="pt-1">
            <div className="mb-1.5 flex items-center justify-between gap-2">
              <span className={cn("min-w-0 truncate", sectionLabelClassName)}>{t("Models list")}</span>
              <div className="flex shrink-0 items-center gap-2">
                <Button
                  className="h-7 gap-1 px-2 text-caption"
                  disabled={!baseUrl.trim() || !hasUsableKey || provider.models.length === 0}
                  onClick={() => {
                    // The connectivity check reads the persisted provider config
                    // (not the in-form drafts), so flush any unsaved Base URL
                    // edits first. Otherwise the user could see a green result
                    // for the previously saved URL while the input box shows a
                    // different (and possibly broken) one.
                    if (baseUrlDraft.trim() !== baseUrl) {
                      setProviderBaseUrl(index, baseUrlDraft);
                    }
                    setCheckOpen(true);
                  }}
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
                  onClick={addModel}
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
            {provider.models.length === 0 ? (
              <div className="rounded-md border border-dashed border-border bg-muted/20 px-3 py-5 text-center text-body text-muted-foreground">{t("No models configured")}</div>
            ) : (
              <div className="overflow-hidden rounded-md border border-border/60 bg-background/60">
                <div className="divide-y divide-border/50">
                  {provider.models.map((model) => (
                    <ProviderModelRow
                      index={index}
                      key={model}
                      model={model}
                      notify={notify}
                      onEditModel={onEditModel}
                      provider={provider}
                      removeModel={removeModel}
                    />
                  ))}
                </div>
              </div>
            )}
          </div>
        </div>
      </div>

      <ProviderConnectivityCheckDialog
        models={provider.models}
        onCheck={(models) => checkProviderConnectivity(index, models)}
        onClose={() => setCheckOpen(false)}
        open={checkOpen}
        providerName={provider.name}
      />

      <ProviderSpeedTestDialog
        baseUrl={baseUrlDraft}
        onApply={(url) => {
          setBaseUrlDraft(url);
          setProviderBaseUrl(index, url);
        }}
        onClose={() => setSpeedTestOpen(false)}
        open={speedTestOpen}
        proxy={probeProxyPayload(proxyOverrideFromSelection(proxyDraft.mode, proxyDraft.presetId))}
      />
    </div>
  );
}


// 行级 memo(F4.3):详情面板内的输入(名称/URL/密钥草稿)每次击键都会重渲
// 染本面板;模型行的展示面只依赖 provider 的这几个字段,其余重渲染跳过。
// comparator 不比较回调 —— 回调由父组件按数据依赖重建,数据字段已覆盖其变化面。
export function providerModelRowEqual(
  a: ProviderModelRowProps,
  b: ProviderModelRowProps
): boolean {
  return (
    a.model === b.model &&
    a.provider.name === b.provider.name &&
    // 视觉徽标数据源:输入类型映射引用变化必须重绘(2026-09-10)。
    a.provider.modelInputTypes === b.provider.modelInputTypes &&
    a.provider.modelContextWindow === b.provider.modelContextWindow &&
    a.provider.models === b.provider.models
  );
}

type ProviderModelRowProps = {
  index: number;
  model: string;
  notify: (message: string) => void;
  onEditModel: (provider: GatewayProviderConfig, providerIndex: number, model: string) => void;
  provider: GatewayProviderConfig;
  removeModel: (providerIndex: number, model: string) => void;
};

export const ProviderModelRow = memo(function ProviderModelRow({
  index,
  model,
  notify,
  onEditModel,
  provider,
  removeModel
}: ProviderModelRowProps) {
  const t = useAppText();
  // 2026-09-10:直接展示可调用的全称「供应商/模型名」,名称右侧紧跟复制按钮。
  const fullName = `${provider.name}/${model}`;
  const supportsVision = providerModelSupportsVision(provider, model);
  const contextWindow = provider.modelContextWindow?.[model];
  const [copied, setCopied] = useState(false);
  const copiedTimer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(copiedTimer.current), []);

  function copyFullName() {
    void copyTextToClipboard(fullName).then(() => {
      setCopied(true);
      window.clearTimeout(copiedTimer.current);
      copiedTimer.current = window.setTimeout(() => setCopied(false), 1500);
    });
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
          aria-label={`${t("Edit model")} ${fullName}`}
          className="h-7 w-7 p-0"
          onClick={() => onEditModel(provider, index, model)}
          size="iconSm"
          title={t("Edit model")}
          type="button"
          variant="ghost"
        >
          <Pencil className="h-3.5 w-3.5" />
        </Button>
        <Button
          aria-label={`${t("Remove")} ${fullName}`}
          className="h-7 w-7 p-0 hover:bg-destructive/20 hover:text-destructive"
          onClick={() => {
            removeModel(index, model);
            notify(`${t("Removed")} ${fullName}`);
          }}
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
}, providerModelRowEqual);

/** 「启用/已禁用」双胶囊开关(供应商详情页首创;聚合模型详情页同款复用,
 *  可传自定义 aria/提示文案)。 */
export function EnabledToggle({
  enabled,
  onToggle,
  ariaLabel,
  disableTitle,
  enableTitle
}: {
  enabled: boolean;
  onToggle: () => void;
  ariaLabel?: string;
  disableTitle?: string;
  enableTitle?: string;
}) {
  const t = useAppText();
  return (
    <div
      aria-label={t(ariaLabel ?? "Provider status")}
      className="flex shrink-0 overflow-hidden rounded-full border border-border/70 p-0.5 text-caption font-medium"
      role="group"
    >
      <button
        aria-pressed={enabled}
        className={cn(
          "rounded-full px-2.5 py-0.5 outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring/30",
          enabled ? "bg-emerald-500/15 text-emerald-600" : "text-muted-foreground hover:text-foreground"
        )}
        onClick={() => !enabled && onToggle()}
        title={t(enableTitle ?? "Enable provider")}
        type="button"
      >
        {t("Enabled")}
      </button>
      <button
        aria-pressed={!enabled}
        className={cn(
          "rounded-full px-2.5 py-0.5 outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring/30",
          !enabled ? "bg-muted text-muted-foreground" : "text-muted-foreground hover:text-foreground"
        )}
        onClick={() => enabled && onToggle()}
        title={t(disableTitle ?? "Disable provider")}
        type="button"
      >
        {t("Disabled")}
      </button>
    </div>
  );
}
