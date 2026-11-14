import { rpcErrorMessage, type RpcPayload } from "@/lib/rpc-error";
import { isTauriShell as isTauriShellRuntime } from "@/lib/runtime";

type WrApi = NonNullable<Window["wr"]>;

const rpcEndpoint = "/api/wr/rpc";
const sessionEndpoint = "/api/wr/session";
const webAuthHeader = "x-wrouter-web-auth";
const webAuthQueryParam = "wr_web_token";
const webAuthStorageKey = "wr.webAuthToken";
// 同步兜底令牌:URL 参数(无头/远程逃生通道)或上次的 sessionStorage。
// Web 模式下会被 `/api/wr/session` 返回的令牌覆盖(见 bootstrapSessionToken)。
let webAuthToken = readWebAuthToken();
// 令牌就绪信号:Web 模式启动时先取一次 session 令牌,取不到才沿用兜底令牌。
// 首个 RPC 必须 await 它,否则会带着空令牌打出去(401)。
let tokenReady: Promise<void> = Promise.resolve();

async function rpc(method: string, args: unknown[] = []): Promise<unknown> {
  // 等令牌就绪(桌面模式下恒为已就绪,无额外开销)。
  await tokenReady;
  const response = await fetch(rpcEndpoint, {
    body: JSON.stringify({ args: trimTrailingUndefined(args), method }),
    headers: {
      "content-type": "application/json",
      ...(webAuthToken ? { [webAuthHeader]: webAuthToken } : {})
    },
    method: "POST"
  });
  let payload: RpcPayload | undefined;
  try {
    payload = await response.json() as RpcPayload;
  } catch {
    payload = undefined;
  }
  if (!response.ok || !payload?.ok) {
    const message = rpcErrorMessage(payload, response.status);
    console.error(`[wr:rpc] ${method} FAILED`, { status: response.status, message, hasToken: Boolean(webAuthToken) });
    throw new Error(message);
  }
  if (import.meta.env.DEV && method === "getGatewayStatus") {
    console.log(`[wr:rpc] ${method} OK`, JSON.stringify(payload.value));
  }
  return payload.value;
}

function trimTrailingUndefined(args: unknown[]): unknown[] {
  let end = args.length;
  while (end > 0 && args[end - 1] === undefined) {
    end -= 1;
  }
  return end === args.length ? args : args.slice(0, end);
}

function readWebAuthToken(): string {
  const tokenFromUrl = readWebAuthTokenFromUrl();
  if (tokenFromUrl) {
    writeStoredWebAuthToken(tokenFromUrl);
    return tokenFromUrl;
  }
  return readStoredWebAuthToken();
}

function readWebAuthTokenFromUrl(): string {
  try {
    const url = new URL(window.location.href);
    const token = url.searchParams.get(webAuthQueryParam)?.trim() ?? "";
    if (!token) {
      return "";
    }
    url.searchParams.delete(webAuthQueryParam);
    window.history.replaceState(window.history.state, "", `${url.pathname}${url.search}${url.hash}`);
    return token;
  } catch {
    return "";
  }
}

function readStoredWebAuthToken(): string {
  try {
    return window.sessionStorage.getItem(webAuthStorageKey)?.trim() ?? "";
  } catch {
    return "";
  }
}

function writeStoredWebAuthToken(token: string): void {
  try {
    window.sessionStorage.setItem(webAuthStorageKey, token);
  } catch {
    // The in-memory token still works for the current page load if storage is unavailable.
  }
}

/**
 * 20261102_01 P1-7:从同源 `/api/wr/session` 取管理令牌。
 *
 * 后端仅在开启静态托管(`serve_ui`)且来源为 loopback 时返回令牌,否则 403
 * 或 404。**令牌绝不放进 URL**(URL 会进浏览器历史 / 日志 / Referer),
 * 只经同源 fetch 获取,成功后写入 sessionStorage 供 SSE 客户端复用。
 *
 * 取不到时不报错:沿用 URL 参数 / sessionStorage 的兜底令牌
 * (无头与远程场景的逃生通道)。
 */
async function bootstrapSessionToken(): Promise<void> {
  try {
    const response = await fetch(sessionEndpoint, {
      headers: { accept: "application/json" },
      method: "GET"
    });
    if (!response.ok) {
      return;
    }
    const payload = await response.json() as { token?: unknown };
    const token = typeof payload?.token === "string" ? payload.token.trim() : "";
    if (!token) {
      return;
    }
    webAuthToken = token;
    writeStoredWebAuthToken(token);
  } catch {
    // 无 session 端点(后端未开启静态托管)→ 保持兜底令牌。
  }
}

function noopSubscription(): () => void {
  return () => undefined;
}

// A1-T5：桌面专属能力在 Web 模式统一抛友好错误（消息为 i18n key，
// 经 useAppErrorText/formatAppError 展示时会被翻译），避免透出后端 501 原始错误。
const desktopOnlyMessage = "This action requires the WRouter desktop app.";

function desktopOnlyStub(): Promise<never> {
  return Promise.reject(new Error(desktopOnlyMessage));
}

function normalizeExternalHttpUrl(value: string): string {
  const url = new URL(value.trim());
  if (url.protocol !== "http:" && url.protocol !== "https:") {
    throw new Error("Only http and https URLs can be opened.");
  }
  return url.toString();
}

// ---------------------------------------------------------------------------
// 桥接层装配(20261102_01 P3-4)
// ---------------------------------------------------------------------------
//
// 一份前端 bundle 同时服务三种运行时:
//
// | 运行时   | window.wr 来源              | 业务方法            | 桌面方法          |
// |----------|-----------------------------|---------------------|-------------------|
// | 浏览器   | 本文件装配                   | 本文件 RPC          | 本文件兜底        |
// | Tauri    | Tauri init script 注入      | **本文件补齐**      | Tauri 命令        |
//
// Tauri 路径的关键约束(init script 在页面脚本之前注入):本文件**只能补齐
// 业务方法**,不能整体替换 `window.wr` —— 否则会把 Tauri 注入的
// `runtime: "desktop"` 与 8 个桌面方法覆盖掉(方案 §6 P3-4 约束)。

/**
 * 业务方法(桌面 / Web 共用同一套 RPC 逻辑)。
 *
 * 集合与后端 `managementRpc` 一致(28 项)——
 * `exportData` / `getUpdateStatus` 虽在后端 `managementRpc` 里,但它们同时
 * 是**桌面方法**(由壳层提供),故不在此列。
 */
const businessBridge: Partial<WrApi> = {
  checkProviderConnectivity: (request) => rpc("checkProviderConnectivity", [request]) as ReturnType<WrApi["checkProviderConnectivity"]>,
  getAppInfo: () => rpc("getAppInfo") as ReturnType<WrApi["getAppInfo"]>,
  getCallLogs: (filter) => rpc("getCallLogs", [filter]) as ReturnType<WrApi["getCallLogs"]>,
  getConfig: () => rpc("getConfig") as ReturnType<WrApi["getConfig"]>,
  getGatewayStatus: () => rpc("getGatewayStatus") as ReturnType<WrApi["getGatewayStatus"]>,
  // 20261102_01 P6:服务控制(暂停 / 恢复 / 退出服务)——通用能力。
  getRuntimeControl: () => rpc("getRuntimeControl") as ReturnType<WrApi["getRuntimeControl"]>,
  setGatewayEnabled: (enabled) => rpc("setGatewayEnabled", [enabled]) as ReturnType<WrApi["setGatewayEnabled"]>,
  stopService: () => rpc("stopService") as ReturnType<WrApi["stopService"]>,
  getApiKeyLastUsed: () => rpc("getApiKeyLastUsed") as ReturnType<WrApi["getApiKeyLastUsed"]>,
  getProviderAccountSnapshots: (provider, options) => rpc("getProviderAccountSnapshots", [provider, options]) as ReturnType<WrApi["getProviderAccountSnapshots"]>,
  getProviderCredentialStats: (request) => rpc("getProviderCredentialStats", [request]) as ReturnType<WrApi["getProviderCredentialStats"]>,
  clearCredentialCooldown: (request) => rpc("clearCredentialCooldown", [request]) as ReturnType<WrApi["clearCredentialCooldown"]>,
  getRequestLogDetail: (request) => rpc("getRequestLogDetail", [request]) as ReturnType<WrApi["getRequestLogDetail"]>,
  getRequestLogTrace: (request) => rpc("getRequestLogTrace", [request]) as ReturnType<WrApi["getRequestLogTrace"]>,
  getSavingsStats: (range) => rpc("getSavingsStats", [range]) as ReturnType<WrApi["getSavingsStats"]>,
  getUsageStats: (range, filter) => rpc("getUsageStats", [range, filter]) as ReturnType<WrApi["getUsageStats"]>,
  probeProviderCandidates: (request) => rpc("probeProviderCandidates", [request]) as ReturnType<WrApi["probeProviderCandidates"]>,
  // 20261017_01:添加模型弹窗「获取模型」。
  fetchProviderModels: (request) => rpc("fetchProviderModels", [request]) as ReturnType<WrApi["fetchProviderModels"]>,
  // 20261030_01:模型指纹检测与指纹库管理。
  detectModelFingerprint: (request) => rpc("detectModelFingerprint", [request]) as ReturnType<WrApi["detectModelFingerprint"]>,
  listModelFingerprints: (request) => rpc("listModelFingerprints", [request]) as ReturnType<WrApi["listModelFingerprints"]>,
  enrollModelFingerprint: (request) => rpc("enrollModelFingerprint", [request]) as ReturnType<WrApi["enrollModelFingerprint"]>,
  deleteModelFingerprint: (request) => rpc("deleteModelFingerprint", [request]) as ReturnType<WrApi["deleteModelFingerprint"]>,
  exportModelFingerprints: (request) => rpc("exportModelFingerprints", [request]) as ReturnType<WrApi["exportModelFingerprints"]>,
  importModelFingerprint: (request) => rpc("importModelFingerprint", [request]) as ReturnType<WrApi["importModelFingerprint"]>,
  saveApiKeys: (apiKeys) => rpc("saveApiKeys", [apiKeys]) as ReturnType<WrApi["saveApiKeys"]>,
  saveConfig: (config, options) => rpc("saveConfig", [config, options]) as ReturnType<WrApi["saveConfig"]>,
  testApiEndpoints: (request) => rpc("testApiEndpoints", [request]) as ReturnType<WrApi["testApiEndpoints"]>,
  testProxyPreset: (request) => rpc("testProxyPreset", [request]) as ReturnType<WrApi["testProxyPreset"]>
};

/**
 * Web 模式专有的桌面方法实现(纯浏览器没有原生能力)。
 *
 * Tauri 壳下这些方法由壳层提供,这里的实现**不会**生效。
 */
const webDesktopBridge: Partial<WrApi> = {
  exportData: () => rpc("exportData") as ReturnType<WrApi["exportData"]>,
  getUpdateStatus: () => rpc("getUpdateStatus") as ReturnType<WrApi["getUpdateStatus"]>,
  openExternal: async (url) => {
    window.open(normalizeExternalHttpUrl(url), "_blank", "noopener,noreferrer");
  },
  setAppLanguage: () => Promise.resolve(),
  updateCheck: () => desktopOnlyStub() as ReturnType<WrApi["updateCheck"]>,
  updateDownload: () => desktopOnlyStub() as ReturnType<WrApi["updateDownload"]>,
  updateInstall: () => desktopOnlyStub() as ReturnType<WrApi["updateInstall"]>,
  onOpenSettingsRequest: noopSubscription,
  onOpenUpdateRequest: noopSubscription,
  onUpdateStatusChanged: noopSubscription
};

/**
 * Tauri 桌面壳判定:Tauri 运行时会往 window 上挂 `__TAURI_INTERNALS__`。
 *
 * 用它而不是「`window.wr` 是否存在」来分流,是因为 Web 模式也会注入
 * `window.wr` —— 那条路径**不能**动(已注入的桥要原样保留)。
 */
const isTauriShell = isTauriShellRuntime();

if (isTauriShell) {
  // 20261102_01 P3-4:Tauri 侧已注入 runtime + 8 个桌面方法;这里**就地
  // 补齐业务方法**(复用同一套 RPC 逻辑),绝不覆盖 Tauri 注入的值。
  const injected = (window.wr ?? {}) as WrApi;
  injected.runtime = "desktop";
  Object.assign(injected, businessBridge);
  window.wr = injected;
  // Tauri 壳不注入令牌 → 走同源 `/api/wr/session` 取一次(与 Web 模式同路)。
  tokenReady = bootstrapSessionToken();
} else if (!window.wr) {
  // 纯浏览器:装完整 Web 桥。
  tokenReady = bootstrapSessionToken();
  window.wr = {
    ...businessBridge,
    ...webDesktopBridge,
    // A1：运行时标志。Web 桥固定为 "web"，供 lib/runtime.ts 区分桌面/Web 能力。
    runtime: "web"
  } as WrApi;
}
// 桌面壳(Tauri)已注入完整桥 → 保持原样,只补业务方法。
