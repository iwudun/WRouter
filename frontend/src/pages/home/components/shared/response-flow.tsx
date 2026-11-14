import { useMemo, useState } from "react";
import { ChevronDown, Copy } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { copyTextToClipboard, useAppText } from "../../shared";
import type { ResponsePreviewSection, WireProtocol } from "../../shared/request-body-preview";
import { MarkdownContent } from "./markdown";

/** 响应正文的消息式渲染:
 *  思考琥珀折叠块 → 正文 markdown → 工具调用卡片;refusal 红块。 */
export function ResponseFlowView({ protocol, sections }: { protocol: WireProtocol; sections: ResponsePreviewSection[] }) {
  const t = useAppText();
  const grouped = useMemo(() => ({
    content: sections.filter((section) => section.kind === "content").map((section) => section.text).filter(Boolean).join("\n\n"),
    reasoning: sections.filter((section) => section.kind === "reasoning").map((section) => section.text).filter(Boolean).join("\n\n"),
    refusal: sections.filter((section) => section.kind === "refusal").map((section) => section.text).filter(Boolean).join("\n\n"),
    tools: sections.filter((section) => section.kind === "tool"),
  }), [sections]);

  if (!grouped.content && !grouped.reasoning && !grouped.refusal && !grouped.tools.length) return null;
  return (
    <div className="space-y-4 rounded-xl border border-border/60 bg-muted/10 p-4">
      {grouped.reasoning ? <ReasoningBlock text={grouped.reasoning} /> : null}
      {grouped.content ? <MarkdownContent text={grouped.content} /> : null}
      {grouped.refusal ? (
        <div className="rounded-md border border-red-500/30 bg-red-500/5 p-3 text-body text-red-600 dark:text-red-400">{grouped.refusal}</div>
      ) : null}
      {grouped.tools.length ? (
        <div className="space-y-3">
          {grouped.tools.map((section, index) => (
            <ResponseToolCard arguments={section.text} key={`${section.name}-${index}`} name={section.name ?? "tool"} />
          ))}
        </div>
      ) : null}
      {protocol === "unknown" ? <Badge variant="outline">{t("未识别协议,按原文解析")}</Badge> : null}
    </div>
  );
}

/** 思考折叠块(琥珀虚线框;默认收起,点击展开)。 */
function ReasoningBlock({ text }: { text: string }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="rounded-md border border-dashed border-amber-500/40 bg-amber-500/5 p-2.5">
      <button className="flex w-full items-center gap-1.5 text-caption font-semibold tracking-wider text-amber-600 dark:text-amber-400" onClick={() => setOpen((value) => !value)} type="button">
        <ChevronDown className={`h-3.5 w-3.5 transition-transform${open ? " rotate-180" : ""}`} />
        ◆ REASONING
        <span className="ml-auto font-normal normal-case opacity-70">{text.length.toLocaleString()} chars</span>
      </button>
      {open ? <div className="mt-2"><MarkdownContent text={text} /></div> : null}
    </div>
  );
}

function ResponseToolCard({ arguments: args, name }: { arguments: string; name: string }) {
  const t = useAppText();
  const [copied, setCopied] = useState(false);
  const pretty = useMemo(() => {
    try {
      return JSON.stringify(JSON.parse(args), null, 2);
    } catch {
      return args;
    }
  }, [args]);
  return (
    <div className="overflow-hidden rounded-md border border-border/60 border-l-4 border-l-purple-500/40 bg-background/60">
      <div className="flex items-center gap-2 border-b border-border/60 px-3 py-2">
        <span className="font-mono text-body font-semibold text-purple-600 dark:text-purple-400">{name}</span>
        <Badge variant="outline">{t("工具调用")}</Badge>
        <button
          className="ml-auto flex items-center gap-1 text-caption text-muted-foreground hover:text-foreground"
          onClick={() => void copyTextToClipboard(pretty).then(() => { setCopied(true); window.setTimeout(() => setCopied(false), 1200); })}
          type="button"
        >
          <Copy className="h-3 w-3" />{copied ? t("已复制") : t("复制参数")}
        </button>
      </div>
      <pre className="max-h-[320px] overflow-auto whitespace-pre-wrap break-words p-3 font-mono text-caption leading-5">{pretty}</pre>
    </div>
  );
}
