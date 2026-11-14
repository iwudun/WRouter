import { useEffect, useMemo, useState } from "react";
import { Reorder, useDragControls } from "motion/react";
import { CircleAlert, Eye, EyeOff, GripVertical, RotateCcw, Trash2, X } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Dialog, DialogBody, DialogContent, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { api } from "@/lib/api";
import {
  GatewayProviderConfig, ProviderCredentialAccountStat, ProviderCredentialConfig,
  Toggle,
  fillDefaultAccountLabels, formatCooldownRemaining,
  numberValue, providerCredentialAccounts, providerKeyMode, useAppText,
} from "../../shared";

// K6/K7 (方案 20260905_02;2026-09-06 用户多轮调整):供应商「API key」章节
// 的多密钥内容。2026-10-11 统一:核心交互抽成 draft 无关的
// ProviderApiKeysEditor(添加页与详情页同一形态、同一交互),详情面板的
// ProviderApiKeysSection 只做「已落库供应商」的接线(配置写入 + 健康态)。
// 策略选择(固定密钥/负载均衡两模式)与刷新按钮在面板标题行;条目单行
// 格式从左至右:拖拽手柄 / 名称 / apikey / 权重 / 状态 / 开关 / 删除
// (优先级已移除)。
// - 密钥修改沿用原 WR 方式:内联输入框 + 眼睛明文切换,onBlur 保存,无
//   确认按钮;真值来自 get_config 注水的 ``accounts[].api_key``。
// - 固定密钥模式(= first/pinned)单启用:开启一把即停用其余,经调用方
//   单次配置写入(含 pinned 记忆同步;原先两次写入会被 buildConfigUpdate
//   的草稿快照相互覆盖,导致开关点击失效)。
// - 存量无名称密钥在挂载时补默认名「密钥 N」(历史数据兼容)。
// - 拖拽排序(2026-09-06 第五轮重做):motion Reorder + 手柄发起
//   (dragListener=false,不干扰行内输入框)。拖拽期间只在**本地顺序态**
//   实时重排(被拖行随指针走,其余行弹簧让位,配置零写入),pointerup
//   一次性提交;拖回原位不产生写入。
// 数据通道:密钥数组随 GatewayProviderConfig / AddProviderDraft 走现有
// 保存链路(写 ``accounts`` 键,后端密钥通道只认它)。

type ProviderApiKeysEditorProps = {
  accounts: ProviderCredentialConfig[];
  fixedMode: boolean;
  notify: (message: string) => void;
  onAccountsChange: (accounts: ProviderCredentialConfig[]) => void;
  /** 固定密钥模式开启一把时的单启用提交(含 pinned 记忆,调用方一次写入;
   *  详情面板走 setProviderFixedKey,草稿态写草稿字段)。 */
  onFixedKeyPick: (accountId: string) => void;
  /** 健康态(可选;仅详情面板提供 —— 草稿态没有已落库供应商可查)。 */
  statsByAccountId?: Map<string, ProviderCredentialAccountStat>;
  statsFetchedAt?: number;
  /** 解冷却(可选;详情面板接线 getProviderCredentialCooldown 清除接口)。 */
  onClearCooldown?: (accountId: string) => Promise<void>;
};

export function ProviderApiKeysEditor({
  accounts,
  fixedMode,
  notify,
  onAccountsChange,
  onFixedKeyPick,
  statsByAccountId = new Map(),
  statsFetchedAt = 0,
  onClearCooldown
}: ProviderApiKeysEditorProps) {
  const t = useAppText();

  // 行内编辑草稿(名称/密钥/权重)按密钥 id 暂存,onBlur 提交,对齐详情
  // 面板 Base URL/API Key 的 blur-commit 范式。
  const [labelDrafts, setLabelDrafts] = useState<Record<string, string>>({});
  const [apiKeyDrafts, setApiKeyDrafts] = useState<Record<string, string>>({});
  const [weightDrafts, setWeightDrafts] = useState<Record<string, string>>({});
  const [revealedAccountId, setRevealedAccountId] = useState<string>();
  const [clearingAccountId, setClearingAccountId] = useState<string>();
  // 删除确认(2026-09-06):行内删除按钮先弹确认框,确认才真正移除。
  const [deletingAccountId, setDeletingAccountId] = useState<string>();
  // 拖拽排序(2026-09-06 第五轮):visualOrder 持有拖拽期间的本地实时顺序,
  // motion Reorder 每次交换即更新(其余行弹簧让位),pointerup 一次性提交。
  const [visualOrder, setVisualOrder] = useState<ProviderCredentialConfig[]>();
  const [draggingId, setDraggingId] = useState<string>();
  const list = visualOrder ?? accounts;

  // 2026-09-06 历史数据兼容:为无名称的存量密钥补默认名「密钥 N」,
  // 一次写入;补名后 changed=false,不会循环。
  useEffect(() => {
    const filled = fillDefaultAccountLabels(accounts, t("Key label"));
    if (filled.changed) {
      onAccountsChange(filled.accounts);
    }
    // eslint 依赖:t / onAccountsChange 随渲染重建,但补名后 changed=false,无循环。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [accounts]);

  // 冷却倒计时:仅本地秒级递减显示,不发轮询请求;归零后徽章回落"正常"。
  const [now, setNow] = useState(() => Date.now());
  const coolingCount = useMemo(
    () =>
      accounts.filter((account) => {
        const stat = statsByAccountId.get(account.id ?? "");
        return Boolean(stat && stat.state === "cooling" && cooldownRemainingMs(stat, statsFetchedAt, now) > 0);
      }).length,
    [accounts, statsByAccountId, statsFetchedAt, now]
  );
  useEffect(() => {
    if (coolingCount === 0) {
      return;
    }
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [coolingCount]);

  // 密钥集合变化时重置行内草稿与拖拽态(对齐详情面板既有 effect)。
  useEffect(() => {
    setLabelDrafts({});
    setApiKeyDrafts({});
    setWeightDrafts({});
    setRevealedAccountId(undefined);
    setVisualOrder(undefined);
    setDraggingId(undefined);
    setDeletingAccountId(undefined);
  }, [accounts]);

  // 拖拽结束提交:motion 的拖拽由内部指针事件驱动,统一在 window 的
  // pointerup/pointercancel 收口——本地顺序与配置顺序不同才写一次。
  useEffect(() => {
    if (draggingId === undefined) {
      return;
    }
    const commit = () => {
      if (visualOrder) {
        const changed = visualOrder.map((account) => account.id ?? "").join("\u0000")
          !== accounts.map((account) => account.id ?? "").join("\u0000");
        if (changed) {
          onAccountsChange(visualOrder);
        }
      }
      setVisualOrder(undefined);
      setDraggingId(undefined);
    };
    window.addEventListener("pointerup", commit);
    window.addEventListener("pointercancel", commit);
    return () => {
      window.removeEventListener("pointerup", commit);
      window.removeEventListener("pointercancel", commit);
    };
    // 依赖 visualOrder:每次实时重排后重挂监听,保证闭包拿到最新顺序。
  }, [draggingId, visualOrder, accounts, onAccountsChange]);

  function updateAccountRow(accountId: string, patch: Partial<ProviderCredentialConfig>) {
    onAccountsChange(accounts.map((account) => (account.id === accountId ? { ...account, ...patch } : account)));
  }

  function removeAccountRow(accountId: string) {
    onAccountsChange(accounts.filter((account) => account.id !== accountId));
  }

  function confirmRemoveAccount() {
    const account = accounts.find((item) => item.id === deletingAccountId);
    if (account) {
      removeAccountRow(account.id ?? "");
      notify(`${t("Removed")} ${account.label || account.id}`);
    }
    setDeletingAccountId(undefined);
  }

  /** 开关:固定密钥模式下开启一把即停用其余并同步固定侧记忆(单次写入,
   *  交由 onFixedKeyPick);负载均衡自由启停。 */
  function toggleAccountRow(accountId: string) {
    const account = accounts.find((item) => item.id === accountId);
    if (!account) {
      return;
    }
    const enabling = account.enabled === false;
    if (fixedMode && enabling) {
      onFixedKeyPick(accountId);
      return;
    }
    updateAccountRow(accountId, { enabled: enabling });
  }

  function commitLabelDraft(account: ProviderCredentialConfig) {
    const accountId = account.id ?? "";
    if (!(accountId in labelDrafts)) {
      return;
    }
    const trimmed = (labelDrafts[accountId] ?? "").trim();
    // 名称可改但不能重名:与其它行冲突则回退并提示。
    const duplicate = trimmed !== ""
      && accounts.some((item) => item.id !== accountId && (item.label ?? "").trim() === trimmed);
    setLabelDrafts((current) => {
      const next = { ...current };
      delete next[accountId];
      return next;
    });
    if (duplicate) {
      notify(t("Key name already exists."));
      return;
    }
    if (trimmed !== (account.label ?? "")) {
      updateAccountRow(accountId, trimmed ? { label: trimmed } : { label: undefined });
    }
  }

  function commitApiKeyDraft(account: ProviderCredentialConfig) {
    const accountId = account.id ?? "";
    if (!(accountId in apiKeyDrafts)) {
      return;
    }
    const draft = apiKeyDrafts[accountId] ?? "";
    setApiKeyDrafts((current) => {
      const next = { ...current };
      delete next[accountId];
      return next;
    });
    // 空值提交视为取消(清密钥随删除走);有变化才写。
    const trimmed = draft.trim();
    if (trimmed && trimmed !== account.api_key) {
      updateAccountRow(accountId, { api_key: trimmed });
    }
  }

  function commitWeightDraft(account: ProviderCredentialConfig) {
    const draftKey = `${account.id ?? ""}:weight`;
    if (!(draftKey in weightDrafts)) {
      return;
    }
    const raw = (weightDrafts[draftKey] ?? "").trim();
    setWeightDrafts((current) => {
      const next = { ...current };
      delete next[draftKey];
      return next;
    });
    // 清空 = 回到后端 from_dict 缺省(weight 1)。
    const value = raw === "" ? 1 : numberValue(raw);
    if (value !== (account.weight ?? 1)) {
      updateAccountRow(account.id ?? "", { weight: value });
    }
  }

  async function clearCooldown(accountId: string) {
    if (!onClearCooldown || clearingAccountId) {
      return;
    }
    setClearingAccountId(accountId);
    try {
      await onClearCooldown(accountId);
    } finally {
      setClearingAccountId(undefined);
    }
  }

  if (accounts.length === 0) {
    return (
      <div className="rounded-md border border-dashed border-border bg-muted/20 px-3 py-5 text-center text-body text-muted-foreground">
        {t("No API keys yet. Click Add API key to add one.")}
      </div>
    );
  }

  const deletingAccount = accounts.find((account) => account.id === deletingAccountId);

  return (
    <>
    <Reorder.Group
      axis="y"
      as="div"
      className="divide-y divide-border/50 overflow-hidden rounded-md border border-border/60 bg-background/60"
      onReorder={setVisualOrder}
      values={list}
    >
      {list.map((account) => (
        <ProviderApiKeyRow
          account={account}
          clearing={clearingAccountId === account.id}
          dragging={draggingId === account.id}
          key={account.id ?? `index-${list.indexOf(account)}`}
          keyDraft={apiKeyDrafts[account.id ?? ""]}
          labelDraft={labelDrafts[account.id ?? ""]}
          onClearCooldown={() => void clearCooldown(account.id ?? "")}
          onCommitKey={() => commitApiKeyDraft(account)}
          onCommitLabel={() => commitLabelDraft(account)}
          onCommitWeight={() => commitWeightDraft(account)}
          onDragPointerDown={() => setDraggingId(account.id ?? "")}
          onKeyDraftChange={(value) => setApiKeyDrafts((current) => ({ ...current, [account.id ?? ""]: value }))}
          onLabelDraftChange={(value) => setLabelDrafts((current) => ({ ...current, [account.id ?? ""]: value }))}
          onRemove={() => setDeletingAccountId(account.id ?? "")}
          onReveal={() => setRevealedAccountId(revealedAccountId === account.id ? undefined : account.id)}
          onToggleEnabled={() => toggleAccountRow(account.id ?? "")}
          onWeightDraftChange={(value) => setWeightDrafts((current) => ({ ...current, [`${account.id ?? ""}:weight`]: value }))}
          revealed={revealedAccountId === account.id}
          showClearCooldown={Boolean(onClearCooldown)}
          stat={statsByAccountId.get(account.id ?? "")}
          statsFetchedAt={statsFetchedAt}
          now={now}
          weightDraft={weightDrafts[`${account.id ?? ""}:weight`]}
        />
      ))}
    </Reorder.Group>
    {/* 条件挂载(对齐 DeleteProviderDialog 模式):Dialog 缺 open 属性时
        挂载即弹出,故只在有待确认删除时才渲染。 */}
    {deletingAccount ? (
      <DeleteApiKeyDialog
        account={deletingAccount}
        onClose={() => setDeletingAccountId(undefined)}
        onConfirm={confirmRemoveAccount}
      />
    ) : null}
    </>
  );
}

type ProviderApiKeysProps = {
  index: number;
  notify: (message: string) => void;
  provider: GatewayProviderConfig;
  setProviderAccounts: (providerIndex: number, accounts: ProviderCredentialConfig[]) => void;
  setProviderFixedKey: (providerIndex: number, accountId: string) => void;
  /** 健康态(详情面板的 getProviderCredentialStats 查询结果,按 accountId 索引)。 */
  statsByAccountId: Map<string, ProviderCredentialAccountStat>;
  statsFetchedAt: number;
  /** 解冷却后触发面板侧 stats 重取(查询本体在详情面板)。 */
  onStatsRefetch: () => void;
};

/** 详情面板接线层:把已落库供应商的读写与健康态接到共享编辑器上。 */
export function ProviderApiKeysSection({
  index,
  notify,
  provider,
  setProviderAccounts,
  setProviderFixedKey,
  statsByAccountId,
  statsFetchedAt,
  onStatsRefetch
}: ProviderApiKeysProps) {
  const t = useAppText();
  const accounts = useMemo(() => providerCredentialAccounts(provider), [provider]);
  const fixedMode = providerKeyMode(provider.credentialStrategy) === "fixed";
  return (
    <ProviderApiKeysEditor
      accounts={accounts}
      fixedMode={fixedMode}
      notify={notify}
      onAccountsChange={(next) => setProviderAccounts(index, next)}
      onFixedKeyPick={(accountId) => setProviderFixedKey(index, accountId)}
      statsByAccountId={statsByAccountId}
      statsFetchedAt={statsFetchedAt}
      onClearCooldown={async (accountId) => {
        const providerId = provider.id || provider.name;
        if (!providerId) {
          return;
        }
        await api.clearCredentialCooldown({ accountId, providerId });
        await onStatsRefetch();
        notify(t("Cooldown cleared"));
      }}
    />
  );
}

/** 删除密钥确认对话框(2026-09-06):沿用 DeleteProviderDialog 的交互语言。 */
function DeleteApiKeyDialog({
  account,
  onClose,
  onConfirm
}: {
  account: ProviderCredentialConfig;
  onClose: () => void;
  onConfirm: () => void;
}) {
  const t = useAppText();
  return (
    <Dialog onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-[520px]">
        <DialogHeader>
          <div className="min-w-0">
            <DialogTitle>{t("Delete API key")}</DialogTitle>
          </div>
          <Button aria-label={t("Close dialog")} onClick={onClose} size="iconSm" title={t("Close")} type="button" variant="ghost">
            <X className="h-4 w-4" />
          </Button>
        </DialogHeader>

        <DialogBody>
          <div className="rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2.5">
            <div className="flex items-start gap-2 text-body font-medium text-destructive">
              <CircleAlert className="mt-0.5 h-3.5 w-3.5 shrink-0" />
              <span>{t("Delete this API key from the provider?")}</span>
            </div>
            <div className="mt-2 space-y-1 text-caption text-muted-foreground">
              <div className="truncate">
                <span className="font-medium text-foreground">{t("Key name")}:</span> {account.label || account.id || t("Not set")}
              </div>
              <div>{t("This action is applied immediately to the draft config and will auto-save with other changes.")}</div>
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

function cooldownRemainingMs(stat: ProviderCredentialAccountStat, fetchedAt: number, now: number): number {
  return Math.max(0, stat.cooldownRemainingMs - Math.max(0, now - fetchedAt));
}

type ProviderApiKeyRowProps = {
  account: ProviderCredentialConfig;
  clearing: boolean;
  dragging: boolean;
  keyDraft?: string;
  labelDraft?: string;
  now: number;
  revealed: boolean;
  /** 解冷却通道是否可用(草稿态无 stats/清除接口,不渲染该按钮)。 */
  showClearCooldown: boolean;
  stat?: ProviderCredentialAccountStat;
  statsFetchedAt: number;
  weightDraft?: string;
  onClearCooldown: () => void;
  onCommitKey: () => void;
  onCommitLabel: () => void;
  onCommitWeight: () => void;
  onDragPointerDown: () => void;
  onKeyDraftChange: (value: string) => void;
  onLabelDraftChange: (value: string) => void;
  onRemove: () => void;
  onReveal: () => void;
  onToggleEnabled: () => void;
  onWeightDraftChange: (value: string) => void;
};

/** 单行条目(2026-09-06 用户调整),从左至右:
 *  拖拽手柄 / 名称 / apikey / 权重 / 状态 / 开关 / 删除。
 *  根节点为 motion Reorder.Item:拖拽期间被拖行随指针移动,其余行弹簧
 *  让位(内置布局补间);拖拽仅由左侧手柄发起(dragListener=false)。 */
function ProviderApiKeyRow({
  account,
  clearing,
  dragging,
  keyDraft,
  labelDraft,
  now,
  revealed,
  showClearCooldown,
  stat,
  statsFetchedAt,
  weightDraft,
  onClearCooldown,
  onCommitKey,
  onCommitLabel,
  onCommitWeight,
  onDragPointerDown,
  onKeyDraftChange,
  onLabelDraftChange,
  onRemove,
  onReveal,
  onToggleEnabled,
  onWeightDraftChange
}: ProviderApiKeyRowProps) {
  const t = useAppText();
  const dragControls = useDragControls();
  const accountId = account.id ?? "";
  const enabled = account.enabled !== false;
  // 健康徽章状态:禁用取自配置(不依赖 stats 时效);冷却取自 stats。
  const cooling = Boolean(stat && stat.state === "cooling");
  const remainingMs = stat && cooling ? cooldownRemainingMs(stat, statsFetchedAt, now) : 0;
  const state = !enabled ? "disabled" : cooling && remainingMs > 0 ? "cooling" : "ok";
  const badgeLabel = state === "disabled" ? t("Disabled") : state === "cooling" ? t("Cooling down") : t("Healthy");
  const badgeTitle = [
    stat?.cooldownReason,
    stat?.lastStatusCode ? `HTTP ${stat.lastStatusCode}` : undefined
  ]
    .filter(Boolean)
    .join(" · ");

  return (
    <Reorder.Item
      as="div"
      value={account}
      className={
        "flex min-w-0 flex-wrap items-center gap-2 px-3 py-2"
        + (dragging ? " relative z-10 rounded-md bg-background/95 shadow-lg shadow-black/10" : "")
      }
      dragControls={dragControls}
      dragListener={false}
    >
      <span
        aria-label={t("Drag to reorder")}
        className="flex h-6 w-4 shrink-0 cursor-grab touch-none items-center justify-center text-muted-foreground/50 outline-none transition-colors hover:text-foreground active:cursor-grabbing"
        onPointerDown={(event) => {
          // touch-none 防止触屏滚动抢占指针;preventDefault 避免拖出文本选区。
          event.preventDefault();
          onDragPointerDown();
          dragControls.start(event);
        }}
        role="button"
        tabIndex={0}
        title={t("Drag to reorder")}
      >
        <GripVertical className="h-3.5 w-3.5" />
      </span>
      <Input
        aria-label={t("Key name")}
        className="h-7 w-[120px] shrink-0"
        onBlur={onCommitLabel}
        onChange={(event) => onLabelDraftChange(event.target.value)}
        placeholder={t("Key name")}
        spellCheck={false}
        value={labelDraft ?? account.label ?? ""}
      />
      {/* 密钥内联编辑(原 WR 方式):blur 保存,眼睛切明文,无确认按钮。 */}
      <div className="relative min-w-0 flex-1">
        <Input
          aria-label={`${t("API key")} ${account.label || accountId}`}
          className="pr-9 font-mono text-caption"
          onBlur={onCommitKey}
          onChange={(event) => onKeyDraftChange(event.target.value)}
          placeholder={t("Set key")}
          spellCheck={false}
          type={revealed ? "text" : "password"}
          value={keyDraft ?? account.api_key ?? ""}
        />
        <button
          aria-label={revealed ? t("Hide") : t("Show")}
          className="absolute right-2 top-1/2 flex h-6 w-6 -translate-y-1/2 items-center justify-center rounded text-muted-foreground outline-none transition-colors hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring/30"
          onClick={onReveal}
          title={revealed ? t("Hide") : t("Show")}
          type="button"
        >
          {revealed ? <EyeOff className="h-3.5 w-3.5" /> : <Eye className="h-3.5 w-3.5" />}
        </button>
      </div>
      <label className="flex shrink-0 items-center gap-1 text-caption text-muted-foreground">
        <span className="shrink-0">{t("Weight")}</span>
        <Input
          aria-label={`${t("Weight")} ${account.label || accountId}`}
          className="h-7 w-14"
          inputMode="numeric"
          onBlur={onCommitWeight}
          onChange={(event) => onWeightDraftChange(event.target.value)}
          value={weightDraft ?? String(account.weight ?? 1)}
        />
      </label>
      <span className="flex shrink-0 items-center gap-0.5">
        <Badge
          title={badgeTitle}
          variant={state === "ok" ? "success" : state === "cooling" ? "warning" : "outline"}
        >
          {state === "cooling" ? `${badgeLabel} ${formatCooldownRemaining(remainingMs)}` : badgeLabel}
        </Badge>
        {state === "cooling" && showClearCooldown ? (
          <Button
            aria-label={t("Clear cooldown")}
            className="h-7 w-7 p-0"
            disabled={clearing}
            onClick={onClearCooldown}
            size="iconSm"
            title={t("Clear cooldown")}
            type="button"
            variant="ghost"
          >
            <RotateCcw className="h-3.5 w-3.5" />
          </Button>
        ) : null}
      </span>
      <Toggle checked={enabled} onChange={onToggleEnabled} size="sm" title={t("Key status")} />
      <Button
        aria-label={`${t("Remove key")} ${account.label || accountId}`}
        className="h-7 w-7 shrink-0 p-0 hover:bg-destructive/20 hover:text-destructive"
        onClick={onRemove}
        size="iconSm"
        title={t("Remove key")}
        type="button"
        variant="ghost"
      >
        <Trash2 className="h-3.5 w-3.5" />
      </Button>
    </Reorder.Item>
  );
}
