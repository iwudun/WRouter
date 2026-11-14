import { useState, type ReactNode } from "react";
import { ChevronRight } from "lucide-react";
import { cn } from "@/lib/utils";
import type { ProviderProxyOverride, ProxyPreset } from "@/contracts/app";
import { BUILTIN_CLIENT_FINGERPRINT_PRESETS, useAppText } from "../../shared";

// 20261017:供应商「高级设置」折叠区 —— 网络代理/客户端指纹/自定义请求头
// 这类「默认值即可用」的进阶字段收进一个可折叠章节,置于 API 密钥之后,
// 核心路径(名称/地址/格式/密钥/模型)前置。字段组件原样复用,折叠只是
// 展示层:档位变化仍经原 onChange 上抛(草稿态)或即时落库(详情页)。
//
// 两处宿主的默认展开规则:
//   新建供应商表单 —— 草稿恒为默认值,不传 defaultOpen(恒收起);
//   供应商详情面板 —— 有非默认配置时传 true(状态可预期:已配置的事实
//   不藏进折叠,避免用户忘配过代理后排障漏查)。
//
// 收起态标题行带「已配置项摘要」,只列非默认项(全默认不显示)。两个
// *Summary 帮助函数供宿主拼摘要,joinAdvancedSummary 以「 · 」连接。

export function providerProxySummary(
  t: (value: string) => string,
  mode: ProviderProxyOverride["mode"],
  presetId: string,
  presets: ProxyPreset[]
): string {
  if (mode === "inherit") {
    return "";
  }
  if (mode === "direct") {
    return `${t("Proxy")} ${t("Direct connection")}`;
  }
  const name = presets.find((preset) => preset.id === presetId)?.name;
  return name ? `${t("Proxy")} ${name}` : "";
}

export function providerFingerprintSummary(
  t: (value: string) => string,
  mode: "off" | "passthrough" | "preset",
  presetId: string
): string {
  if (mode === "off") {
    return "";
  }
  if (mode === "passthrough") {
    return `${t("Fingerprint")} ${t("Pass through")}`;
  }
  const label = BUILTIN_CLIENT_FINGERPRINT_PRESETS.find((preset) => preset.id === presetId)?.label;
  return label ? `${t("Fingerprint")} ${label}` : "";
}

export function joinAdvancedSummary(pieces: string[]): string {
  return pieces.filter(Boolean).join(" · ");
}

export function ProviderAdvancedSettings({
  className,
  contentClassName,
  defaultOpen = false,
  summary,
  children
}: {
  className?: string;
  /** 展开内容列的间距(两宿主节奏不同:详情 space-y-4 / 表单 space-y-3)。 */
  contentClassName?: string;
  /** 有非默认配置时展开(详情页宿主);新建表单草稿恒收起。 */
  defaultOpen?: boolean;
  /** 收起态标题行的已配置摘要(仅非默认项;空 = 全默认不显示)。 */
  summary?: string;
  children: ReactNode;
}) {
  const t = useAppText();
  const [open, setOpen] = useState(defaultOpen);
  return (
    <div className={cn("min-w-0", className)}>
      <button
        aria-expanded={open}
        className="flex w-full items-center gap-1.5 text-left"
        onClick={() => setOpen((value) => !value)}
        type="button"
      >
        {/* 收起=箭头向右,展开=旋转向下(经典 disclosure 语义)。 */}
        <ChevronRight
          className={cn(
            "h-3.5 w-3.5 shrink-0 text-muted-foreground transition-transform",
            open && "rotate-90"
          )}
        />
        <span className="shrink-0 text-caption font-medium text-foreground">
          {t("Advanced settings")}
        </span>
        {summary ? (
          <span className="min-w-0 truncate text-caption text-muted-foreground">{summary}</span>
        ) : null}
      </button>
      {open ? <div className={cn("min-w-0 pt-2", contentClassName)}>{children}</div> : null}
    </div>
  );
}
