import { memo, useEffect, useRef, useState, type PointerEvent } from "react";
import { Reorder, useDragControls } from "motion/react";
import { Blend, Check, Copy, Globe, GripVertical, Plus, Unplug, type LucideIcon } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import type { GatewayProviderConfig } from "@/contracts/app";
import { cn } from "@/lib/utils";

import {
  AnimatedIconSwap, ModelGroupConfig, copyTextToClipboard, isModelGroupEnabled,
  providerBaseUrl, providerDisplayIcon, providerIsEnabled, useAppText,
} from "../../../shared/index";
import { ProviderPresetIcon } from "../providers-deep-link";

export type ModelGroupDraftView = {
  draft?: ModelGroupConfig;
  editId?: string;
};

export function TemporaryListRow({
  className,
  icon: Icon,
  name,
  onSelect,
  selected
}: {
  className?: string;
  icon: LucideIcon;
  name: string;
  onSelect: () => void;
  selected: boolean;
}) {
  const t = useAppText();
  return (
    <button
      aria-current={selected ? "true" : undefined}
      className={cn(
        "flex w-full items-center gap-2 rounded-md border border-dashed px-2 py-2 text-left outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring/30",
        selected ? "border-primary/50 bg-accent/60 text-accent-foreground" : "border-primary/30 bg-primary/5 hover:bg-primary/10",
        className
      )}
      onClick={onSelect}
      type="button"
    >
      <span className="flex h-7 w-7 shrink-0 items-center justify-center rounded-md bg-primary/10 text-primary">
        <Icon aria-hidden="true" className="h-4 w-4" />
      </span>
      <span className="min-w-0 flex-1 truncate text-body font-medium">{name}</span>
      <Badge className="shrink-0" variant="outline">{t("Unsaved")}</Badge>
    </button>
  );
}

// 侧栏章节头:图标 + 大写标题 + 右上角新增按钮。供应商与聚合模型两个章节
// 共用,新增按钮为同一样式(实心方形 Plus,原顶栏「添加供应商」按钮缩小版)。
export function SidebarSectionHeader({
  addLabel,
  className,
  icon: Icon,
  onAdd,
  title
}: {
  addLabel: string;
  className?: string;
  icon: LucideIcon;
  onAdd: () => void;
  title: string;
}) {
  return (
    <div className={cn("flex items-center gap-1 px-1 pb-1 pt-2", className)}>
      <Icon aria-hidden="true" className="h-3.5 w-3.5 shrink-0 text-primary/80" />
      <span className="min-w-0 flex-1 truncate text-caption font-medium uppercase tracking-wide text-muted-foreground">
        {title}
      </span>
      <Button aria-label={addLabel} className="h-7 w-7 p-0" onClick={onAdd} title={addLabel} type="button">
        <Plus className="h-4 w-4" />
      </Button>
    </div>
  );
}

// 行级 memo(F4.3):SSE config-changed 失效 → refetch → 新 providers 数组时,
// 展示面(图标/名称/启停点/选中态)未变的行跳过重渲染。i18n 走 context,
// 语言切换不受 memo 影响。index/dragging 参与比较:拖拽重排后行按新位置
// 重绘(选中态与 onSelect 闭包都依赖 index)。
export function providerListRowEqual(
  a: ProviderListRowProps,
  b: ProviderListRowProps
): boolean {
  if (a.selected !== b.selected || a.index !== b.index || a.dragging !== b.dragging) {
    return false;
  }
  // 搜索态手柄显隐切换也要重绘(过滤视图内不提供拖拽)。
  if (a.showDragHandle !== b.showDragHandle) {
    return false;
  }
  // 20260906_01:代理摘要变了也要重绘(否则徽标停留在旧出口)。
  if (a.proxyLabel !== b.proxyLabel || a.proxyOverride !== b.proxyOverride) {
    return false;
  }
  if (a.modelCount !== b.modelCount) {
    return false;
  }
  if (a.provider === b.provider) {
    return true;
  }
  return (
    a.provider.name === b.provider.name &&
    providerDisplayIcon(a.provider) === providerDisplayIcon(b.provider) &&
    providerIsEnabled(a.provider) === providerIsEnabled(b.provider)
  );
}

export type ProviderListRowProps = {
  dragging: boolean;
  index: number;
  /** 该供应商的模型条目数(行内 muted 小字,与聚合模型行的摘要平衡信息密度)。 */
  modelCount: number;
  provider: GatewayProviderConfig;
  /** 20260906_01:代理来源文案(预设名 / 直连 / 跟随全局默认),进 title。 */
  proxyLabel: string;
  /** 行内代理小图标的解析形态(2026-09-13:显式覆盖用其自身,跟随全局默认
   *  解析默认策略,恒有图标)。 */
  proxyOverride?: "direct" | "preset";
  selected: boolean;
  /** 搜索过滤时隐藏手柄(过滤子集与全量 values 的重排映射有歧义)。 */
  showDragHandle: boolean;
  /** 2026-09-13:拖拽开始通知(父层记下被拖供应商以便提交重排)。 */
  onDragStart: () => void;
  onSelect: (index: number) => void;
};

/** 侧栏拖拽手柄(API 密钥条目最左侧同款):Reorder 手柄发起,不抢行内点击。 */
function SidebarDragHandle({ onStart }: { onStart: (event: PointerEvent) => void }) {
  const t = useAppText();
  return (
    <span
      aria-label={t("Drag to reorder")}
      className="flex h-6 w-4 shrink-0 cursor-grab touch-none items-center justify-center text-muted-foreground/50 outline-none transition-colors hover:text-foreground active:cursor-grabbing"
      onPointerDown={(event) => {
        // touch-none 防止触屏滚动抢占指针;preventDefault 避免拖出文本选区。
        event.preventDefault();
        onStart(event);
      }}
      role="button"
      tabIndex={0}
      title={t("Drag to reorder")}
    >
      <GripVertical className="h-3.5 w-3.5" />
    </span>
  );
}

export const ProviderListRow = memo(function ProviderListRow({
  dragging,
  index,
  modelCount,
  provider,
  proxyLabel,
  proxyOverride,
  selected,
  showDragHandle,
  onDragStart,
  onSelect
}: ProviderListRowProps) {
  const t = useAppText();
  const dragControls = useDragControls();
  const enabled = providerIsEnabled(provider);
  return (
    <Reorder.Item
      as="div"
      value={provider}
      className={cn(
        "flex w-full items-center gap-0.5 rounded-md transition-colors",
        selected ? "bg-accent/60 text-accent-foreground" : "hover:bg-muted/60",
        dragging && "relative z-10 bg-background/95 shadow-lg shadow-black/10"
      )}
      dragControls={dragControls}
      dragListener={false}
    >
      {showDragHandle ? (
        <SidebarDragHandle
          onStart={(event) => {
            onDragStart();
            dragControls.start(event);
          }}
        />
      ) : null}
      <button
        aria-current={selected ? "true" : undefined}
        aria-label={`${provider.name || t("Unnamed")} · ${enabled ? t("Enabled") : t("Disabled")}`}
        className="flex min-w-0 flex-1 items-center gap-2 rounded-md px-2 py-2 text-left outline-none focus-visible:ring-2 focus-visible:ring-ring/30"
        onClick={() => onSelect(index)}
        title={`${provider.name || t("Unnamed")} · ${providerBaseUrl(provider) || t("Not set")} · ${t("Network proxy")}: ${proxyLabel}`}
        type="button"
      >
        <ProviderPresetIcon className="h-7 w-7 shrink-0 rounded-md" iconUrl={providerDisplayIcon(provider)} />
        <span className="min-w-0 flex-1 truncate text-body font-medium">{provider.name || t("Unnamed")}</span>
        {/* 2026-09-13:模型数量徽章(替代「N 模型」文本,数字单列更易扫读)。 */}
        <Badge
          className="shrink-0 tabular-nums"
          title={`${modelCount} ${t("Models")}`}
          variant="secondary"
        >
          {modelCount}
        </Badge>
        <span
          aria-hidden="true"
          className="shrink-0 text-muted-foreground"
          title={`${t("Network proxy")}: ${proxyLabel}`}
        >
          {proxyOverride === "direct" ? (
            <Unplug className="h-3.5 w-3.5" />
          ) : (
            <Globe className="h-3.5 w-3.5" />
          )}
        </span>
        <span
          aria-hidden="true"
          className={cn(
            "h-2 w-2 shrink-0 rounded-full",
            enabled ? "bg-emerald-500 shadow-[0_0_0_3px_rgba(16,185,129,0.16)]" : "bg-muted-foreground/45"
          )}
        />
      </button>
    </Reorder.Item>
  );
}, providerListRowEqual);

// 聚合模型侧栏行:调用名(WRouter/<聚合名>) + 快速复制 + 启停点(点按即时启停)。
// 行图标用 Blend(条目族,双圆混合),与页面族 Layers3 区分(2026-09-26 四类图标)。
export type ModelGroupListRowProps = {
  dragging: boolean;
  group: ModelGroupConfig;
  selected: boolean;
  /** 搜索过滤时隐藏手柄(过滤子集与全量 values 的重排映射有歧义)。 */
  showDragHandle: boolean;
  onDragStart: () => void;
  onSelect: () => void;
  onToggleEnabled: (enabled: boolean) => void;
};

function modelGroupRowEqual(a: ModelGroupListRowProps, b: ModelGroupListRowProps): boolean {
  return a.selected === b.selected && a.dragging === b.dragging
    && a.showDragHandle === b.showDragHandle && a.group === b.group;
}

export const ModelGroupListRow = memo(function ModelGroupListRow({
  dragging,
  group,
  selected,
  showDragHandle,
  onDragStart,
  onSelect,
  onToggleEnabled
}: ModelGroupListRowProps) {
  const t = useAppText();
  const dragControls = useDragControls();
  const enabled = isModelGroupEnabled(group);
  const callableName = `WRouter/${group.name}`;
  const [copied, setCopied] = useState(false);
  const copiedTimer = useRef<number>();
  useEffect(() => () => window.clearTimeout(copiedTimer.current), []);

  function copyName() {
    void copyTextToClipboard(callableName).then(() => {
      setCopied(true);
      window.clearTimeout(copiedTimer.current);
      copiedTimer.current = window.setTimeout(() => setCopied(false), 1600);
    });
  }

  return (
    <Reorder.Item
      as="div"
      value={group}
      className={cn(
        "flex w-full items-center gap-0.5 rounded-md transition-colors",
        selected ? "bg-accent/60 text-accent-foreground" : "hover:bg-muted/60",
        dragging && "relative z-10 bg-background/95 shadow-lg shadow-black/10"
      )}
      dragControls={dragControls}
      dragListener={false}
    >
      {showDragHandle ? (
        <SidebarDragHandle
          onStart={(event) => {
            onDragStart();
            dragControls.start(event);
          }}
        />
      ) : null}
      <button
        aria-current={selected ? "true" : undefined}
        aria-label={`${callableName} · ${enabled ? t("Enabled") : t("Disabled")}`}
        className="flex min-w-0 flex-1 items-center gap-2 rounded-md py-2 pl-2 text-left outline-none focus-visible:ring-2 focus-visible:ring-ring/30"
        onClick={onSelect}
        title={callableName}
        type="button"
      >
        <span className="flex h-7 w-7 shrink-0 items-center justify-center rounded-md bg-primary/10 text-primary">
          <Blend aria-hidden="true" className="h-4 w-4" />
        </span>
        <span className="min-w-0 flex-1 truncate text-body font-medium">{callableName}</span>
      </button>
      <button
        aria-label={copied ? t("Copied") : t("Copy")}
        className={cn(
          "mr-0.5 shrink-0 rounded-md p-1 outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring/30",
          copied ? "text-emerald-600" : "text-muted-foreground hover:bg-muted hover:text-foreground"
        )}
        onClick={copyName}
        title={copied ? t("Copied") : t("Copy")}
        type="button"
      >
        <AnimatedIconSwap iconKey={copied ? "copied" : "copy"}>
          {copied ? <Check aria-hidden="true" className="h-3.5 w-3.5" /> : <Copy aria-hidden="true" className="h-3.5 w-3.5" />}
        </AnimatedIconSwap>
      </button>
      <button
        aria-label={enabled ? t("Disable") : t("Enable")}
        className="mr-1 shrink-0 rounded-full p-1 outline-none focus-visible:ring-2 focus-visible:ring-ring/30"
        onClick={() => onToggleEnabled(!enabled)}
        title={enabled ? t("Disable") : t("Enable")}
        type="button"
      >
        <span
          aria-hidden="true"
          className={cn(
            "block h-2 w-2 rounded-full",
            enabled ? "bg-emerald-500 shadow-[0_0_0_3px_rgba(16,185,129,0.16)]" : "bg-muted-foreground/45"
          )}
        />
      </button>
    </Reorder.Item>
  );
}, modelGroupRowEqual);
