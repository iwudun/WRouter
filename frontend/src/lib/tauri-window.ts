// Tauri 壳的窗口层通道(20261102_01 P4-4 / P4-5)。
//
// 这些能力**不属于** `window.wr` 的 8 个桌面方法面
// (contracts/wrouter-capabilities.json 的 desktopMethods)—— 那 8 项是
// **跨运行时公共面**(Tauri / 浏览器同一份前端 bundle 都要认)。
//
// 窗口控制是**壳层 chrome**:Tauri v2 没有 `titleBarOverlay` 之类的等价 API
// → 只能自建(去掉系统标题栏,前端自绘 min/max/close)。
//
// 因此走**独立命令通道**:直接调 `window.__TAURI_INTERNALS__.invoke()`,
// 不进 `window.wr` —— 契约保持 8 项不变,一致性校验不受影响。
//
// 非 Tauri 运行时(纯浏览器)下全部是安全的空操作。

import { isTauriShell } from "@/lib/runtime";

type TauriInternals = {
  invoke: (command: string, args?: Record<string, unknown>) => Promise<unknown>;
};

type WindowThemePreference = "light" | "dark" | "system";

function tauriInternals(): TauriInternals | undefined {
  if (!isTauriShell()) {
    return undefined;
  }
  const internals = (window as unknown as { __TAURI_INTERNALS__?: TauriInternals }).__TAURI_INTERNALS__;
  return internals && typeof internals.invoke === "function" ? internals : undefined;
}

/** 调一条壳层命令;不在 Tauri 壳里(或调用失败)返回 undefined。 */
async function invokeShell<T>(command: string, args?: Record<string, unknown>): Promise<T | undefined> {
  const internals = tauriInternals();
  if (!internals) {
    return undefined;
  }
  try {
    return (await internals.invoke(command, args)) as T;
  } catch (error) {
    console.error(`[wr:shell] ${command} FAILED`, error);
    return undefined;
  }
}

/** 最小化窗口。 */
export async function minimizeWindow(): Promise<void> {
  await invokeShell("wr_window_minimize");
}

/**
 * 最大化 / 还原切换。
 *
 * 返回切换**之后**的最大化状态(壳层命令回传);不在 Tauri 壳里 → undefined。
 */
export async function toggleMaximizeWindow(): Promise<boolean | undefined> {
  return invokeShell<boolean>("wr_window_toggle_maximize");
}

/** 关闭窗口(= 应用退出)。 */
export async function closeWindow(): Promise<void> {
  await invokeShell("wr_window_close");
}

/** 当前是否已最大化。 */
export async function isWindowMaximized(): Promise<boolean> {
  return (await invokeShell<boolean>("wr_window_is_maximized")) ?? false;
}

/**
 * 标题栏配色随应用主题同步(P4-5)。
 *
 * - `preference`:应用的主题偏好。**只有显式 light/dark 才强制**窗口主题
 *   —— 强制会把 webview 的 `prefers-color-scheme` 钉死,系统主题在运行期
 *   切换就传不到前端;`"system"` 必须放行。
 * - `background`:页面底色(前端从 CSS 变量 `--background` 读出)→ 设为
 *   webview 默认底色,最大化/缩放时不闪白。
 */
export async function applyWindowTheme(
  preference: WindowThemePreference,
  background: string
): Promise<void> {
  await invokeShell("wr_window_apply_theme", { theme: preference, background });
}

/** 读页面背景色(CSS 变量 `--background`,如 `#f4f4f6`)。 */
export function readPageBackground(): string {
  if (typeof window === "undefined") {
    return "";
  }
  const value = window
    .getComputedStyle(document.documentElement)
    .getPropertyValue("--background")
    .trim();
  return value;
}

/**
 * 开机自启开关立即生效(P4-3)。
 *
 * 配置已由前端存进后端;这里再通知壳同步 OS 登录项 —— 免得等下次启动。
 */
export async function syncLaunchAtLogin(enabled: boolean): Promise<void> {
  await invokeShell("wr_sync_launch_at_login", { enabled });
}
