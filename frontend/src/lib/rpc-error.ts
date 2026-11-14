// RPC 错误信封解包（纯函数，无副作用，便于单测）。
//
// 后端错误响应有两种形状：
//   - 普通 RPC 错误：{ok:false, error:{message}}（顶层）
//   - FastAPI 信封（501 NOT_IMPLEMENTED / 401 等）：{detail:{ok:false,error:{message}}}
//     或 {detail:"..."}（detail 为字符串）
// 此前桥接层只读顶层 error.message，detail 信封下取到 undefined 而回退成
// "Unknown error"（web 桥甚至直接抛 TypeError），无法定位真实原因。

/** RPC 响应体（宽松）。 */
export type RpcPayload = {
  detail?: unknown;
  error?: { message?: string; stack?: string } | null;
  ok?: boolean;
  value?: unknown;
};

/** 从一个错误节点抽出人类可读消息：字符串本身 / node.error.message / node.message。 */
export function readErrorMessage(node: unknown): string | undefined {
  if (typeof node === "string") {
    return node.trim() ? node : undefined;
  }
  if (node && typeof node === "object") {
    const obj = node as Record<string, unknown>;
    const err = obj.error;
    if (err && typeof err === "object") {
      const message = (err as Record<string, unknown>).message;
      if (typeof message === "string" && message.trim()) return message;
    }
    if (typeof obj.message === "string" && obj.message.trim()) return obj.message;
  }
  return undefined;
}

/** 统一错误消息提取；无法解析时回退到 HTTP 状态描述。 */
export function rpcErrorMessage(payload: RpcPayload | undefined, status: number): string {
  if (payload) {
    const direct = readErrorMessage(payload);
    if (direct) return direct;
    if (payload.detail !== undefined) {
      const fromDetail = readErrorMessage(payload.detail);
      if (fromDetail) return fromDetail;
      if (payload.detail !== null) return JSON.stringify(payload.detail);
    }
  }
  return `WRouter web API failed with HTTP ${status}`;
}
