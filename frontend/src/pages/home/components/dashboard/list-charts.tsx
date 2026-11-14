// R7 split: measured chart frame shared by the dashboard views. Extracted
// from the original dashboard.tsx; the unused tooltip/label primitives were
// removed with the agent-analysis feature (2026-08).
//
// F1.5: this module also hosts the chart primitives that BOTH the eager
// usage-stats view and the lazy usage-stats-charts chunk need
// (ChartEmptyState, MODEL_PALETTE). They live here — not in usage-stats.tsx —
// so the lazy chunk can import them without a static edge back to the eager
// module (which would defeat the code split).

import { Inbox } from "lucide-react";
import type { ReactNode } from "react";
import { useLayoutEffect, useRef, useState } from "react";
import { cn } from "@/lib/utils";

// 模型配色：暖琥珀主调（琥珀/金/陶土）+ 鼠尾草绿与暖蓝做冷色区分，
// 末位暖灰留给「其他」。中等明度，暗/浅双主题下均可读。
export const MODEL_PALETTE = ["#e8944f", "#d9b04c", "#cf6f4a", "#8fae7e", "#7aa2d6", "#a3907c"];

export function ChartEmptyState({
  className,
  compact = false,
  label
}: {
  className?: string;
  compact?: boolean;
  label: string;
}) {
  return (
    <div className={cn(
      "overview-empty-state overview-nested-surface flex min-h-0 flex-col items-center justify-center border border-dashed px-4 text-center text-muted-foreground",
      compact ? "py-7" : "py-10",
      className
    )}>
      <span aria-hidden="true" className="overview-empty-state-icon">
        <Inbox className="h-4 w-4" />
      </span>
      <span className="mt-2 text-body font-medium">{label}</span>
    </div>
  );
}

export function ChartFrame({ children, fill = false }: { children: (size: { height: number; width: number }) => ReactNode; fill?: boolean }) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState({ height: 0, width: 0 });

  // 绘制前测量：useLayoutEffect 在浏览器首帧绘制前完成 getBoundingClientRect
  // 测量并同步触发图表渲染，图表首帧即可出现，消除“先空 div 再出图表”的闪烁。
  useLayoutEffect(() => {
    const container = containerRef.current;
    if (!container) {
      return;
    }

    const updateSize = (width: number, height: number) => {
      const next = {
        height: Math.max(0, Math.floor(height)),
        width: Math.max(0, Math.floor(width))
      };
      setSize((current) => (current.height === next.height && current.width === next.width ? current : next));
    };

    const rect = container.getBoundingClientRect();
    updateSize(rect.width, rect.height);

    const observer = new ResizeObserver(([entry]) => {
      if (!entry) {
        return;
      }
      updateSize(entry.contentRect.width, entry.contentRect.height);
    });

    observer.observe(container);
    return () => observer.disconnect();
  }, []);

  return (
    <div className={cn(fill ? "h-full w-full min-h-[120px]" : "h-[260px]", "min-w-0")} ref={containerRef}>
      {size.height > 0 && size.width > 0 ? children(size) : null}
    </div>
  );
}
