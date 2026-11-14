// F4.1 — log-json-panel 正文服务(主线程侧)。
//
// live 形态:懒加载 inline Worker(CSP 需允许 worker-src blob:;旧 Electron file://
// 下普通文件 Worker 被 Chromium 禁止,必须走 inline blob)。
// 回退形态:jsdom/无 Worker/Worker 创建或运行失败时,退化为主线程同步计算
// (与 F4 之前行为等价),保证功能不因环境缺失而降级为不可用。
import type { RequestLogBody } from "@/contracts/app";

import type {
  LogBodyChildrenBatch,
  LogBodyExpandAllResult,
  LogBodyFilterResult,
  LogBodyStringResult,
  LogBodyView,
  LogBodyViewMeta
} from "./log-body-core";
import { LogBodyCache } from "./log-body-core";
// F4.1: 静态 ?worker&inline —— 产物内嵌 base64 Blob Worker,file:// 下
// 普通 URL Worker 被 Chromium 禁止,inline 是唯一可行形态;构造失败时运行时回退同步。
import LogBodyWorkerCtor from "./log-body-worker?worker&inline";
import type { LogBodyWorkerRequest, LogBodyWorkerResponse } from "./log-body-worker";

type WorkerCtor = new () => Worker;
// Omit 不在联合类型上分发,展开为每个成员分别去 id
type DistributiveOmit<T, K extends PropertyKey> = T extends unknown ? Omit<T, K> : never;
type LogBodyWorkerRequestData = DistributiveOmit<LogBodyWorkerRequest, "id">;

function loadWorkerCtor(): Promise<WorkerCtor | null> {
  return Promise.resolve().then(() => {
    if (typeof Worker === "undefined") {
      return null;
    }
    return LogBodyWorkerCtor as WorkerCtor;
  });
}

class WorkerTransport {
  private nextId = 1;
  private pending = new Map<number, (response: LogBodyWorkerResponse) => void>();
  dead = false;

  constructor(private readonly worker: Worker) {
    worker.onmessage = (event: MessageEvent<LogBodyWorkerResponse>) => {
      const resolve = this.pending.get(event.data.id);
      if (!resolve) {
        return;
      }
      this.pending.delete(event.data.id);
      resolve(event.data);
    };
    worker.onerror = () => this.markDead();
    worker.onmessageerror = () => this.markDead();
  }

  private markDead() {
    this.dead = true;
    for (const resolve of this.pending.values()) {
      resolve({ id: -1, ok: false, error: "log body worker failed" });
    }
    this.pending.clear();
  }

  request(request: LogBodyWorkerRequestData): Promise<LogBodyWorkerResponse> {
    if (this.dead) {
      return Promise.resolve({ id: -1, ok: false, error: "log body worker failed" });
    }
    const id = this.nextId++;
    return new Promise<LogBodyWorkerResponse>((resolve) => {
      this.pending.set(id, resolve);
      this.worker.postMessage({ ...request, id } satisfies LogBodyWorkerRequest);
    });
  }

  dispose() {
    this.markDead();
    this.worker.terminate();
  }
}

class LogBodyService {
  private syncCache = new LogBodyCache(12);
  private transport: WorkerTransport | null = null;
  private transportReady = false;
  // ctor 加载期间的并发调用必须共享同一装载 Promise:若各自拿到 null 会静默
  // 走同步回退,导致该 key 只存在于 syncCache 而 worker 缓存缺失(children 等
  // 后续请求返回空批次)。G7 实测抓出的真缺陷。
  private transportLoading: Promise<WorkerTransport | null> | null = null;

  private async ensureTransport(): Promise<WorkerTransport | null> {
    if (this.transport || this.transportReady) {
      return this.transport;
    }
    this.transportLoading ??= (async () => {
      const ctor = await loadWorkerCtor();
      debugRecord(`worker ctor=${ctor ? "ok" : "unavailable"}`);
      if (!ctor) {
        return null;
      }
      try {
        this.transport = new WorkerTransport(new ctor());
      } catch {
        this.transport = null;
      }
      return this.transport;
    })();
    const transport = await this.transportLoading;
    this.transportReady = true;
    return transport;
  }

  private disableTransport() {
    this.transport?.dispose();
    this.transport = null;
  }

  private async viaWorker<T>(
    request: LogBodyWorkerRequestData,
    pick: (response: LogBodyWorkerResponse) => T | undefined,
    fallback: () => T
  ): Promise<T> {
    const transport = await this.ensureTransport();
    if (transport) {
      const response = await transport.request(request);
      debugRecord(`${request.type} ok=${response.ok}${response.ok ? "" : ` err=${"error" in response ? response.error : ""}`}`);
      const picked = response.ok ? pick(response) : undefined;
      if (picked !== undefined) {
        return picked;
      }
      this.disableTransport();
      debugRecord(`transport disabled on ${request.type}`);
    }
    debugRecord(`${request.type} fallback`);
    return fallback();
  }

  format(key: string, body: RequestLogBody | null | undefined): Promise<LogBodyViewMeta> {
    return this.viaWorker<LogBodyViewMeta>(
      { type: "format", key, body: body ?? null },
      (response) => (response.ok && response.type === "format" ? response.view : undefined),
      () => {
        const { json: _json, ...view } = this.syncCache.format(key, body);
        return view;
      }
    );
  }

  children(key: string, path: string): Promise<LogBodyChildrenBatch> {
    return this.viaWorker<LogBodyChildrenBatch>(
      { type: "children", key, path },
      (response) => (response.ok && response.type === "children" ? response.result : undefined),
      () => this.syncCache.children(key, path)
    );
  }

  expandAll(key: string): Promise<LogBodyExpandAllResult> {
    return this.viaWorker<LogBodyExpandAllResult>(
      { type: "expandAll", key },
      (response) => (response.ok && response.type === "expandAll" ? response.result : undefined),
      () => this.syncCache.expandAll(key)
    );
  }

  stringAt(key: string, path: string): Promise<LogBodyStringResult> {
    return this.viaWorker<LogBodyStringResult>(
      { type: "stringAt", key, path },
      (response) => (response.ok && response.type === "stringAt" ? response.result : undefined),
      () => this.syncCache.stringAt(key, path)
    );
  }

  filter(key: string, query: string): Promise<LogBodyFilterResult> {
    return this.viaWorker<LogBodyFilterResult>(
      { type: "filter", key, query },
      (response) => (response.ok && response.type === "filter" ? response.result : undefined),
      () => this.syncCache.filter(key, query)
    );
  }

  forget(key: string): void {
    this.syncCache.forget(key);
    this.transport?.request({ type: "forget", key });
  }
}

let service: LogBodyService | null = null;

// 诊断钩子:G7 实测与排障用(观察 worker/sync 形态与事件序),生产体积可忽略。
// 320 条上限:长时间运行不会无限增长,仍足以覆盖一次完整排障会话。
const DEBUG_EVENT_LIMIT = 320;
const debugLog: string[] = [];
function debugRecord(event: string) {
  if (debugLog.length >= DEBUG_EVENT_LIMIT) {
    debugLog.shift();
  }
  debugLog.push(`${Date.now() % 100000} ${event}`);
  if (typeof window !== "undefined") {
    (window as { __wrLogBodyDebug?: unknown }).__wrLogBodyDebug = {
      events: debugLog,
      mode: service?.["transport"] ? "worker" : "sync"
    };
  }
}

export function getLogBodyService(): LogBodyService {
  service ??= new LogBodyService();
  debugRecord("service created");
  return service;
}

export type { LogBodyView };
