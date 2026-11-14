import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { ArrowUp, ChevronDown, ChevronsDownUp, ChevronsUpDown, FileText, ListTree, Search, Wrench } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { copyTextToClipboard, formatCompactNumber, useAppNumberLocale, useAppText } from "../../shared";
import type { ConversationMessage, ParsedToolCall, PreviewParams, PreviewTool, RequestBodyPreview } from "../../shared/request-body-preview";

/** 会话查看器的筛选/开关设置(2026-10-17 从组件内 state 提升为受控):
 *  控件由宿主工具栏行渲染([ConversationToolbarControls]),状态随宿主
 *  (tab 切换/重挂载即重置,与迁移前内部 state 生命周期一致)。 */
export type ConversationViewSettings = {
  search: string;
  roleFilter: string;
  showReasoning: boolean;
  showToolArgs: boolean;
  showToolResult: boolean;
  showSystem: boolean;
  expandAll: boolean;
};

export function createConversationViewSettings(): ConversationViewSettings {
  return {
    expandAll: false,
    roleFilter: "all",
    search: "",
    showReasoning: true,
    showSystem: true,
    showToolArgs: true,
    showToolResult: true
  };
}

/** 会话查看器:
 *  统计格 + 角色侧导航 + 彩色消息卡(思考/工具跳转) + Tools 声明区块 +
 *  回到顶部;筛选/开关控件已迁出到宿主工具栏行([ConversationToolbarControls]),
 *  不再渲染 sticky 工具栏。 */
export function ConversationViewer({ onSettingsChange, preview, settings }: {
  onSettingsChange: (patch: Partial<ConversationViewSettings>) => void;
  preview: RequestBodyPreview;
  settings: ConversationViewSettings;
}) {
  const t = useAppText();
  const numberLocale = useAppNumberLocale();
  const messages = preview.messages ?? [];
  const tools = preview.tools ?? [];
  const [rawOpenIndex, setRawOpenIndex] = useState<number | null>(null);
  const [showBackTop, setShowBackTop] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);

  const jumpTo = useCallback((index: number) => {
    document.getElementById(`conv-msg-${index}`)?.scrollIntoView({ behavior: "smooth", block: "start" });
  }, []);

  // 滚动监听/回顶都作用于最近的滚动祖先(详情页是内滚动容器)。
  useEffect(() => {
    const scroller = findScrollAncestor(rootRef.current);
    if (!scroller) return;
    const onScroll = () => setShowBackTop(scroller.scrollTop > 400);
    scroller.addEventListener("scroll", onScroll, { passive: true });
    return () => scroller.removeEventListener("scroll", onScroll);
  }, []);

  const toolCallByCallId = useMemo(() => {
    const map = new Map<string, number>();
    for (const message of messages) for (const call of message.toolCalls ?? []) if (call.id) map.set(call.id, message.index);
    return map;
  }, [messages]);
  const toolResultByCallId = useMemo(() => {
    const map = new Map<string, number>();
    for (const message of messages) {
      for (const result of message.toolResults ?? []) if (result.callId) map.set(result.callId, message.index);
      if (message.role === "tool" && message.toolCallId) map.set(message.toolCallId, message.index);
    }
    return map;
  }, [messages]);

  const visibleMessages = useMemo(() => {
    const query = settings.search.trim().toLowerCase();
    return messages.filter((message) => {
      if (settings.roleFilter !== "all" && message.role !== settings.roleFilter) return false;
      if (message.role === "system" && !settings.showSystem) return false;
      if (!query) return true;
      return [message.content, message.reasoning ?? "", ...(message.toolCalls ?? []).flatMap((call) => [call.name, call.arguments])].join("\n").toLowerCase().includes(query);
    });
  }, [messages, settings]);

  const sidebarGroups = useMemo(() => {
    // 侧导航(「内容目录」)与消息流同源:被筛选掉的消息不出现,保证
    // 「所见即可导航」。2026-10-18 改三块结构:系统 / 对话 / 工具声明;
    // 对话块保持原始顺序,条目前置角色图标区分 user/assistant/tool。
    const system = visibleMessages.filter((message) => message.role === "system");
    const dialogue = visibleMessages.filter((message) => message.role !== "system");
    const blocks: Array<{ key: string; label: string; icon: string; entries: Array<{ target: string; role: string | null; index: number | null; preview: string }> }> = [];
    if (system.length) {
      blocks.push({
        key: "system", icon: ROLE_ICON.system, label: "系统",
        entries: system.map((message) => ({ target: String(message.index), role: message.role, index: message.index, preview: messagePreview(message) })),
      });
    }
    if (dialogue.length) {
      blocks.push({
        key: "dialogue", icon: "💬", label: "对话",
        entries: dialogue.map((message) => ({ target: String(message.index), role: message.role, index: message.index, preview: messagePreview(message) })),
      });
    }
    if (tools.length) {
      blocks.push({
        key: "tools", icon: ROLE_ICON.tool, label: "工具声明",
        entries: tools.map((tool) => ({ target: "conv-tools", role: null, index: null, preview: tool.name })),
      });
    }
    return blocks;
  }, [visibleMessages, tools]);

  const jumpToDirectoryEntry = useCallback((target: string) => {
    if (target === "conv-tools") {
      document.getElementById(target)?.scrollIntoView({ behavior: "smooth", block: "start" });
      return;
    }
    const index = Number(target);
    if (Number.isFinite(index)) jumpTo(index);
  }, [jumpTo]);

  const roleCounts = useMemo(() => {
    const counts = new Map<string, number>();
    for (const message of messages) counts.set(message.role, (counts.get(message.role) ?? 0) + 1);
    return [...counts.entries()];
  }, [messages]);
  const totalChars = messages.reduce((sum, message) => sum + message.content.length, 0);
  const totalToolCalls = messages.reduce((sum, message) => sum + (message.toolCalls?.length ?? 0), 0);
  const params: PreviewParams = preview.params ?? {};

  return (
    <div className="space-y-4" ref={rootRef}>
      {/* 筛选/开关控件在宿主工具栏行(ConversationToolbarControls);
          这里直接从统计格开始。 */}

      {/* 统计格 */}
      <div className="grid grid-cols-2 gap-2 sm:grid-cols-3 lg:grid-cols-5">
        <Stat label={t("消息")} value={formatCompactNumber(messages.length, numberLocale)} />
        <Stat jumpId="conv-tools" label={t("工具声明")} onJump={jumpToId(jumpTo)} value={formatCompactNumber(tools.length, numberLocale)} />
        <Stat label={t("工具调用")} value={formatCompactNumber(totalToolCalls, numberLocale)} />
        <Stat label="max_tokens" value={params.maxTokens != null ? formatCompactNumber(params.maxTokens, numberLocale) : "—"} />
        <Stat label="stream" value={params.stream != null ? String(params.stream) : "—"} />
        <Stat label="tool_choice" value={params.toolChoice === undefined ? "—" : typeof params.toolChoice === "string" ? params.toolChoice : JSON.stringify(params.toolChoice)} />
        <Stat label={t("字符数")} value={formatCompactNumber(totalChars, numberLocale)} />
        <Stat label="≈Tokens" value={formatCompactNumber(Math.round(totalChars / 3.5), numberLocale)} />
        <Stat label={t("角色分布")} value={<span className="flex flex-wrap gap-1">{roleCounts.map(([role, count]) => <span className={cnRolePill(role)} key={role}>{role} {count}</span>)}</span>} />
      </div>

      {/* 侧导航(内容目录) + 消息流 */}
      <div className="grid grid-cols-1 gap-4 lg:grid-cols-[220px_1fr]">
        <div className="hidden lg:block">
          {/* 工具栏迁出后容器顶即列表顶,sticky 偏移归零。 */}
          <div className="sticky top-0 max-h-[calc(100vh-220px)] overflow-y-auto rounded-lg border border-border/60 bg-muted/20 p-2.5">
            <div className="mb-2 flex items-center gap-1.5 border-b border-border/50 pb-1.5 text-caption font-semibold text-foreground/90">
              <ListTree className="h-3.5 w-3.5 shrink-0 text-primary" />
              {t("内容目录")}
            </div>
            {sidebarGroups.length === 0 ? (
              <div className="px-1 py-2 text-caption text-muted-foreground">{t("无匹配消息")}</div>
            ) : sidebarGroups.map((block) => (
              <div className="mb-2.5 last:mb-0" key={block.key}>
                <div className="flex items-center gap-1.5 px-0.5 py-0.5 text-caption font-bold uppercase tracking-wider text-muted-foreground">
                  <span aria-hidden className="text-caption not-italic">{block.icon}</span>
                  {t(block.label)}
                  <span className="font-normal opacity-70">({block.entries.length})</span>
                </div>
                <div className="ml-2 space-y-px border-l border-border/60 pl-1.5">
                  {block.entries.map((entry, entryIndex) => (
                    <button
                      className="flex w-full items-center gap-1.5 rounded px-1 py-0.5 text-left font-mono text-caption text-muted-foreground hover:bg-muted/50 hover:text-foreground"
                      key={`${entry.target}-${entryIndex}`}
                      onClick={() => jumpToDirectoryEntry(entry.target)}
                      type="button"
                    >
                      {entry.role ? <span aria-hidden className="shrink-0 text-caption">{ROLE_ICON[entry.role] ?? "◇"}</span> : <span aria-hidden className="w-[11px] shrink-0" />}
                      {entry.index != null ? <span className="w-6 shrink-0 text-right opacity-60">#{entry.index}</span> : null}
                      <span className="truncate">{entry.preview}</span>
                    </button>
                  ))}
                </div>
              </div>
            ))}
          </div>
        </div>
        <div className="min-w-0 space-y-5">
          <div>
            <div className="mb-2.5 text-body font-semibold text-muted-foreground">{t("消息")} <span className="opacity-70">({visibleMessages.length})</span></div>
            {visibleMessages.length === 0 ? (
              <div className="flex h-32 items-center justify-center rounded-lg border border-border/60 bg-muted/20 text-body text-muted-foreground">{t("无匹配消息")}</div>
            ) : (
              <div className="space-y-3">
                {visibleMessages.map((message) => (
                  <MessageCard
                    expandAll={settings.expandAll}
                    key={message.index}
                    message={message}
                    rawOpen={rawOpenIndex === message.index}
                    showReasoning={settings.showReasoning}
                    showToolArgs={settings.showToolArgs}
                    showToolResult={settings.showToolResult}
                    toolCallByCallId={toolCallByCallId}
                    toolResultByCallId={toolResultByCallId}
                    onToggleRaw={(index) => setRawOpenIndex((current) => (current === index ? null : index))}
                    jumpTo={jumpTo}
                  />
                ))}
              </div>
            )}
          </div>
          <div className="scroll-mt-24" id="conv-tools">
            <div className="mb-2.5 text-body font-semibold text-muted-foreground">{t("工具声明")} <span className="opacity-70">({tools.length})</span></div>
            {tools.length === 0 ? (
              <div className="flex h-20 items-center justify-center rounded-lg border border-border/60 bg-muted/20 text-body text-muted-foreground">{t("请求未声明工具")}</div>
            ) : (
              <div className="space-y-2">{tools.map((tool, index) => <ToolDeclarationCard key={`${tool.name}-${index}`} tool={tool} />)}</div>
            )}
          </div>
        </div>
      </div>

      {showBackTop ? (
        <Button
          aria-label={t("回到顶部")}
          className="fixed bottom-5 right-5 z-50 h-11 w-11 rounded-full shadow-lg"
          onClick={() => findScrollAncestor(rootRef.current)?.scrollTo({ top: 0, behavior: "smooth" })}
          size="icon"
          title={t("回到顶部")}
          variant="outline"
        >
          <ArrowUp className="h-5 w-5" />
        </Button>
      ) : null}
    </div>
  );
}

/** 会话筛选/开关控件(2026-10-17 自原 sticky 工具栏迁出):渲染在宿主的
 *  「对话阅览/JSON 原文」工具栏行里右对齐;设置经 onSettingsChange 上抛,
 *  由宿主持有(会话模式激活时才渲染)。
 *  2026-10-18:删模型名徽章(与概览卡「请求模型」重复)。 */
export function ConversationToolbarControls({ onSettingsChange, preview, settings }: {
  onSettingsChange: (patch: Partial<ConversationViewSettings>) => void;
  preview: RequestBodyPreview;
  settings: ConversationViewSettings;
}) {
  const t = useAppText();
  const roleCounts = useMemo(() => {
    const counts = new Map<string, number>();
    for (const message of preview.messages ?? []) counts.set(message.role, (counts.get(message.role) ?? 0) + 1);
    return [...counts.entries()];
  }, [preview.messages]);
  return (
    <div className="flex flex-wrap items-center gap-2">
      <div className="relative">
        <Search className="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
        <Input
          className="h-8 w-40 pl-8 text-caption md:w-52"
          onChange={(event) => onSettingsChange({ search: event.target.value })}
          placeholder={t("搜索消息")}
          value={settings.search}
        />
      </div>
      <select
        className="h-8 rounded-md border border-border bg-muted/40 px-2 text-caption text-muted-foreground outline-none"
        onChange={(event) => onSettingsChange({ roleFilter: event.target.value })}
        value={settings.roleFilter}
      >
        <option value="all">{t("全部角色")}</option>
        {roleCounts.map(([role]) => <option key={role} value={role}>{role}</option>)}
      </select>
      <ToolbarToggle checked={settings.showReasoning} label={t("思考")} onChange={(next) => onSettingsChange({ showReasoning: next })} />
      <ToolbarToggle checked={settings.showToolArgs} label={t("工具参数")} onChange={(next) => onSettingsChange({ showToolArgs: next })} />
      <ToolbarToggle checked={settings.showToolResult} label={t("工具结果")} onChange={(next) => onSettingsChange({ showToolResult: next })} />
      <ToolbarToggle checked={settings.showSystem} label="system" onChange={(next) => onSettingsChange({ showSystem: next })} />
      <Button
        className="h-8 px-2.5 text-caption"
        onClick={() => onSettingsChange({ expandAll: !settings.expandAll })}
        size="sm"
        variant="outline"
      >
        {settings.expandAll ? <ChevronsDownUp className="h-3.5 w-3.5" /> : <ChevronsUpDown className="h-3.5 w-3.5" />}
        {settings.expandAll ? t("全部折叠") : t("全部展开")}
      </Button>
    </div>
  );
}

const ROLE_PILL: Record<string, string> = {
  assistant: "bg-green-500/10 text-green-600 dark:text-green-400",
  system: "bg-purple-500/10 text-purple-600 dark:text-purple-400",
  tool: "bg-orange-500/10 text-orange-600 dark:text-orange-400",
  user: "bg-blue-500/10 text-blue-600 dark:text-blue-400",
};
const ROLE_BORDER: Record<string, string> = {
  assistant: "border-green-500/40",
  system: "border-purple-500/40",
  tool: "border-orange-500/40",
  user: "border-blue-500/40",
};
const ROLE_ICON: Record<string, string> = { assistant: "🤖", system: "⚙", tool: "🔧", user: "👤" };

function cnRolePill(role: string): string {
  return ["inline-flex items-center rounded-full px-1.5 py-0.5 font-mono text-caption", ROLE_PILL[role] ?? "bg-muted text-muted-foreground"].join(" ");
}

function messagePreview(message: ConversationMessage): string {
  return (message.content || (message.toolCalls ?? []).map((call) => call.name).join(", ") || (message.reasoning ? "…" : "")).replace(/\n/g, " ").slice(0, 44);
}

function findScrollAncestor(node: HTMLElement | null): HTMLElement | null {
  let current: HTMLElement | null = node;
  while (current) {
    if (/(auto|scroll|overlay)/.test(getComputedStyle(current).overflowY)) return current;
    current = current.parentElement;
  }
  return null;
}

function ToolbarToggle({ checked, label, onChange }: { checked: boolean; label: string; onChange: (next: boolean) => void }) {
  return (
    <label className="flex cursor-pointer items-center gap-1.5 whitespace-nowrap text-caption text-muted-foreground">
      <input checked={checked} className="accent-blue-500" onChange={(event) => onChange(event.target.checked)} type="checkbox" />
      {label}
    </label>
  );
}

function Stat({ label, value, jumpId, onJump }: { label: string; value: ReactNode; jumpId?: string; onJump?: (id: string) => void }) {
  return (
    <div
      className={`rounded-lg border border-border/60 bg-muted/20 px-3 py-2${jumpId ? " cursor-pointer hover:border-primary" : ""}`}
      onClick={jumpId && onJump ? () => onJump(jumpId) : undefined}
      role={jumpId ? "button" : undefined}
    >
      <div className="text-micro uppercase tracking-wider text-muted-foreground">{label}</div>
      <div className="mt-0.5 truncate font-mono text-body font-semibold">{value}</div>
    </div>
  );
}

function jumpToId(jump: (index: number) => void) {
  return (id: string) => {
    if (id === "conv-tools") {
      document.getElementById(id)?.scrollIntoView({ behavior: "smooth", block: "start" });
      return;
    }
    const index = Number(id);
    if (Number.isFinite(index)) jump(index);
  };
}

/** 长文本块:1000 字符截断 + 渐变遮罩 + 展开(受全局「全部展开」控制)。 */
function CollapseBlock({ text, expandAll }: { text: string; expandAll: boolean }) {
  const t = useAppText();
  const [expanded, setExpanded] = useState(false);
  const long = text.length > 1000;
  const collapsed = long && !expandAll && !expanded;
  return (
    <div className="relative">
      <div className={collapsed ? "max-h-60 overflow-hidden" : undefined}>
        <pre className="whitespace-pre-wrap break-words font-mono text-body leading-relaxed text-foreground">{text}</pre>
      </div>
      {collapsed ? <div className="pointer-events-none absolute inset-x-0 bottom-7 h-14 bg-gradient-to-t from-background to-transparent" /> : null}
      {long && !expandAll ? (
        <button className="mt-1 flex w-full items-center justify-center gap-1 rounded-md border border-border/60 bg-muted/40 px-2 py-1 text-caption text-muted-foreground transition-colors hover:text-foreground" onClick={() => setExpanded((value) => !value)} type="button">
          {expanded ? `${t("收起")} ▲` : `${t("展开全部")}（${text.length.toLocaleString()} chars）▼`}
        </button>
      ) : null}
    </div>
  );
}

/** 多模态 part:文本正常渲染,image/audio 打标,其余 part 展示 JSON。 */
const SKIP_PART_TYPES = new Set(["thinking", "redacted_thinking", "tool_use", "tool_result", "functionCall", "functionResponse", "reasoning"]);

function ContentParts({ parts, expandAll }: { parts: unknown[]; expandAll: boolean }) {
  return (
    <div className="space-y-2">
      {parts.map((part, index) => {
        if (typeof part === "string") return <CollapseBlock expandAll={expandAll} key={index} text={part} />;
        if (typeof part !== "object" || part === null) return null;
        const record = part as Record<string, unknown>;
        const type = typeof record.type === "string" ? record.type : undefined;
        if (type && SKIP_PART_TYPES.has(type)) return null;
        if ((type === "text" || type === "input_text" || type === "output_text") && typeof record.text === "string") {
          return <CollapseBlock expandAll={expandAll} key={index} text={record.text} />;
        }
        if (type === "image_url" || type === "image" || record.image_url || record.source) {
          return <MediaPartBadge key={index} label="[image]" />;
        }
        if (type === "input_audio" || type === "audio") {
          return <MediaPartBadge key={index} label="[audio]" />;
        }
        return <pre className="overflow-x-auto rounded-md border border-border/60 bg-muted/30 p-2 font-mono text-caption text-muted-foreground" key={index}>{JSON.stringify(part, null, 2)}</pre>;
      })}
    </div>
  );
}

function MediaPartBadge({ label }: { label: string }) {
  return <div className="inline-flex items-center gap-1 rounded-md border border-border/60 bg-muted/30 px-2 py-1 font-mono text-caption text-muted-foreground">{label}</div>;
}

function ToolCallCard({ call, resultIndex, showArgs, jumpTo }: { call: ParsedToolCall; resultIndex?: number; showArgs: boolean; jumpTo: (index: number) => void }) {
  const t = useAppText();
  const [argsOpen, setArgsOpen] = useState(false);
  return (
    <div className="rounded-md border border-border/60 border-l-4 border-l-purple-500/40 bg-muted/30 p-2.5 pl-3">
      <div className="flex flex-wrap items-center gap-2">
        <span className="font-mono text-body font-semibold text-purple-600 dark:text-purple-400">{call.name}</span>
        {call.id ? <span className="font-mono text-caption text-muted-foreground">{call.id}</span> : null}
        {resultIndex !== undefined ? (
          <button className="ml-auto text-caption text-muted-foreground underline decoration-dotted underline-offset-2 hover:text-foreground" onClick={() => jumpTo(resultIndex)} type="button">
            {t("跳转工具结果")} #{resultIndex} ↓
          </button>
        ) : null}
      </div>
      {showArgs && call.arguments ? (
        <>
          <button className="mt-1.5 text-caption font-medium text-muted-foreground hover:text-foreground" onClick={() => setArgsOpen((value) => !value)} type="button">
            {argsOpen ? `${t("收起参数")} ▲` : `${t("查看参数 JSON")} ▼`}
          </button>
          {argsOpen ? <pre className="mt-1 overflow-x-auto rounded-md border border-border/60 bg-muted/40 p-2 font-mono text-caption text-muted-foreground">{call.arguments}</pre> : null}
        </>
      ) : null}
    </div>
  );
}

function ToolResultCard({ content, callIndex, jumpTo }: { content: string; callIndex?: number; jumpTo: (index: number) => void }) {
  const t = useAppText();
  return (
    <div className="rounded-md border border-border/60 border-l-4 border-l-orange-500/40 bg-muted/30 p-2.5 pl-3">
      <div className="mb-1 flex flex-wrap items-center gap-2">
        <span className="text-caption font-semibold tracking-wider text-orange-600 dark:text-orange-400">{t("工具结果").toUpperCase()}</span>
        {callIndex !== undefined ? (
          <button className="font-mono text-caption text-muted-foreground underline decoration-dotted underline-offset-2 hover:text-foreground" onClick={() => jumpTo(callIndex)} title={t("跳转工具调用")} type="button">
            #{callIndex} ↑
          </button>
        ) : null}
      </div>
      <CollapseBlock expandAll={false} text={content} />
    </div>
  );
}

function MessageCard({ expandAll, jumpTo, message, rawOpen, showReasoning, showToolArgs, showToolResult, toolCallByCallId, toolResultByCallId, onToggleRaw }: {
  expandAll: boolean;
  jumpTo: (index: number) => void;
  message: ConversationMessage;
  rawOpen: boolean;
  showReasoning: boolean;
  showToolArgs: boolean;
  showToolResult: boolean;
  toolCallByCallId: Map<string, number>;
  toolResultByCallId: Map<string, number>;
  onToggleRaw: (index: number) => void;
}) {
  const t = useAppText();
  const role = message.role || "tool";
  const headMeta: string[] = [];
  if (message.content.length) headMeta.push(`${message.content.length.toLocaleString()} chars`);
  if (message.reasoning?.length) headMeta.push(`reasoning ${message.reasoning.length.toLocaleString()}`);
  if (message.toolCalls?.length) headMeta.push(`${message.toolCalls.length} tool call(s)`);

  let body: ReactNode = null;
  if (role !== "tool") {
    if (message.contentParts?.length) body = <ContentParts expandAll={expandAll} parts={message.contentParts} />;
    else if (message.content) body = <CollapseBlock expandAll={expandAll} text={message.content} />;
  }
  if (message.reasoning && showReasoning) {
    body = (
      <div className="space-y-2.5">
        <details className="rounded-md border border-dashed border-amber-500/40 bg-amber-500/5 p-2.5" open>
          <summary className="cursor-pointer text-caption font-semibold tracking-wider text-amber-600 dark:text-amber-400">◆ REASONING</summary>
          <div className="mt-1.5"><CollapseBlock expandAll={expandAll} text={message.reasoning} /></div>
        </details>
        {body}
      </div>
    );
  }
  if (message.toolCalls?.length) {
    body = (
      <div className="space-y-2.5">
        {body}
        <div className="space-y-1.5">
          {message.toolCalls.map((call, index) => (
            <ToolCallCard call={call} jumpTo={jumpTo} key={call.id ?? index} resultIndex={call.id ? toolResultByCallId.get(call.id) : undefined} showArgs={showToolArgs} />
          ))}
        </div>
      </div>
    );
  }
  if (role === "tool" && showToolResult) {
    const callSource = message.toolCallId ? toolCallByCallId.get(message.toolCallId) : undefined;
    const results = message.toolResults;
    body = (
      <div className="space-y-2.5">
        {body}
        {(results?.length ? results : [{ callId: message.toolCallId, content: message.content, isError: false }]).map((result, index) => (
          <ToolResultCard callIndex={result.callId ? toolCallByCallId.get(result.callId) : callSource} content={result.content} jumpTo={jumpTo} key={index} />
        ))}
      </div>
    );
  }
  if (!body) body = <span className="text-body italic text-muted-foreground">({t("空")})</span>;

  return (
    <div className={`overflow-hidden rounded-lg border border-border/60 bg-muted/20 ${ROLE_BORDER[role] ?? ""}`} id={`conv-msg-${message.index}`}>
      <div className="flex flex-wrap items-center gap-2 border-b border-border/60 px-3 py-2">
        <span className={cnRolePill(role)}>{role}</span>
        <span className="font-mono text-caption text-muted-foreground">#{message.index}</span>
        {message.itemId ? <span className="truncate font-mono text-caption text-muted-foreground" title={message.itemId}>{message.itemId}</span> : null}
        <div className="ml-auto flex flex-wrap items-center gap-1.5">
          {headMeta.map((meta) => <span className="rounded-full border border-border/60 bg-muted/40 px-2 py-0.5 font-mono text-caption text-muted-foreground" key={meta}>{meta}</span>)}
          <Button className="h-6 w-6" onClick={() => onToggleRaw(message.index)} size="icon" title={t("查看原始 JSON")} variant="ghost">
            <FileText className="h-3.5 w-3.5" />
          </Button>
        </div>
      </div>
      {rawOpen ? (
        <div className="border-b border-border/60 bg-muted/40 px-3 py-2.5">
          <div className="mb-1 flex items-center justify-between font-mono text-caption text-muted-foreground">
            <span>Raw JSON — message #{message.index}</span>
            <button className="underline decoration-dotted underline-offset-2 hover:text-foreground" onClick={() => void copyTextToClipboard(JSON.stringify(message.raw, null, 2))} type="button">{t("Copy")}</button>
          </div>
          <pre className="max-h-72 overflow-auto whitespace-pre-wrap break-words font-mono text-caption leading-relaxed text-muted-foreground">{safePrettyJson(message.raw)}</pre>
        </div>
      ) : null}
      <div className="space-y-2.5 p-3">{body}</div>
    </div>
  );
}

function ToolDeclarationCard({ tool }: { tool: PreviewTool }) {
  const params = typeof tool.parameters === "object" && tool.parameters !== null && !Array.isArray(tool.parameters) ? tool.parameters as Record<string, unknown> : {};
  const required = Array.isArray(params.required) ? params.required.join(", ") : "";
  const propCount = params.properties && typeof params.properties === "object" ? Object.keys(params.properties as Record<string, unknown>).length : 0;
  const [open, setOpen] = useState(false);
  return (
    <div className="overflow-hidden rounded-lg border border-border/60 bg-muted/20">
      <button className="flex w-full cursor-pointer items-center gap-3 px-3.5 py-2.5 text-left" onClick={() => setOpen((value) => !value)} type="button">
        <Wrench className="h-4 w-4 shrink-0 text-cyan-600 dark:text-cyan-400" />
        <span className="font-mono text-body-lg font-semibold text-cyan-700 dark:text-cyan-300">{tool.name}</span>
        {tool.description ? <span className="min-w-0 flex-1 truncate text-caption text-muted-foreground">{tool.description.replace(/\s+/g, " ").slice(0, 140)}{tool.description.length > 140 ? "…" : ""}</span> : null}
        <span className="shrink-0 font-mono text-caption text-muted-foreground">{propCount} props{required ? ` · required: ${required}` : ""}</span>
        <ChevronDown className={`h-4 w-4 shrink-0 text-muted-foreground transition-transform${open ? " rotate-180" : ""}`} />
      </button>
      {open ? (
        <div className="border-t border-border/60 p-3.5">
          {tool.description ? <div className="mb-2.5 whitespace-pre-wrap text-caption text-muted-foreground">{tool.description}</div> : null}
          <div className="mb-1 font-mono text-caption text-muted-foreground">
            Parameters (JSON Schema) · required: <span className="text-orange-600 dark:text-orange-400">{required || "—"}</span>
          </div>
          <pre className="overflow-x-auto rounded-md border border-border/60 bg-muted/40 p-2.5 font-mono text-caption text-muted-foreground">{safePrettyJson(tool.parameters)}</pre>
        </div>
      ) : null}
    </div>
  );
}

function safePrettyJson(value: unknown): string {
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}
