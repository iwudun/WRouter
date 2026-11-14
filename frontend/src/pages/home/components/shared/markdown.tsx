import { memo } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

/** 响应正文 markdown 渲染(观感要求)。
 *  组件映射手工承接排版,不引入 typography 插件 —— 详情页只需紧凑的
 *  阅读排版,风格走 WRouter 既有 token(text-caption/body、border-border)。 */
export const MarkdownContent = memo(function MarkdownContent({ text }: { text: string }) {
  return (
    <div className="min-w-0 text-body leading-relaxed">
      <ReactMarkdown
        components={{
          a: (props) => <a className="text-primary underline underline-offset-2" rel="noreferrer" target="_blank" {...props} />,
          blockquote: (props) => <blockquote className="border-l-2 border-border pl-3 text-muted-foreground" {...props} />,
          code: ({ className, children, ...props }: { className?: string; children?: React.ReactNode }) => {
            const isBlock = /language-/.test(className ?? "");
            if (!isBlock) {
              return <code className="rounded border border-border/60 bg-muted/40 px-1 py-0.5 font-mono text-caption" {...props}>{children}</code>;
            }
            return <code className={cnBlock(className)} {...props}>{children}</code>;
          },
          h1: (props) => <h1 className="mb-2 mt-4 text-title font-semibold first:mt-0" {...props} />,
          h2: (props) => <h2 className="mb-2 mt-4 text-body-lg font-semibold first:mt-0" {...props} />,
          h3: (props) => <h3 className="mb-1.5 mt-3 text-body font-semibold first:mt-0" {...props} />,
          h4: (props) => <h4 className="mb-1.5 mt-3 text-body font-semibold first:mt-0" {...props} />,
          hr: () => <hr className="my-3 border-border/60" />,
          img: (props) => <img className="max-w-full rounded-md border border-border/60" {...props} />,
          li: (props) => <li className="ml-4 list-disc marker:text-muted-foreground [&>ol>&]:list-decimal" {...props} />,
          ol: (props) => <ol className="my-2 list-decimal space-y-1 pl-2" {...props} />,
          p: (props) => <p className="my-2 first:mt-0 last:mb-0 whitespace-pre-wrap" {...props} />,
          pre: (props) => <pre className="network-code my-2 max-w-full overflow-auto rounded-md border border-border/60 p-3 font-mono text-caption leading-5" {...props} />,
          table: (props) => (
            <div className="my-2 overflow-x-auto rounded-md border border-border/60">
              <table className="w-full border-collapse text-caption" {...props} />
            </div>
          ),
          tbody: (props) => <tbody className="divide-y divide-border/50" {...props} />,
          td: (props) => <td className="px-2.5 py-1.5 align-top" {...props} />,
          th: (props) => <th className="bg-muted/30 px-2.5 py-1.5 text-left font-semibold" {...props} />,
          ul: (props) => <ul className="my-2 list-disc space-y-1 pl-2" {...props} />
        }}
        remarkPlugins={[remarkGfm]}
      >
        {text}
      </ReactMarkdown>
    </div>
  );
});

function cnBlock(className?: string): string {
  return ["font-mono text-caption", className].filter(Boolean).join(" ");
}
