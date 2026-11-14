import { useEffect, useRef, useState, type ReactNode } from "react";
import { ChevronDown, CircleAlert, Eye, EyeOff, Globe, ListChecks, LoaderCircle, Pencil, Plus, ShieldCheck, Trash2, Waypoints, X } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Dialog, DialogBody, DialogContent, DialogFooter, DialogHeader, DialogTitle
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { api } from "@/lib/api";
import { cn } from "@/lib/utils";
import type { AppConfig, ProxyPreset, ProxyPresetTestResult } from "@/contracts/app";
import {
  AppCopy, DEFAULT_PROXY_PRESET_PORT, PROXY_TEST_TARGETS, Field, SelectControl, createProxyPreset,
  formatAppError, normalizeProxyPresets, normalizeProxyUpstreamConfig,
  parseProxyPresetTarget, proxyPresetAddress, proxyPresetIssue, proxyPresetReferenceCounts,
  useAppText
} from "../../shared/index";

// 方案 20260906_01 F2:代理分区 = 默认策略(none/引用某预设)
// + 命名预设 CRUD(设置页集中管理,供应商页只做选择)。
// 写入仍走既有的草稿配置通道(onChange → changeProxyConfig → persistConfig),
// 没有新增 RPC;「测试」用 testProxyPreset,被测对象是**当前编辑值**。
// 2026-09-13:「使用系统代理」模式退役 —— 只剩不使用代理/自定义代理两档,
// 默认策略卡改为双选择框(档位 + 预设引用)。

type PresetTestState = {
  busy: boolean;
  error?: string;
  id: string;
  result?: ProxyPresetTestResult;
};

export function ProxySettingsSection({
  config,
  copy,
  onChange
}: {
  config: AppConfig;
  copy: AppCopy;
  onChange: (patch: Partial<AppConfig["proxy"]>) => void;
}) {
  const t = useAppText();
  const presets = config.proxy.presets ?? [];
  const upstream = normalizeProxyUpstreamConfig(config.proxy.upstream, presets);
  const gatewayPort = Number(config.PORT || config.gateway?.port || 0);
  const referenceCounts = proxyPresetReferenceCounts(config.Providers ?? []);

  // 2026-09-13 七轮:检测目标统一在代理章节管理 —— 内置目标(前端镜像后端
  // PRESET_TEST_TARGETS)+ 自定义目标(config.proxy.testTargets,随配置落盘),
  // 「检测可用性」弹窗只做本次运行的勾选。
  const customTargets = config.proxy.testTargets ?? [];
  const checkTargets = [
    ...PROXY_TEST_TARGETS,
    ...customTargets.filter((url) => !PROXY_TEST_TARGETS.includes(url))
  ];

  function submitTarget() {
    const url = targetDraft.trim();
    if (!url) {
      return;
    }
    if (!/^https?:\/\//.test(url)) {
      setTargetIssue("Only http and https URLs are supported.");
      return;
    }
    if (checkTargets.includes(url)) {
      setTargetIssue("This address already exists.");
      return;
    }
    if (customTargets.length >= 12) {
      setTargetIssue("At most 12 custom check targets.");
      return;
    }
    onChange({ testTargets: [...customTargets, url] });
    setTargetDraft("");
    setTargetIssue(null);
  }

  function removeTarget(url: string) {
    onChange({ testTargets: customTargets.filter((item) => item !== url) });
  }

  // 内联编辑态:新增时 draft 持有一个尚未落库的新 id;编辑时 draft.id == 预设 id。
  const [draft, setDraft] = useState<ProxyPreset | null>(null);
  const [isNew, setIsNew] = useState(false);
  const [revealed, setRevealed] = useState(false);
  const [test, setTest] = useState<PresetTestState | null>(null);
  const [pendingDelete, setPendingDelete] = useState<ProxyPreset | null>(null);
  // 2026-09-13 五轮:章节级「检测可用性」弹窗(逐条「测试代理」按钮撤除)。
  const [checkOpen, setCheckOpen] = useState(false);
  // 2026-09-13 七轮:自定义检测目标的添加行(管理卡内)。
  const [targetDraft, setTargetDraft] = useState("");
  const [targetIssue, setTargetIssue] = useState<string | null>(null);
  // 2026-09-13 八轮:检测目标卡默认收起,头部整行可点展开/折叠。
  const [targetsOpen, setTargetsOpen] = useState(false);

  const issue = draft ? proxyPresetIssue(draft, presets, gatewayPort) : null;
  // 2026-09-13:档位与预设拆成两个选择框;预设下拉在自定义代理档位下显示。
  const defaultModeOptions = [
    { label: t("Do not use proxy"), value: "none" },
    { label: t("Use custom proxy"), value: "preset" }
  ];
  const selectedPresetId = presets.some((preset) => preset.id === upstream.presetId)
    ? upstream.presetId
    : "";
  const presetOptions = [
    ...(selectedPresetId ? [] : [{ label: t("No proxy preset selected"), value: "" }]),
    ...presets.map((preset) => ({ label: `${preset.name} (${proxyPresetAddress(preset)})`, value: preset.id }))
  ];

  function changeDefaultMode(value: string) {
    // 切到自定义代理时默认引用第一个预设(有可选时);未选具体预设时落空串,
    // 运行时后端 warn + 直连,与「预设缺失」路径一致。
    onChange({
      upstream:
        value === "preset"
          ? { mode: "preset", presetId: selectedPresetId || presets[0]?.id || "" }
          : { mode: "none", presetId: "" }
    });
  }

  function startAdd() {
    setDraft(createProxyPreset());
    setIsNew(true);
    setRevealed(false);
    setTest(null);
  }

  function startEdit(preset: ProxyPreset) {
    setDraft({ ...preset });
    setIsNew(false);
    setRevealed(false);
    setTest(null);
  }

  function cancelEdit() {
    setDraft(null);
    setIsNew(false);
    setTest(null);
  }

  function saveDraft() {
    if (!draft || issue) {
      return;
    }
    const [normalized] = normalizeProxyPresets([draft]);
    if (!normalized) {
      return;
    }
    onChange({
      presets: isNew
        ? [...presets, normalized]
        : presets.map((preset) => (preset.id === normalized.id ? normalized : preset))
    });
    cancelEdit();
  }

  function confirmDelete() {
    if (!pendingDelete) {
      return;
    }
    const id = pendingDelete.id;
    // 决策 #6:不阻止删除。引用方(供应商/全局默认)在归一与运行时都会自动
    // 回落,配置里不留需要手工清理的悬空引用。
    onChange({ presets: presets.filter((preset) => preset.id !== id) });
    if (draft?.id === id) {
      cancelEdit();
    }
    setPendingDelete(null);
  }

  async function runTest(preset: ProxyPreset) {
    setTest({ busy: true, id: preset.id });
    try {
      const result = await api.testProxyPreset({ preset });
      setTest({ busy: false, id: preset.id, result });
    } catch (error) {
      setTest({ busy: false, error: formatAppError(copy, error), id: preset.id });
    }
  }

  // 2026-09-10 二轮:新增/编辑代理收进弹窗(条件挂载),列表区只保留条目。
  const editingForm = (preset: ProxyPreset) => (
    <Dialog onOpenChange={(open) => { if (!open) cancelEdit(); }} open>
      <DialogContent className="max-w-[560px]">
        <DialogHeader>
          <div className="min-w-0">
            <DialogTitle>{isNew ? t("Add proxy") : t("Edit proxy")}</DialogTitle>
          </div>
          <Button
            aria-label={t("Close dialog")}
            onClick={cancelEdit}
            size="iconSm"
            title={t("Close")}
            type="button"
            variant="ghost"
          >
            <X className="h-4 w-4" />
          </Button>
        </DialogHeader>
        <DialogBody>
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
      <Field className="sm:col-span-2" label={t("Proxy name")}>
        <Input
          onChange={(event) => setDraft({ ...preset, name: event.target.value })}
          placeholder="Clash"
          value={preset.name}
        />
      </Field>
      <Field label={t("Proxy server")}>
        <Input
          onChange={(event) => setDraft({ ...preset, server: event.target.value })}
          placeholder="127.0.0.1"
          value={preset.server}
        />
      </Field>
      <Field label={t("Port")}>
        <Input
          max={65535}
          min={1}
          onChange={(event) => {
            const port = Number(event.target.value);
            setDraft({ ...preset, port: Number.isFinite(port) ? Math.trunc(port) : 0 });
          }}
          placeholder={String(DEFAULT_PROXY_PRESET_PORT)}
          type="number"
          value={preset.port ? String(preset.port) : ""}
        />
      </Field>
      <Field label={t("Username")}>
        <Input
          autoComplete="off"
          onChange={(event) => setDraft({ ...preset, username: event.target.value })}
          placeholder={t("Optional")}
          value={preset.username}
        />
      </Field>
      <Field label={t("Password")}>
        <div className="relative">
          <Input
            autoComplete="new-password"
            className="pr-9"
            onChange={(event) => setDraft({ ...preset, password: event.target.value })}
            placeholder={t("Optional")}
            type={revealed ? "text" : "password"}
            value={preset.password}
          />
          <button
            aria-label={revealed ? t("Hide") : t("Show")}
            className="absolute right-2 top-1/2 flex h-6 w-6 -translate-y-1/2 items-center justify-center rounded text-muted-foreground outline-none transition-colors hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring/30"
            onClick={() => setRevealed((value) => !value)}
            title={revealed ? t("Hide") : t("Show")}
            type="button"
          >
            {revealed ? <EyeOff className="h-3.5 w-3.5" /> : <Eye className="h-3.5 w-3.5" />}
          </button>
        </div>
      </Field>
      {issue ? (
        <p className="flex items-start gap-1.5 text-caption text-destructive sm:col-span-2">
          <CircleAlert className="mt-0.5 h-3.5 w-3.5 shrink-0" />
          <span>{t(issue)}</span>
        </p>
      ) : null}
            {test?.id === preset.id ? <ProxyTestSummary test={test} /> : null}
          </div>
        </DialogBody>
        <DialogFooter>
          <Button
            disabled={Boolean(test?.busy) || !parseProxyPresetTarget(preset)}
            onClick={() => runTest(preset)}
            type="button"
            variant="outline"
          >
            {test?.busy && test.id === preset.id ? (
              <LoaderCircle className="h-4 w-4 animate-spin" />
            ) : null}
            {test?.busy && test.id === preset.id ? t("Testing proxy") : t("Test proxy")}
          </Button>
          <Button onClick={cancelEdit} type="button" variant="ghost">
            {t("Cancel")}
          </Button>
          <Button disabled={Boolean(issue)} onClick={saveDraft} type="button">
            {t("Save")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );

  return (
    <section className="grid grid-cols-1 gap-3">
      <h4 className="text-body-lg font-semibold text-foreground">{copy.settings.proxy}</h4>

      {/* 2026-09-10:代理章节拆成两部分 —— 默认代理配置(网关默认出口)与
          自定义代理管理(命名预设 CRUD),各自成卡、各带小节头。 */}
      {/* 2026-09-13 二轮:两个选择框恒显示 —— 「不使用代理」时预设框冻结
          (disabled)但不消失。 */}
      <div className="flex min-w-0 items-center gap-2 rounded-md border border-border/70 bg-card/70 p-3">
        <ProxySubHeader
          className="min-w-0 flex-1"
          description={t("Providers can override this per provider.")}
          icon={Globe}
          title={t("Default proxy configuration")}
        />
        <SelectControl
          aria-label={t("Default proxy policy")}
          className="w-[150px] shrink-0"
          onChange={changeDefaultMode}
          options={defaultModeOptions}
          value={upstream.mode}
        />
        <SelectControl
          aria-label={t("Custom proxies")}
          className="w-[220px] shrink-0"
          disabled={upstream.mode !== "preset"}
          onChange={(value) => value && onChange({ upstream: { mode: "preset", presetId: value } })}
          options={presetOptions}
          value={selectedPresetId}
        />
      </div>

      <div className="grid grid-cols-1 gap-3 rounded-md border border-border/70 bg-card/70 p-3">
        <ProxySubHeader
          action={
            <div className="flex shrink-0 items-center gap-2">
              <Button
                disabled={presets.length === 0}
                onClick={() => setCheckOpen(true)}
                size="sm"
                type="button"
                variant="outline"
              >
                <ShieldCheck className="h-4 w-4" />
                {t("Check Availability")}
              </Button>
              <Button onClick={startAdd} size="sm" type="button" variant="outline">
                <Plus className="h-4 w-4" />
                {t("Add proxy")}
              </Button>
            </div>
          }
          description={t("Named proxies referenced by the global default or individual providers.")}
          icon={Waypoints}
          title={t("Custom proxy management")}
        />

        {presets.length === 0 && !draft ? (
          <div className="rounded-md border border-dashed border-border bg-muted/20 px-3 py-6 text-center">
            <Waypoints aria-hidden="true" className="mx-auto mb-2 h-6 w-6 text-muted-foreground/40" />
            <div className="text-body text-muted-foreground">{t("No custom proxies yet.")}</div>
            <Button className="mt-3" onClick={startAdd} size="sm" type="button" variant="ghost">
              <Plus className="h-3.5 w-3.5" />
              {t("Add proxy")}
            </Button>
          </div>
        ) : null}

        <div className="grid grid-cols-1 gap-2">
          {presets.map((preset) => {
            const count = referenceCounts[preset.id] ?? 0;
            return (
              <div
                className="grid grid-cols-1 gap-2 rounded-md border border-border/60 bg-background/60 p-2.5"
                key={preset.id}
              >
                {/* 2026-09-13 七轮:四组键值弹性框 —— 固定小间距,余量按比例
                    分给各框(密码框增幅最大),任意窗口宽度下都不出现大间隙,
                    同比例列跨条目对齐;密码框内置小眼睛;空值渲染空框。 */}
                <div className="flex min-w-0 items-center gap-1.5">
                  <span className="shrink-0 text-caption text-muted-foreground">{t("Proxy name")}</span>
                  <ProxyValueBox boxClassName="min-w-0 flex-[2_1_76px]" title={preset.name} value={preset.name} />
                  <span className="shrink-0 text-caption text-muted-foreground">{t("Proxy address")}</span>
                  <ProxyValueBox
                    boxClassName="min-w-0 flex-[3_1_142px]"
                    title={proxyPresetAddress(preset)}
                    value={proxyPresetAddress(preset)}
                  />
                  <span className="shrink-0 text-caption text-muted-foreground">{t("Username")}</span>
                  <ProxyValueBox
                    boxClassName="min-w-0 flex-[1.5_1_80px]"
                    title={preset.username || undefined}
                    value={preset.username}
                  />
                  <span className="shrink-0 text-caption text-muted-foreground">{t("Password")}</span>
                  <ProxyPasswordChip password={preset.password} />
                  <Badge className="shrink-0" variant={count > 0 ? "secondary" : "outline"}>
                    {count > 0 ? `${count} ${t("Providers")}` : t("Unused")}
                  </Badge>
                  <Button
                    aria-label={t("Edit proxy")}
                    onClick={() => startEdit(preset)}
                    size="iconSm"
                    title={t("Edit proxy")}
                    type="button"
                    variant="ghost"
                  >
                    <Pencil className="h-4 w-4" />
                  </Button>
                  <Button
                    aria-label={t("Delete proxy")}
                    onClick={() => setPendingDelete(preset)}
                    size="iconSm"
                    title={t("Delete proxy")}
                    type="button"
                    variant="ghost"
                  >
                    <Trash2 className="h-4 w-4" />
                  </Button>
                </div>
              </div>
            );
          })}
        </div>
      </div>

      {/* 2026-09-13 八轮:检测目标卡可展开/折叠(默认收起) —— 头部整行可点,
          右侧 chevron 指示;自定义目标增删与落盘逻辑不变。 */}
      <div className="grid grid-cols-1 gap-3 rounded-md border border-border/70 bg-card/70 p-3">
        <button
          aria-expanded={targetsOpen}
          className="-m-1 flex w-full items-center gap-2 rounded-md p-1 text-left transition-colors hover:bg-muted/40"
          onClick={() => setTargetsOpen((value) => !value)}
          type="button"
        >
          <ProxySubHeader
            className="min-w-0 flex-1"
            description={t("Addresses probed when checking proxy availability. Custom targets are saved with the configuration.")}
            icon={ListChecks}
            title={t("Check targets")}
          />
          <ChevronDown
            aria-hidden="true"
            className={cn(
              "h-4 w-4 shrink-0 text-muted-foreground transition-transform",
              targetsOpen && "rotate-180"
            )}
          />
        </button>
        {targetsOpen ? (
          <>
            <div className="grid grid-cols-1 gap-1.5">
              {checkTargets.map((url) => {
                const builtin = PROXY_TEST_TARGETS.includes(url);
                return (
                  <div
                    className="flex min-w-0 items-center gap-2 rounded-md border border-border/60 bg-background/60 px-2.5 py-1.5"
                    key={url}
                  >
                    <span className="min-w-0 flex-1 truncate font-mono text-caption text-foreground" title={url}>
                      {url}
                    </span>
                    {builtin ? (
                      <Badge className="shrink-0" variant="outline">{t("Built-in")}</Badge>
                    ) : (
                      <Button
                        aria-label={t("Delete")}
                        onClick={() => removeTarget(url)}
                        size="iconSm"
                        title={t("Delete")}
                        type="button"
                        variant="ghost"
                      >
                        <Trash2 className="h-3.5 w-3.5" />
                      </Button>
                    )}
                  </div>
                );
              })}
            </div>
            <div className="flex items-center gap-2">
              <Input
                aria-label={t("Check targets")}
                className="h-8 flex-1 font-mono text-caption"
                onChange={(event) => { setTargetDraft(event.target.value); setTargetIssue(null); }}
                onKeyDown={(event) => { if (event.key === "Enter") { event.preventDefault(); submitTarget(); } }}
                placeholder="https://api.example.com"
                spellCheck={false}
                value={targetDraft}
              />
              <Button
                disabled={!targetDraft.trim() || Boolean(targetIssue)}
                onClick={submitTarget}
                size="sm"
                type="button"
                variant="outline"
              >
                <Plus className="h-4 w-4" />
                {t("Add")}
              </Button>
            </div>
            {targetIssue ? <p className="text-caption text-destructive">{t(targetIssue)}</p> : null}
          </>
        ) : null}
      </div>

      {/* 新增/编辑弹窗(条件挂载:只在有草稿时渲染) */}
      {draft ? editingForm(draft) : null}

      {/* 2026-09-13 五轮:批量检测可用性弹窗(条件挂载)。 */}
      {checkOpen ? (
        <ProxyCheckDialog
          formatError={(error) => formatAppError(copy, error)}
          onClose={() => setCheckOpen(false)}
          open
          presets={presets}
          targets={checkTargets}
        />
      ) : null}

      <Dialog onOpenChange={(open) => !open && setPendingDelete(null)} open={Boolean(pendingDelete)}>
        <DialogContent className="max-w-[520px]">
          <DialogHeader>
            <div className="min-w-0">
              <DialogTitle>{t("Delete this proxy?")}</DialogTitle>
            </div>
            <Button
              aria-label={t("Close dialog")}
              onClick={() => setPendingDelete(null)}
              size="iconSm"
              title={t("Close")}
              type="button"
              variant="ghost"
            >
              <X className="h-4 w-4" />
            </Button>
          </DialogHeader>
          <DialogBody>
            <div className="rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2.5">
              <div className="flex items-start gap-2 text-body font-medium text-destructive">
                <CircleAlert className="mt-0.5 h-3.5 w-3.5 shrink-0" />
                <span>{pendingDelete?.name}</span>
              </div>
              <div className="mt-2 space-y-1 text-caption text-muted-foreground">
                <div className="truncate">
                  <span className="font-medium text-foreground">{t("Proxy server")}:</span>{" "}
                  {pendingDelete ? proxyPresetAddress(pendingDelete) : ""}
                </div>
                {pendingDelete && (referenceCounts[pendingDelete.id] ?? 0) > 0 ? (
                  <div>
                    {`${referenceCounts[pendingDelete.id]} ${t("Providers")}`} —{" "}
                    {t("Providers using this proxy will fall back to the global default.")}
                  </div>
                ) : null}
              </div>
            </div>
          </DialogBody>
          <DialogFooter>
            <Button autoFocus onClick={() => setPendingDelete(null)} type="button" variant="outline">
              {t("Cancel")}
            </Button>
            <Button onClick={confirmDelete} type="button" variant="destructive">
              {t("Delete")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </section>
  );
}

// 2026-09-13 五轮:章节级「检测可用性」弹窗 —— 勾选要检测的代理,批量经
// testProxyPreset 逐个探测、结果就地增量展示。结构参照供应商详情页
// 「模型列表」的检测可用性弹窗(勾选列表 + 全选/清空 + 开始检测)。
export function ProxyCheckDialog({
  formatError,
  onClose,
  open,
  presets,
  targets
}: {
  formatError: (error: unknown) => string;
  onClose: () => void;
  open: boolean;
  presets: ProxyPreset[];
  /** 本次可勾选的检测目标(设置页管理的内置+自定义合并列表)。 */
  targets: string[];
}) {
  const t = useAppText();
  const [busy, setBusy] = useState(false);
  const [selection, setSelection] = useState<string[]>([]);
  const [targetSelection, setTargetSelection] = useState<string[]>([]);
  // 2026-09-13 九轮:取消粒度 = 单个目标 —— 前端按「代理×目标」逐个发探测
  // (单目标最坏 15s 超时),点取消/关窗后当前目标跑完即停,已完成结果保留。
  const [results, setResults] = useState<Record<string, { targets: ProxyPresetTestResult["targets"] }>>({});
  const cancelRef = useRef(false);

  // 只在打开沿初始化勾选(默认全选),与连通性检查弹窗同款 —— 父级重渲染
  // 不清结果;弹窗内改动勾选时显式清掉对应结果。
  const prevOpenRef = useRef(false);
  useEffect(() => {
    if (open && !prevOpenRef.current) {
      setSelection(presets.map((preset) => preset.id));
      setTargetSelection(targets);
      setResults({});
      cancelRef.current = false;
    }
    prevOpenRef.current = open;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  /** 检测中关窗也先置取消位,后台循环在当前目标结束后即停。 */
  function requestClose() {
    cancelRef.current = true;
    onClose();
  }

  function toggle(presetId: string) {
    setSelection((current) =>
      current.includes(presetId) ? current.filter((id) => id !== presetId) : [...current, presetId]
    );
    setResults((current) => {
      if (!current[presetId]) {
        return current;
      }
      const next = { ...current };
      delete next[presetId];
      return next;
    });
  }

  function toggleTarget(url: string) {
    setTargetSelection((current) =>
      current.includes(url) ? current.filter((item) => item !== url) : [...current, url]
    );
    setResults({});
  }

  async function runCheck() {
    setBusy(true);
    cancelRef.current = false;
    try {
      const acc: Record<string, { targets: ProxyPresetTestResult["targets"] }> = {};
      outer: for (const preset of presets.filter((item) => selection.includes(item.id))) {
        for (const url of targets.filter((item) => targetSelection.includes(item))) {
          if (cancelRef.current) {
            break outer;
          }
          try {
            const result = await api.testProxyPreset({ preset, targets: [url] });
            const row = result.targets[0] ?? {
              error: t("Proxy test failed."),
              ok: false,
              url
            };
            const entry = acc[preset.id] ?? { targets: [] as ProxyPresetTestResult["targets"] };
            entry.targets = [...entry.targets, row];
            acc[preset.id] = entry;
          } catch (error) {
            const entry = acc[preset.id] ?? { targets: [] as ProxyPresetTestResult["targets"] };
            entry.targets = [...entry.targets, { error: formatError(error), ok: false, url }];
            acc[preset.id] = entry;
          }
          // 增量上屏:每测完一个目标就刷新,取消后已完成的部分仍在。
          setResults({ ...acc });
        }
      }
    } finally {
      setBusy(false);
    }
  }

  const counted = presets.filter((preset) => results[preset.id]);
  const passedCount = counted.filter((preset) => results[preset.id].targets.every((item) => item.ok)).length;
  const failedCount = counted.length - passedCount;

  return (
    <Dialog onOpenChange={(nextOpen) => { if (!nextOpen) requestClose(); }} open={open}>
      <DialogContent className="max-w-[520px]">
        <DialogHeader>
          <div className="min-w-0">
            <DialogTitle>{t("Check Availability")}</DialogTitle>
          </div>
          <Button
            aria-label={t("Close dialog")}
            onClick={requestClose}
            size="iconSm"
            title={busy ? t("Cancel") : t("Close")}
            type="button"
            variant="ghost"
          >
            <X className="h-4 w-4" />
          </Button>
        </DialogHeader>
        <DialogBody>
          <div className="space-y-3">
            <div className="rounded-md border border-border bg-background p-2">
              <div className="mb-2 flex min-w-0 flex-wrap items-center justify-between gap-2">
                <div className="min-w-0 truncate text-body font-semibold">{t("Proxies to check")}</div>
                <div className="flex shrink-0 gap-1">
                  <Button
                    className="h-6 px-1.5 text-caption"
                    disabled={busy || presets.length === 0}
                    onClick={() => { setSelection(presets.map((preset) => preset.id)); setResults({}); }}
                    type="button"
                    variant="outline"
                  >
                    {t("Select all")}
                  </Button>
                  <Button
                    className="h-6 px-1.5 text-caption"
                    disabled={busy || selection.length === 0}
                    onClick={() => { setSelection([]); setResults({}); }}
                    type="button"
                    variant="outline"
                  >
                    {t("Clear all")}
                  </Button>
                </div>
              </div>
              <div className="max-h-[180px] overflow-auto">
                <div className="grid grid-cols-1 gap-2">
                  {presets.map((preset) => {
                    const checked = selection.includes(preset.id);
                    return (
                      <Label
                        className={cn(
                          "flex min-h-8 min-w-0 cursor-pointer items-center gap-2 rounded-md border border-border bg-background px-2 py-1.5 text-left text-body transition-colors hover:bg-muted",
                          checked && "border-primary bg-accent"
                        )}
                        key={preset.id}
                      >
                        <Checkbox checked={checked} disabled={busy} onCheckedChange={() => toggle(preset.id)} />
                        <span className="min-w-0 flex-1 truncate text-body" title={preset.name}>{preset.name}</span>
                        <span className="shrink-0 text-caption text-muted-foreground">
                          {proxyPresetAddress(preset)}
                        </span>
                      </Label>
                    );
                  })}
                </div>
              </div>
            </div>

            <div className="rounded-md border border-border bg-background p-2">
              <div className="mb-2 flex min-w-0 flex-wrap items-center justify-between gap-2">
                <div className="min-w-0 truncate text-body font-semibold">{t("Targets to check")}</div>
                <div className="flex shrink-0 gap-1">
                  <Button
                    className="h-6 px-1.5 text-caption"
                    disabled={busy || targets.length === 0}
                    onClick={() => { setTargetSelection(targets); setResults({}); }}
                    type="button"
                    variant="outline"
                  >
                    {t("Select all")}
                  </Button>
                  <Button
                    className="h-6 px-1.5 text-caption"
                    disabled={busy || targetSelection.length === 0}
                    onClick={() => { setTargetSelection([]); setResults({}); }}
                    type="button"
                    variant="outline"
                  >
                    {t("Clear all")}
                  </Button>
                </div>
              </div>
              <div className="max-h-[180px] overflow-auto">
                <div className="grid grid-cols-1 gap-2">
                  {targets.map((url) => {
                    const checked = targetSelection.includes(url);
                    return (
                      <Label
                        className={cn(
                          "flex min-h-8 min-w-0 cursor-pointer items-center gap-2 rounded-md border border-border bg-background px-2 py-1.5 text-left text-body transition-colors hover:bg-muted",
                          checked && "border-primary bg-accent"
                        )}
                        key={url}
                      >
                        <Checkbox checked={checked} disabled={busy} onCheckedChange={() => toggleTarget(url)} />
                        <span className="min-w-0 flex-1 truncate font-mono text-caption" title={url}>{url}</span>
                        {PROXY_TEST_TARGETS.includes(url) ? (
                          <Badge className="shrink-0" variant="outline">{t("Built-in")}</Badge>
                        ) : null}
                      </Label>
                    );
                  })}
                </div>
              </div>
            </div>

            {/* 2026-09-13 九轮:结果区仿模型检测弹窗的「检测结果」面板 —— 统计
                徽标 + 内部滚动上限,自定义代理再多也不撑高弹窗。 */}
            {Object.keys(results).length > 0 ? (
              <div className="rounded-md border border-border bg-muted/20">
                <div className="flex min-w-0 items-center justify-between gap-2 border-b border-border px-3 py-2">
                  <div className="min-w-0 truncate text-body font-semibold">{t("Check results")}</div>
                  <div className="flex shrink-0 gap-1">
                    <Badge variant={passedCount > 0 ? "success" : "outline"}>
                      {passedCount} {t("Available")}
                    </Badge>
                    <Badge variant={failedCount > 0 ? "warning" : "outline"}>
                      {failedCount} {t("Unavailable")}
                    </Badge>
                  </div>
                </div>
                <div className="max-h-[240px] space-y-1.5 overflow-auto p-2">
                {presets
                  .filter((preset) => results[preset.id])
                  .map((preset) => {
                    const result = results[preset.id];
                    const failed = result.targets.some((item) => !item.ok);
                    return (
                      <div
                        className="min-w-0 rounded-md border border-border bg-background px-2 py-1.5 text-caption"
                        key={preset.id}
                      >
                        <div className="flex min-w-0 items-center justify-between gap-2">
                          <span className="min-w-0 truncate font-semibold" title={preset.name}>{preset.name}</span>
                          <Badge variant={failed ? "warning" : "success"}>
                            {failed ? t("Proxy test failed.") : t("Proxy is reachable.")}
                          </Badge>
                        </div>
                        <div className="mt-1 space-y-0.5">
                          {result.targets.map((target) => (
                            <div className="flex items-center gap-2 text-muted-foreground" key={target.url}>
                              <span className="min-w-0 flex-1 truncate">{target.url}</span>
                              {target.ok ? (
                                <span className="shrink-0">{`${Math.round(target.latencyMs ?? 0)} ms`}</span>
                              ) : (
                                <span className="shrink-0 text-destructive">
                                  {target.error ?? t("Proxy test failed.")}
                                </span>
                              )}
                            </div>
                          ))}
                        </div>
                      </div>
                    );
                  })}
                </div>
              </div>
            ) : null}
          </div>
        </DialogBody>
        <DialogFooter>
          {busy ? (
            <Button onClick={() => { cancelRef.current = true; }} type="button" variant="outline">
              {t("Cancel")}
            </Button>
          ) : (
            <Button onClick={requestClose} type="button" variant="outline">
              {Object.keys(results).length > 0 ? t("Close") : t("Cancel")}
            </Button>
          )}
          <Button
            disabled={busy || selection.length === 0 || targetSelection.length === 0}
            onClick={() => void runCheck()}
            type="button"
          >
            {busy ? <LoaderCircle className="h-4 w-4 animate-spin" /> : <ShieldCheck className="h-4 w-4" />}
            {t("Start check")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

// 条目内的密码值框:掩码 + 小眼睛就地查看;空密码只渲染空框。
// 弹性宽度(grow 最大)与名称/地址/用户名框同款,保证跨条目列对齐。
function ProxyPasswordChip({ password }: { password: string }) {
  const t = useAppText();
  const [revealed, setRevealed] = useState(false);
  return (
    <span
      className={cn(
        "flex h-7 min-w-0 flex-[4_1_76px] items-center overflow-hidden rounded-md border border-border/70 bg-background/60 text-body font-semibold text-foreground",
        password ? "gap-1 pl-2.5 pr-1" : "px-2.5"
      )}
      title={password && revealed ? password : undefined}
    >
      {password ? (
        <>
          <span className="min-w-0 flex-1 truncate">{revealed ? password : "•••••"}</span>
          <button
            aria-label={revealed ? t("Hide") : t("Show")}
            className="flex h-5 w-5 shrink-0 items-center justify-center rounded text-muted-foreground outline-none transition-colors hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring/30"
            onClick={() => setRevealed((value) => !value)}
            title={revealed ? t("Hide") : t("Show")}
            type="button"
          >
            {revealed ? <EyeOff className="h-3.5 w-3.5" /> : <Eye className="h-3.5 w-3.5" />}
          </button>
        </>
      ) : null}
    </span>
  );
}

// 条目内的通用值框(名称/地址/用户名):弹性宽度(flex-basis 起步,余量按
// grow 比例分摊,同比例跨条目列对齐)、加粗、截断带 title。
function ProxyValueBox({ boxClassName, title, value }: { boxClassName: string; title?: string; value: string }) {
  return (
    <span
      className={cn(
        "flex h-7 items-center overflow-hidden rounded-md border border-border/70 bg-background/60 px-2.5 text-body font-semibold text-foreground",
        boxClassName
      )}
      title={title || undefined}
    >
      <span className="min-w-0 truncate">{value}</span>
    </span>
  );
}

function ProxyTestSummary({ test }: { test: PresetTestState }) {
  const t = useAppText();
  if (test.busy) {
    return (
      <p className="flex items-center gap-1.5 text-caption text-muted-foreground">
        <LoaderCircle className="h-3.5 w-3.5 animate-spin" />
        {t("Testing proxy")}
      </p>
    );
  }
  if (test.error) {
    return <p className="text-caption text-destructive">{test.error}</p>;
  }
  const result = test.result;
  if (!result) {
    return null;
  }
  return (
    <div className="grid grid-cols-1 gap-1 sm:col-span-2">
      <div
        className={cn(
          "text-caption font-medium",
          result.ok ? "text-foreground" : "text-destructive"
        )}
      >
        {result.ok ? t("Proxy is reachable.") : t("Proxy test failed.")}
        {result.proxy ? <span className="font-normal text-muted-foreground"> · {result.proxy}</span> : null}
      </div>
      {result.targets.map((target) => (
        <div className="flex items-center gap-2 text-caption text-muted-foreground" key={target.url}>
          <span className="min-w-0 flex-1 truncate">{target.url}</span>
          {target.ok ? (
            <span className="shrink-0">{`${Math.round(target.latencyMs ?? 0)} ms`}</span>
          ) : (
            <span className="shrink-0 text-destructive">{target.error ?? t("Proxy test failed.")}</span>
          )}
        </div>
      ))}
    </div>
  );
}

// 代理小节头(2026-09-10):图标片 + 标题 + 一句话说明(+ 右侧动作)。
// 「默认代理配置」「自定义代理管理」两张卡共用,替代原先混在一起的单卡布局。
function ProxySubHeader({
  action,
  className,
  description,
  icon: Icon,
  title
}: {
  action?: ReactNode;
  className?: string;
  description: string;
  icon: typeof Globe;
  title: string;
}) {
  return (
    <div className={cn("flex min-w-0 items-center gap-2", className)}>
      <span className="flex h-6 w-6 shrink-0 items-center justify-center rounded-md bg-primary/10 text-primary">
        <Icon aria-hidden="true" className="h-3.5 w-3.5" />
      </span>
      <span className="shrink-0 text-body font-semibold text-foreground">{title}</span>
      <span className="min-w-0 flex-1 truncate text-caption text-muted-foreground" title={description}>
        {description}
      </span>
      {action ?? null}
    </div>
  );
}
