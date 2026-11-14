// F2b.1/F2b.2/F2b.6: 管理面 SSE 事件客户端 + 事件→Query 失效桥。
//
// 传输实现偏离说明（记录于方案 #14 完成记录）：原生 EventSource 无法携带
// `x-wrouter-web-auth` 自定义头（Rust auth_middleware 只认该头，mod.rs:105），
// 因此本客户端用 fetch + ReadableStream 实现同语义：text/event-stream 帧解析、
// 断线指数退避重连（1s→2s→…→30s 封顶）、重连成功后回调（全量刷新活跃
// query）。后端零改动；桌面壳（Tauri 走 http://127.0.0.1:3462/，同源）与纯浏览器
// （同源）双形态可用。
//
// 事件契约：contracts/events-schema.json —— `event: <type>` + `data: <json>`；
// 四类事件 config-changed / log-appended / usage-updated / gateway-status
// （Rust R5 事件总线，gateway-status 同时承担 1s 心跳）。
import { queryKeys } from "@/lib/queryKeys";

export type GatewayEvent = {
  type: string;
  data: unknown;
};

export type GatewayEventsStatus = "connecting" | "live" | "reconnecting";

/**
 * 服务停机通告事件类型(2026-11-03)。
 *
 * 后端收到停机请求时,给**所有**已连上的 SSE 连接补发这一帧,然后收流。
 * 客户端收到后必须**停止自动重连** —— 后端正在退出,重连只会徒劳地刷失败请求。
 */
export const SERVICE_STOPPING_EVENT = "service-stopping";

// ---------------------------------------------------------------------------
// 后端连接信息（双形态）
// ---------------------------------------------------------------------------

const WEB_AUTH_STORAGE_KEY = "wr.webAuthToken"; // 与 web-client-bridge.ts 的约定一致

export function resolveBackendConnection(): { apiBase: string; authToken: string } {
  // 桌面：preload 暴露注入的 --wrouter-api/--wrouter-token。
  const wr = typeof window !== "undefined" ? window.wr : undefined;
  if (wr?.runtime === "desktop" && typeof wr.backendConnection === "function") {
    return wr.backendConnection();
  }
  // 纯浏览器：同源（页面由管理端口 3462 服务）+ web 桥落地的 token。
  let authToken = "";
  try {
    authToken = window.sessionStorage.getItem(WEB_AUTH_STORAGE_KEY)?.trim() ?? "";
  } catch {
    // storage 不可用时退化为无 token（后端 401 → 重连循环）。
  }
  return { apiBase: "", authToken };
}

// ---------------------------------------------------------------------------
// SSE 帧解析（增量 buffer）
// ---------------------------------------------------------------------------

export function createSseFrameParser(onFrame: (frame: { event: string; data: string }) => void) {
  let buffer = "";
  return (chunk: string) => {
    buffer += chunk;
    let boundary = buffer.indexOf("\n\n");
    while (boundary >= 0) {
      const frame = buffer.slice(0, boundary);
      buffer = buffer.slice(boundary + 2);
      const eventLines: string[] = [];
      const dataLines: string[] = [];
      for (const line of frame.split("\n")) {
        if (line.startsWith("event:")) {
          eventLines.push(line.slice("event:".length).trim());
        } else if (line.startsWith("data:")) {
          dataLines.push(line.slice("data:".length).trimStart());
        }
        // id:/retry:/注释行：契约声明不支持 Last-Event-ID replay，忽略。
      }
      if (dataLines.length > 0) {
        onFrame({ event: eventLines[0] ?? "message", data: dataLines.join("\n") });
      }
      boundary = buffer.indexOf("\n\n");
    }
  };
}

// ---------------------------------------------------------------------------
// 事件 → 失效目标映射表（F2b.2，映射集中管理）
// ---------------------------------------------------------------------------

export type EventKeyGroup = "config" | "callLogs" | "usage" | "status";

export type InvalidationTarget = { group: EventKeyGroup; prefix: readonly unknown[] };

export function eventInvalidationTargets(type: string): InvalidationTarget[] {
  switch (type) {
    case "config-changed":
      // 配置变更：providers 列表来自 config、路由诊断需要重编译。
      return [
        { group: "config", prefix: queryKeys.config },
        { group: "config", prefix: queryKeys.routeDiagnostics }
      ];
    case "log-appended":
      // 前缀匹配全部 callLogs key（当前页 + 各筛选态）。
      return [{ group: "callLogs", prefix: ["callLogs"] }];
    case "inflight-changed":
      // 在途登记/进入处理中（调用日志 等待中/处理中 实时行；完成仍走
      // log-appended）。与 callLogs 同组，共用 300ms 去抖。
      return [{ group: "callLogs", prefix: ["callLogs"] }];
    case "usage-updated":
      return [{ group: "usage", prefix: ["usage"] }];
    case "gateway-status":
      // 1s 心跳也走这里——由 status 组 3s 节流约束（见 Coalescer）。
      return [
        { group: "status", prefix: queryKeys.gatewayStatus }
      ];
    case SERVICE_STOPPING_EVENT:
      // 停机通告:没有需要失效的查询 —— 客户端据此切「正在停止」态并取消重连。
      return [];
    default:
      return [];
  }
}

/**
 * F2b.6 失效合并：同组目标去抖（config/callLogs/usage 300ms），status 组节流
 * （3s 最小间隔——心跳 1/s 不至于把状态查询打回 1s 轮询）。
 */
export class EventInvalidationCoalescer {
  private pending = new Map<EventKeyGroup, Map<string, readonly unknown[]>>();
  private timers = new Map<EventKeyGroup, ReturnType<typeof setTimeout>>();
  private lastFlushAt = new Map<EventKeyGroup, number>();

  private static readonly debounceMs: Record<EventKeyGroup, number> = {
    config: 300,
    callLogs: 300,
    usage: 300,
    status: 0
  };
  private static readonly minIntervalMs: Record<EventKeyGroup, number> = {
    config: 0,
    callLogs: 0,
    usage: 0,
    status: 3000
  };

  constructor(private readonly onFlush: (prefixes: ReadonlyArray<readonly unknown[]>) => void) {}

  add(eventType: string): void {
    for (const target of eventInvalidationTargets(eventType)) {
      const bucket = this.pending.get(target.group) ?? new Map<string, readonly unknown[]>();
      bucket.set(JSON.stringify(target.prefix), target.prefix);
      this.pending.set(target.group, bucket);

      if (this.timers.has(target.group)) {
        continue; // 已有待触发定时器（同组合并）
      }
      const now = Date.now();
      const last = this.lastFlushAt.get(target.group);
      // 首次触达：立即按 debounce 调度；此后受 minInterval 节流约束。
      const due = last === undefined
        ? now + EventInvalidationCoalescer.debounceMs[target.group]
        : Math.max(
            now + EventInvalidationCoalescer.debounceMs[target.group],
            last + EventInvalidationCoalescer.minIntervalMs[target.group]
          );
      const timer = setTimeout(() => {
        this.timers.delete(target.group);
        this.flush(target.group);
      }, Math.max(0, due - now));
      this.timers.set(target.group, timer);
    }
  }

  flush(group: EventKeyGroup): void {
    const bucket = this.pending.get(group);
    this.pending.delete(group);
    this.lastFlushAt.set(group, Date.now());
    if (bucket && bucket.size > 0) {
      this.onFlush([...bucket.values()]);
    }
  }

  dispose(): void {
    for (const timer of this.timers.values()) {
      clearTimeout(timer);
    }
    this.timers.clear();
    this.pending.clear();
  }
}

// ---------------------------------------------------------------------------
// 连接客户端（重连状态机）
// ---------------------------------------------------------------------------

export interface GatewayEventsClientOptions {
  onEvent: (event: GatewayEvent) => void;
  onStatus: (status: GatewayEventsStatus) => void;
  /** 从 reconnecting 恢复为 live 时回调（全量刷新活跃 query）。 */
  onReconnected: () => void;
  /**
   * 收到 `service-stopping` 停机通告时回调(2026-11-03)。
   *
   * 回调后客户端**立即停止**:不再重连、不再消费事件流 —— 后端正在退出。
   */
  onServiceStopping?: () => void;
  fetchImpl?: typeof fetch;
  /** 指数退避封顶（测试用低值）。 */
  maxBackoffMs?: number;
}

export interface GatewayEventsClient {
  start(): void;
  stop(): void;
}

const RECONNECT_BASE_MS = 1000;
const RECONNECT_MAX_MS = 30000;

export function createGatewayEventsClient(options: GatewayEventsClientOptions): GatewayEventsClient {
  const fetchImpl = options.fetchImpl ?? fetch.bind(globalThis);
  const maxBackoffMs = options.maxBackoffMs ?? RECONNECT_MAX_MS;
  let attempt = 0;
  let disposed = false;
  let running = false;
  let controller: AbortController | undefined;
  let wakeUp: (() => void) | undefined;

  const sleep = (ms: number) =>
    new Promise<void>((resolve) => {
      const timer = setTimeout(() => {
        wakeUp = undefined;
        resolve();
      }, ms);
      wakeUp = () => {
        clearTimeout(timer);
        wakeUp = undefined;
        resolve();
      };
    });

  async function run(): Promise<void> {
    running = true;
    while (!disposed) {
      controller = new AbortController();
      try {
        options.onStatus(attempt === 0 ? "connecting" : "reconnecting");
        const conn = resolveBackendConnection();
        const response = await fetchImpl(`${conn.apiBase}/api/wr/events`, {
          headers: conn.authToken ? { "x-wrouter-web-auth": conn.authToken } : {},
          signal: controller.signal
        });
        if (!response.ok || !response.body) {
          throw new Error(`WRouter events stream failed with HTTP ${response.status}`);
        }

        const reconnected = attempt > 0;
        attempt = 0;
        options.onStatus("live");
        if (reconnected) {
          options.onReconnected();
        }

        // 停机通告:后端开始退出时会补发一帧 `service-stopping`。
        // 收到即收工 —— 取消订阅 + **不再自动重连**(重连只会徒劳地刷失败请求)。
        let stopping = false;
        const parseFrame = createSseFrameParser((frame) => {
          if (frame.event === SERVICE_STOPPING_EVENT) {
            stopping = true;
            options.onServiceStopping?.();
            return;
          }
          try {
            options.onEvent({ type: frame.event, data: JSON.parse(frame.data) });
          } catch {
            // 非 JSON data 帧：契约外内容，忽略（心跳/注释兼容）。
          }
        });

        const reader = response.body.getReader();
        const decoder = new TextDecoder();
        for (;;) {
          const { done, value } = await reader.read();
          if (done) {
            break;
          }
          parseFrame(decoder.decode(value, { stream: true }));
          if (stopping) {
            break;
          }
        }
        if (stopping) {
          disposed = true;
          controller.abort();
          return;
        }
        // 服务端正常收流（断开）→ 走重连。
      } catch {
        // 网络错误/非 2xx：走重连。AbortError 仅在 stop() 时发生，disposed 已置位。
      }
      if (disposed) {
        break;
      }
      options.onStatus("reconnecting");
      const delay = Math.min(maxBackoffMs, RECONNECT_BASE_MS * 2 ** attempt);
      attempt += 1;
      await sleep(delay);
    }
  }

  return {
    start() {
      if (!disposed && !running) {
        void run();
      }
    },
    stop() {
      disposed = true;
      controller?.abort();
      wakeUp?.();
    }
  };
}
