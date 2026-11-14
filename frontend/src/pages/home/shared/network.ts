// 日志/统计页共享的通用格式化工具（原网络捕获页辅助函数的存留部分）。

export function networkHeaderRows(headers: Record<string, string | string[]>): Array<[string, string]> {
  return Object.entries(headers).map(([key, value]) => [formatHeaderName(key), Array.isArray(value) ? value.join(", ") : value]);
}

export function formatHeaderName(value: string): string {
  return value
    .split("-")
    .map((part) => part ? `${part.charAt(0).toUpperCase()}${part.slice(1)}` : part)
    .join("-");
}

export function formatDuration(value: number | undefined): string {
  if (value === undefined) {
    return "pending";
  }
  if (value < 1000) {
    // 2026-10-20:毫秒值可能带浮点长尾(重试退避由 f64 计算),
    // 展示最多两位小数,避免 "734.123456789 ms"。
    return `${Math.round(value * 100) / 100} ms`;
  }
  return `${(value / 1000).toFixed(value < 10_000 ? 1 : 0)} s`;
}

export function formatBytes(value: number): string {
  if (value < 1024) {
    return `${value} B`;
  }
  if (value < 1024 * 1024) {
    return `${(value / 1024).toFixed(value < 10 * 1024 ? 1 : 0)} KB`;
  }
  return `${(value / (1024 * 1024)).toFixed(1)} MB`;
}
