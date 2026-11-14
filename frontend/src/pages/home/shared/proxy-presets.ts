// 方案 20260906_01:命名代理预设的纯逻辑 —— 设置页 CRUD 校验、供应商侧
// 摘要与引用计数。口径与后端 `wr-infra/src/proxy.rs` 对齐(scheme 白名单、
// host/port 解析、loopback 判定、自指拒绝,§4.8);形态归一在 shared/config.ts。
import type {
  GatewayProviderConfig,
  ProviderProxyOverride,
  ProxyPreset,
  ProxyUpstreamConfig
} from "@/contracts/app";
import { normalizeProviderProxy } from "./config";

/** 预设 server 可带的 scheme(决策 #5:预设面放开 socks5/socks5h)。 */
export const PROXY_PRESET_SCHEMES: readonly string[] = ["http", "https", "socks5", "socks5h"];

/** 内置代理检测目标(镜像后端 `PRESET_TEST_TARGETS`,wr-server
 *  connectivity_probe.rs;口径:连通性探针 + 国际/国内主流 LLM 上游)。
 *  自定义目标存 config.proxy.testTargets,两者合并在设置页管理。 */
export const PROXY_TEST_TARGETS: readonly string[] = [
  "https://www.gstatic.com/generate_204",
  "https://api.openai.com",
  "https://api.anthropic.com",
  "https://api.deepseek.com",
  "https://open.bigmodel.cn",
  "https://dashscope.aliyuncs.com",
  "https://api.moonshot.cn"
];

/** 常见默认端口,仅作弹窗「端口」输入框的虚字提示(2026-09-13 三轮:
 *  新预设不再预填端口,留空由保存期校验拦截)。 */
export const DEFAULT_PROXY_PRESET_PORT = 7890;

export function createProxyPresetId(): string {
  return `preset-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
}

export function createProxyPreset(): ProxyPreset {
  return {
    id: createProxyPresetId(),
    name: "",
    password: "",
    port: 0,
    server: "",
    username: ""
  };
}

export function findProxyPreset(presets: ProxyPreset[], id: string | undefined): ProxyPreset | undefined {
  if (!id) {
    return undefined;
  }
  return presets.find((preset) => preset.id === id);
}

function splitProxyServer(server: string): { authority: string; scheme: string } {
  const separator = server.indexOf("://");
  const scheme = separator >= 0 ? server.slice(0, separator).trim().toLowerCase() : "http";
  const rest = separator >= 0 ? server.slice(separator + 3) : server;
  return { authority: rest.split(/[/?#]/)[0] ?? "", scheme };
}

function isValidPort(port: number): boolean {
  return Number.isInteger(port) && port >= 1 && port <= 65535;
}

/** 与后端 `ProxyPreset::to_proxy_url` + `preset_host_port` 同口径:
 *  解析出生效的 host/port(server 自带端口时以它为准,否则用 port 字段)。
 *  结构非法(空 server / scheme 不在白名单 / 无可用端口)→ undefined。 */
export function parseProxyPresetTarget(preset: ProxyPreset): { host: string; port: number } | undefined {
  const server = preset.server.trim();
  if (!server) {
    return undefined;
  }
  const { authority, scheme } = splitProxyServer(server);
  if (!PROXY_PRESET_SCHEMES.includes(scheme) || !authority) {
    return undefined;
  }
  const at = authority.lastIndexOf("@");
  const hostPort = at >= 0 ? authority.slice(at + 1) : authority;
  if (!hostPort) {
    return undefined;
  }
  // IPv6 字面量 `[::1]:1080`。
  if (hostPort.startsWith("[")) {
    const close = hostPort.indexOf("]");
    if (close < 0) {
      return undefined;
    }
    const host = hostPort.slice(0, close + 1);
    const rest = hostPort.slice(close + 1);
    const port = rest.startsWith(":") ? Number(rest.slice(1)) : preset.port;
    return isValidPort(port) ? { host, port } : undefined;
  }
  const colon = hostPort.lastIndexOf(":");
  if (colon > 0) {
    const explicit = Number(hostPort.slice(colon + 1));
    if (isValidPort(explicit)) {
      return { host: hostPort.slice(0, colon), port: explicit };
    }
  }
  return isValidPort(preset.port) ? { host: hostPort, port: preset.port } : undefined;
}

/** loopback 判定(镜像 `wr_infra::proxy::is_loopback_host`)。 */
export function isLoopbackProxyHost(host: string): boolean {
  const value = host.trim().toLowerCase();
  if (value === "localhost" || value === "::1" || value === "[::1]" || value === "0.0.0.0") {
    return true;
  }
  const parts = value.replace(/^\[/, "").replace(/\]$/, "").split(".");
  if (parts.length !== 4 || parts[0] !== "127") {
    return false;
  }
  return parts.every((part) => /^\d{1,3}$/.test(part) && Number(part) <= 255);
}

/** 列表摘要用的地址(不带 scheme 与凭据;解析失败时原样回显 server)。 */
export function proxyPresetAddress(preset: ProxyPreset): string {
  const target = parseProxyPresetTarget(preset);
  return target ? `${target.host}:${target.port}` : preset.server.trim();
}

/** §4.8 保存期校验的前端镜像(后端 `validate_proxy_presets` 是最终闸门)。
 *  返回可直接交给 `t()` 的文案键;合法时 null。 */
export function proxyPresetIssue(preset: ProxyPreset, presets: ProxyPreset[], gatewayPort: number): string | null {
  const name = preset.name.trim();
  if (!name) {
    return "Preset name is required.";
  }
  const duplicated = presets.some(
    (other) => other.id !== preset.id && other.name.trim().toLowerCase() === name.toLowerCase()
  );
  if (duplicated) {
    return "Preset name already exists.";
  }
  if (!preset.server.trim()) {
    return "Proxy server is required.";
  }
  if (!PROXY_PRESET_SCHEMES.includes(splitProxyServer(preset.server.trim()).scheme)) {
    return "Only http, https, socks5 and socks5h are supported.";
  }
  const target = parseProxyPresetTarget(preset);
  if (!target) {
    return "Port must be between 1 and 65535.";
  }
  // 决策 #8 / 验收 #6:指向 WRouter 自身监听端口 = 自指递归,保存期就拦。
  if (target.port === gatewayPort && isLoopbackProxyHost(target.host)) {
    return "This proxy points at WRouter's own gateway port and would loop requests back.";
  }
  return null;
}

/** 各预设被多少供应商引用(设置页的「被 N 个供应商使用」徽标与删除确认,
 *  决策 #6)。全局默认策略的引用不计入 —— 它就在同一个分区里,可见。 */
export function proxyPresetReferenceCounts(providers: GatewayProviderConfig[]): Record<string, number> {
  const counts: Record<string, number> = {};
  for (const provider of providers ?? []) {
    const presetId = provider?.proxy?.mode === "preset" ? provider.proxy.presetId?.trim() ?? "" : "";
    if (presetId) {
      counts[presetId] = (counts[presetId] ?? 0) + 1;
    }
  }
  return counts;
}

/** 供应商的生效覆盖(预设已删 → undefined = 跟随全局默认,与后端同口径)。 */
export function providerProxyOverride(
  provider: GatewayProviderConfig | undefined,
  presets: ProxyPreset[]
): ProviderProxyOverride | undefined {
  return normalizeProviderProxy(provider?.proxy, presets);
}

/** 供应商表单「网络代理」的模式档位(缺省/非法一律 inherit)。 */
export function providerProxyMode(proxy: ProviderProxyOverride | undefined): ProviderProxyOverride["mode"] {
  if (proxy?.mode === "direct") {
    return "direct";
  }
  return proxy?.mode === "preset" ? "preset" : "inherit";
}

/** 覆盖字段 → 表单选择(编辑已存供应商时的初值)。 */
export function proxySelectionFromOverride(
  proxy: ProviderProxyOverride | undefined
): { mode: ProviderProxyOverride["mode"]; presetId: string } {
  return { mode: providerProxyMode(proxy), presetId: proxy?.mode === "preset" ? proxy.presetId ?? "" : "" };
}

/** 表单选择 → 落库的覆盖字段。`inherit`、或选了「自定义代理」但还没选到
 *  具体预设(presetId 空)时返回 undefined = 不写 `proxy` 键,与后端
 *  `ProviderProxyOverride::from_value` 的 None 归一一致。 */
export function proxyOverrideFromSelection(
  mode: ProviderProxyOverride["mode"],
  presetId: string
): ProviderProxyOverride | undefined {
  if (mode === "direct") {
    return { mode: "direct" };
  }
  const trimmed = presetId.trim();
  return mode === "preset" && trimmed ? { mode: "preset", presetId: trimmed } : undefined;
}

/** 全局默认策略的展示文案(设置页摘要 / 供应商侧「跟随全局默认」的展开)。 */
export function globalProxySummary(
  upstream: ProxyUpstreamConfig,
  presets: ProxyPreset[],
  t: (value: string) => string
): string {
  if (upstream.mode === "none") {
    return t("Do not use proxy");
  }
  const preset = findProxyPreset(presets, upstream.presetId);
  return preset ? preset.name : t("No proxy preset selected");
}

/** 供应商条目代理图标的解析形态(2026-09-13):显式覆盖用其自身;「跟随全局
 *  默认」解析全局策略 —— 预设缺失按直连,与后端决策链 warn + 直连同口径。 */
export function resolvedProxyIconMode(
  override: ProviderProxyOverride | undefined,
  upstream: ProxyUpstreamConfig,
  presets: ProxyPreset[]
): "direct" | "preset" {
  if (override?.mode === "direct" || override?.mode === "preset") {
    return override.mode;
  }
  return upstream.mode === "preset" && Boolean(findProxyPreset(presets, upstream.presetId))
    ? "preset"
    : "direct";
}

/** 供应商代理来源的展示文案(列表/详情摘要)。 */
export function providerProxySummary(
  proxy: ProviderProxyOverride | undefined,
  presets: ProxyPreset[],
  t: (value: string) => string
): string {
  if (proxy?.mode === "direct") {
    return t("Direct connection");
  }
  if (proxy?.mode === "preset") {
    const preset = findProxyPreset(presets, proxy.presetId);
    if (preset) {
      return preset.name;
    }
  }
  return t("Follow global default");
}

/** 探针/测速载荷里的代理覆盖(决策 #9:显式随载荷传)。
 *  inherit → 不带字段,后端回落全局默认。 */
export function probeProxyPayload(proxy: ProviderProxyOverride | undefined): ProviderProxyOverride | undefined {
  if (proxy?.mode === "direct") {
    return { mode: "direct" };
  }
  if (proxy?.mode === "preset" && proxy.presetId) {
    return { mode: "preset", presetId: proxy.presetId };
  }
  return undefined;
}
