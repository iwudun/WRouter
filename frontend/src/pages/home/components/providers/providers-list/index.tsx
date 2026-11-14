import { useEffect, useMemo, useState } from "react";
import { motion, Reorder } from "motion/react";
import { Blend, Box, CircleAlert, Layers3, Plus, Search, Server, Trash2, X } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Dialog, DialogBody, DialogContent, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import type { GatewayProviderConfig, ProviderModelInputType } from "@/contracts/app";
import {
  AddProviderDraft, GatewayProviderProtocol, GatewayProviderProbeResult, ModelGroupConfig,
  ProviderConnectivityCheckReport, ProviderCredentialConfig, ProviderKeyMode,
  ProviderClientFingerprintOverride, ProviderProxyOverride, ProviderUrlMode, ProxyPreset,
  ProxyUpstreamConfig, fetchProviderModelsForProvider, globalProxySummary,
  providerMatchesQuery, providerModelInputTypes, providerProxyOverride, providerProxySummary,
  resolvedProxyIconMode, useAppText,
} from "../../../shared/index";
import { ProviderDetailPanel } from "../provider-detail-panel";
import { ProviderDraftPanel } from "../provider-draft-panel";
import { ModelFormDialog, type ModelFormFields } from "../provider-model-form";
import { ModelGroupPanel } from "../model-group-panel";
import {
  ModelGroupListRow, ProviderListRow, SidebarSectionHeader, TemporaryListRow,
  type ModelGroupDraftView,
} from "./rows";

export { providerListRowEqual } from "./rows";
export type { ModelGroupDraftView } from "./rows";

// 20261107_01 W5-6:侧栏行与比较函数移入 `rows.tsx`,本文件只保留视图骨架。

export function ProvidersView({
  addModel, addModels, addProvider, checkProviderConnectivity, closeProviderDraft, draft, notify, onCheckProviderDraft,
  onUpdateProviderDraft, providerTemporary, providers, providerConnectivityLoading, providerConnectivityProbe, providerCreatedIndex,
  providerDraftError, removeModel, removeProvider, onReorderProviders, onReorderModelGroups,
  setProviderBaseUrl, setProviderCreatedIndex, setProviderCredentialMode,
  setProviderClientFingerprint,
  setProviderHeaders,
  setProviderUrlMode,
  setProviderEnabled, setProviderFixedKey, migrateProviderLegacyKey, setProviderAccounts,
  setProviderProtocol, setProviderProxy, setProviderName, submitProviderDraft, updateModel, canSubmitProvider,
  onOpenProxySettings, proxyPresets, proxyUpstream,
  modelGroups, modelGroupDraft, temporaryModelGroupDraft, modelGroupError, modelGroupCreatedId, setModelGroupCreatedId,
  openAddModelGroup, selectTemporaryModelGroup, openEditModelGroup,
  closeModelGroupDraft, submitModelGroupDraft, updateModelGroupDraft, updateModelGroupTarget, addModelGroupTarget,
  addModelGroupTargetsForModelName, removeModelGroupTarget, reorderModelGroupTargets,
  setModelGroupEnabled, removeModelGroup
}: {
  addModel: (providerIndex: number, name: string, fields: ModelFormFields) => void;
  /** 20261017_01:批量添加(获取模型面板多选),单次落库。 */
  addModels: (providerIndex: number, names: string[], fields: ModelFormFields) => void;
  addProvider: () => void;
  canSubmitProvider: boolean;
  checkProviderConnectivity: (providerIndex: number, models?: string[]) => Promise<ProviderConnectivityCheckReport>;
  closeProviderDraft: () => void;
  draft: AddProviderDraft;
  notify: (message: string) => void;
  onCheckProviderDraft: (models?: string[]) => Promise<ProviderConnectivityCheckReport>;
  onUpdateProviderDraft: (patch: Partial<AddProviderDraft>, resetProbe?: boolean) => void;
  providerTemporary: boolean;
  providers: Array<{ provider: GatewayProviderConfig; index: number }>;
  providerConnectivityLoading: boolean;
  providerConnectivityProbe?: GatewayProviderProbeResult;
  providerCreatedIndex?: number;
  providerDraftError: string;
  removeModel: (providerIndex: number, model: string) => void;
  removeProvider: (index: number) => void;
  /** 侧栏供应商拖拽排序(2026-09-13):拖完一次性提交整序。 */
  onReorderProviders: (next: GatewayProviderConfig[]) => void;
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
  setProviderCreatedIndex: (index: number | undefined) => void;
  setProviderCredentialMode: (providerIndex: number, mode: ProviderKeyMode) => void;
  setProviderEnabled: (providerIndex: number, enabled: boolean) => void;
  setProviderFixedKey: (providerIndex: number, accountId: string) => void;
  migrateProviderLegacyKey: (providerIndex: number) => void;
  setProviderAccounts: (providerIndex: number, accounts: ProviderCredentialConfig[]) => void;
  setProviderName: (providerIndex: number, name: string) => void;
  setProviderProtocol: (providerIndex: number, protocol: GatewayProviderProtocol) => void;
  /** 20260906_01 §4.9b:供应商级网络代理(undefined = 跟随全局默认)。 */
  setProviderProxy: (providerIndex: number, proxy: ProviderProxyOverride | undefined) => void;
  /** 20260906_01 验收 #5:预设列表为空时跳转设置页代理分区。 */
  onOpenProxySettings: () => void;
  proxyPresets: ProxyPreset[];
  proxyUpstream: ProxyUpstreamConfig;
  submitProviderDraft: () => Promise<boolean>;
  updateModel: (providerIndex: number, model: string, patch: ModelFormFields & { name?: string }) => void;
  // ---- 聚合模型(20260905_04 M6)----
  modelGroups: ModelGroupConfig[];
  modelGroupDraft: ModelGroupDraftView;
  temporaryModelGroupDraft?: ModelGroupConfig;
  modelGroupError: string;
  modelGroupCreatedId?: string;
  setModelGroupCreatedId: (id: string | undefined) => void;
  openAddModelGroup: () => void;
  selectTemporaryModelGroup: () => void;
  openEditModelGroup: (groupId: string) => void;
  closeModelGroupDraft: () => void;
  submitModelGroupDraft: () => Promise<boolean>;
  updateModelGroupDraft: (patch: Partial<ModelGroupConfig>) => void;
  updateModelGroupTarget: (index: number, patch: Partial<ModelGroupConfig["targets"][number]>) => void;
  addModelGroupTarget: (providerId: string, model: string) => void;
  addModelGroupTargetsForModelName: (pairs: Array<{ model: string; providerId: string }>) => void;
  removeModelGroupTarget: (index: number) => void;
  /** 目标行拖拽排序(2026-09-26):整序替换,顺序即优先级。 */
  reorderModelGroupTargets: (next: ModelGroupConfig["targets"]) => void;
  setModelGroupEnabled: (groupId: string, enabled: boolean) => void;
  /** 侧栏聚合模型拖拽排序(2026-09-13):拖完一次性提交整序。 */
  onReorderModelGroups: (next: ModelGroupConfig[]) => void;
  removeModelGroup: (groupId: string) => Promise<boolean>;
}) {
  const t = useAppText();
  const [query, setQuery] = useState("");
  const [selection, setSelection] = useState<
    { kind: "group-new" } | { groupId: string; kind: "group" } | { kind: "provider-new" } | { index: number; kind: "provider" } | undefined
  >(undefined);
  const [addModelOpen, setAddModelOpen] = useState(false);
  const [groupDeleteId, setGroupDeleteId] = useState<string>();
  const normalizedQuery = query.trim().toLowerCase();
  // 侧栏拖拽排序(2026-09-13,API 密钥条目同款机制):拖拽期间只在本地
  // 顺序态实时重排(配置零写入),pointerup 一次性提交;搜索过滤时隐藏
  // 手柄(过滤视图内拖拽的落位映射有歧义,不做)。
  const [providerOrder, setProviderOrder] = useState<GatewayProviderConfig[]>();
  const [draggingProvider, setDraggingProvider] = useState<GatewayProviderConfig>();
  const [groupOrder, setGroupOrder] = useState<ModelGroupConfig[]>();
  const [draggingGroup, setDraggingGroup] = useState<ModelGroupConfig>();
  const queryActive = normalizedQuery !== "";
  const temporaryProviderVisible = providerTemporary
    && (!normalizedQuery || t("Add provider").toLowerCase().includes(normalizedQuery));
  const temporaryGroupVisible = Boolean(temporaryModelGroupDraft)
    && (!normalizedQuery || t("Add model group").toLowerCase().includes(normalizedQuery));
  const providerItems = useMemo(() => providers.map(({ provider }) => provider), [providers]);
  const providerList = providerOrder ?? providerItems;
  const groupList = groupOrder ?? modelGroups;
  // 拖拽期间行仍按原始 index 高亮/选中(视觉重排不改 index,避免选中态跳动)。
  const providerIndexById = useMemo(() => {
    const map = new Map<GatewayProviderConfig, number>();
    providerItems.forEach((provider, index) => {
      if (!map.has(provider)) {
        map.set(provider, index);
      }
    });
    return map;
  }, [providerItems]);
  const visibleProviders = useMemo(
    () => providerList
      .map((provider) => ({ provider, index: providerIndexById.get(provider) ?? -1 }))
      .filter(({ provider, index }) => index >= 0 && providerMatchesQuery(provider, normalizedQuery)),
    [normalizedQuery, providerIndexById, providerList]
  );
  const visibleGroups = useMemo(
    () =>
      groupList.filter(
        (group) =>
          !normalizedQuery ||
          group.name.toLowerCase().includes(normalizedQuery) ||
          group.targets.some((target) => target.model.toLowerCase().includes(normalizedQuery))
      ),
    [groupList, normalizedQuery]
  );

  function commitProviderOrder(next: GatewayProviderConfig[]) {
    const changed = next.length !== providerItems.length
      || next.some((provider, position) => provider !== providerItems[position]);
    if (!changed) {
      return;
    }
    // 选中是 index 基:重排后把选中指向同一供应商的新位置。
    if (selection?.kind === "provider") {
      const current = providers[selection.index]?.provider;
      const newIndex = next.indexOf(current);
      if (current && newIndex >= 0 && newIndex !== selection.index) {
        setSelection({ index: newIndex, kind: "provider" });
      }
    }
    onReorderProviders(next);
  }

  function commitGroupOrder(next: ModelGroupConfig[]) {
    const changed = next.length !== modelGroups.length
      || next.some((group, position) => group !== modelGroups[position]);
    if (changed) {
      onReorderModelGroups(next);
    }
  }

  // 拖拽结束提交(motion 拖拽由内部指针驱动):window pointerup/cancel 收口,
  // 本地顺序与配置顺序不同才写一次;拖回原位不产生写入。
  useEffect(() => {
    if (!draggingProvider && !draggingGroup) {
      return;
    }
    const commit = () => {
      if (providerOrder) {
        commitProviderOrder(providerOrder);
      }
      if (groupOrder) {
        commitGroupOrder(groupOrder);
      }
      setProviderOrder(undefined);
      setDraggingProvider(undefined);
      setGroupOrder(undefined);
      setDraggingGroup(undefined);
    };
    window.addEventListener("pointerup", commit);
    window.addEventListener("pointercancel", commit);
    return () => {
      window.removeEventListener("pointerup", commit);
      window.removeEventListener("pointercancel", commit);
    };
    // 依赖随每次实时重排变化:重挂监听保证闭包拿到最新顺序(对齐 API 密钥列表)。
  }, [draggingProvider, draggingGroup, providerOrder, groupOrder]);
  // Auto-select the first provider when the list changes and the current
  // selection is gone (e.g. after a delete) or never set.
  useEffect(() => {
    if (providers.length === 0) {
      setSelection((current) => {
        if (current === undefined && providerTemporary) {
          return { kind: "provider-new" };
        }
        return current?.kind === "provider" && !providerTemporary ? undefined : current;
      });
      return;
    }
    setSelection((current) => {
      if (current === undefined) {
        return providerTemporary ? { kind: "provider-new" } : { index: 0, kind: "provider" };
      }
      if (current.kind === "provider" && !providers[current.index]) {
        return { index: 0, kind: "provider" };
      }
      if (current.kind === "provider-new" && !providerTemporary) {
        return { index: 0, kind: "provider" };
      }
      return current;
    });
  }, [providerTemporary, providers]);

  // 供应商新增草稿关闭后清掉临时选中;正式条目点击不会触碰草稿。
  useEffect(() => {
    if (!providerTemporary && selection?.kind === "provider-new") {
      setSelection(providers.length > 0 ? { index: 0, kind: "provider" } : undefined);
    }
  }, [providerTemporary, providers.length, selection]);

  // 组被删除/新建取消后清掉失效选中。
  useEffect(() => {
    setSelection((current) => {
      if (current?.kind === "group" && !modelGroups.some((group) => group.id === current.groupId)) {
        return providers.length > 0 ? { index: 0, kind: "provider" } : undefined;
      }
      if (current?.kind === "group" && (!modelGroupDraft.draft || modelGroupDraft.editId !== current.groupId)) {
        return providers.length > 0 ? { index: 0, kind: "provider" } : undefined;
      }
      if (current?.kind === "group-new" && !temporaryModelGroupDraft && !modelGroupCreatedId) {
        return providers.length > 0 ? { index: 0, kind: "provider" } : undefined;
      }
      return current;
    });
  }, [modelGroupCreatedId, modelGroupDraft.draft, modelGroupDraft.editId, modelGroups, providers.length, temporaryModelGroupDraft]);

  // Select a newly created provider right after a draft submit succeeds.
  useEffect(() => {
    if (providerCreatedIndex === undefined) {
      return;
    }
    setSelection({ index: providerCreatedIndex, kind: "provider" });
    setProviderCreatedIndex(undefined);
  }, [providerCreatedIndex, setProviderCreatedIndex]);

  // 保存成功后的新聚合模型从临时行切换为正式行。
  useEffect(() => {
    if (!modelGroupCreatedId) {
      return;
    }
    setSelection({ groupId: modelGroupCreatedId, kind: "group" });
    setModelGroupCreatedId(undefined);
  }, [modelGroupCreatedId, setModelGroupCreatedId]);

  const selected =
    selection?.kind === "provider" && providers[selection.index]
      ? providers[selection.index]
      : undefined;
  const showProviderDraft = providerTemporary && selection?.kind === "provider-new";
  const showGroupPanel = Boolean(modelGroupDraft.draft) && selection?.kind === "group";
  const showGroupNewPanel = Boolean(modelGroupDraft.draft) && selection?.kind === "group-new";
  const groupDeleteItem = modelGroups.find((group) => group.id === groupDeleteId);

  const [editTarget, setEditTarget] = useState<{
    contextWindow?: number;
    inputTypes: ProviderModelInputType[];
    maxOutputTokens?: number;
    model: string;
    providerIndex: number;
  }>();

  function openEditModel(provider: GatewayProviderConfig, providerIndex: number, model: string) {
    setEditTarget({
      contextWindow: provider.modelContextWindow?.[model],
      inputTypes: providerModelInputTypes(provider, model),
      maxOutputTokens: provider.modelMaxOutputTokens?.[model],
      model,
      providerIndex
    });
  }

  function closeEditModel() {
    setEditTarget(undefined);
  }

  function saveEditModel(name: string, fields: ModelFormFields) {
    if (!editTarget) {
      return;
    }
    updateModel(editTarget.providerIndex, editTarget.model, { name, ...fields });
    closeEditModel();
  }

  function openAddModel() {
    setAddModelOpen(true);
  }

  function saveAddModel(name: string, fields: ModelFormFields) {
    if (selection?.kind !== "provider") {
      return;
    }
    addModel(selection.index, name, fields);
    notify(`${t("Added")} ${name}`);
    setAddModelOpen(false);
  }

  // 批量获取添加(20261017_01):addModels 单次落库;面板已禁选既有模型,
  // hook 内仍按跳过语义兜底。
  function saveAddManyModels(names: string[], fields: ModelFormFields) {
    if (selection?.kind !== "provider") {
      return;
    }
    addModels(selection.index, names, fields);
    notify(`${t("Added")} ${names.length}`);
    setAddModelOpen(false);
  }

  // 获取回调:仅正式供应商条目提供(聚合/草稿视图不显示入口);结果翻译
  // 成面板契约(ok=false / 桥缺失 → throw,走 inline 红字)。
  async function fetchProviderModels() {
    if (selection?.kind !== "provider") {
      throw new Error("Failed to fetch models.");
    }
    const result = await fetchProviderModelsForProvider(providers[selection.index].provider);
    if (!result?.ok) {
      throw new Error(result?.message || "Failed to fetch models.");
    }
    return result.models;
  }

  async function confirmGroupDelete() {
    if (!groupDeleteId) {
      return;
    }
    if (await removeModelGroup(groupDeleteId)) {
      notify(t("Model group deleted"));
    }
    setGroupDeleteId(undefined);
  }

  return (
    // 20261104_01:恢复卡片壳 —— 此前为「透出背景」整页摊在页面底色上
    // (无边框、无卡片底色),现在 master-detail 回到有边框的卡片容器里;
    // 左右两栏之间仍用 1px 分隔线分区。
    <motion.div
      animate={{ opacity: 1 }}
      className="flex h-full min-h-0 min-w-0 overflow-hidden rounded-lg border border-border bg-card text-card-foreground shadow-card"
      data-module="providers-card"
      initial={{ opacity: 0 }}
      transition={{ duration: 0.15 }}
    >
      <div className="flex w-[280px] min-w-0 shrink-0 flex-col border-r border-border/60">
        <div className="flex h-12 shrink-0 items-center border-b border-border/60 px-3">
          <div className="relative min-w-0 flex-1">
            <Search className="pointer-events-none absolute left-2.5 top-1/2 z-[1] h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
            <Input
              aria-label={t("Search providers and models")}
              className="pl-8"
              onChange={(event) => setQuery(event.target.value)}
              placeholder={t("Search providers and models")}
              value={query}
            />
          </div>
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto p-2">
          <SidebarSectionHeader
            addLabel={t("Add provider")}
            icon={Server}
            onAdd={() => {
              addProvider();
              setSelection({ kind: "provider-new" });
            }}
            title={t("Providers")}
          />
          {providers.length === 0 && !providerTemporary ? (
            <div className="rounded-lg border border-dashed border-border bg-muted/30 px-3 py-10 text-center">
              <Server className="mx-auto mb-2 h-7 w-7 text-muted-foreground/40" />
              <div className="text-body text-muted-foreground">{t("No providers configured")}</div>
              <div className="mt-1 text-caption text-muted-foreground/60">{t("Click Add to create one")}</div>
            </div>
          ) : null}
          {providers.length > 0 && visibleProviders.length === 0 && !temporaryProviderVisible ? (
            <div className="rounded-lg border border-dashed border-border bg-muted/30 px-3 py-10 text-center text-body text-muted-foreground">{t("No matching providers and models")}</div>
          ) : null}
          {visibleProviders.length > 0 ? (
            <Reorder.Group
              axis="y"
              as="div"
              className="space-y-0.5"
              onReorder={setProviderOrder}
              values={providerList}
            >
              {visibleProviders.map(({ provider, index }) => {
                // 生效覆盖:预设已被删时归一为 undefined(= 跟随全局默认),
                // 与后端 ProviderProxyOverride::from_value 同口径。
                const override = providerProxyOverride(provider, proxyPresets);
                // 2026-09-13:跟随全局默认也显示图标 —— 解析默认策略的出口形态
                // (预设缺失按直连);摘要文案同步展开默认策略,用户不用猜。
                const summary = providerProxySummary(override, proxyPresets, t);
                return (
                  <ProviderListRow
                    dragging={draggingProvider === provider}
                    index={index}
                    key={provider.id || provider.name || `provider-${index}`}
                    modelCount={provider.models?.length ?? 0}
                    provider={provider}
                    proxyLabel={
                      override
                        ? summary
                        : `${summary} · ${globalProxySummary(proxyUpstream, proxyPresets, t)}`
                    }
                    proxyOverride={resolvedProxyIconMode(override, proxyUpstream, proxyPresets)}
                    selected={selection?.kind === "provider" && selection.index === index}
                    showDragHandle={!queryActive}
                    onDragStart={() => setDraggingProvider(provider)}
                    onSelect={(selectedIndex) => {
                      setSelection({ index: selectedIndex, kind: "provider" });
                    }}
                  />
                );
              })}
            </Reorder.Group>
          ) : null}
          {/* 临时「添加供应商」条目固定在列表最下方,不参与拖拽排序。 */}
          {temporaryProviderVisible ? (
            <TemporaryListRow
              className="mt-0.5"
              icon={Box}
              name={t("Add provider")}
              selected={selection?.kind === "provider-new"}
              onSelect={() => {
                addProvider();
                setSelection({ kind: "provider-new" });
              }}
            />
          ) : null}

          {/* 聚合模型区块(20260905_04 M6;2026-09-07 章节头与新增按钮同供应商样式) */}
          <SidebarSectionHeader
            addLabel={t("Add model group")}
            className="mt-3 border-t border-border/60 pt-2"
            icon={Layers3}
            onAdd={() => {
              openAddModelGroup();
              setSelection({ kind: "group-new" });
            }}
            title={t("Model groups")}
          />
          {modelGroups.length === 0 && !temporaryModelGroupDraft ? (
            <div className="rounded-lg border border-dashed border-border bg-muted/30 px-3 py-6 text-center">
              <Layers3 className="mx-auto mb-2 h-6 w-6 text-muted-foreground/40" />
              <div className="text-body text-muted-foreground">{t("No model groups configured")}</div>
              <div className="mt-1 text-caption text-muted-foreground/60">
                {t("Combine models from multiple providers behind one callable name.")}
              </div>
              <Button className="mt-3" onClick={() => { openAddModelGroup(); setSelection({ kind: "group-new" }); }} size="sm" type="button" variant="ghost">
                <Plus className="h-3.5 w-3.5" />
                {t("Add model group")}
              </Button>
            </div>
          ) : null}
          {modelGroups.length > 0 && visibleGroups.length === 0 && !temporaryGroupVisible ? (
            <div className="px-2 py-2 text-caption text-muted-foreground/70">{t("No matching model groups")}</div>
          ) : null}
          <Reorder.Group
            axis="y"
            as="div"
            className="space-y-0.5"
            onReorder={setGroupOrder}
            values={groupList}
          >
            {visibleGroups.map((group) => (
              <ModelGroupListRow
                dragging={draggingGroup === group}
                group={group}
                key={group.id}
                selected={selection?.kind === "group" && selection.groupId === group.id}
                showDragHandle={!queryActive}
                onDragStart={() => setDraggingGroup(group)}
                onSelect={() => {
                  setSelection({ groupId: group.id, kind: "group" });
                  openEditModelGroup(group.id);
                }}
                onToggleEnabled={(enabled) => setModelGroupEnabled(group.id, enabled)}
              />
            ))}
          </Reorder.Group>
          {/* 临时「添加聚合模型」条目固定在列表最下方,不参与拖拽排序。 */}
          {temporaryGroupVisible ? (
            <TemporaryListRow
              className="mt-0.5"
              icon={Blend}
              name={t("Add model group")}
              selected={selection?.kind === "group-new"}
              onSelect={() => {
                selectTemporaryModelGroup();
                setSelection({ kind: "group-new" });
              }}
            />
          ) : null}
        </div>
      </div>

      <div className="min-w-0 flex-1">
        {showProviderDraft ? (
          <ProviderDraftPanel
            canSubmit={canSubmitProvider}
            connectivityLoading={providerConnectivityLoading}
            connectivityProbe={providerConnectivityProbe}
            draft={draft}
            error={providerDraftError}
            notify={notify}
            onCheck={onCheckProviderDraft}
            onChange={onUpdateProviderDraft}
            onClose={closeProviderDraft}
            onOpenProxySettings={onOpenProxySettings}
            onSubmit={submitProviderDraft}
            proxyPresets={proxyPresets}
          />
        ) : modelGroupDraft.draft && (showGroupPanel || showGroupNewPanel) ? (
          <ModelGroupPanel
            draft={modelGroupDraft.draft}
            error={modelGroupError}
            groups={modelGroups}
            isEdit={modelGroupDraft.editId !== undefined}
            onChange={updateModelGroupDraft}
            onClose={closeModelGroupDraft}
            onDelete={() => modelGroupDraft.editId && setGroupDeleteId(modelGroupDraft.editId)}
            onAddTarget={addModelGroupTarget}
            onReorderTargets={reorderModelGroupTargets}
            onQuickAdd={addModelGroupTargetsForModelName}
            onRemoveTarget={removeModelGroupTarget}
            onSubmit={submitModelGroupDraft}
            onToggleEnabled={(enabled) => {
              // 启停是草稿字段:随「保存」一起提交(与名称/策略/目标一致),
              // 不做即时落库,避免与草稿双写。
              updateModelGroupDraft({ enabled });
            }}
            onUpdateTarget={updateModelGroupTarget}
            providers={providers.map(({ provider }) => provider)}
          />
        ) : selected ? (
          <ProviderDetailPanel
            addModel={openAddModel}
            checkProviderConnectivity={checkProviderConnectivity}
            index={selected.index}
            notify={notify}
            onEditModel={openEditModel}
            onOpenProxySettings={onOpenProxySettings}
            provider={selected.provider}
            providers={providers}
            removeModel={removeModel}
            removeProvider={removeProvider}
            setProviderBaseUrl={setProviderBaseUrl}
            setProviderUrlMode={setProviderUrlMode}
            setProviderClientFingerprint={setProviderClientFingerprint}
            setProviderHeaders={setProviderHeaders}
            setProviderCredentialMode={setProviderCredentialMode}
            setProviderEnabled={setProviderEnabled}
            setProviderName={setProviderName}
            setProviderFixedKey={setProviderFixedKey}
            migrateProviderLegacyKey={migrateProviderLegacyKey}
            setProviderProtocol={setProviderProtocol}
            setProviderProxy={setProviderProxy}
            setProviderAccounts={setProviderAccounts}
            proxyPresets={proxyPresets}
          />
        ) : (
          <div className="flex h-full min-h-0 flex-col items-center justify-center gap-2 px-6 text-center">
            <Server className="h-8 w-8 text-muted-foreground/40" />
            <div className="text-body text-muted-foreground">{t("Select a provider to view details")}</div>
          </div>
        )}
      </div>

      <ModelFormDialog
        existingModels={selected?.provider.models ?? []}
        fetchModels={selection?.kind === "provider" ? fetchProviderModels : undefined}
        mode="add"
        onClose={() => setAddModelOpen(false)}
        onSave={saveAddModel}
        onSaveMany={saveAddManyModels}
        open={addModelOpen}
      />
      <ModelFormDialog
        existingModels={selected?.provider.models ?? []}
        mode="edit"
        initial={editTarget ? {
          contextWindow: editTarget.contextWindow,
          inputTypes: editTarget.inputTypes,
          maxOutputTokens: editTarget.maxOutputTokens,
          name: editTarget.model
        } : undefined}
        onClose={closeEditModel}
        onSave={saveEditModel}
        open={Boolean(editTarget)}
      />
      {groupDeleteItem ? (
        <DeleteModelGroupDialog
          group={groupDeleteItem}
          onClose={() => setGroupDeleteId(undefined)}
          onConfirm={confirmGroupDelete}
        />
      ) : null}
    </motion.div>
  );
}

// 删除聚合模型确认对话框(与 DeleteProviderDialog 同范式)。
export function DeleteModelGroupDialog({
  group,
  onClose,
  onConfirm
}: {
  group: ModelGroupConfig;
  onClose: () => void;
  onConfirm: () => void;
}) {
  const t = useAppText();
  return (
    <Dialog onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-[520px]">
        <DialogHeader>
          <div className="min-w-0">
            <DialogTitle>{t("Delete model group")}</DialogTitle>
          </div>
          <Button aria-label={t("Close dialog")} onClick={onClose} size="iconSm" title={t("Close")} type="button" variant="ghost">
            <X className="h-4 w-4" />
          </Button>
        </DialogHeader>

        <DialogBody>
          <div className="rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2.5">
            <div className="flex items-start gap-2 text-body font-medium text-destructive">
              <CircleAlert className="mt-0.5 h-3.5 w-3.5 shrink-0" />
              <span>{t("Delete this model group from the configuration?")}</span>
            </div>
            <div className="mt-2 space-y-1 text-caption text-muted-foreground">
              <div className="truncate">
                <span className="font-medium text-foreground">{t("Name")}:</span> {group.name}
              </div>
              <div>
                <span className="font-medium text-foreground">{t("Targets")}:</span>{" "}
                {group.targets.map((target) => `${target.providerId}/${target.model}`).join(", ")}
              </div>
              <div>{t("Clients calling this aggregate name will stop resolving.")}</div>
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
