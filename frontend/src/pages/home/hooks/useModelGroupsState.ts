// 聚合模型域(方案 20260905_04 M6):走现有 buildConfigUpdate + persistConfig
// 配置链路(聚合定义在 config.modelGroups 内,无需新增 RPC)。
//
// 2026-09-26 临时条目:新建聚合模型 = 唯一的内存临时草稿(侧栏「添加聚合模
// 型」行),点「添加」才落库;编辑既有组对齐供应商详情的交互模型 —— 不再有
// 编辑草稿,名称/策略/目标变更即时写配置并持久化。临时草稿与正式组互不覆
// 盖:切换只是换 activeKey,临时草稿原样保留。
//
// 2026-09-26 目标行重设计:排列顺序即优先级 —— 正式组每次落库、临时草稿
// 转正、打开编辑三种路径都会把 targets 序列化成「列表序 = 尝试序」
// (serializeTargetsByOrder);打开旧配置时先按旧 priority 迁移(migrateTargetsByOrder)。
import { useMemo, useState } from "react";
import {
  AppConfig, AppSaveConfigOptions, ModelGroupConfig,
  createModelGroupDraft, isDuplicateModelGroupTarget,
  migrateTargetsByOrder, modelGroupNameConflict, normalizeModelGroups,
  serializeTargetsByOrder
} from "../shared/index";

export type ModelGroupsStateOptions = {
  draftConfig: AppConfig;
  buildConfigUpdate: (mutator: (config: AppConfig) => AppConfig) => AppConfig;
  setConfigDraft: (config: AppConfig) => AppConfig;
  persistConfig: (config: AppConfig, setError: (message: string) => void, options?: AppSaveConfigOptions) => Promise<boolean>;
  setActionError: (message: string) => void;
  t: (value: string) => string;
};

export type ModelGroupDraftState = {
  draft?: ModelGroupConfig;
  /** 编辑既有组时为组 id;新建临时条目为 undefined。 */
  editId?: string;
};

const TEMPORARY_MODEL_GROUP_KEY = "__temporary_model_group__";

export function useModelGroupsState({
  draftConfig, buildConfigUpdate, setConfigDraft, persistConfig, setActionError, t
}: ModelGroupsStateOptions) {
  const [temporaryDraft, setTemporaryDraft] = useState<ModelGroupConfig>();
  const [activeKey, setActiveKey] = useState<string>();
  const [groupDraftError, setGroupDraftError] = useState("");
  const [modelGroupCreatedId, setModelGroupCreatedId] = useState<string>();

  const modelGroups = draftConfig.modelGroups ?? [];

  // 当前激活编辑器:临时 key → 内存草稿;正式组 id → 配置里的实时对象。
  const groupDraft = useMemo<ModelGroupDraftState>(() => {
    if (!activeKey) {
      return {};
    }
    if (activeKey === TEMPORARY_MODEL_GROUP_KEY) {
      return temporaryDraft ? { draft: temporaryDraft, editId: undefined } : {};
    }
    const group = modelGroups.find((item) => item.id === activeKey);
    return group ? { draft: group, editId: activeKey } : {};
  }, [activeKey, modelGroups, temporaryDraft]);

  function openAddModelGroup() {
    setGroupDraftError("");
    setTemporaryDraft((current) => current ?? createModelGroupDraft());
    setActiveKey(TEMPORARY_MODEL_GROUP_KEY);
  }

  function selectTemporaryModelGroup() {
    if (!temporaryDraft) {
      return;
    }
    setGroupDraftError("");
    setActiveKey(TEMPORARY_MODEL_GROUP_KEY);
  }

  function openEditModelGroup(groupId: string) {
    if (!modelGroups.some((item) => item.id === groupId)) {
      return;
    }
    setGroupDraftError("");
    setActiveKey(groupId);
    migrateGroupTargetsOnOpen(groupId);
  }

  /** 打开编辑即迁移到「顺序即优先级」语义(2026-09-26 目标行重设计):按旧
   *  priority 升序稳定排序后重新序列化,迁移后配置顺序 = 实际优先序,面板
   *  展示/拖拽才不撒谎。幂等:无差异不落库,重复点击行不重复写。 */
  function migrateGroupTargetsOnOpen(groupId: string) {
    const current = modelGroups.find((item) => item.id === groupId);
    if (!current) {
      return;
    }
    if (JSON.stringify(migrateTargetsByOrder(current)) === JSON.stringify(current.targets)) {
      return;
    }
    const next = buildConfigUpdate((config) => {
      const groups = normalizeModelGroups(config.modelGroups);
      const group = groups.find((item) => item.id === groupId);
      if (group) {
        group.targets = migrateTargetsByOrder(group);
      }
      config.modelGroups = groups;
      return config;
    });
    setConfigDraft(next);
    void persistConfig(next, setGroupDraftError);
  }

  function closeModelGroupDraft() {
    // 临时条目:取消 = 丢弃草稿;正式组:仅取消激活(变更已即时落库)。
    if (activeKey === TEMPORARY_MODEL_GROUP_KEY) {
      setTemporaryDraft(undefined);
    }
    setActiveKey(undefined);
    setGroupDraftError("");
  }

  function mutateTemporaryDraft(mutator: (draft: ModelGroupConfig) => ModelGroupConfig) {
    setTemporaryDraft((current) => (current ? mutator(current) : current));
    setGroupDraftError("");
  }

  /** 编辑既有组:直接写正式配置并即时落库(错误进面板错误区)。 */
  function mutateFormalGroup(mutator: (group: ModelGroupConfig) => void) {
    if (!activeKey || activeKey === TEMPORARY_MODEL_GROUP_KEY) {
      return;
    }
    if (!modelGroups.some((item) => item.id === activeKey)) {
      return;
    }
    const groupId = activeKey;
    const next = buildConfigUpdate((config) => {
      const groups = normalizeModelGroups(config.modelGroups);
      const group = groups.find((item) => item.id === groupId);
      if (group) {
        mutator(group);
        // 顺序即优先级:每次落库按当前排列序重排 priority(first/sticky 逐条
        // 递增,weighted 全体同层),启用开关已移除 → 统一 enabled。
        group.targets = serializeTargetsByOrder(group.targets, group.strategy);
      }
      config.modelGroups = groups;
      return config;
    });
    setConfigDraft(next);
    void persistConfig(next, setGroupDraftError);
  }

  function updateModelGroupDraft(patch: Partial<ModelGroupConfig>) {
    if (!activeKey) {
      return;
    }
    if (activeKey === TEMPORARY_MODEL_GROUP_KEY) {
      mutateTemporaryDraft((draft) => ({ ...draft, ...patch }));
      return;
    }
    // 正式组名称:校验通过才落库(空名/斜杠/重名会让归一化丢组或破坏解析)。
    if (patch.name !== undefined) {
      const name = patch.name.trim();
      if (!name) {
        setGroupDraftError(t("Enter a name."));
        return;
      }
      if (name.includes("/")) {
        // `Provider/model` 是显式选择器语法,聚合名带斜杠会破坏解析优先级。
        setGroupDraftError(t("Name cannot contain a slash."));
        return;
      }
      if (modelGroupNameConflict(name, modelGroups, activeKey)) {
        setGroupDraftError(t("Model group name already exists."));
        return;
      }
      patch = { ...patch, name };
    }
    mutateFormalGroup((group) => {
      Object.assign(group, patch);
    });
    setGroupDraftError("");
  }

  function updateModelGroupTarget(index: number, patch: Partial<ModelGroupConfig["targets"][number]>) {
    if (!activeKey) {
      return;
    }
    if (activeKey === TEMPORARY_MODEL_GROUP_KEY) {
      mutateTemporaryDraft((draft) => ({
        ...draft,
        targets: draft.targets.map((target, position) =>
          position === index ? { ...target, ...patch } : target
        )
      }));
      return;
    }
    mutateFormalGroup((group) => {
      if (group.targets[index]) {
        group.targets[index] = { ...group.targets[index], ...patch };
      }
    });
  }

  function addModelGroupTarget(providerId: string, model: string) {
    if (!activeKey) {
      return;
    }
    const target = { model, providerId };
    if (activeKey === TEMPORARY_MODEL_GROUP_KEY) {
      mutateTemporaryDraft((draft) =>
        isDuplicateModelGroupTarget(draft, target)
          ? draft
          : { ...draft, targets: [...draft.targets, target] }
      );
      return;
    }
    mutateFormalGroup((group) => {
      if (!isDuplicateModelGroupTarget(group, target)) {
        group.targets = [...group.targets, target];
      }
    });
  }

  /** 「按模型名跨供应商」快捷带入:同名模型的全部提供方各加一条目标。 */
  function addModelGroupTargetsForModelName(pairs: Array<{ model: string; providerId: string }>) {
    if (!activeKey) {
      return;
    }
    const appendMissing = (targets: ModelGroupConfig["targets"]) => {
      for (const pair of pairs) {
        if (!targets.some((item) => item.providerId === pair.providerId && item.model.trim().toLowerCase() === pair.model.trim().toLowerCase())) {
          targets.push({ model: pair.model, providerId: pair.providerId });
        }
      }
      return targets;
    };
    if (activeKey === TEMPORARY_MODEL_GROUP_KEY) {
      mutateTemporaryDraft((draft) => ({ ...draft, targets: appendMissing([...draft.targets]) }));
      return;
    }
    mutateFormalGroup((group) => {
      group.targets = appendMissing([...group.targets]);
    });
  }

  function removeModelGroupTarget(index: number) {
    if (!activeKey) {
      return;
    }
    if (activeKey === TEMPORARY_MODEL_GROUP_KEY) {
      mutateTemporaryDraft((draft) => ({
        ...draft,
        targets: draft.targets.filter((_, position) => position !== index)
      }));
      return;
    }
    mutateFormalGroup((group) => {
      group.targets = group.targets.filter((_, position) => position !== index);
    });
  }

  /** 目标行拖拽排序(2026-09-26):整序替换,顺序即优先级(正式组落库前由
   *  mutateFormalGroup 统一序列化,临时草稿在 submit 时序列化)。 */
  function reorderModelGroupTargets(next: ModelGroupConfig["targets"]) {
    if (!activeKey) {
      return;
    }
    if (activeKey === TEMPORARY_MODEL_GROUP_KEY) {
      mutateTemporaryDraft((draft) => ({ ...draft, targets: next }));
      return;
    }
    mutateFormalGroup((group) => {
      group.targets = next;
    });
  }

  /** 仅用于临时条目:校验后写入正式配置,草稿迁移为正式编辑上下文。 */
  async function submitModelGroupDraft(): Promise<boolean> {
    const draft = temporaryDraft;
    if (!draft || activeKey !== TEMPORARY_MODEL_GROUP_KEY) {
      return false;
    }
    const name = draft.name.trim();
    if (!name) {
      setGroupDraftError(t("Enter a name."));
      return false;
    }
    if (name.includes("/")) {
      // `Provider/model` 是显式选择器语法,聚合名带斜杠会破坏解析优先级。
      setGroupDraftError(t("Name cannot contain a slash."));
      return false;
    }
    if (draft.targets.length === 0) {
      setGroupDraftError(t("Add at least one target model."));
      return false;
    }
    if (modelGroupNameConflict(name, modelGroups, draft.id)) {
      setGroupDraftError(t("Model group name already exists."));
      return false;
    }
    const next = buildConfigUpdate((config) => {
      const groups = normalizeModelGroups(config.modelGroups);
      groups.push({
        ...draft,
        name,
        // 顺序即优先级:临时草稿转正时按列表序序列化目标。
        targets: serializeTargetsByOrder(draft.targets, draft.strategy)
      });
      config.modelGroups = groups;
      return config;
    });
    setConfigDraft(next);
    if (await persistConfig(next, setGroupDraftError)) {
      setTemporaryDraft(undefined);
      // 草稿转正:激活键切到正式组 id,侧栏行同步从「添加聚合模型」变为
      // 正式条目,编辑上下文保持不中断。
      setActiveKey(draft.id);
      setModelGroupCreatedId(draft.id);
      return true;
    }
    return false;
  }

  function setModelGroupEnabled(groupId: string, enabled: boolean) {
    const next = buildConfigUpdate((config) => {
      const groups = normalizeModelGroups(config.modelGroups);
      const group = groups.find((item) => item.id === groupId);
      if (group) {
        group.enabled = enabled;
      }
      config.modelGroups = groups;
      return config;
    });
    setConfigDraft(next);
    void persistConfig(next, setActionError);
  }

  /** 侧栏聚合模型拖拽排序(2026-09-13):整体数组替换,随现有配置链路落库。 */
  function reorderModelGroups(next: ModelGroupConfig[]) {
    const updated = buildConfigUpdate((config) => {
      config.modelGroups = next;
      return config;
    });
    setConfigDraft(updated);
    void persistConfig(updated, setActionError);
  }

  async function removeModelGroup(groupId: string): Promise<boolean> {
    const next = buildConfigUpdate((config) => {
      config.modelGroups = normalizeModelGroups(config.modelGroups).filter((item) => item.id !== groupId);
      return config;
    });
    setConfigDraft(next);
    const saved = await persistConfig(next, setActionError);
    if (saved && activeKey === groupId) {
      setActiveKey(undefined);
      setGroupDraftError("");
    }
    return saved;
  }

  return {
    modelGroups,
    groupDraft,
    temporaryModelGroupDraft: temporaryDraft,
    groupDraftError,
    setGroupDraftError,
    modelGroupCreatedId,
    setModelGroupCreatedId,
    openAddModelGroup,
    selectTemporaryModelGroup,
    openEditModelGroup,
    closeModelGroupDraft,
    updateModelGroupDraft,
    updateModelGroupTarget,
    addModelGroupTarget,
    addModelGroupTargetsForModelName,
    removeModelGroupTarget,
    reorderModelGroupTargets,
    submitModelGroupDraft,
    setModelGroupEnabled,
    reorderModelGroups,
    removeModelGroup
  };
}
