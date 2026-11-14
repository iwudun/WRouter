// 自定义标题栏的窗口控制键(20261102_01 P4-4)。
//
// 只在 **Tauri 壳 + Windows** 下渲染:
// - 浏览器:没有窗口可言;
// - macOS:Tauri 保留系统标题栏(红绿灯),不画。
//
// 高度对齐 28px(前端对应 `h-7`),与标题栏拖拽条同高。

import { useCallback, useEffect, useState } from "react";
import { Minus, Square, X } from "lucide-react";
import { cn } from "@/lib/utils";
import {
  closeWindow,
  isWindowMaximized,
  minimizeWindow,
  toggleMaximizeWindow
} from "@/lib/tauri-window";

/** 单个控制键的尺寸(正方形,吃满 28px 标题栏高度)。 */
const CONTROL_SIZE = "h-7 w-11";

export function WindowControls() {
  const [maximized, setMaximized] = useState(false);

  // 最大化状态要跟着窗口走:壳层事件不通前端 → 失焦/聚焦时轮询一次,
  // 另加一个低频轮询兜底(用户拖到屏幕边缘触发系统贴靠最大化时)。
  useEffect(() => {
    let disposed = false;
    const refresh = async () => {
      const next = await isWindowMaximized();
      if (!disposed) {
        setMaximized(next);
      }
    };
    void refresh();
    const timer = window.setInterval(() => void refresh(), 1000);
    window.addEventListener("focus", refresh);
    return () => {
      disposed = true;
      window.clearInterval(timer);
      window.removeEventListener("focus", refresh);
    };
  }, []);

  const onToggleMaximize = useCallback(async () => {
    const next = await toggleMaximizeWindow();
    if (typeof next === "boolean") {
      setMaximized(next);
    }
  }, []);

  return (
    // data-tauri-drag-region="false":显式阻断拖拽 —— Tauri 的拖拽区脚本
    // 遇到该属性即判定「不可拖拽」(按钮本身也是 clickable 元素,双保险)。
    <div
      className="app-window-controls flex shrink-0 items-stretch"
      data-tauri-drag-region="false"
    >
      <button
        aria-label="Minimize"
        className={cn(
          CONTROL_SIZE,
          "flex items-center justify-center text-muted-foreground transition-colors",
          "hover:bg-muted hover:text-foreground focus-visible:outline-none"
        )}
        onClick={() => void minimizeWindow()}
        type="button"
      >
        <Minus aria-hidden="true" className="h-3.5 w-3.5" />
      </button>
      <button
        aria-label={maximized ? "Restore" : "Maximize"}
        className={cn(
          CONTROL_SIZE,
          "flex items-center justify-center text-muted-foreground transition-colors",
          "hover:bg-muted hover:text-foreground focus-visible:outline-none"
        )}
        onClick={() => void onToggleMaximize()}
        type="button"
      >
        {maximized ? (
          <Square aria-hidden="true" className="h-3 w-3" />
        ) : (
          <Square aria-hidden="true" className="h-3.5 w-3.5" />
        )}
      </button>
      <button
        aria-label="Close"
        className={cn(
          CONTROL_SIZE,
          "flex items-center justify-center text-muted-foreground transition-colors",
          "hover:bg-destructive hover:text-destructive-foreground focus-visible:outline-none"
        )}
        onClick={() => void closeWindow()}
        type="button"
      >
        <X aria-hidden="true" className="h-3.5 w-3.5" />
      </button>
    </div>
  );
}
