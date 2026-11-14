/**
 * 是否跑在 Tauri 壳里(20261102_01 P3-4 / P4-4)。
 *
 * 判据用 `window.__TAURI_INTERNALS__` 而不是「`window.wr` 是否存在」——
 * 浏览器模式也会注入 `window.wr`(Web 桥),那条路径不能误判。
 *
 * 用途:
 * - `web-client-bridge.ts` 分流「补齐业务方法」还是「装完整 Web 桥」;
 * - P4-4 自定义标题栏只在 Tauri 壳渲染(其它运行时没有可控制的窗口)。
 */
export function isTauriShell(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}
