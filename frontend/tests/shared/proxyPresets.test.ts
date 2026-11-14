import { describe, expect, it } from "vitest";
import type { GatewayProviderConfig, ProxyPreset, ProxyUpstreamConfig } from "@/contracts/app";
import {
  normalizeProviderProxy, normalizeProxyPresets, normalizeProxyTestTargets, normalizeProxyUpstreamConfig
} from "@/pages/home/shared/config";
import {
  createProxyPreset, createProxyPresetId, findProxyPreset,
  globalProxySummary, isLoopbackProxyHost,
  parseProxyPresetTarget, probeProxyPayload, providerProxyOverride, providerProxySummary,
  proxyOverrideFromSelection, proxyPresetAddress, proxyPresetIssue, proxyPresetReferenceCounts,
  proxySelectionFromOverride, resolvedProxyIconMode
} from "@/pages/home/shared/proxy-presets";

// 方案 20260906_01:命名代理预设 + 供应商级覆盖的前端纯逻辑。
// 口径必须与后端 wr-infra/src/proxy.rs、wr-infra/src/config.rs 一致 ——
// 这些测试就是「两侧同源」的钉子。

/** 恒等翻译(英文键原样返回),用于摘要文案断言。 */
const t = (value: string) => value;

function preset(patch: Partial<ProxyPreset> = {}): ProxyPreset {
  return {
    id: "clash",
    name: "Clash 本地",
    password: "",
    port: 7890,
    server: "127.0.0.1",
    username: "",
    ...patch
  };
}

function upstream(patch: Partial<ProxyUpstreamConfig> = {}): ProxyUpstreamConfig {
  return { mode: "none", presetId: "", ...patch };
}

function provider(patch: Partial<GatewayProviderConfig> = {}): GatewayProviderConfig {
  return { id: "p1", models: [], name: "P1", ...patch } as GatewayProviderConfig;
}

describe("normalizeProxyPresets(与后端 parse_proxy_presets 同口径)", () => {
  it("非数组 → 空表(旧配置零迁移)", () => {
    expect(normalizeProxyPresets(undefined)).toEqual([]);
    expect(normalizeProxyPresets("nope" as unknown as Partial<ProxyPreset>[])).toEqual([]);
  });

  it("id 非空且去重(首个胜出),name 缺省回退 id,字符串 trim", () => {
    const normalized = normalizeProxyPresets([
      { id: " p1 ", name: "  First  ", password: "pw", port: 7890, server: " a.lan ", username: " u " },
      { id: "p1", name: "Dup", port: 1080, server: "b.lan" },
      { id: "", name: "No id", port: 1, server: "c.lan" },
      { name: "Missing id", port: 2, server: "d.lan" },
      "junk" as unknown as Partial<ProxyPreset>
    ]);
    expect(normalized).toHaveLength(1);
    expect(normalized[0]).toEqual({
      id: "p1",
      name: "First",
      password: "pw",
      port: 7890,
      server: "a.lan",
      username: "u"
    });
  });

  it("端口只取整不钳制(范围属于校验职责),非法端口归零", () => {
    expect(normalizeProxyPresets([{ id: "p", port: 7890.9 }])[0].port).toBe(7890);
    expect(normalizeProxyPresets([{ id: "p", port: 99999 }])[0].port).toBe(99999);
    expect(normalizeProxyPresets([{ id: "p", port: Number.NaN }])[0].port).toBe(0);
    expect(normalizeProxyPresets([{ id: "p" }])[0].port).toBe(0);
  });

  it("name 为空回退 id(与后端一致)", () => {
    expect(normalizeProxyPresets([{ id: "p9", name: "   " }])[0].name).toBe("p9");
  });
});

describe("normalizeProxyTestTargets(自定义检测目标)", () => {
  it("仅收 http/https,去重保序,截断到 12 条;非数组 → 空", () => {
    expect(
      normalizeProxyTestTargets(["https://a.com", " http://b.com ", "ftp://c.com", "https://a.com", "nope"])
    ).toEqual(["https://a.com", "http://b.com"]);
    expect(normalizeProxyTestTargets(undefined)).toEqual([]);
    expect(normalizeProxyTestTargets("https://a.com")).toEqual([]);
    const twelve = Array.from({ length: 15 }, (_, index) => `https://t${index}.com`);
    expect(normalizeProxyTestTargets(twelve)).toHaveLength(12);
  });
});

describe("normalizeProxyUpstreamConfig(全局默认策略)", () => {
  const presets = [preset()];

  it("mode 白名单 none/preset;system 退役与未知值一并落到默认(none)", () => {
    expect(normalizeProxyUpstreamConfig({ mode: "none", presetId: "" }, presets).mode).toBe("none");
    expect(normalizeProxyUpstreamConfig({ mode: "preset", presetId: "clash" }, presets).mode).toBe("preset");
    // 2026-09-13:system 模式退役 —— 存量配置读侧落到默认 none。
    expect(normalizeProxyUpstreamConfig({ mode: "system" as unknown as "none", presetId: "" }, presets).mode).toBe("none");
    // 决策 #4:后端已把 legacy custom 迁移为 preset,前端读到 custom 视为未知。
    const legacy = normalizeProxyUpstreamConfig(
      { mode: "custom" as unknown as "none", presetId: "" },
      presets
    );
    expect(legacy.mode).toBe("none");
  });

  it("preset 指向已删预设 → 清空 presetId(UI 回到「未选择」,运行时 warn + 直连)", () => {
    expect(normalizeProxyUpstreamConfig({ mode: "preset", presetId: "gone" }, presets)).toEqual({
      mode: "preset",
      presetId: ""
    });
    expect(normalizeProxyUpstreamConfig({ mode: "preset", presetId: "clash" }, presets).presetId).toBe("clash");
  });
});

describe("normalizeProviderProxy(与后端 ProviderProxyOverride::from_value 同口径)", () => {
  const presets = [preset()];

  it("direct 保留;preset 命中保留;其余归一为 undefined(= inherit)", () => {
    expect(normalizeProviderProxy({ mode: "direct" }, presets)).toEqual({ mode: "direct" });
    expect(normalizeProviderProxy({ mode: "preset", presetId: "clash" }, presets)).toEqual({
      mode: "preset",
      presetId: "clash"
    });
    expect(normalizeProviderProxy({ mode: "inherit" }, presets)).toBeUndefined();
    expect(normalizeProviderProxy(undefined, presets)).toBeUndefined();
    expect(normalizeProviderProxy({ mode: "preset" }, presets)).toBeUndefined();
    expect(normalizeProviderProxy({ mode: "preset", presetId: "  " }, presets)).toBeUndefined();
    // 决策 #6:预设被删 → 引用方回落 inherit。
    expect(normalizeProviderProxy({ mode: "preset", presetId: "gone" }, presets)).toBeUndefined();
    expect(normalizeProviderProxy({ mode: "round-robin" as unknown as "direct" }, presets)).toBeUndefined();
  });
});

describe("parseProxyPresetTarget(与后端 to_proxy_url + preset_host_port 同口径)", () => {
  it("裸 host 用 port 字段;自带 scheme/端口时以 server 为准", () => {
    expect(parseProxyPresetTarget(preset())).toEqual({ host: "127.0.0.1", port: 7890 });
    expect(parseProxyPresetTarget(preset({ server: "socks5://127.0.0.1:1080", port: 0 }))).toEqual({
      host: "127.0.0.1",
      port: 1080
    });
    expect(parseProxyPresetTarget(preset({ server: "gw.corp", port: 8080 }))).toEqual({ host: "gw.corp", port: 8080 });
  });

  it("凭据与 path 不参与 host/port;IPv6 字面量可解析", () => {
    expect(
      parseProxyPresetTarget(preset({ password: "p", server: "http://u:p@proxy.lan/extra", username: "u" }))
    ).toEqual({ host: "proxy.lan", port: 7890 });
    expect(parseProxyPresetTarget(preset({ server: "[::1]", port: 1080 }))).toEqual({ host: "[::1]", port: 1080 });
    expect(parseProxyPresetTarget(preset({ server: "socks5h://[::1]:1080" }))).toEqual({ host: "[::1]", port: 1080 });
  });

  it("结构非法 → undefined(空 server / scheme 非白名单 / 端口不可用)", () => {
    expect(parseProxyPresetTarget(preset({ server: "   " }))).toBeUndefined();
    expect(parseProxyPresetTarget(preset({ server: "ftp://proxy.lan" }))).toBeUndefined();
    expect(parseProxyPresetTarget(preset({ port: 0 }))).toBeUndefined();
    expect(parseProxyPresetTarget(preset({ port: 70000 }))).toBeUndefined();
  });

  it("地址摘要不带 scheme 与凭据", () => {
    expect(proxyPresetAddress(preset())).toBe("127.0.0.1:7890");
    expect(proxyPresetAddress(preset({ password: "secret", server: "http://proxy.lan", username: "u" }))).toBe(
      "proxy.lan:7890"
    );
  });
});

describe("isLoopbackProxyHost(镜像后端 is_loopback_host)", () => {
  it("loopback 家族命中,其余不命中", () => {
    for (const host of ["localhost", "127.0.0.1", "127.0.0.7", "::1", "[::1]", "0.0.0.0", "LOCALHOST"]) {
      expect(isLoopbackProxyHost(host), host).toBe(true);
    }
    for (const host of ["gw.corp", "128.0.0.1", "127.999.1.1", "127.0.0", "[fe80::1]"]) {
      expect(isLoopbackProxyHost(host), host).toBe(false);
    }
  });
});

describe("proxyPresetIssue(§4.8 保存期校验的前端镜像)", () => {
  const presets = [preset()];

  it("名称必填且列表内唯一(大小写不敏感)", () => {
    expect(proxyPresetIssue(preset({ id: "other", name: "  " }), presets, 3460)).toBe(
      "Preset name is required."
    );
    expect(proxyPresetIssue(preset({ id: "other", name: "clash 本地" }), presets, 3460)).toBe(
      "Preset name already exists."
    );
    // 自身(同 id)不算重名 —— 编辑时不会自撞。
    expect(proxyPresetIssue(preset(), presets, 3460)).toBeNull();
  });

  it("地址必填、scheme 白名单、端口范围", () => {
    expect(proxyPresetIssue(preset({ id: "x", name: "X", server: "" }), presets, 3460)).toBe(
      "Proxy server is required."
    );
    expect(proxyPresetIssue(preset({ id: "x", name: "X", server: "ftp://a.lan" }), presets, 3460)).toBe(
      "Only http, https, socks5 and socks5h are supported."
    );
    expect(proxyPresetIssue(preset({ id: "x", name: "X", port: 0 }), presets, 3460)).toBe(
      "Port must be between 1 and 65535."
    );
    // 决策 #5:预设面放开 socks5/socks5h。
    expect(proxyPresetIssue(preset({ id: "x", name: "X", server: "socks5h://a.lan" }), presets, 3460)).toBeNull();
  });

  it("决策 #8 / 验收 #6:指向网关自身端口拒绝,普通本机代理放行", () => {
    expect(proxyPresetIssue(preset({ id: "x", name: "X", port: 3460 }), presets, 3460)).toBe(
      "This proxy points at WRouter's own gateway port and would loop requests back."
    );
    expect(
      proxyPresetIssue(preset({ id: "x", name: "X", server: "http://127.0.0.1:3460" }), presets, 3460)
    ).toContain("loop requests back");
    expect(proxyPresetIssue(preset({ id: "x", name: "X" }), presets, 3460)).toBeNull();
    // 远端同端口不算自指。
    expect(
      proxyPresetIssue(preset({ id: "x", name: "X", port: 3460, server: "gw.corp" }), presets, 3460)
    ).toBeNull();
  });
});

describe("引用计数与摘要", () => {
  const presets = [preset(), preset({ id: "corp", name: "公司网关", port: 8080, server: "gw.corp" })];

  it("只统计供应商引用(全局默认不计入)", () => {
    const counts = proxyPresetReferenceCounts([
      provider({ id: "a", proxy: { mode: "preset", presetId: "clash" } }),
      provider({ id: "b", proxy: { mode: "preset", presetId: "clash" } }),
      provider({ id: "c", proxy: { mode: "preset", presetId: "corp" } }),
      provider({ id: "d", proxy: { mode: "direct" } }),
      provider({ id: "e" })
    ]);
    expect(counts).toEqual({ clash: 2, corp: 1 });
  });

  it("全局默认摘要:不使用/预设名/未选择", () => {
    expect(globalProxySummary(upstream(), presets, t)).toBe("Do not use proxy");
    expect(globalProxySummary(upstream({ mode: "preset", presetId: "corp" }), presets, t)).toBe("公司网关");
    expect(globalProxySummary(upstream({ mode: "preset", presetId: "" }), presets, t)).toBe(
      "No proxy preset selected"
    );
  });

  it("供应商摘要:直连/预设名/跟随全局默认(含预设已删的回落)", () => {
    expect(providerProxySummary({ mode: "direct" }, presets, t)).toBe("Direct connection");
    expect(providerProxySummary({ mode: "preset", presetId: "corp" }, presets, t)).toBe("公司网关");
    expect(providerProxySummary(undefined, presets, t)).toBe("Follow global default");
    expect(providerProxySummary({ mode: "preset", presetId: "gone" }, presets, t)).toBe(
      "Follow global default"
    );
  });

  it("providerProxyOverride 走归一(悬空引用 → undefined)", () => {
    expect(providerProxyOverride(provider({ proxy: { mode: "preset", presetId: "clash" } }), presets)).toEqual({
      mode: "preset",
      presetId: "clash"
    });
    expect(providerProxyOverride(provider({ proxy: { mode: "preset", presetId: "gone" } }), presets)).toBeUndefined();
    expect(providerProxyOverride(provider(), presets)).toBeUndefined();
  });
});

describe("草稿选择与图标解析", () => {
  it("条目代理图标:显式覆盖用其自身,跟随全局默认解析默认策略(2026-09-13)", () => {
    const clash = preset();
    expect(resolvedProxyIconMode({ mode: "direct" }, upstream(), [clash])).toBe("direct");
    expect(resolvedProxyIconMode({ mode: "preset", presetId: "clash" }, upstream(), [clash])).toBe("preset");
    // 跟随全局默认:none → 直连;引用存在预设 → preset;预设缺失 → 直连。
    expect(resolvedProxyIconMode(undefined, upstream(), [clash])).toBe("direct");
    expect(resolvedProxyIconMode(undefined, upstream({ mode: "preset", presetId: "clash" }), [clash])).toBe("preset");
    expect(resolvedProxyIconMode(undefined, upstream({ mode: "preset", presetId: "gone" }), [clash])).toBe("direct");
  });

  it("供应商档位 → 落库覆盖:inherit / 空引用不写键", () => {
    expect(proxyOverrideFromSelection("inherit", "")).toBeUndefined();
    expect(proxyOverrideFromSelection("direct", "")).toEqual({ mode: "direct" });
    expect(proxyOverrideFromSelection("preset", "")).toBeUndefined();
    expect(proxyOverrideFromSelection("preset", " clash ")).toEqual({ mode: "preset", presetId: "clash" });
  });

  it("落库覆盖 → 档位往返", () => {
    expect(proxySelectionFromOverride(undefined)).toEqual({ mode: "inherit", presetId: "" });
    expect(proxySelectionFromOverride({ mode: "direct" })).toEqual({ mode: "direct", presetId: "" });
    expect(proxySelectionFromOverride({ mode: "preset", presetId: "clash" })).toEqual({
      mode: "preset",
      presetId: "clash"
    });
  });

  it("探针载荷:inherit → undefined(后端回落全局默认)", () => {
    expect(probeProxyPayload(undefined)).toBeUndefined();
    expect(probeProxyPayload({ mode: "inherit" })).toBeUndefined();
    expect(probeProxyPayload({ mode: "direct" })).toEqual({ mode: "direct" });
    expect(probeProxyPayload({ mode: "preset" })).toBeUndefined();
    expect(probeProxyPayload({ mode: "preset", presetId: "clash" })).toEqual({
      mode: "preset",
      presetId: "clash"
    });
  });
});

describe("预设工厂", () => {
  it("新预设有唯一 id、空名称与常见默认端口", () => {
    const created = createProxyPreset();
    expect(created.id).toMatch(/^preset-/);
    expect(created.name).toBe("");
    expect(created.server).toBe("");
    expect(created.port).toBe(0); // 2026-09-13 三轮:端口不再预填,7890 只作输入框占位
    expect(createProxyPresetId()).not.toBe(created.id);
    expect(findProxyPreset([created], created.id)).toBe(created);
    expect(findProxyPreset([created], "missing")).toBeUndefined();
    expect(findProxyPreset([created], undefined)).toBeUndefined();
  });
});
