import { useState } from "react";
import type { ProviderModelInputType } from "@/contracts/app";
import { LoaderCircle, X } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  AddProviderDraft, AnimatedIconSwap, fetchProviderModelsForDraft, GatewayProviderProbeResult,
  mergeProviderModelLists, normalizeModelInputTypes,
  ProviderConnectivityCheckReport,
  ProxyPreset,
  splitLines,
  useAppText,
} from "../../shared/index";
import { AddProviderForm } from "./providers-form";
import { ProviderPresetIcon } from "./providers-deep-link";
import { ProviderConnectivityCheckDialog } from "./provider-connectivity";
import { ModelFormDialog, type ModelFormFields } from "./provider-model-form";

// F4.3: 从 providers-list.tsx 拆出 —— 新增 provider 草稿面板。

// 模型输入类型的草稿记录补丁:与默认一致(仅文本)的模型不落键,避免草稿
// 提交前在 map 里积累无意义条目。
function modelInputTypesPatch(
  base: Record<string, ProviderModelInputType[]> | undefined,
  name: string,
  inputTypes: ProviderModelInputType[]
): Partial<AddProviderDraft> {
  const next = { ...(base ?? {}) };
  const normalized = normalizeModelInputTypes(inputTypes);
  if (normalized) {
    next[name] = normalized;
  } else {
    delete next[name];
  }
  return Object.keys(next).length > 0 ? { modelInputTypes: next } : {};
}

// 在 draft 各模型配置 map（输入类型 / 上下文窗口 / 最大输出 / 别名）中重命名键。
function renameModelKey<V>(map: Record<string, V> | undefined, oldKey: string, newKey: string): Record<string, V> | undefined {
  if (!map || !(oldKey in map)) {
    return map;
  }
  const next = { ...map };
  next[newKey] = next[oldKey];
  delete next[oldKey];
  return next;
}

// 在 draft 各模型配置 map 中删除键，空 map 归一为 undefined。
function deleteModelKey<V>(map: Record<string, V> | undefined, key: string): Record<string, V> | undefined {
  if (!map || !(key in map)) {
    return map;
  }
  const next = { ...map };
  delete next[key];
  return Object.keys(next).length > 0 ? next : undefined;
}

export function ProviderDraftPanel({
  canSubmit,
  connectivityLoading,
  connectivityProbe,
  draft,
  error,
  notify,
  onCheck,
  onChange,
  onClose,
  onOpenProxySettings,
  onSubmit,
  proxyPresets
}: {
  canSubmit: boolean;
  connectivityLoading: boolean;
  connectivityProbe?: GatewayProviderProbeResult;
  draft: AddProviderDraft;
  error: string;
  /** 密钥编辑器的行内提示(重名等);列表层透传 showToast。 */
  notify?: (message: string) => void;
  onCheck: (models?: string[]) => Promise<ProviderConnectivityCheckReport>;
  onChange: (patch: Partial<AddProviderDraft>, resetProbe?: boolean) => void;
  onClose: () => void;
  onOpenProxySettings: () => void;
  onSubmit: () => Promise<boolean>;
  proxyPresets: ProxyPreset[];
}) {
  const t = useAppText();
  const [submitting, setSubmitting] = useState(false);
  const [checkOpen, setCheckOpen] = useState(false);
  const [addModelOpen, setAddModelOpen] = useState(false);
  const [editingModel, setEditingModel] = useState<string | undefined>(undefined);
  const checkModels = mergeProviderModelLists(draft.selectedModels, splitLines(draft.modelsText));

  function saveAddModel(name: string, fields: ModelFormFields) {
    onChange(
      {
        modelsText: mergeProviderModelLists(splitLines(draft.modelsText), [name]).join("\n"),
        ...modelInputTypesPatch(draft.modelInputTypes, name, fields.inputTypes),
        ...(fields.contextWindow !== undefined ? { modelContextWindow: { ...(draft.modelContextWindow ?? {}), [name]: fields.contextWindow } } : {}),
        ...(fields.maxOutputTokens !== undefined ? { modelMaxOutputTokens: { ...(draft.modelMaxOutputTokens ?? {}), [name]: fields.maxOutputTokens } } : {})
      },
      true
    );
    setAddModelOpen(false);
  }

  // 批量获取添加(20261017_01):一次 onChange 合并全部名单;已存在的模型
  // 由面板禁选天然跳过,元数据值作为批量默认值(与单模型同款补丁语义:
  // 与默认一致(仅文本)的输入类型不落键)。
  function saveAddManyModels(names: string[], fields: ModelFormFields) {
    const fresh = names.filter((name) => !checkModels.includes(name));
    setAddModelOpen(false);
    if (fresh.length === 0) {
      return;
    }
    const inputTypesMap = { ...(draft.modelInputTypes ?? {}) };
    const normalizedInputTypes = normalizeModelInputTypes(fields.inputTypes);
    for (const name of fresh) {
      if (normalizedInputTypes) {
        inputTypesMap[name] = normalizedInputTypes;
      } else {
        delete inputTypesMap[name];
      }
    }
    onChange(
      {
        modelsText: mergeProviderModelLists(splitLines(draft.modelsText), fresh).join("\n"),
        ...(Object.keys(inputTypesMap).length > 0 ? { modelInputTypes: inputTypesMap } : {}),
        ...(fields.contextWindow !== undefined
          ? { modelContextWindow: { ...(draft.modelContextWindow ?? {}), ...Object.fromEntries(fresh.map((name) => [name, fields.contextWindow as number])) } }
          : {}),
        ...(fields.maxOutputTokens !== undefined
          ? { modelMaxOutputTokens: { ...(draft.modelMaxOutputTokens ?? {}), ...Object.fromEntries(fresh.map((name) => [name, fields.maxOutputTokens as number])) } }
          : {})
      },
      true
    );
  }

  // 获取回调:把结果翻译成面板契约(ok=false / 桥缺失 → throw,走 inline 红字)。
  async function fetchDraftModels() {
    const result = await fetchProviderModelsForDraft(draft);
    if (!result?.ok) {
      throw new Error(result?.message || "Failed to fetch models.");
    }
    return result.models;
  }

  async function submit() {
    setSubmitting(true);
    try {
      const saved = await onSubmit();
      if (!saved) {
        setSubmitting(false);
      }
    } catch (error) {
      setSubmitting(false);
      throw error;
    }
  }

  function removeDraftModel(model: string) {
    onChange(
      {
        modelsText: splitLines(draft.modelsText).filter((line) => line !== model).join("\n"),
        selectedModels: draft.selectedModels.filter((item) => item !== model),
        modelDescriptions: deleteModelKey(draft.modelDescriptions, model),
        modelContextWindow: deleteModelKey(draft.modelContextWindow, model),
        modelInputTypes: deleteModelKey(draft.modelInputTypes, model),
        modelMaxOutputTokens: deleteModelKey(draft.modelMaxOutputTokens, model),
        modelDisplayNames: deleteModelKey(draft.modelDisplayNames, model)
      },
      true
    );
  }

  function saveEditModel(name: string, fields: ModelFormFields) {
    const original = editingModel;
    if (!original) {
      return;
    }
    const renamed = name !== original;
    const patch: Partial<AddProviderDraft> = {};

    if (renamed) {
      patch.modelsText = splitLines(draft.modelsText).map((line) => (line === original ? name : line)).join("\n");
      patch.selectedModels = draft.selectedModels.map((item) => (item === original ? name : item));
      patch.modelDisplayNames = renameModelKey(draft.modelDisplayNames, original, name);
    }

    const inputBase = renamed ? (renameModelKey(draft.modelInputTypes, original, name) ?? {}) : { ...(draft.modelInputTypes ?? {}) };
    const normalizedInput = normalizeModelInputTypes(fields.inputTypes);
    if (normalizedInput) {
      inputBase[name] = normalizedInput;
    } else {
      delete inputBase[name];
    }
    patch.modelInputTypes = Object.keys(inputBase).length > 0 ? inputBase : undefined;

    const cwBase = renamed ? (renameModelKey(draft.modelContextWindow, original, name) ?? {}) : { ...(draft.modelContextWindow ?? {}) };
    if (fields.contextWindow !== undefined) {
      cwBase[name] = fields.contextWindow;
    } else {
      delete cwBase[name];
    }
    patch.modelContextWindow = Object.keys(cwBase).length > 0 ? cwBase : undefined;

    const maxBase = renamed ? (renameModelKey(draft.modelMaxOutputTokens, original, name) ?? {}) : { ...(draft.modelMaxOutputTokens ?? {}) };
    if (fields.maxOutputTokens !== undefined) {
      maxBase[name] = fields.maxOutputTokens;
    } else {
      delete maxBase[name];
    }
    patch.modelMaxOutputTokens = Object.keys(maxBase).length > 0 ? maxBase : undefined;

    onChange(patch, true);
    setEditingModel(undefined);
  }

  return (
    <>
      <div className="flex h-full min-h-0 min-w-0 flex-col">
        {/* 头部行内层与下方内容列同 max-w 居中(对齐详情面板 860px 范式),
            全屏时标题/按钮向中间靠拢而不是拉伸到面板两端。 */}
        <div className="h-12 shrink-0 border-b border-border/60 px-4">
          <div className="mx-auto flex h-full w-full max-w-[820px] items-center gap-2">
            <div className="flex min-w-0 flex-1 items-center gap-2">
              {/* 头部图标用条目族(品牌预设图标,Box 兜底):草稿已选图标时实时反映。 */}
              <ProviderPresetIcon className="h-7 w-7 shrink-0 rounded-md" iconUrl={draft.icon} />
              <span className="truncate text-body-lg font-semibold">{t("Add provider")}</span>
              <Badge className="shrink-0" variant="secondary">{t("Provider")}</Badge>
            </div>
            <Button className="h-7 px-2 text-caption" disabled={submitting} onClick={onClose} type="button" variant="outline">
              {t("Cancel")}
            </Button>
            <Button className="h-7 px-2 text-caption" disabled={!canSubmit || submitting || connectivityLoading} onClick={() => void submit()} type="button">
              <AnimatedIconSwap iconKey={submitting || connectivityLoading ? "loading" : "idle"}>
                {submitting || connectivityLoading ? <LoaderCircle className="h-3.5 w-3.5 animate-spin" /> : null}
              </AnimatedIconSwap>
              {submitting || connectivityLoading ? t("Loading") : t("Add")}
            </Button>
            <Button
              aria-label={t("Close dialog")}
              disabled={submitting}
              onClick={onClose}
              size="iconSm"
              title={t("Close")}
              type="button"
              variant="ghost"
            >
              <X className="h-4 w-4" />
            </Button>
          </div>
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto p-4">
          <div className="mx-auto max-w-[820px]">
            <AddProviderForm
              connectivityLoading={connectivityLoading}
              connectivityProbe={connectivityProbe}
              contextWindows={draft.modelContextWindow}
              displayNames={draft.modelDisplayNames}
              inputTypes={draft.modelInputTypes}
              draft={draft}
              error={error}
              notify={notify}
              onAddModel={() => setAddModelOpen(true)}
              onCheck={async () => { setCheckOpen(true); }}
              onEditModel={(model) => setEditingModel(model)}
              onChange={onChange}
              onOpenProxySettings={onOpenProxySettings}
              onRemoveModel={removeDraftModel}
              proxyPresets={proxyPresets}
            />
          </div>
        </div>
      </div>

      <ProviderConnectivityCheckDialog
        models={checkModels}
        onCheck={(models) => onCheck(models)}
        onClose={() => setCheckOpen(false)}
        open={checkOpen}
      />
      <ModelFormDialog
        existingModels={checkModels}
        fetchModels={fetchDraftModels}
        mode="add"
        onClose={() => setAddModelOpen(false)}
        onSave={saveAddModel}
        onSaveMany={saveAddManyModels}
        open={addModelOpen}
      />
      <ModelFormDialog
        existingModels={checkModels}
        initial={editingModel ? {
          contextWindow: draft.modelContextWindow?.[editingModel],
          inputTypes: draft.modelInputTypes?.[editingModel] ?? ["text"],
          maxOutputTokens: draft.modelMaxOutputTokens?.[editingModel],
          name: editingModel
        } : undefined}
        mode="edit"
        onClose={() => setEditingModel(undefined)}
        onSave={saveEditModel}
        open={editingModel !== undefined}
      />
    </>
  );
}
