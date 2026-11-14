import { useEffect, useMemo, useRef, useState, type ComponentProps } from "react";
import { Blend, Box, GripVertical, LoaderCircle, Plus, Trash2, TriangleAlert, Wand2, X } from "lucide-react";
import { Reorder, useDragControls } from "motion/react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Dialog, DialogBody, DialogContent, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";
import type { GatewayProviderConfig } from "@/contracts/app";
import {
  AnimatedIconSwap, Field, FieldLabelHint, ModelGroupConfig, ModelGroupStrategy, ModelGroupTarget, SelectControl,
  distinctProviderModelNames, isModelGroupEnabled, isModelGroupStrategy,
  modelGroupNameConflict, modelGroupTargetWeight, providersServingModel,
  useAppText
} from "../../shared/index";
import { EnabledToggle } from "./provider-detail-panel";

// 20260905_04 M6:聚合模型编辑面板(providers 页右侧)。
// 2026-09-26 临时条目二轮:两种交互模型 ——
//   新建(isEdit=false):对齐「添加供应商」面板 —— 标题「添加聚合模型」,
//     右上角 取消/添加/叉号,草稿模式,点「添加」才落库;
//   编辑(isEdit=true):对齐供应商详情页 —— 右上角 启用/已禁用 + 删除,
//     无保存/取消按钮,名称失焦提交、目标/策略等变更即时落库。
// 2026-09-26 三轮(目标行重设计):
//   名称 = 只读「WRouter」前缀框 + "/" + 可输入聚合名(调用名即 WRouter/<名>);
//   章节为「路由策略」「路由模型列表」;目标行单行化 = 拖拽手柄 + 供应商 +
//   模型(定宽) + 负载均衡权重(恒显) + 状态圆点(有效绿/失效琥珀) + 删除(最右),
//   优先级由排列顺序决定(见 hook 序列化);同供应商同模型在选项级禁用,重复
//   选不出来;添加收敛为章节头右侧「快捷添加」(同名模型跨供应商弹窗)与
//   「自定义添加」(列表底部追加未保存行,供应商+模型选齐后自动转正入草稿)。

const STRATEGY_LABELS: Record<ModelGroupStrategy, string> = {
  first: "Primary/backup",
  weighted: "Load balancing",
  sticky: "Session sticky"
};

const STRATEGY_HINTS: Record<ModelGroupStrategy, string> = {
  first: "Try targets in list order (top first); fall to the next on failure.",
  weighted: "Pick a target at random by weight across all enabled targets.",
  sticky: "Keep the last successful target for the same session; re-pick when it goes unhealthy."
};

/** 失焦/回车才提交的数字输入:编辑模式逐键落库太吵,统一 blur 提交;
 *  新建模式的 weight 也走同一时序(点「添加」前失焦会先提交)。
 *  外部 value 同步用渲染期调整而非挂载 effect:effect 的 setText 会与
 *  用户输入排队竞态(Reorder.Item 下 effect 冲刷更晚,会把刚输入的值
 *  覆盖回旧值),渲染期同步则没有这个时序窗口。 */
function DeferredNumberInput({
  onCommit,
  value,
  ...inputProps
}: {
  onCommit: (value: number) => void;
  value: number;
} & Omit<ComponentProps<typeof Input>, "onChange" | "type" | "value">) {
  const [text, setText] = useState(() => String(value));
  const [prevValue, setPrevValue] = useState(value);
  if (prevValue !== value) {
    setPrevValue(value);
    setText(String(value));
  }
  function commit() {
    const parsed = Number(text);
    if (!Number.isFinite(parsed)) {
      setText(String(value));
      return;
    }
    onCommit(parsed);
  }
  return (
    <Input
      {...inputProps}
      type="number"
      value={text}
      onBlur={commit}
      onChange={(event) => setText(event.target.value)}
      onKeyDown={(event) => {
        if (event.key === "Enter") {
          event.preventDefault();
          commit();
        }
      }}
    />
  );
}

/** 目标行失效检查(决策:不跟随删除,给状态点由用户统一清理):供应商被删
 *  或模型从供应商移除后目标在运行时会被健康门跳过,面板标黄点提示。 */
function targetAvailability(
  target: ModelGroupTarget,
  providers: GatewayProviderConfig[]
): { modelMissing: boolean; providerMissing: boolean } {
  const provider = providers.find((item) => (item.id || item.name) === target.providerId);
  if (!provider) {
    return { modelMissing: false, providerMissing: true };
  }
  return {
    modelMissing: !(provider.models ?? []).some(
      (model) => model.trim().toLowerCase() === target.model.trim().toLowerCase()
    ),
    providerMissing: false
  };
}

type GroupTargetRowProps = {
  dragging: boolean;
  models: string[];
  modelMissing: boolean;
  providerMissing: boolean;
  providerOptions: Array<{ label: string; value: string }>;
  target: ModelGroupTarget;
  /** 已被其他条目占用的 `${providerId}|${model(小写)}` 集合:同供应商同模型
   *  只允许一条,选项级禁用让重复根本选不出来(而非添加后被静默丢弃)。 */
  occupiedKeys: Set<string>;
  onDragStart: () => void;
  onModelChange: (value: string) => void;
  onProviderChange: (value: string) => void;
  onRemove: () => void;
  onWeightCommit: (weight: number) => void;
};

const GroupTargetRow = function GroupTargetRow({
  dragging,
  models,
  modelMissing,
  providerMissing,
  providerOptions,
  target,
  occupiedKeys,
  onDragStart,
  onModelChange,
  onProviderChange,
  onRemove,
  onWeightCommit
}: GroupTargetRowProps) {
  const t = useAppText();
  const dragControls = useDragControls();
  const missing = providerMissing || modelMissing;
  const missingTitle = providerMissing
    ? t("Provider has been removed.")
    : t("Model has been removed from the provider.");
  // 失效目标的现值不在选项里,注入原值让行如实显示当前指向。
  const providerSelectOptions = providerMissing
    ? [{ label: target.providerId, value: target.providerId }, ...providerOptions]
    : providerOptions;
  const modelOptions = [
    { label: t("Select model"), value: "" },
    ...(modelMissing ? [{ label: target.model, value: target.model }] : []),
    ...models.map((model) => ({
      label: model,
      value: model,
      // 自身当前值不禁(否则原生 select 会把选中项一并灰掉),只禁其他条目占用的。
      disabled: model !== target.model
        && occupiedKeys.has(`${target.providerId}|${model.trim().toLowerCase()}`)
    }))
  ];
  return (
    <Reorder.Item
      as="div"
      className={cn(
        "flex items-center gap-1.5 rounded-md border border-border bg-background px-1.5 py-1",
        dragging && "relative z-10 shadow-lg shadow-black/10"
      )}
      dragControls={dragControls}
      dragListener={false}
      value={target}
    >
      {/* 拖拽手柄与侧栏条目同款:Reorder 手柄发起,不抢行内选择器交互。 */}
      <span
        aria-label={t("Drag to reorder")}
        className="flex h-6 w-4 shrink-0 cursor-grab touch-none items-center justify-center text-muted-foreground/50 outline-none transition-colors hover:text-foreground active:cursor-grabbing"
        onPointerDown={(event) => {
          // touch-none 防止触屏滚动抢占指针;preventDefault 避免拖出文本选区。
          event.preventDefault();
          onDragStart();
          dragControls.start(event);
        }}
        role="button"
        tabIndex={0}
        title={t("Drag to reorder")}
      >
        <GripVertical className="h-3.5 w-3.5" />
      </span>
      {/* SelectControl 的 className 落在内层 select(自身 w-full),flex/定宽尺寸必须套外层包装 div。 */}
      <div className="w-[180px] min-w-0 shrink">
        <SelectControl
          aria-label={t("Provider")}
          onChange={onProviderChange}
          options={providerSelectOptions}
          value={target.providerId}
        />
      </div>
      <div className="w-[240px] shrink-0">
        <SelectControl
          aria-label={t("Model")}
          onChange={onModelChange}
          options={modelOptions}
          value={target.model}
        />
      </div>
      <span className="shrink-0 whitespace-nowrap text-caption text-muted-foreground">
        {t("Load balancing weight")}
      </span>
      <DeferredNumberInput
        aria-label={t("Load balancing weight")}
        className="h-6 w-16 shrink-0 text-caption"
        min={0}
        onCommit={onWeightCommit}
        step={1}
        value={modelGroupTargetWeight(target)}
      />
      {/* 状态圆点(供应商条目同款):有效=绿,供应商/模型已删=琥珀警示;ml-auto 吸收左侧弹性,圆点+删除恒贴行尾。 */}
      {missing ? (
        <span
          aria-label={missingTitle}
          className="ml-auto h-2 w-2 shrink-0 rounded-full bg-amber-500 shadow-[0_0_0_3px_rgba(245,158,11,0.16)]"
          title={missingTitle}
        />
      ) : (
        <span
          aria-label={t("Model available.")}
          className="ml-auto h-2 w-2 shrink-0 rounded-full bg-emerald-500 shadow-[0_0_0_3px_rgba(16,185,129,0.16)]"
          title={t("Model available.")}
        />
      )}
      <Button
        aria-label={t("Remove target")}
        className="h-6 w-6 shrink-0 p-0 text-muted-foreground hover:text-destructive"
        onClick={onRemove}
        size="iconSm"
        title={t("Remove target")}
        type="button"
        variant="ghost"
      >
        <Trash2 className="h-3 w-3" />
      </Button>
    </Reorder.Item>
  );
};

export function ModelGroupPanel({
  draft,
  error,
  groups,
  isEdit,
  onChange,
  onClose,
  onDelete,
  onQuickAdd,
  onAddTarget,
  onRemoveTarget,
  onReorderTargets,
  onSubmit,
  onToggleEnabled,
  onUpdateTarget,
  providers
}: {
  draft: ModelGroupConfig;
  error: string;
  groups: ModelGroupConfig[];
  /** true = 编辑既有组(显示删除);false = 新建。 */
  isEdit: boolean;
  onChange: (patch: Partial<ModelGroupConfig>) => void;
  onClose: () => void;
  onDelete: () => void;
  onQuickAdd: (pairs: Array<{ model: string; providerId: string }>) => void;
  onAddTarget: (providerId: string, model: string) => void;
  onRemoveTarget: (index: number) => void;
  /** 拖拽排序整序提交(拖拽期间只在本地顺序态实时重排,pointerup 收口)。 */
  onReorderTargets: (next: ModelGroupTarget[]) => void;
  onSubmit: () => Promise<boolean>;
  onToggleEnabled: (enabled: boolean) => void;
  onUpdateTarget: (index: number, patch: Partial<ModelGroupTarget>) => void;
  providers: GatewayProviderConfig[];
}) {
  const t = useAppText();
  const [busy, setBusy] = useState(false);
  const [quickAddOpen, setQuickAddOpen] = useState(false);
  const [quickModel, setQuickModel] = useState("");
  // 快捷添加勾选集:默认全选提供该模型的供应商,用户可只勾选部分添加。
  const [quickSelected, setQuickSelected] = useState<Set<string>>(new Set());
  // 自定义添加的未保存行:只存在于面板本地,供应商+模型选齐才真正入草稿
  // (配置里永远不出现半填目标,后端/持久化口径不变)。
  const [pendingTargets, setPendingTargets] = useState<Array<{ model: string; providerId: string }>>([]);
  // 拖拽期间本地顺序态(对齐侧栏拖拽:配置零写入,pointerup 一次性提交)。
  const [targetOrder, setTargetOrder] = useState<ModelGroupTarget[]>();
  const [draggingTarget, setDraggingTarget] = useState<ModelGroupTarget>();
  // 名称本地态:新建模式逐键同步进草稿;编辑模式失焦/回车才提交(即时落库)。
  // 实体切换(draft.id 变化)在渲染期同步 —— 不放进 effect,避免测试/并发
  // 环境下挂载 effect 推迟执行把未提交输入复位。
  const [nameDraft, setNameDraft] = useState(draft.name);
  const [nameDraftOwnerId, setNameDraftOwnerId] = useState(draft.id);
  if (nameDraftOwnerId !== draft.id) {
    setNameDraftOwnerId(draft.id);
    setNameDraft(draft.name);
  }

  useEffect(() => {
    setBusy(false);
    setQuickAddOpen(false);
    setQuickModel("");
    setQuickSelected(new Set());
    setPendingTargets([]);
    setTargetOrder(undefined);
    setDraggingTarget(undefined);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [draft.id]);

  // 拖拽结束提交:window pointerup/cancel 收口,顺序真变了才写一次。
  useEffect(() => {
    if (!draggingTarget) {
      return;
    }
    const commit = () => {
      if (targetOrder) {
        const changed = targetOrder.length !== draft.targets.length
          || targetOrder.some((target, index) => target !== draft.targets[index]);
        if (changed) {
          onReorderTargets(targetOrder);
        }
      }
      setTargetOrder(undefined);
      setDraggingTarget(undefined);
    };
    window.addEventListener("pointerup", commit);
    window.addEventListener("pointercancel", commit);
    return () => {
      window.removeEventListener("pointerup", commit);
      window.removeEventListener("pointercancel", commit);
    };
    // 依赖随每次实时重排变化:重挂监听保证闭包拿到最新顺序(对齐侧栏列表)。
  }, [draggingTarget, draft.targets, onReorderTargets, targetOrder]);

  // 冲突提示跟随输入框显示值(编辑模式未提交的输入也能实时预览)。
  const conflict = useMemo(
    () => modelGroupNameConflict(nameDraft, groups, draft.id),
    [draft.id, nameDraft, groups]
  );
  const modelNames = useMemo(() => distinctProviderModelNames(providers), [providers]);
  const quickProviders = useMemo(
    () => providersServingModel(providers, quickModel),
    [providers, quickModel]
  );
  const strategy = isModelGroupStrategy(draft.strategy) ? draft.strategy : "first";

  const providerOptions = useMemo(
    () =>
      providers.map((provider) => ({
        label: provider.name || provider.id || t("Unnamed"),
        value: provider.id || provider.name || ""
      })),
    [providers, t]
  );
  // 已占用 (providerId|模型名小写) 集合:正式条目 + 未保存行都算占用,
  // 模型选择框把占用项禁用,重复根本选不出来。
  const occupiedKeys = useMemo(() => {
    const keys = new Set<string>();
    for (const target of draft.targets) {
      if (target.providerId && target.model) {
        keys.add(`${target.providerId}|${target.model.trim().toLowerCase()}`);
      }
    }
    for (const pending of pendingTargets) {
      if (pending.providerId && pending.model) {
        keys.add(`${pending.providerId}|${pending.model.trim().toLowerCase()}`);
      }
    }
    return keys;
  }, [draft.targets, pendingTargets]);
  const weightTotal = useMemo(
    () => draft.targets.reduce((sum, target) => sum + modelGroupTargetWeight(target), 0),
    [draft.targets]
  );

  // 拖拽期间按本地顺序渲染;平时即草稿顺序。
  const targets = targetOrder ?? draft.targets;

  // Reorder.Item 的稳定 key(目标对象无 id,WeakMap 按对象身份发号)。
  const targetKeyStore = useRef(new WeakMap<ModelGroupTarget, string>());
  const targetKeySeq = useRef(0);
  function targetKey(target: ModelGroupTarget): string {
    const existing = targetKeyStore.current.get(target);
    if (existing) {
      return existing;
    }
    const key = `target-${++targetKeySeq.current}`;
    targetKeyStore.current.set(target, key);
    return key;
  }

  function modelsForProvider(providerId: string): string[] {
    const provider = providers.find((item) => (item.id || item.name) === providerId);
    return provider?.models ?? [];
  }

  function addPendingTarget() {
    setPendingTargets((current) => [...current, { model: "", providerId: "" }]);
  }

  function updatePendingTarget(index: number, patch: { model?: string; providerId?: string }) {
    const next = pendingTargets.map((item, position) => (position === index ? { ...item, ...patch } : item));
    const completed = next.filter((item) => item.providerId && item.model);
    for (const item of completed) {
      // 入草稿(hook 侧按 providerId+模型名幂等去重)。
      onAddTarget(item.providerId, item.model);
    }
    setPendingTargets(next.filter((item) => !(item.providerId && item.model)));
  }

  function removePendingTarget(index: number) {
    setPendingTargets((current) => current.filter((_, position) => position !== index));
  }

  /** 快捷添加:选择模型时默认全选提供方;勾选集决定添加哪些供应商。 */
  function pickQuickModel(model: string) {
    setQuickModel(model);
    setQuickSelected(
      new Set(
        providersServingModel(providers, model).map((provider) => provider.id || provider.name || "")
      )
    );
  }

  function toggleQuickProvider(providerKey: string, checked: boolean) {
    setQuickSelected((current) => {
      const next = new Set(current);
      if (checked) {
        next.add(providerKey);
      } else {
        next.delete(providerKey);
      }
      return next;
    });
  }

  function confirmQuickAdd() {
    onQuickAdd(
      quickProviders
        .filter((provider) => quickSelected.has(provider.id || provider.name || ""))
        .map((provider) => ({
          model:
            (provider.models ?? []).find(
              (candidate) => candidate.trim().toLowerCase() === quickModel.trim().toLowerCase()
            ) ?? quickModel,
          providerId: provider.id || provider.name || ""
        }))
    );
    setQuickModel("");
    setQuickSelected(new Set());
    setQuickAddOpen(false);
  }

  async function submit() {
    if (busy) {
      return;
    }
    setBusy(true);
    try {
      await onSubmit();
    } finally {
      setBusy(false);
    }
  }

  /** 编辑模式:名称失焦/回车提交(hook 侧校验,非法名不落库)。 */
  function commitName() {
    if (!isEdit || nameDraft === draft.name) {
      return;
    }
    onChange({ name: nameDraft });
  }

  return (
    <div className="flex h-full min-h-0 min-w-0 flex-col">
      {/* 头部行内层与下方内容列同 max-w 居中(对齐详情面板 860px 范式),
          全屏时标题/开关/按钮向中间靠拢而不是拉伸到面板两端。 */}
      <div className="h-12 shrink-0 border-b border-border/60 px-4">
        <div className="mx-auto flex h-full w-full max-w-[860px] items-center gap-2">
          <div className="flex min-w-0 flex-1 items-center gap-2">
            {/* 头部图标用条目族 Blend(双圆混合,与侧栏行一致),与页面族 Layers3 区分。 */}
            <span className="flex h-7 w-7 shrink-0 items-center justify-center rounded-md bg-primary/10 text-primary">
              <Blend aria-hidden="true" className="h-4 w-4" />
            </span>
            <span className="truncate text-body-lg font-semibold">
              {/* 对外调用名即 wrouter/<聚合名>(大小写不敏感):标题直接展示统一调用前缀。 */}
              {isEdit ? (draft.name.trim() ? `WRouter/${draft.name}` : t("Model group")) : t("Add model group")}
            </span>
            <Badge className="shrink-0" variant="secondary">{t("Aggregate model")}</Badge>
          </div>
          {isEdit ? (
            <>
              <EnabledToggle
                ariaLabel="Status"
                disableTitle="Disable"
                enabled={isModelGroupEnabled(draft)}
                enableTitle="Enable"
                onToggle={() => onToggleEnabled(!isModelGroupEnabled(draft))}
              />
              <Button
                aria-label={t("Delete model group")}
                className="bg-muted/50 text-foreground/85 hover:bg-destructive/20 hover:text-destructive"
                onClick={onDelete}
                size="iconSm"
                title={t("Delete model group")}
                type="button"
                variant="ghost"
              >
                <Trash2 className="h-3.5 w-3.5" />
              </Button>
            </>
          ) : (
            <>
              <Button className="h-7 px-2 text-caption" disabled={busy} onClick={onClose} type="button" variant="outline">
                {t("Cancel")}
              </Button>
              <Button className="h-7 px-2 text-caption" disabled={busy} onClick={() => void submit()} type="button">
                <AnimatedIconSwap iconKey={busy ? "loading" : "idle"}>
                  {busy ? <LoaderCircle className="h-3.5 w-3.5 animate-spin" /> : null}
                </AnimatedIconSwap>
                {t("Add")}
              </Button>
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
            </>
          )}
        </div>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto p-4">
        {/* 内容列与供应商详情面板同宽(860px),两个详情页内容区左右边缘对齐。 */}
        <div className="mx-auto max-w-[860px] space-y-4">
          {error ? (
            <div className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-body text-destructive" role="alert">
              {error}
            </div>
          ) : null}

          <Field
            label={t("Group name")}
            labelHint={
              <FieldLabelHint>
                {t("Clients call this aggregate as WRouter/<name>; it resolves to the target models below.")}
              </FieldLabelHint>
            }
          >
            <div className="flex items-center gap-2">
              {/* 调用名前缀只读展示:WRouter 命名空间不可改,用户只填聚合名。 */}
              <Input
                aria-hidden="true"
                className="w-24 shrink-0 select-none bg-muted/50 text-center text-muted-foreground"
                readOnly
                tabIndex={-1}
                value="WRouter"
              />
              <span aria-hidden="true" className="shrink-0 text-body text-muted-foreground">/</span>
              <Input
                aria-label={t("Group name")}
                className="min-w-0 flex-1"
                onBlur={commitName}
                onChange={(event) => {
                  const value = event.target.value;
                  setNameDraft(value);
                  if (!isEdit) {
                    onChange({ name: value });
                  }
                }}
                onKeyDown={(event) => {
                  if (event.key === "Enter") {
                    event.preventDefault();
                    commitName();
                  }
                }}
                placeholder={t("e.g. smart-router")}
                value={nameDraft}
              />
            </div>
            {conflict ? (
              <p className="flex items-center gap-1 text-caption text-destructive">
                <TriangleAlert aria-hidden="true" className="h-3.5 w-3.5 shrink-0" />
                <span>{t("Model group name already exists.")}</span>
              </p>
            ) : null}
          </Field>

          <Field
            label={t("Routing strategy")}
            labelHint={<FieldLabelHint>{t(STRATEGY_HINTS[strategy])}</FieldLabelHint>}
          >
            <SelectControl
              aria-label={t("Routing strategy")}
              onChange={(value) => onChange({ strategy: value as ModelGroupStrategy })}
              options={[
                { label: t(STRATEGY_LABELS.first), value: "first" },
                { label: t(STRATEGY_LABELS.weighted), value: "weighted" },
                { label: t(STRATEGY_LABELS.sticky), value: "sticky" }
              ]}
              value={strategy}
            />
          </Field>

          <Field
            action={
              <div className="flex items-center gap-1.5">
                <Button
                  className="h-7 gap-1 px-2 text-caption"
                  onClick={() => setQuickAddOpen(true)}
                  size="sm"
                  type="button"
                  variant="outline"
                >
                  <Wand2 className="h-3.5 w-3.5" />
                  {t("Quick add")}
                </Button>
                <Button
                  className="h-7 gap-1 px-2 text-caption"
                  onClick={addPendingTarget}
                  size="sm"
                  type="button"
                  variant="outline"
                >
                  <Plus className="h-3.5 w-3.5" />
                  {t("Custom add")}
                </Button>
              </div>
            }
            label={t("Target model pool")}
            labelHint={
              <FieldLabelHint>
                {t("Priority follows list order: the higher up, the earlier it is tried.")}
              </FieldLabelHint>
            }
          >
            <div className="space-y-1.5 rounded-md border border-border bg-muted/20 p-2">
              {targets.length === 0 && pendingTargets.length === 0 ? (
                <div className="px-1 py-4 text-center text-body text-muted-foreground">
                  {t("No target models yet")}
                </div>
              ) : null}
              <Reorder.Group
                axis="y"
                as="div"
                className="space-y-1.5"
                onReorder={setTargetOrder}
                values={targets}
              >
                {targets.map((target, index) => {
                  const availability = targetAvailability(target, providers);
                  return (
                    <GroupTargetRow
                      dragging={draggingTarget === target}
                      key={targetKey(target)}
                      models={modelsForProvider(target.providerId)}
                      modelMissing={availability.modelMissing}
                      occupiedKeys={occupiedKeys}
                      providerMissing={availability.providerMissing}
                      providerOptions={providerOptions}
                      target={target}
                      onDragStart={() => setDraggingTarget(target)}
                      onModelChange={(value) => onUpdateTarget(index, { model: value })}
                      onProviderChange={(value) => onUpdateTarget(index, { providerId: value, model: "" })}
                      onRemove={() => onRemoveTarget(index)}
                      onWeightCommit={(weight) => onUpdateTarget(index, {
                        weight: Number.isFinite(weight) && weight >= 0 ? weight : 0
                      })}
                    />
                  );
                })}
              </Reorder.Group>
              {/* 自定义添加的未保存行:虚线边框区分,选齐供应商+模型即转正。 */}
              {pendingTargets.map((pending, index) => (
                <div
                  className="flex items-center gap-1.5 rounded-md border border-dashed border-border bg-background px-1.5 py-1"
                  key={`pending-${index}`}
                >
                  <div className="w-[180px] min-w-0 shrink">
                    <SelectControl
                      aria-label={t("Provider")}
                      onChange={(value) => updatePendingTarget(index, { providerId: value, model: "" })}
                      options={providerOptions}
                      value={pending.providerId}
                    />
                  </div>
                  <div className="w-[240px] shrink-0">
                    <SelectControl
                      aria-label={t("Model")}
                      disabled={!pending.providerId}
                      onChange={(value) => updatePendingTarget(index, { model: value })}
                      options={[
                        { label: t("Select model"), value: "" },
                        ...modelsForProvider(pending.providerId).map((model) => ({
                          label: model,
                          value: model,
                          disabled: occupiedKeys.has(`${pending.providerId}|${model.trim().toLowerCase()}`)
                        }))
                      ]}
                      value={pending.model}
                    />
                  </div>
                  <Button
                    aria-label={t("Remove target")}
                    className="ml-auto h-6 w-6 shrink-0 p-0 text-muted-foreground hover:text-destructive"
                    onClick={() => removePendingTarget(index)}
                    size="iconSm"
                    title={t("Remove target")}
                    type="button"
                    variant="ghost"
                  >
                    <Trash2 className="h-3 w-3" />
                  </Button>
                </div>
              ))}
              {strategy === "weighted" && draft.targets.length > 0 ? (
                <div className="flex items-center gap-2 border-t border-border/60 pt-2 text-caption text-muted-foreground">
                  <span>{t("Total weight")}:</span>
                  <Badge className="tabular-nums" variant="secondary">{weightTotal}</Badge>
                </div>
              ) : null}
            </div>
          </Field>
        </div>
      </div>

      {/* 快捷添加弹窗:同名模型跨供应商,勾选部分提供方一次带入
          (条件挂载,Radix Dialog 无 open 即弹)。 */}
      {quickAddOpen ? (
        <Dialog onOpenChange={(open) => !open && setQuickAddOpen(false)}>
          <DialogContent className="max-w-[520px]">
            <DialogHeader>
              <div className="min-w-0">
                <DialogTitle>{t("Quick add target models")}</DialogTitle>
              </div>
              <Button
                aria-label={t("Close dialog")}
                onClick={() => setQuickAddOpen(false)}
                size="iconSm"
                title={t("Close")}
                type="button"
                variant="ghost"
              >
                <X className="h-4 w-4" />
              </Button>
            </DialogHeader>
            <DialogBody>
              {/* 功能说明常驻在「模型」章节上方,不随选择状态消失。 */}
              <p className="text-caption text-muted-foreground">
                {t("Pick a model name and check the providers to add; one target will be added per checked provider.")}
              </p>
              <Field label={t("Model")}>
                <SelectControl
                  aria-label={t("Select model")}
                  onChange={pickQuickModel}
                  options={[
                    { label: t("Select model"), value: "" },
                    ...modelNames.map((model) => ({ label: model, value: model }))
                  ]}
                  value={quickModel}
                />
              </Field>
              {quickModel ? (
                quickProviders.length > 0 ? (
                  <div className="space-y-2">
                    <div className="flex items-center justify-between">
                      <span className="text-caption font-medium uppercase tracking-wide text-muted-foreground">
                        {t("Providers")}
                      </span>
                      <div className="flex items-center gap-1">
                        <Button
                          className="h-6 px-1.5 text-caption"
                          onClick={() => setQuickSelected(new Set(quickProviders.map((provider) => provider.id || provider.name || "")))}
                          size="sm"
                          type="button"
                          variant="ghost"
                        >
                          {t("Select all")}
                        </Button>
                        <Button
                          className="h-6 px-1.5 text-caption"
                          onClick={() => setQuickSelected(new Set())}
                          size="sm"
                          type="button"
                          variant="ghost"
                        >
                          {t("Clear all")}
                        </Button>
                      </div>
                    </div>
                    <div className="space-y-1">
                      {quickProviders.map((provider) => {
                        const providerKey = provider.id || provider.name || "";
                        return (
                          <label
                            className="flex cursor-pointer items-center gap-2 rounded-md border border-border bg-background px-2 py-1.5"
                            key={provider.id || provider.name}
                          >
                            <Checkbox
                              checked={quickSelected.has(providerKey)}
                              onCheckedChange={(checked) => toggleQuickProvider(providerKey, checked)}
                            />
                            <Box aria-hidden="true" className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
                            <span className="min-w-0 flex-1 truncate text-body">
                              {provider.name || provider.id || t("Unnamed")}
                            </span>
                          </label>
                        );
                      })}
                    </div>
                  </div>
                ) : (
                  <p className="text-caption text-muted-foreground">{t("No provider serves this model.")}</p>
                )
              ) : null}
            </DialogBody>
            <DialogFooter>
              <Button onClick={() => setQuickAddOpen(false)} type="button" variant="outline">
                {t("Cancel")}
              </Button>
              <Button disabled={quickSelected.size === 0} onClick={confirmQuickAdd} type="button">
                <Wand2 className="h-4 w-4" />
                {t("Add")}
              </Button>
            </DialogFooter>
          </DialogContent>
        </Dialog>
      ) : null}
    </div>
  );
}
