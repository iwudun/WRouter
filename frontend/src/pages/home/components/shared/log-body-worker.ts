// F4.1 — log-json-panel 解析 Worker(vite ?worker&inline,兼容 file:// 加载)。
// 持有按 key 缓存的解析结果;主线程只收到视图元数据与虚拟化节点批次。
import type { RequestLogBody } from "@/contracts/app";

import { LogBodyCache } from "./log-body-core";

export type LogBodyWorkerRequest =
  | { id: number; type: "format"; key: string; body: RequestLogBody | null }
  | { id: number; type: "children"; key: string; path: string }
  | { id: number; type: "expandAll"; key: string }
  | { id: number; type: "stringAt"; key: string; path: string }
  | { id: number; type: "filter"; key: string; query: string }
  | { id: number; type: "forget"; key: string };

export type LogBodyWorkerResponse =
  | { id: number; ok: true; type: "format"; view: import("./log-body-core").LogBodyViewMeta }
  | { id: number; ok: true; type: "children"; result: import("./log-body-core").LogBodyChildrenBatch }
  | { id: number; ok: true; type: "expandAll"; result: import("./log-body-core").LogBodyExpandAllResult }
  | { id: number; ok: true; type: "stringAt"; result: import("./log-body-core").LogBodyStringResult }
  | { id: number; ok: true; type: "filter"; result: import("./log-body-core").LogBodyFilterResult }
  | { id: number; ok: true; type: "forget" }
  | { id: number; ok: false; error: string };

const cache = new LogBodyCache(4);

const scope = self as unknown as {
  onmessage: ((event: MessageEvent<LogBodyWorkerRequest>) => void) | null;
  postMessage: (message: LogBodyWorkerResponse) => void;
};

scope.onmessage = (event: MessageEvent<LogBodyWorkerRequest>) => {
  const request = event.data;
  try {
    switch (request.type) {
      case "format": {
        const { json: _json, ...view } = cache.format(request.key, request.body);
        scope.postMessage({ id: request.id, ok: true, type: "format", view });
        return;
      }
      case "children":
        scope.postMessage({ id: request.id, ok: true, type: "children", result: cache.children(request.key, request.path) });
        return;
      case "expandAll":
        scope.postMessage({ id: request.id, ok: true, type: "expandAll", result: cache.expandAll(request.key) });
        return;
      case "stringAt":
        scope.postMessage({ id: request.id, ok: true, type: "stringAt", result: cache.stringAt(request.key, request.path) });
        return;
      case "filter":
        scope.postMessage({ id: request.id, ok: true, type: "filter", result: cache.filter(request.key, request.query) });
        return;
      case "forget":
        cache.forget(request.key);
        scope.postMessage({ id: request.id, ok: true, type: "forget" });
        return;
    }
  } catch (error) {
    scope.postMessage({ id: request.id, ok: false, error: error instanceof Error ? error.message : String(error) });
  }
};
