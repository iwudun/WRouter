import { Fragment, type ComponentProps, lazy, Suspense, useEffect, useState, type ReactNode } from "react";
import { AnimatePresence, motion } from "motion/react";
import {
  Activity, ArrowLeft, CircleAlert, Info, LoaderCircle, Palette, Settings, X, type LucideIcon
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import { isTauriShell } from "@/lib/runtime";
import { BrandLockup } from "./ambient/brand-lockup";
import { WindowControls } from "./window-controls";
import {
  AppCopy,
  listSpringTransition, motionEase,
  NavigationId,
  navigationGroups,
  reducedMotionTransition, SettingsPageId, ViewId,
  useAppText, viewUsesInternalScroll
} from "../shared/index";

// 20261101_03:设置分区菜单上移到主侧栏 —— 设置模式下覆盖首页菜单
// (「返回应用」置顶,分区按钮与首页导航同一视觉语言)。
const settingsNavPages: Array<{ icon: LucideIcon; id: SettingsPageId }> = [
  { icon: Settings, id: "general" },
  { icon: Palette, id: "appearance" },
  { icon: Activity, id: "observability" },
  { icon: Info, id: "about" }
];

// R7 lazy views: each view is code-split into its own chunk and loaded on
// first activation (Vite/Rollup splitting, F1). Named exports are adapted to
// default for React.lazy.
// 20261101_03:快速开始页 —— 应用默认第一页。
const QuickstartView = lazy(() => import("./quickstart").then((m) => ({ default: m.QuickstartView })));
const UsageStatsView = lazy(() => import("./dashboard").then((m) => ({ default: m.UsageStatsView })));
const ApiKeysView = lazy(() => import("./api-keys").then((m) => ({ default: m.ApiKeysView })));
const CallLogsView = lazy(() => import("./call-logs").then((m) => ({ default: m.CallLogsView })));
const CallLogDetailView = lazy(() => import("./call-log-detail").then((m) => ({ default: m.CallLogDetailView })));
// 2026-11-01:模型指纹从设置分区升级为独立视图。
const ModelFingerprintsView = lazy(() => import("./model-fingerprints").then((m) => ({ default: m.ModelFingerprintsView })));
const ProvidersView = lazy(() => import("./providers").then((m) => ({ default: m.ProvidersView })));
// 2026-09-10:设置视图化 —— 与其它视图同池懒加载。
const SettingsView = lazy(() => import("./settings").then((m) => ({ default: m.SettingsView })));

type MainViewProps = {
  apiKeys: ComponentProps<typeof ApiKeysView>;
  callLogDetail: ComponentProps<typeof CallLogDetailView>;
  callLogs: ComponentProps<typeof CallLogsView>;
  modelFingerprints: ComponentProps<typeof ModelFingerprintsView>;
  providers: ComponentProps<typeof ProvidersView>;
  quickstart: ComponentProps<typeof QuickstartView>;
  settings: ComponentProps<typeof SettingsView>;
  usageStats: ComponentProps<typeof UsageStatsView>;
};

export function MainLayout({
  actionError,
  activeView,
  compactLayout,
  copy,
  gatewayAddress,
  gatewayEventsStatus,
  gatewayStartupError,
  isDevShell,
  isMac,
  isWindows,
  onDismissActionError,
  onExitSettings,
  onOpenServerSettings,
  onSelectNavigationItem,
  onSelectSettingsPage,
  settingsPage,
  shouldReduceMotion,
  sidebarOpen,
  viewProps
}: {
  actionError?: string;
  activeView: ViewId;
  compactLayout: boolean;
  copy: AppCopy;
  /** 网关地址展示(去协议前缀,如 127.0.0.1:3460),取自运行状态端点。 */
  gatewayAddress: string;
  /** F2b.1: SSE 连接状态（实时/重连中角标）；断连降级轮询期间显示重连中。 */
  gatewayEventsStatus?: "connecting" | "live" | "reconnecting";
  gatewayStartupError?: string;
  /**
   * 仅桌面 dev 构建为 true:侧栏顶部 logo 右侧的「开发」徽章可见
   * (20261102_01 P7-2:取代原先左下角的「开发」按钮)。
   */
  isDevShell: boolean;
  isMac: boolean;
  /** 20261101_03:Windows 隐藏式标题栏(titleBarOverlay)后同样需要应用内拖拽区。 */
  isWindows: boolean;
  onDismissActionError?: () => void;
  /** 设置模式:顶部「返回应用」回到上一次的首页视图。 */
  onExitSettings: () => void;
  onOpenServerSettings: () => void;
  onSelectNavigationItem: (id: NavigationId) => void;
  onSelectSettingsPage: (page: SettingsPageId) => void;
  settingsPage: SettingsPageId;
  shouldReduceMotion: boolean | null;
  sidebarOpen: boolean;
  viewProps: MainViewProps;
}) {
  // macOS hiddenInset 标题栏的红绿灯占位宽度（侧边栏内容需避让）。
  const windowControlSafeAreaWidth = isMac ? 84 : 0;
  const t = useAppText();
  // 20261102_01 P4-4/P5-5:Tauri 壳自绘标题栏(去掉系统标题栏 + 前端 min/max/close)。
  // Electron 退场后,只有 Tauri 壳有可控制的窗口。
  const tauriShell = isTauriShell();
  // 标题栏 —— 系统不画标题栏行,顶部由应用提供拖拽区,颜色与页面背景一致。
  // Tauri 壳只在 Windows 走自绘标题栏(macOS 保留系统标题栏);
  // 其它运行时没有窗口 chrome —— 浏览器里不该留一条空标题栏占位。
  const usesCustomTitlebar = tauriShell && isWindows;
  // 设置模式:设置分区菜单覆盖首页菜单。
  const settingsMode = activeView === "settings";
  return (
    <>
      <motion.aside
        animate={{
          width: sidebarOpen ? (compactLayout ? "100%" : 248) : 0
        }}
        aria-hidden={!sidebarOpen}
        className={cn(
          "app-sidebar relative z-[1] flex shrink-0 flex-col overflow-hidden bg-sidebar max-[720px]:h-auto",
          sidebarOpen && compactLayout && "border-b border-border",
          // 20261101_03:侧栏与内容区之间的分割线 —— 侧栏与内容同背景色,
          // 靠这条 1px 边线区分两栏(紧凑布局下侧栏横排,保持分隔线在下)。
          sidebarOpen && !compactLayout && "border-r border-border"
        )}
        id="primary-sidebar"
        initial={false}
        style={{ pointerEvents: sidebarOpen ? "auto" : "none" }}
        transition={shouldReduceMotion ? reducedMotionTransition : { duration: 0.3, ease: motionEase }}
      >
        <AnimatePresence initial={false}>
          {sidebarOpen ? (
            <motion.div
              animate={{ opacity: 1 }}
              className="flex min-h-0 w-[248px] flex-1 flex-col max-[720px]:w-full"
              exit={{ opacity: 0 }}
              initial={{ opacity: 0 }}
              transition={shouldReduceMotion ? reducedMotionTransition : { duration: 0.3, ease: motionEase }}
            >
            {usesCustomTitlebar ? (
              // 隐藏式标题栏(macOS hiddenInset / Windows titleBarOverlay):
              // 顶部拖拽区。macOS 红绿灯悬浮左上需避让;Windows 窗口控制键
              // 由叠加层绘制在右上,条带高度与叠加层同高(h-7 = 28px,
              // 20261101_03 收紧顶部空隙)。
              <div
                className={cn(
                  "app-drag flex shrink-0 items-center",
                  isWindows ? "h-7" : "h-8 max-[720px]:h-7"
                )}
                // 20261102_01 P4-4:Tauri 壳的拖拽区。用内建的
                // data-tauri-drag-region 属性
                // (Tauri 自带的拖拽脚本会接管,并顺带支持双击最大化)。
                data-tauri-drag-region=""
              >
                {isMac ? (
                  <div className="app-no-drag shrink-0" style={{ width: windowControlSafeAreaWidth }} />
                ) : null}
              </div>
            ) : null}

            {settingsMode ? (
              // 设置模式:顶部「返回应用」替代品牌行(覆盖首页菜单的入口)。
              <div className="shrink-0 px-2 pt-2 max-[720px]:px-1 max-[720px]:pt-1">
                <Button
                  className="flex h-8 w-full min-w-0 items-center gap-1.5 rounded-md px-2 text-left text-body-lg font-medium text-foreground transition-colors hover:bg-muted/80"
                  onClick={onExitSettings}
                  type="button"
                  unstyled
                >
                  <ArrowLeft className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
                  <span className="min-w-0 flex-1 truncate">{t("Back to app")}</span>
                </Button>
              </div>
            ) : (
              <div className="flex h-7 shrink-0 items-center gap-2 px-3 pt-2 max-[720px]:px-1 max-[720px]:pt-1">
                {/* 20261102_01 P7-2:开发态标识放在 logo 右侧(取代原左下角「开发」按钮)。 */}
                <BrandLockup isDev={isDevShell} />
              </div>
            )}

            <nav
              className="flex min-h-0 flex-1 flex-col gap-1 px-2 pt-1 pb-3 max-[720px]:flex-none max-[720px]:flex-row max-[720px]:overflow-x-auto max-[720px]:py-2"
              aria-label={settingsMode ? copy.settings.title : copy.sidebar.primaryNavigation}
            >
              {settingsMode ? (
                <>
                  <SidebarSectionLabel label={copy.settings.title} />
                  {settingsNavPages.map((page) => (
                    <SidebarNavButton
                      active={settingsPage === page.id}
                      icon={page.icon}
                      key={page.id}
                      label={copy.settings[page.id]}
                      onClick={() => onSelectSettingsPage(page.id)}
                      shouldReduceMotion={shouldReduceMotion}
                    />
                  ))}
                </>
              ) : (
                navigationGroups.map((group) => (
                  <Fragment key={group.label}>
                    <SidebarSectionLabel label={t(group.label)} />
                    {group.items.map((item) => (
                      <SidebarNavButton
                        active={activeView === item.id || (activeView === "call-log-detail" && item.id === "call-logs")}
                        icon={item.icon}
                        key={item.id}
                        label={copy.navigation[item.id]}
                        navId={item.id}
                        onClick={() => onSelectNavigationItem(item.id)}
                        shouldReduceMotion={shouldReduceMotion}
                      />
                    ))}
                  </Fragment>
                ))
              )}
            </nav>

{/* 20261101_03:左下角设置区——设置入口 + 网关地址行
                (原侧栏顶部「实时」状态点迁入)。
                20261102_01 P7-2:原此处还有的「开发」按钮已移除 ——
                开发态标识改到侧栏顶部 logo 右侧的徽章(BrandLockup)。
20261104_01:底部区上方补一道横线(
border-top)——把设置区与上方导航分区隔开;紧凑布局下
                侧栏横排,横线不适用,故去掉。 */}
            <div
              className="flex shrink-0 flex-col gap-1 border-t border-border/60 px-2 pt-2 pb-3 max-[720px]:flex-row max-[720px]:items-center max-[720px]:gap-2 max-[720px]:border-t-0 max-[720px]:px-1 max-[720px]:pt-0 max-[720px]:pb-2"
              data-module="sidebar-settings"
            >
              <div className="min-w-0 max-[720px]:flex-1">
                <SidebarNavButton
                  active={settingsMode}
                  icon={Settings}
                  label={copy.navigation.settings}
                  onClick={() => onSelectNavigationItem("settings")}
                  shouldReduceMotion={shouldReduceMotion}
                />
              </div>
              {gatewayEventsStatus ? (
                <SidebarGatewayRow address={gatewayAddress} status={gatewayEventsStatus} t={t} />
              ) : null}
            </div>
          </motion.div>
          ) : null}
        </AnimatePresence>
      </motion.aside>

      <main className="relative z-[1] flex min-h-0 min-w-0 flex-1 flex-col">
        {usesCustomTitlebar ? (
          // 隐藏式标题栏需要顶部拖拽区。macOS h-12;Windows h-7 与窗口控制
          // 叠加层同高(28px,20261101_03 收紧顶部空隙),内容避让右上角控制键。
          <div
            className={cn(
              "app-drag relative flex shrink-0 items-center px-5",
              isWindows ? "h-7" : "h-12 max-[720px]:h-auto max-[720px]:px-3 max-[720px]:py-2"
            )}
            // 20261102_01 P4-4:Tauri 壳的拖拽区(见侧栏上方同款说明)。
            data-tauri-drag-region=""
          >
            {isMac ? (
              <div className="app-no-drag absolute left-0 top-0 h-full" style={{ width: windowControlSafeAreaWidth }} />
            ) : null}
            {tauriShell && isWindows ? (
              // Tauri 自绘窗口控制键(右对齐,贴窗口右边缘)。
              <div className="-mr-5 ml-auto">
                <WindowControls />
              </div>
            ) : null}
          </div>
        ) : null}
        <GatewayStartupErrorBanner
          className="mx-5 mt-3 max-[720px]:mx-3"
          message={gatewayStartupError}
          onOpenServerSettings={onOpenServerSettings}
        />
        <ActionErrorBanner
          className="mx-5 mt-3 max-[720px]:mx-3"
          message={actionError}
          onDismiss={onDismissActionError}
        />
{/* 20261101_02 T4:主内容收口居中(密度参照,取值自定);
            call-logs 网络三栏与宽表在 1200 下过挤,例外放宽 1600。
            两个 max-w 任意值互斥渲染,避免样式表顺序决定胜出。 */}
        <div
          className={cn(
            "mx-auto min-h-0 w-full flex-1 px-5 pb-5 pt-3 max-[720px]:px-3 max-[720px]:pb-3 max-[720px]:pt-3",
            activeView === "call-logs" || activeView === "call-log-detail" ? "max-w-[1600px]" : "max-w-[1200px]",
            viewUsesInternalScroll(activeView) ? "overflow-hidden" : "overflow-auto"
          )}
        >
          <MainViewSwitch
            activeView={activeView}
            shouldReduceMotion={shouldReduceMotion}
            viewProps={viewProps}
          />
        </div>
      </main>
    </>
  );
}

export function ActionErrorBanner({
  className,
  message,
  onDismiss
}: {
  className?: string;
  message?: string;
  onDismiss?: () => void;
}) {
  const t = useAppText();
  const detail = message?.trim();
  if (!detail) {
    return null;
  }

  return (
    <div
      aria-live="assertive"
      className={cn(
        "app-no-drag flex min-w-0 items-start gap-3 rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2 text-body text-destructive",
        className
      )}
      role="alert"
    >
      <CircleAlert className="mt-0.5 h-3.5 w-3.5 shrink-0" />
      <div className="min-w-0 flex-1 whitespace-pre-wrap break-words">{detail}</div>
      {onDismiss ? (
        <button
          aria-label={t("Close")}
          className="inline-flex h-6 w-6 shrink-0 items-center justify-center rounded-md text-destructive/80 transition-colors hover:bg-destructive/10 hover:text-destructive"
          onClick={onDismiss}
          type="button"
        >
          <X className="h-3.5 w-3.5" />
        </button>
      ) : null}
    </div>
  );
}

export function GatewayStartupErrorBanner({
  className,
  message,
  onOpenServerSettings
}: {
  className?: string;
  message?: string;
  onOpenServerSettings?: () => void;
}) {
  const t = useAppText();
  const detail = message?.trim();
  if (!detail) {
    return null;
  }

  return (
    <div
      aria-live="assertive"
      className={cn(
        "app-no-drag flex min-w-0 items-start gap-3 rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2 text-body text-destructive",
        className
      )}
      role="alert"
    >
      <CircleAlert className="mt-0.5 h-3.5 w-3.5 shrink-0" />
      <div className="min-w-0 flex-1">
        <div className="font-semibold">{t("Service failed to start")}</div>
        <div className="mt-0.5 whitespace-pre-wrap break-words">{detail}</div>
      </div>
      {onOpenServerSettings ? (
        <Button
          className="shrink-0 border-destructive/30 bg-background/80 text-destructive hover:bg-destructive/10"
          onClick={onOpenServerSettings}
          size="sm"
          type="button"
          variant="outline"
        >
          <Settings className="h-3.5 w-3.5" />
          {t("Server")}
        </Button>
      ) : null}
    </div>
  );
}

// 20261101_03:侧栏章节小标题。紧凑布局下侧栏横排,章节
// 标题隐藏(水平滚动条里分章无意义)。
function SidebarSectionLabel({ label }: { label: string }) {
  return (
    <div
      className="px-2 pt-3 pb-0.5 text-caption font-medium text-muted-foreground max-[720px]:hidden"
      aria-hidden="true"
    >
      {label}
    </div>
  );
}

// 侧栏导航按钮:首页导航 / 设置分区菜单 / 左下角设置与开发入口共用同一
// 视觉语言(图标芯片 + 文本,激活态卡片底 + 主色描边阴影)。
function SidebarNavButton({
  active,
  icon: Icon,
  label,
  navId,
  onClick,
  shouldReduceMotion
}: {
  active: boolean;
  icon: LucideIcon;
  label: string;
  /** 视图 id(测试与深链定位用)。 */
  navId?: string;
  onClick: () => void;
  shouldReduceMotion: boolean | null;
}) {
  return (
    <Button
      className={cn(
        "flex h-8 w-full min-w-0 items-center gap-1.5 rounded-md px-2 text-left text-body-lg font-medium transition-all duration-150 max-[720px]:min-w-[118px]",
        active
          ? "bg-card text-foreground shadow-[0_0_0_1px_color-mix(in_oklab,var(--primary)_22%,transparent),0_2px_10px_rgba(15,23,42,0.10)]"
          : "text-muted-foreground hover:bg-muted/80 hover:text-foreground"
      )}
      data-nav={navId}
      onClick={onClick}
      type="button"
      unstyled
    >
      <motion.span
        className={cn(
          "flex h-5 w-5 shrink-0 items-center justify-center rounded-md transition-colors",
          active && "bg-primary/10 text-primary"
        )}
        layout="position"
        transition={shouldReduceMotion ? reducedMotionTransition : listSpringTransition}
      >
        <Icon className="h-3.5 w-3.5" />
      </motion.span>
      <span className="min-w-0 flex-1 truncate">{label}</span>
    </Button>
  );
}

// 20261101_03:左下角网关行 —— 状态点(实时=绿实心 / 重连中=琥珀脉冲,
// 原侧栏顶部「实时」角标迁入)+ 网关地址(去协议前缀)。
function SidebarGatewayRow({
  address,
  status,
  t
}: {
  address: string;
  status: "connecting" | "live" | "reconnecting";
  t: (value: string) => string;
}) {
  const live = status === "live";
  return (
    <div
      aria-live="polite"
      className="flex h-8 min-w-0 flex-1 items-center gap-1.5 rounded-md px-2 text-body-lg text-muted-foreground max-[720px]:max-w-[220px]"
      title={live ? t("Realtime updates connected") : t("Reconnecting to realtime updates")}
    >
      <span aria-hidden="true" className="flex h-5 w-5 shrink-0 items-center justify-center rounded-md">
        <span className={cn("h-1.5 w-1.5 rounded-full", live ? "bg-emerald-500" : "animate-pulse bg-amber-500")} />
      </span>
      <span className="shrink-0">{t("Gateway")}</span>
      <span className="min-w-0 flex-1 truncate font-mono text-caption">{address}</span>
    </div>
  );
}

// Lightweight placeholder shown while a view chunk loads. Keeps the
// ViewMotionShell layout stable so the AnimatePresence transition is not
// interrupted by the Suspense fallback.
function ViewLoadingFallback() {
  return (
    <div className="flex h-full min-h-0 items-center justify-center">
      <LoaderCircle className="h-5 w-5 animate-spin text-muted-foreground" />
    </div>
  );
}

function MainViewSwitch({
  activeView,
  shouldReduceMotion,
  viewProps
}: {
  activeView: ViewId;
  shouldReduceMotion: boolean | null;
  viewProps: MainViewProps;
}) {
  const [visited, setVisited] = useState<ViewId[]>([activeView]);
  useEffect(() => {
    setVisited((current) => (current.includes(activeView) ? current : [...current, activeView]));
  }, [activeView]);

  const views: Array<{ id: ViewId; node: ReactNode }> = [
    { id: "quickstart", node: <QuickstartView {...viewProps.quickstart} /> },
    { id: "usage-stats", node: <UsageStatsView {...viewProps.usageStats} /> },
    { id: "api-keys", node: <ApiKeysView {...viewProps.apiKeys} /> },
    { id: "call-logs", node: <CallLogsView {...viewProps.callLogs} /> },
    { id: "call-log-detail", node: <CallLogDetailView {...viewProps.callLogDetail} /> },
    { id: "model-fingerprints", node: <ModelFingerprintsView {...viewProps.modelFingerprints} /> },
    { id: "providers", node: <ProvidersView {...viewProps.providers} /> },
    { id: "settings", node: <SettingsView {...viewProps.settings} /> }
  ];

  return (
    <div className="relative h-full min-h-0">
      {views
        .filter((view) => visited.includes(view.id))
        .map((view) => (
          <KeepAliveViewPane key={view.id} active={view.id === activeView} shouldReduceMotion={shouldReduceMotion} view={view.id}>
            <Suspense fallback={<ViewLoadingFallback />}>{view.node}</Suspense>
          </KeepAliveViewPane>
        ))}
    </div>
  );
}

// 视图保活面板：激活过的视图常驻挂载。激活面板留在文档流中（relative），
// 未激活面板 absolute inset-0 + overflow-hidden，淡出完成后补 visibility:hidden。
// 用 visibility 而非 display:none，让 ChartFrame 的 ResizeObserver 始终量到
// 真实尺寸、recharts 图表不被卸载，二次进入零挂载成本；overflow-hidden 防止
// 隐藏面板的溢出内容撑大共享滚动容器。
function KeepAliveViewPane({
  active,
  children,
  shouldReduceMotion,
  view
}: {
  active: boolean;
  children: ReactNode;
  shouldReduceMotion: boolean | null;
  view: ViewId;
}) {
  const [mounted, setMounted] = useState(active);
  const [hidden, setHidden] = useState(false);
  useEffect(() => {
    if (active) {
      setMounted(true);
      setHidden(false);
    }
  }, [active]);

  if (!mounted) {
    return null;
  }

  return (
    <motion.div
      animate={{ opacity: active ? 1 : 0 }}
      aria-hidden={!active}
      className={cn(
        "min-h-0",
        active ? "relative z-[1] h-full" : "absolute inset-0 z-0 overflow-hidden pointer-events-none",
        hidden && "invisible"
      )}
      data-view={view}
      initial={false}
      onAnimationComplete={() => {
        if (!active) {
          setHidden(true);
        }
      }}
      transition={shouldReduceMotion ? { duration: 0 } : { duration: 0.16, ease: motionEase }}
    >
      {children}
    </motion.div>
  );
}
