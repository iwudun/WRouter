import * as React from "react";
import { cn } from "@/lib/utils";

const Card = React.forwardRef<HTMLDivElement, React.HTMLAttributes<HTMLDivElement>>(
  ({ className, style, ...props }, ref) => (
    <div
      // 20261104_02:边框必须显式给颜色 —— Tailwind v4 的裸 `border` 只输出
      // border-style/width,不输出 border-color,颜色落到 CSS 初始值 currentColor;
      // 本组件自带 text-card-foreground(暗色 #ededf1 近白),于是描边变成白边。
      // 与「模型管理」页(providers-list.tsx 的 `border border-border`)对齐,
      // 统一走 --border token(暗色 #333339 暗灰细线)。
      className={cn("overflow-hidden rounded-lg border border-border bg-card text-card-foreground transition-shadow duration-200 shadow-card", className)}
      ref={ref}
      style={style}
      {...props}
    />
  )
);
Card.displayName = "Card";

const CardHeader = React.forwardRef<HTMLDivElement, React.HTMLAttributes<HTMLDivElement>>(
  ({ className, ...props }, ref) => (
    <div className={cn("flex min-h-12 flex-col justify-center border-b border-border/60 px-4 py-3", className)} ref={ref} {...props} />
  )
);
CardHeader.displayName = "CardHeader";

const CardTitle = React.forwardRef<HTMLHeadingElement, React.HTMLAttributes<HTMLHeadingElement>>(
  ({ className, ...props }, ref) => (
    <h2 className={cn("truncate text-title font-semibold tracking-[-0.01em]", className)} ref={ref} {...props} />
  )
);
CardTitle.displayName = "CardTitle";

const CardContent = React.forwardRef<HTMLDivElement, React.HTMLAttributes<HTMLDivElement>>(
  ({ className, ...props }, ref) => (
    <div className={cn("p-4", className)} ref={ref} {...props} />
  )
);
CardContent.displayName = "CardContent";

export { Card, CardContent, CardHeader, CardTitle };
