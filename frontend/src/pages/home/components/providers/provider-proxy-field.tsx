import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import type { ProviderProxyOverride, ProxyPreset } from "@/contracts/app";
import {
  Field, SelectControl, proxyPresetAddress, useAppText
} from "../../shared/index";

// 方案 20260906_01 F3:供应商「网络代理」字段 —— 新建供应商表单(草稿态)与
// 已存供应商详情面板共用同一份交互:
//   [模式:跟随全局默认 | 直连 | 自定义代理] [自定义代理下拉]
// 2026-09-13 二轮:两个选择框**恒显示** —— 未选「自定义代理」时预设框冻结
// (disabled,显示「未选择代理」占位);预设框占满剩余宽度使该行与其他行对齐
// (SelectControl 的 className 落在内层 select 上,flex 尺寸必须套在包装 div 上)。
// 预设列表为空且选了「自定义代理」时,就地提示 + 跳转设置页代理分区(验收 #5)。
// 组件本身不持久化:档位变化经 onChange 上抛,由调用方决定是写草稿还是直接落库。

export function ProviderProxyField({
  className,
  labelClassName,
  mode,
  onChange,
  onOpenProxySettings,
  presetId,
  presets
}: {
  className?: string;
  /** 透传给 Field 的标签样式(详情页章节小标题样式时用)。 */
  labelClassName?: string;
  mode: ProviderProxyOverride["mode"];
  onChange: (mode: ProviderProxyOverride["mode"], presetId: string) => void;
  onOpenProxySettings: () => void;
  presetId: string;
  presets: ProxyPreset[];
}) {
  const t = useAppText();
  const modeOptions = [
    { label: t("Follow global default"), value: "inherit" },
    { label: t("Direct connection"), value: "direct" },
    { label: t("Use a custom proxy"), value: "preset" }
  ];
  // 纯受控:悬空预设引用(预设刚被删)由父级经 normalizeProviderProxy 归一为
  // inherit,这里只做显示回退,不自作主张回写(否则与父级的写回互相触发)。
  const hasSelection = presets.some((preset) => preset.id === presetId);
  const presetValue = hasSelection ? presetId : "";
  const presetDisabled = mode !== "preset" || presets.length === 0;
  const presetOptions = [
    ...(presetValue ? [] : [{ label: t("No proxy preset selected"), value: "" }]),
    ...presets.map((preset) => ({
      label: `${preset.name} (${proxyPresetAddress(preset)})`,
      value: preset.id
    }))
  ];

  return (
    <div className={cn("grid grid-cols-1 gap-3", className)}>
      <Field label={t("Network proxy")} labelClassName={labelClassName}>
        <div className="flex items-center gap-2">
          {/* 模式框固定窄宽;预设框包装 div 占满余宽(select 本身 w-full)。 */}
          <div className="w-[180px] shrink-0">
            <SelectControl
              aria-label={t("Network proxy")}
              onChange={(value) => {
                const next = value as ProviderProxyOverride["mode"];
                // 切到「自定义代理」时默认选中第一个预设(有可选时),省一次点击。
                onChange(next, next === "preset" ? presetId || presets[0]?.id || "" : "");
              }}
              options={modeOptions}
              value={mode}
            />
          </div>
          <div className="min-w-0 flex-1">
            <SelectControl
              aria-label={t("Custom proxies")}
              disabled={presetDisabled}
              onChange={(value) => value && onChange("preset", value)}
              options={presetOptions}
              value={presetValue}
            />
          </div>
        </div>
      </Field>

      {mode === "preset" && presets.length === 0 ? (
        <div className="flex flex-wrap items-center justify-between gap-2 rounded-md border border-amber-300/40 bg-amber-50/60 px-3 py-2 dark:border-amber-500/25 dark:bg-amber-500/10">
          <span className="text-caption text-foreground">{t("No custom proxies yet.")}</span>
          <Button onClick={onOpenProxySettings} size="sm" type="button" variant="outline">
            {t("Configure proxies in Settings")}
          </Button>
        </div>
      ) : null}
    </div>
  );
}
