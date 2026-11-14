// 用量统计的时间范围选择器。
//
// 形态:四个预设段(今天 / 7 天 / 30 天 / 全部)+ 一枚「自定义」段。
// 「自定义」不是预设 —— 它是一枚带日历图标的独立段:未选时写「自定义」,
// 选过之后写所选日期区间;点它弹出选日面板(先点首日、再点末日),
// 面板里另配四个快捷区间(昨天 / 本月 / 上月 / 90 天)。
//
// 线格式:自定义窗口以 "YYYY-MM-DD..YYYY-MM-DD" 传给后端(本地日历日,
// 两端都含),与后端 `range_bounds` 同口径。
import { useEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { CalendarDays, ChevronLeft, ChevronRight } from "lucide-react";
import { Button } from "@/components/ui/button";
import { PopoverContent } from "@/components/ui/popover";
import { cn } from "@/lib/utils";
import {
  UsageStatsRange,
  encodeUsageCustomRange,
  formatUsageCustomRange,
  parseUsageCustomRange,
  parseUsageDayKey,
  usageRangeOptions,
  useAppNumberLocale,
  useAppText
} from "../../shared/index";

type PickedRange = { from: string; to: string };

/** 选日面板宽度,与 PopoverContent 的 w-[272px] 同值(视口夹取计算用)。 */
const PICKER_WIDTH_PX = 272;
/** 面板与触发按钮/视口边缘的最小间距。 */
const PICKER_GAP_PX = 8;

export function UsageStatsRangeSelector({
  range,
  setRange
}: {
  range: UsageStatsRange;
  setRange: (range: UsageStatsRange) => void;
}) {
  const t = useAppText();
  const custom = parseUsageCustomRange(range);
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const [panelPos, setPanelPos] = useState<{ left: number; top: number } | null>(null);

  // 点击面板外 / Escape → 收起(面板是自绘浮层,没有 Radix 兜底)。
  useEffect(() => {
    if (!open) {
      return;
    }
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target as Node;
      // 面板挂在 document.body 上(portal),不在 rootRef 内 —— 点面板内部
      // 不能当成"点外面",否则点日期会被先收起再点不到。
      if (rootRef.current?.contains(target) || panelRef.current?.contains(target)) {
        return;
      }
      setOpen(false);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        setOpen(false);
      }
    };
    document.addEventListener("pointerdown", onPointerDown);
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown);
      document.removeEventListener("keydown", onKeyDown);
    };
  }, [open]);

  // 20261104_03:面板改走 portal + fixed(见渲染处注释),定位坐标随窗口尺寸与
  // 滚动变化,故 resize / scroll 时重算,并夹在视口内。
  useEffect(() => {
    if (!open) {
      setPanelPos((prev) => (prev === null ? prev : null));
      return;
    }
    const measure = () => {
      const trigger = triggerRef.current;
      if (!trigger) {
        return;
      }
      const rect = trigger.getBoundingClientRect();
      const maxLeft = Math.max(
        PICKER_GAP_PX,
        window.innerWidth - PICKER_WIDTH_PX - PICKER_GAP_PX
      );
      const left = Math.min(Math.max(rect.right - PICKER_WIDTH_PX, PICKER_GAP_PX), maxLeft);
      const top = rect.bottom + PICKER_GAP_PX;
      setPanelPos((prev) =>
        prev && prev.left === left && prev.top === top ? prev : { left, top }
      );
    };
    measure();
    window.addEventListener("resize", measure);
    // capture:true —— 内容区滚动容器(overflow-auto)上的 scroll 也要响应。
    window.addEventListener("scroll", measure, true);
    return () => {
      window.removeEventListener("resize", measure);
      window.removeEventListener("scroll", measure, true);
    };
  }, [open]);

  return (
    <div
      aria-label={t("Usage over time")}
      className="overview-segmented relative flex"
      ref={rootRef}
      role="group"
    >
      {usageRangeOptions.map((option) => (
        <Button
          aria-pressed={range === option.value}
          className={cn(
            "overview-segmented-item h-7 px-2.5 text-caption font-medium text-muted-foreground hover:text-foreground",
            range === option.value && "text-foreground"
          )}
          data-active={range === option.value}
          key={option.value}
          onClick={() => setRange(option.value)}
          type="button"
          unstyled
        >
          {t(option.label)}
        </Button>
      ))}
      <Button
        ref={triggerRef}
        aria-expanded={open}
        aria-haspopup="dialog"
        aria-label={
          custom
            ? t("Days picked: {days}").replace("{days}", formatUsageCustomRange(custom))
            : t("Pick days")
        }
        aria-pressed={custom !== undefined}
        className={cn(
          // 20261104_03:必须显式 inline-flex —— Tailwind v4 preflight 把 svg
          // 设成 display:block,按钮不建成 flex 容器时图标独占一行,「自定义」
          // 被挤到图标下方(按钮宽度只剩 min-content)。
          "overview-segmented-item inline-flex h-7 items-center gap-1 whitespace-nowrap px-2.5 text-caption font-medium text-muted-foreground hover:text-foreground",
          custom !== undefined && "text-foreground"
        )}
        data-active={custom !== undefined}
        onClick={() => setOpen((value) => !value)}
        type="button"
        unstyled
      >
        <CalendarDays className="h-3.5 w-3.5 shrink-0" />
        {custom ? formatUsageCustomRange(custom) : t("Custom")}
      </Button>
      {/* 20261104_03:面板改走 portal + fixed。原先它 absolute 挂在分段控件内,
        而主内容区是 overflow-auto 的滚动容器(layout.tsx:283):面板左缘会越过
        容器左界被裁掉(实测面板左缘 x=237,容器左界 x=248,左侧 11px 连边框
        一起消失)。fixed + portal 逃出该容器,位置按触发按钮实测坐标算。 */}
      {open && panelPos
        ? createPortal(
            <PopoverContent
              aria-label={t("Pick days")}
              className="fixed z-50 w-[272px] max-h-[calc(100vh-16px)] overflow-y-auto p-2.5"
              ref={panelRef}
              role="dialog"
              style={{ left: panelPos.left, top: panelPos.top }}
            >
              <UsageStatsRangePicker
                current={custom}
                onPick={(from, to) => {
                  setRange(encodeUsageCustomRange(from, to));
                  setOpen(false);
                }}
              />
            </PopoverContent>,
            document.body
          )
        : null}
    </div>
  );
}

function UsageStatsRangePicker({
  current,
  onPick
}: {
  current?: PickedRange;
  onPick: (from: string, to: string) => void;
}) {
  const t = useAppText();
  const locale = useAppNumberLocale();
  const today = startOfDay(new Date());
  const todayKey = dayKeyOf(today);
  const [shown, setShown] = useState(() => {
    const anchor = current?.to ? parseUsageDayKey(current.to) : undefined;
    return anchor ? startOfMonth(anchor) : startOfMonth(today);
  });
  // 先点的那一天;点第二下就定出区间。
  const [pendingStart, setPendingStart] = useState<string | undefined>(undefined);

  const from = pendingStart ?? current?.from;
  const to = pendingStart ? undefined : current?.to;

  const quickSpans: Array<{ from: Date; label: string; to: Date }> = [
    { from: addDays(today, -1), label: "Yesterday", to: addDays(today, -1) },
    {
      from: new Date(today.getFullYear(), today.getMonth(), 1),
      label: "This month",
      to: today
    },
    {
      from: new Date(today.getFullYear(), today.getMonth() - 1, 1),
      label: "Last month",
      to: new Date(today.getFullYear(), today.getMonth(), 0)
    },
    { from: addDays(today, -89), label: "90 days", to: today }
  ];

  // 周一为首(与仓库日历一致);星期名走界面语言。
  const weekHead = Array.from({ length: 7 }, (_, index) =>
    new Date(2024, 0, 1 + index, 12).toLocaleDateString(locale, { weekday: "narrow" })
  );
  const firstWeekday = (shown.getDay() + 6) % 7;
  const daysInMonth = new Date(shown.getFullYear(), shown.getMonth() + 1, 0).getDate();
  const nextMonthStart = new Date(shown.getFullYear(), shown.getMonth() + 1, 1);

  const cells: ReactNode[] = [];
  for (let index = 0; index < firstWeekday; index += 1) {
    cells.push(<span aria-hidden="true" className="h-7" key={`pad-${index}`} />);
  }
  for (let day = 1; day <= daysInMonth; day += 1) {
    const date = new Date(shown.getFullYear(), shown.getMonth(), day);
    const key = dayKeyOf(date);
    const disabled = key > todayKey;
    const isEdge = key === from || key === to;
    const inside = Boolean(from && to && key > from && key < to);
    cells.push(
      <button
        aria-label={date.toLocaleDateString(locale, {
          day: "numeric",
          month: "long",
          year: "numeric"
        })}
        className={cn(
          "h-7 rounded-md text-caption tabular-nums transition-colors",
          "hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring/30 focus-visible:outline-none",
          disabled && "cursor-not-allowed opacity-35 hover:bg-transparent",
          !disabled && !isEdge && !inside && "hover:bg-muted/70",
          inside && "bg-primary/10 text-foreground",
          isEdge && "bg-primary/20 font-semibold text-foreground",
          key === todayKey && !isEdge && "text-primary"
        )}
        disabled={disabled}
        key={key}
        onClick={() => {
          if (!pendingStart) {
            setPendingStart(key);
            return;
          }
          onPick(pendingStart <= key ? pendingStart : key, pendingStart <= key ? key : pendingStart);
        }}
        type="button"
      >
        {day}
      </button>
    );
  }

  return (
    <div className="flex flex-col gap-2">
      <div className="text-caption font-medium text-muted-foreground">
        {pendingStart ? t("Now the last day") : t("Pick the first day, then the last")}
      </div>

      <div className="flex flex-wrap gap-1">
        {quickSpans.map((span) => {
          const spanFrom = dayKeyOf(span.from);
          const spanTo = dayKeyOf(span.to);
          const active = current?.from === spanFrom && current?.to === spanTo;
          return (
            <Button
              className={cn(
                "h-6 rounded-md border border-border/70 px-1.5 text-micro font-medium text-muted-foreground hover:bg-muted/70 hover:text-foreground",
                active && "border-primary/50 bg-primary/10 text-primary"
              )}
              key={span.label}
              onClick={() => onPick(spanFrom, spanTo)}
              type="button"
              unstyled
            >
              {t(span.label)}
            </Button>
          );
        })}
      </div>

      <div className="flex items-center justify-between gap-1">
        <Button
          aria-label={t("Previous month")}
          className="inline-flex h-6 w-6 items-center justify-center rounded-md text-muted-foreground hover:bg-muted/70 hover:text-foreground"
          onClick={() => setShown(startOfMonth(new Date(shown.getFullYear(), shown.getMonth() - 1, 1)))}
          type="button"
          unstyled
        >
          <ChevronLeft className="h-3.5 w-3.5" />
        </Button>
        <span className="text-caption font-medium text-foreground">
          {shown.toLocaleDateString(locale, { month: "long", year: "numeric" })}
        </span>
        <Button
          aria-label={t("Next month")}
          className="inline-flex h-6 w-6 items-center justify-center rounded-md text-muted-foreground hover:bg-muted/70 hover:text-foreground"
          disabled={nextMonthStart > today}
          onClick={() => setShown(startOfMonth(new Date(shown.getFullYear(), shown.getMonth() + 1, 1)))}
          type="button"
          unstyled
        >
          <ChevronRight className="h-3.5 w-3.5" />
        </Button>
      </div>

      <div className="grid grid-cols-7 gap-0.5">
        {weekHead.map((label) => (
          <span
            aria-hidden="true"
            className="flex h-6 items-center justify-center text-micro font-medium text-muted-foreground"
            key={label}
          >
            {label}
          </span>
        ))}
        {cells}
      </div>
    </div>
  );
}

function startOfDay(date: Date): Date {
  const start = new Date(date);
  start.setHours(0, 0, 0, 0);
  return start;
}

function startOfMonth(date: Date): Date {
  const start = new Date(date.getFullYear(), date.getMonth(), 1);
  start.setHours(0, 0, 0, 0);
  return start;
}

function addDays(date: Date, days: number): Date {
  const next = new Date(date);
  next.setDate(next.getDate() + days);
  return next;
}

/** 本地日历日 → "YYYY-MM-DD"(后端按本地日历切窗口)。 */
function dayKeyOf(date: Date): string {
  const month = String(date.getMonth() + 1).padStart(2, "0");
  const day = String(date.getDate()).padStart(2, "0");
  return `${date.getFullYear()}-${month}-${day}`;
}
