import {
  BarChart3, Fingerprint, KeyRound, Layers3, Rocket, ScrollText, type LucideIcon
} from "lucide-react";
import type {
  CallLogStatusFilter,
  GatewayProviderProtocol,
  RouterCompressionTier,
  RouterFallbackMode,
  UsageStatsRange
} from "@/contracts/app";
// R8: these were private shadow copies of unions already defined in ./types;
// import the single canonical definitions instead.
import type {
  ApiKeyExpirationPreset,
  ApiKeyLimitMetric,
  LimitWindowPreset,
  NavigationId,
  QuotaWindowPreset
} from "./types";

// 时间范围预设:今天 / 7 天 / 30 天 / 全部。
// 「自定义」不是预设 —— 它是一枚带日历图标的独立段(见 usage-stats-range.tsx),
// 选中后 range 变成 "YYYY-MM-DD..YYYY-MM-DD" 形式的窗口串。
export const usageRangeOptions: Array<{ label: string; value: UsageStatsRange }> = [
  { label: "Today", value: "today" },
  { label: "7d", value: "7d" },
  { label: "30d", value: "30d" },
  { label: "All", value: "all" }
];

export const providerProtocolOptions: Array<{ label: string; value: GatewayProviderProtocol }> = [
  { label: "Chat Completions (/chat/completions)", value: "openai_chat_completions" },
  { label: "Anthropic Messages (/v1/messages)", value: "anthropic_messages" },
  { label: "Responses (/responses)", value: "openai_responses" }
];

export const apiKeyExpirationOptions: Array<{ label: string; value: ApiKeyExpirationPreset }> = [
  { label: "Never", value: "never" },
  { label: "7 days", value: "7d" },
  { label: "30 days", value: "30d" },
  { label: "90 days", value: "90d" },
  { label: "Custom", value: "custom" }
];

export const limitWindowOptions: Array<{ label: string; value: LimitWindowPreset }> = [
  { label: "per minute", value: "minute" },
  { label: "per hour", value: "hour" },
  { label: "per day", value: "day" }
];

// 配额统计窗口(20261105 C-9):后端 quotaWindowMs 是任意毫秒数,这里只列**已知档位**;
// 不在档位内的原值走 "custom",由编辑态原样保留(见 api-keys.ts quotaWindowPresetFromMs)。
export const quotaWindowOptions: Array<{ label: string; value: QuotaWindowPreset }> = [
  { label: "per minute", value: "minute" },
  { label: "per hour", value: "hour" },
  { label: "per day", value: "day" },
  { label: "per week", value: "week" },
  { label: "per month", value: "month" }
];

export const apiKeyLimitMetricOptions: Array<{ label: string; value: ApiKeyLimitMetric }> = [
  { label: "Requests", value: "requests" },
  { label: "Tokens", value: "tokens" }
];

// 成本节省选项(20261101_02 T3-B):压缩档位改为全局 savings 配置,
// 规则级 cacheControl/运算符/改写/条件源选项随规则域删除。
export const routerCompressionTierOptions: Array<{ label: string; value: RouterCompressionTier }> = [
  { label: "Off", value: "off" },
  { label: "Auto", value: "auto" },
  { label: "Full", value: "full" }
];

export const routerFallbackModeOptions: Array<{ label: string; value: RouterFallbackMode }> = [
  { label: "Off", value: "off" },
  { label: "Retry", value: "retry" },
  { label: "Fallback targets", value: "model-chain" }
];

// 20261101_03:侧栏分章(小标题分隔)。「设置」不在章内 ——
// 入口在侧栏左下角设置区,点按后设置分区菜单覆盖首页菜单。NavigationId
// 仍含 "settings"(copy.navigation Record 与 openSettingsView 深链需要)。
// 章节:开始(接入指引)/概览(用量)/接入(上游供应商+下游密钥)/观测(日志+指纹)。
export const navigationGroups: Array<{ items: Array<{ icon: LucideIcon; id: NavigationId }>; label: string }> = [
  {
    items: [{ icon: Rocket, id: "quickstart" }],
    label: "Get started"
  },
  {
    items: [{ icon: BarChart3, id: "usage-stats" }],
    label: "Overview"
  },
  {
    items: [
      { icon: Layers3, id: "providers" },
      { icon: KeyRound, id: "api-keys" }
    ],
    label: "Access"
  },
  {
    items: [
      { icon: ScrollText, id: "call-logs" },
      // 2026-11-01:模型指纹从设置分区升级为独立视图。
      { icon: Fingerprint, id: "model-fingerprints" }
    ],
    label: "Observability"
  }
];

// 五档状态:pending/processing 为在途实时行,
// completed/failed/canceled 为落库行推导。
export const callLogStatusChips: Array<{ label: string; value: CallLogStatusFilter }> = [
  { label: "All", value: "all" },
  { label: "Pending", value: "pending" },
  { label: "Processing", value: "processing" },
  { label: "Completed", value: "completed" },
  { label: "Failed", value: "failed" },
  { label: "Canceled", value: "canceled" }
];

export const callLogPageSizeOptions: Array<{ label: string; value: string }> = [
  { label: "25", value: "25" },
  { label: "50", value: "50" },
  { label: "100", value: "100" }
];
