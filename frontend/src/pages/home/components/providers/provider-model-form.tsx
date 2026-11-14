import { useEffect, useMemo, useRef, useState } from "react";
import { ListPlus, LoaderCircle, Lock, Search } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Dialog, DialogBody, DialogContent, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import type { ProviderModelInputType } from "@/contracts/app";
import { cn } from "@/lib/utils";
import { Field, useAppText } from "../../shared/index";

// F4.3: 从 providers-list.tsx 拆出 —— 模型新增/编辑共用表单对话框。
// 2026-09-10: 按参考稿重排 —— 名称/上下文窗口/最大输出 Token 三个直填字段
// + 输入类型(文本恒选锁定,勾「图片」即视觉模型)/输出类型(暂只有文本)。
// 默认值对齐参考稿:上下文窗口 1000000、最大输出 128000。
// 20261017_01: 新增模式内置「获取模型」——按当前表单上下文拉上游模型
// 列表,面板内搜索/多选/批量添加;多选时表单元数据值作为批量默认值。

export type ModelFormFields = {
  contextWindow?: number;
  inputTypes: ProviderModelInputType[];
  maxOutputTokens?: number;
};

// Shared model form dialog: used for both adding a new model and editing the
// fields of an existing one (including renaming it — the name is the record
// key, so renaming migrates every per-model field). Duplicate names are
// rejected inline: existingModels must contain every currently registered
// model name (edit mode includes the model's own name).
export function ModelFormDialog({
  existingModels,
  fetchModels,
  initial,
  mode,
  onClose,
  onSave,
  onSaveMany,
  open
}: {
  existingModels: string[];
  /** 20261017_01:「获取模型」回调(宿主按草稿/已存供应商拼装并把失败
   *  翻译成 throw);缺省或编辑模式不显示入口。 */
  fetchModels?: () => Promise<string[]>;
  initial?: {
    contextWindow?: number;
    inputTypes?: ProviderModelInputType[];
    maxOutputTokens?: number;
    name: string;
  };
  mode: "add" | "edit";
  onClose: () => void;
  onSave: (name: string, fields: ModelFormFields) => void;
  /** 批量添加(≥2 个选中):表单当前元数据值作为批量默认值。 */
  onSaveMany?: (names: string[], fields: ModelFormFields) => void;
  open: boolean;
}) {
  const t = useAppText();
  const [name, setName] = useState("");
  const [contextWindow, setContextWindow] = useState("1000000");
  const [maxOutputTokens, setMaxOutputTokens] = useState("128000");
  const [inputTypes, setInputTypes] = useState<ProviderModelInputType[]>(["text"]);
  const [fetchedModels, setFetchedModels] = useState<string[] | undefined>(undefined);
  const [fetchingModels, setFetchingModels] = useState(false);
  const [fetchError, setFetchError] = useState("");
  const [modelSearch, setModelSearch] = useState("");
  const [selectedFetched, setSelectedFetched] = useState<string[]>([]);

  // Initialize the form only on the open rising edge. `initial` is a fresh
  // object literal on every parent render, so depending on it here would reset
  // in-progress edits whenever the parent re-renders (e.g. gateway polling).
  const prevOpenRef = useRef(false);
  useEffect(() => {
    if (open && !prevOpenRef.current) {
      setName(mode === "edit" ? initial?.name ?? "" : "");
      setContextWindow(mode === "edit" ? String(initial?.contextWindow ?? "") : "1000000");
      setMaxOutputTokens(mode === "edit" ? String(initial?.maxOutputTokens ?? "") : "128000");
      setInputTypes(mode === "edit" ? initial?.inputTypes ?? ["text"] : ["text"]);
      setFetchedModels(undefined);
      setFetchingModels(false);
      setFetchError("");
      setModelSearch("");
      setSelectedFetched([]);
    }
    prevOpenRef.current = open;
  }, [open]);

  const trimmedName = name.trim();
  const parsedContextWindow = Number.parseInt(contextWindow, 10);
  const parsedMaxOutputTokens = Number.parseInt(maxOutputTokens, 10);
  const isEdit = mode === "edit";
  // A name collides when it belongs to another model (in edit mode the model's
  // own current name is exempt).
  const duplicateName = Boolean(
    trimmedName &&
    existingModels.includes(trimmedName) &&
    (mode === "add" || trimmedName !== initial?.name)
  );
  const fields: ModelFormFields = {
    contextWindow: Number.isFinite(parsedContextWindow) && parsedContextWindow > 0 ? parsedContextWindow : undefined,
    inputTypes,
    maxOutputTokens: Number.isFinite(parsedMaxOutputTokens) && parsedMaxOutputTokens > 0 ? parsedMaxOutputTokens : undefined
  };
  // 已存在的模型在面板里禁选(「已添加」),批量确认天然不重复、不覆盖。
  const selectableFetched = useMemo(
    () => (fetchedModels ?? []).filter((model) => !existingModels.includes(model)),
    [fetchedModels, existingModels]
  );
  const filteredFetched = useMemo(() => {
    const query = modelSearch.trim().toLowerCase();
    if (!query) {
      return fetchedModels ?? [];
    }
    return (fetchedModels ?? []).filter((model) => model.toLowerCase().includes(query));
  }, [fetchedModels, modelSearch]);
  // 面板模式按勾选集确认;手填模式维持既有单模型校验(名称 + 重名)。
  const panelMode = fetchedModels !== undefined;
  const canConfirm = panelMode
    ? selectedFetched.length > 0 && (selectedFetched.length === 1 || Boolean(onSaveMany))
    : Boolean(trimmedName) && !duplicateName;

  function toggleInputType(type: ProviderModelInputType, checked: boolean) {
    setInputTypes((current) =>
      checked ? Array.from(new Set(["text", ...current, type])) as ProviderModelInputType[] : current.filter((item) => item !== type)
    );
  }

  async function handleFetchModels() {
    if (!fetchModels || fetchingModels) {
      return;
    }
    setFetchingModels(true);
    setFetchError("");
    try {
      const models = await fetchModels();
      setFetchedModels(models);
      setSelectedFetched([]);
      setModelSearch("");
    } catch (error) {
      setFetchError(error instanceof Error ? error.message : String(error));
    } finally {
      setFetchingModels(false);
    }
  }

  function confirmSelection() {
    if (panelMode) {
      if (selectedFetched.length === 1) {
        onSave(selectedFetched[0], fields);
      } else if (selectedFetched.length > 1 && onSaveMany) {
        onSaveMany(selectedFetched, fields);
      }
      return;
    }
    onSave(trimmedName, fields);
  }

  return (
    <Dialog onOpenChange={(nextOpen) => { if (!nextOpen) onClose(); }} open={open}>
      <DialogContent className="max-w-[480px]">
        <DialogHeader>
          <DialogTitle>{isEdit ? t("Edit model") : t("Add model")}</DialogTitle>
        </DialogHeader>
        <DialogBody className="space-y-3">
          <Field
            action={
              !isEdit && fetchModels ? (
                <Button
                  disabled={fetchingModels}
                  onClick={() => void handleFetchModels()}
                  size="sm"
                  type="button"
                  variant="ghost"
                >
                  {fetchingModels ? (
                    <LoaderCircle aria-hidden="true" className="h-3.5 w-3.5 animate-spin" />
                  ) : (
                    <ListPlus aria-hidden="true" className="h-3.5 w-3.5" />
                  )}
                  {fetchingModels ? t("Fetching models…") : t("Fetch models")}
                </Button>
              ) : undefined
            }
            label={t("Model name")}
          >
            {panelMode ? (
              <div className="space-y-1">
                <div className="flex items-center gap-1.5">
                  <div className="relative min-w-0 flex-1">
                    <Search
                      aria-hidden="true"
                      className="pointer-events-none absolute top-1/2 left-2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground"
                    />
                    <Input
                      className="h-8 pl-7.5"
                      onChange={(event) => setModelSearch(event.target.value)}
                      placeholder={t("Search models")}
                      spellCheck={false}
                      value={modelSearch}
                    />
                  </div>
                  <Button
                    disabled={selectableFetched.length === 0}
                    onClick={() => setSelectedFetched(selectableFetched)}
                    size="sm"
                    type="button"
                    variant="outline"
                  >
                    {t("Select all")}
                  </Button>
                  <Button
                    disabled={selectedFetched.length === 0}
                    onClick={() => setSelectedFetched([])}
                    size="sm"
                    type="button"
                    variant="outline"
                  >
                    {t("Clear")}
                  </Button>
                  <Button
                    onClick={() => { setFetchedModels(undefined); setFetchError(""); }}
                    size="sm"
                    type="button"
                    variant="ghost"
                  >
                    {t("Back")}
                  </Button>
                </div>
                <div className="max-h-64 overflow-y-auto rounded-md border border-border bg-background">
                  {filteredFetched.map((model) => {
                    const added = existingModels.includes(model);
                    const selected = selectedFetched.includes(model);
                    return (
                      <label
                        className={cn(
                          "flex cursor-pointer items-center gap-2 px-2.5 py-1.5 text-body transition-colors",
                          added ? "opacity-60" : "hover:bg-accent",
                          selected && !added && "bg-primary/5"
                        )}
                        key={model}
                      >
                        <Checkbox
                          checked={selected}
                          disabled={added}
                          onCheckedChange={() => {
                            setSelectedFetched((current) =>
                              current.includes(model) ? current.filter((item) => item !== model) : [...current, model]
                            );
                          }}
                        />
                        <span className="min-w-0 flex-1 truncate font-mono text-body-lg" title={model}>{model}</span>
                        {added ? <Badge className="shrink-0" variant="secondary">{t("Added")}</Badge> : null}
                      </label>
                    );
                  })}
                  {filteredFetched.length === 0 ? (
                    <div className="px-2.5 py-3 text-caption text-muted-foreground">{t("No matching models")}</div>
                  ) : null}
                </div>
              </div>
            ) : (
              <div className="space-y-1">
                <Input
                  autoFocus
                  onChange={(event) => setName(event.target.value)}
                  placeholder={t("Model name")}
                  spellCheck={false}
                  value={name}
                />
                {duplicateName ? (
                  <span className="block text-caption leading-4 text-destructive">{t("Model name already exists.")}</span>
                ) : null}
              </div>
            )}
            {fetchError ? (
              <span className="block text-caption leading-4 text-destructive">{fetchError}</span>
            ) : null}
          </Field>
          <Field label={t("Context window")}>
            <Input
              inputMode="numeric"
              min={1}
              onChange={(event) => setContextWindow(event.target.value)}
              placeholder="1000000"
              spellCheck={false}
              type="number"
              value={contextWindow}
            />
          </Field>
          <Field label={t("Max output tokens")}>
            <Input
              inputMode="numeric"
              min={1}
              onChange={(event) => setMaxOutputTokens(event.target.value)}
              placeholder="128000"
              spellCheck={false}
              type="number"
              value={maxOutputTokens}
            />
          </Field>
          <Field label={t("Input types")}>
            <div className="flex flex-wrap gap-2">
              <InputTypeChip checked disabled label={t("Text")} locked />
              <InputTypeChip
                checked={inputTypes.includes("image")}
                label={t("Image")}
                onCheckedChange={(checked) => toggleInputType("image", checked)}
              />
              <InputTypeChip
                checked={inputTypes.includes("video")}
                label={t("Video")}
                onCheckedChange={(checked) => toggleInputType("video", checked)}
              />
              <InputTypeChip
                checked={inputTypes.includes("pdf")}
                label={t("PDF")}
                onCheckedChange={(checked) => toggleInputType("pdf", checked)}
              />
            </div>
          </Field>
          <Field label={t("Output types")}>
            <div className="flex flex-wrap gap-2">
              <InputTypeChip checked disabled label={t("Text")} locked />
            </div>
          </Field>
        </DialogBody>
        <DialogFooter>
          <Button onClick={onClose} type="button" variant="outline">
            {t("Cancel")}
          </Button>
          <Button
            disabled={!canConfirm}
            onClick={confirmSelection}
            type="button"
          >
            {panelMode && selectedFetched.length > 1
              ? `${t("Add model")} (${selectedFetched.length})`
              : isEdit && !panelMode
                ? t("Save")
                : t("Add")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

// 参考稿样式的类型选项:圆角边框小胶囊 + 复选框 + 文案;锁定项(「文本」)
// 恒选并带小锁,提示不可取消。
function InputTypeChip({
  checked,
  disabled,
  label,
  locked,
  onCheckedChange
}: {
  checked: boolean;
  disabled?: boolean;
  label: string;
  locked?: boolean;
  onCheckedChange?: (checked: boolean) => void;
}) {
  return (
    <label
      className={cn(
        "flex items-center gap-1.5 rounded-md border border-border bg-background px-2.5 py-1.5 text-body text-foreground transition-colors",
        disabled ? "opacity-80" : "cursor-pointer hover:border-muted-foreground/45"
      )}
    >
      <Checkbox
        checked={checked}
        disabled={disabled}
        onCheckedChange={onCheckedChange}
      />
      <span>{label}</span>
      {locked ? <Lock aria-hidden="true" className="h-3 w-3 text-muted-foreground" /> : null}
    </label>
  );
}
