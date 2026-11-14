export function isJsonContainer(value: unknown): value is Record<string, unknown> | unknown[] {
  return value !== null && typeof value === "object";
}

export function jsonChildPath(parentPath: string, key: string): string {
  return `${parentPath}/${encodeURIComponent(key)}`;
}
