import { cn } from "@/lib/utils";
import appLogoUrl from "@/assets/logo.png";
import { AppCopy, AppInfo } from "../../shared/index";

const settingsPageContentWidthClassName = "mx-auto w-full max-w-[900px]";

// 应用"关于"页：版本信息 + 更新通道状态。
// 原应用菜单（文件/帮助）中的 设置 / 检查更新 / 关于 已移入设置页，这里是它们的落点。
//
// 20261102_01 P5-3（决策 D2）：v2 **不接自动更新通道** —— 没有 updater 插件、
// 没有 update feed。后端 `getUpdateStatus` 恒返回「不支持 / idle」的只读状态。
// 因此本页把更新区做成**纯只读展示**：不再提供「检查更新」按钮（点了也没通道可走），
// 只如实告知「自动更新暂不可用」，升级方式为手动下载安装包。
export function AboutSettingsPage({
  appInfo,
  copy
}: {
  appInfo: AppInfo;
  copy: AppCopy;
}) {
  const t = (value: string) => copy.text[value] ?? value;
  return (
    <div className={cn(settingsPageContentWidthClassName, "grid min-h-[520px] content-start gap-4")}>

      <div className="grid grid-cols-1 gap-4 lg:grid-cols-2">
        <section className="grid gap-3 rounded-md border border-border bg-background p-4">
          <div className="flex min-w-0 items-center gap-3">
            <img alt="" className="h-11 w-11 shrink-0" src={appLogoUrl} />
            <div className="min-w-0">
              <div className="truncate text-title font-semibold text-foreground">{appInfo.name}</div>
              <div className="mt-0.5 text-caption text-muted-foreground">v{appInfo.version}</div>
            </div>
          </div>
          <p className="text-body leading-5 text-muted-foreground">{copy.settings.aboutDescription}</p>
        </section>

        <section className="grid gap-3 rounded-md border border-border bg-background p-4">
          <div className="text-body font-semibold text-foreground">{t("Online updates")}</div>
          <div className="flex min-w-0 items-center justify-between gap-3">
            <span className="text-caption text-muted-foreground">{t("Current version")}</span>
            <span className="text-body font-medium tabular-nums">{appInfo.version}</span>
          </div>
          <div className="flex min-w-0 items-center justify-between gap-3">
            <span className="text-caption text-muted-foreground">{t("Update status")}</span>
            <span className="text-body font-medium text-muted-foreground">{t("Unavailable")}</span>
          </div>
          {/* D2 只读说明：明确告诉用户「没有自动更新」，以及该怎么升级。 */}
          <div className="rounded-md border border-border bg-muted/30 px-3 py-2 text-caption leading-5 text-muted-foreground">
            <div>{t("Automatic updates are not available in this build.")}</div>
            <div>{t("Please download and install the latest version manually.")}</div>
          </div>
        </section>
      </div>
    </div>
  );
}
