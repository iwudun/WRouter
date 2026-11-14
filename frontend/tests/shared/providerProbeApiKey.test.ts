import { describe, expect, it } from "vitest";
import type { GatewayProviderConfig } from "@/contracts/app";
import { providerProbeApiKey, providerProbeApiKeyFromDraft } from "@/pages/home/shared/providers";

/**
 * 2026-10-11 根因修复重写:探测密钥解析必须与网关选号同序。
 * 旧规则「供应商级 legacy api_key 第一优先」会让 secrets 里残留的影子键
 * 劫持「检测可用性」(load_app_config 会把 `provider:{id}:api_key` 注水回
 * provider),无视用户固定/启用的账号 —— 新规则:池非空一律池优先
 * (固定模式 pinned → priority 降序;负载均衡取最高层代表),legacy 仅在
 * 无池/池无明文时回落;priority 方向对齐网关 pick_first_usable(取最大)。
 */

function makeProvider(patch: Partial<GatewayProviderConfig> & { accounts?: Array<Record<string, unknown>> }): GatewayProviderConfig {
  const { accounts, ...rest } = patch;
  return {
    accounts: accounts as GatewayProviderConfig["accounts"],
    ...rest
  } as GatewayProviderConfig;
}

describe("providerProbeApiKey(探测生效密钥,与网关同序)", () => {
  it("池非空时供应商级影子键不再劫持(2026-10-11 根因主场景)", () => {
    const provider = makeProvider({
      accounts: [{ id: "a", enabled: true, api_key: "sk-account" }],
      api_key: "sk-legacy-shadow",
      credentialStrategy: "first",
      pinnedAccountId: "a"
    });
    expect(providerProbeApiKey(provider)).toBe("sk-account");
  });

  it("固定模式 pinned 指向的启用账号优先", () => {
    const provider = makeProvider({
      accounts: [
        { id: "a", enabled: true, api_key: "sk-first" },
        { id: "b", enabled: true, api_key: "sk-pinned" }
      ],
      credentialStrategy: "first",
      pinnedAccountId: "b"
    });
    expect(providerProbeApiKey(provider)).toBe("sk-pinned");
  });

  it("pinned 指向停用账号时不硬选,落 priority 降序(网关 pinned 空选不回落,探测取可用代表)", () => {
    const provider = makeProvider({
      accounts: [
        { id: "a", enabled: false, api_key: "sk-off" },
        { id: "b", enabled: true, api_key: "sk-live" }
      ],
      credentialStrategy: "first",
      pinnedAccountId: "a"
    });
    expect(providerProbeApiKey(provider)).toBe("sk-live");
  });

  it("priority 降序决定取钥顺序(与网关 first/balanced 的最大优先级层一致)", () => {
    const provider = makeProvider({
      accounts: [
        { id: "a", priority: 2, api_key: "sk-high" },
        { id: "b", priority: 1, api_key: "sk-low" }
      ],
      credentialStrategy: "first"
    });
    expect(providerProbeApiKey(provider)).toBe("sk-high");
  });

  it("负载均衡模式忽略 pinned,取最高优先级层的代表密钥", () => {
    const provider = makeProvider({
      accounts: [
        { id: "a", enabled: true, api_key: "sk-tier1" },
        { id: "b", enabled: true, api_key: "sk-tier2" }
      ],
      credentialStrategy: "balanced",
      pinnedAccountId: "b"
    });
    expect(providerProbeApiKey(provider)).toBe("sk-tier1");
  });

  it("停用账号与空明文账号跳过", () => {
    const provider = makeProvider({
      accounts: [
        { id: "a", enabled: false, api_key: "sk-off" },
        { id: "b", enabled: true, api_key: "   " },
        { id: "c", enabled: true, api_key: "sk-live" }
      ],
      credentialStrategy: "first"
    });
    expect(providerProbeApiKey(provider)).toBe("sk-live");
  });

  it("无池/池无明文时回落供应商级密钥(纯 legacy 配置形态)", () => {
    expect(providerProbeApiKey(makeProvider({ api_key: "sk-own" }))).toBe("sk-own");
    expect(providerProbeApiKey(makeProvider({
      accounts: [{ id: "a", enabled: true, api_key: "" }],
      api_key: "sk-own"
    }))).toBe("sk-own");
  });

  it("无任何可用密钥时回空串(探测按无密钥发送)", () => {
    expect(providerProbeApiKey(makeProvider({ accounts: [{ id: "a", enabled: false, api_key: "sk-off" }] }))).toBe("");
    expect(providerProbeApiKey(makeProvider({}))).toBe("");
    expect(providerProbeApiKey(undefined)).toBe("");
  });
});

describe("providerProbeApiKeyFromDraft(草稿态同语义)", () => {
  it("草稿密钥列表优先,单密钥手输仅作无池回落", () => {
    expect(providerProbeApiKeyFromDraft({
      accounts: [{ id: "a", enabled: true, api_key: "sk-draft" }],
      apiKey: "sk-legacy",
      credentialStrategy: "first"
    })).toBe("sk-draft");
    expect(providerProbeApiKeyFromDraft({
      accounts: [],
      apiKey: "sk-legacy",
      credentialStrategy: "first"
    })).toBe("sk-legacy");
  });

  it("草稿固定模式沿用 pinned 记忆", () => {
    expect(providerProbeApiKeyFromDraft({
      accounts: [
        { id: "a", enabled: false, api_key: "sk-off" },
        { id: "b", enabled: true, api_key: "sk-live" }
      ],
      apiKey: "",
      credentialStrategy: "first",
      pinnedAccountId: "b"
    })).toBe("sk-live");
  });
});
