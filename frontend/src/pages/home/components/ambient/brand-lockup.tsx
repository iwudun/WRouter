import appLogoUrl from "@/assets/logo.png";
import { Badge } from "@/components/ui/badge";
import { cn } from "@/lib/utils";
import { useAppText } from "../../shared/index";

// 品牌锁定区：直接使用原始 logo（自带圆角瓷砖），仅配 wordmark。
//
// 20261102_01 P7-2：`isDev` 时在 wordmark 右侧挂一枚「开发」徽章，
// 用于一眼区分当前是否为开发态（dev 构建读仓库 `.dev/` 数据，
// 与发布态的生产数据不是同一套）。
// 取代原先侧栏**左下角**的「开发」按钮（那个按钮的功能是打开 DevTools，
// 与「是否开发态」是两件事 —— P7 一并移除）。
export function BrandLockup({ className, isDev = false }: { className?: string; isDev?: boolean }) {
  const t = useAppText();
  return (
    <div className={cn("flex min-w-0 items-center gap-2.5", className)}>
      <img alt="" className="h-7 w-7 shrink-0" src={appLogoUrl} />
      <span className="min-w-0 truncate text-title font-semibold tracking-[-0.01em] text-foreground max-[720px]:hidden">
        WRouter
      </span>
      {isDev ? (
        // 紧凑布局下 wordmark 会隐藏，徽章仍保留 → 用 title 兜底一个可解释的提示。
        <Badge
          className="shrink-0"
          title={t("Dev build: data comes from the repository .dev/ directory")}
          variant="warning"
        >
          {t("Development")}
        </Badge>
      ) : null}
    </div>
  );
}
