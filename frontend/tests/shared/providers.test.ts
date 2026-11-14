import { describe, expect, it } from "vitest";
import type { AppConfig } from "@/contracts/app";
import {
  createProviderAccount,
  createProviderAccountId,
  createProviderDraft,
  createProviderDraftFromProvider,
  createRouteModelOptions,
  formatCooldownRemaining,
  fillDefaultAccountLabels,
  nextProviderAccountLabel,
  enabledAccountIds,
  applyEnabledIds,
  enforceSingleEnabledKey,
  isReservedProviderName,
  providerKeyMode,
  markClearedProviderApiKeys,
  providerCredentialAccounts,
  providerNameSlug,
  providerProbeCandidates,
  uniqueProviderId
} from "@/pages/home/shared/providers";
import type { GatewayProviderConfig } from "@/contracts/app";

/**
 * Provider id generation: the id doubles as the backend secrets-table key
 * (``provider:{id}:api_key``), so distinct providers must never share one.
 * Non-ASCII names (千问, 火山方舟 …) used to all fall back to the constant
 * "provider", which made their API keys clobber each other on save.
 */

describe("providerNameSlug", () => {
  it("slugs ASCII names normally", () => {
    expect(providerNameSlug("DeepSeek")).toBe("deepseek");
    expect(providerNameSlug("My Provider")).toBe("my-provider");
    expect(providerNameSlug("opencode_go")).toBe("opencode_go");
  });

  it("derives a stable unique id for pure non-ASCII names", () => {
    expect(providerNameSlug("千问")).toBe("provider-gg3tm6");
    expect(providerNameSlug("火山方舟")).toBe("provider-m7fi9dk3dppb");
    expect(providerNameSlug("千问")).not.toBe(providerNameSlug("火山方舟"));
  });

  it("is deterministic for the same name", () => {
    expect(providerNameSlug("千问")).toBe(providerNameSlug(" 千问 "));
  });
});

describe("uniqueProviderId", () => {
  it("keeps the base id when unused", () => {
    expect(uniqueProviderId("deepseek", ["provider", "minimax"])).toBe("deepseek");
  });

  it("suffixes -2, -3 … when the id is taken", () => {
    expect(uniqueProviderId("provider", ["provider"])).toBe("provider-2");
    expect(uniqueProviderId("provider", ["provider", "provider-2"])).toBe("provider-3");
  });

  it("resolves the two-Chinese-providers scenario", () => {
    const first = providerNameSlug("千问");
    const second = uniqueProviderId(providerNameSlug("火山方舟"), [first]);
    expect(second).toBe("provider-m7fi9dk3dppb");
    expect(new Set([first, second]).size).toBe(2);
  });
});

/**
 * markClearedProviderApiKeys: the backend keeps stored secrets on
 * absent/empty api_key and only clears them on an explicit null. A key that
 * went from non-empty (saved) to empty (draft) is the user clearing the
 * input, so it must be sent as null — everything else round-trips untouched.
 */
describe("markClearedProviderApiKeys", () => {
  const saved = {
    Providers: [
      { id: "a", name: "A", models: [], api_key: "sk-keep" },
      { id: "b", name: "B", models: [], api_key: "sk-updated" },
      { id: "c", name: "C", models: [], api_key: "" },
    ],
  } as unknown as AppConfig;
  const draft = {
    Providers: [
      { id: "a", name: "A", models: [], api_key: "" },
      { id: "b", name: "B", models: [], api_key: "sk-new" },
      { id: "c", name: "C", models: [], api_key: "" },
    ],
  } as unknown as AppConfig;

  it("marks user-cleared keys as null and leaves the rest untouched", () => {
    const result = markClearedProviderApiKeys(draft, saved);
    expect(result.Providers[0].api_key).toBeNull();
    expect(result.Providers[1].api_key).toBe("sk-new");
    // Was already empty in saved — not a clear, stays empty (no secret to delete).
    expect(result.Providers[2].api_key).toBe("");
  });

  it("does not mutate the input config", () => {
    markClearedProviderApiKeys(draft, saved);
    expect(draft.Providers[0].api_key).toBe("");
  });

  it("returns the same object when nothing changed", () => {
    const unchanged = { Providers: [{ id: "b", name: "B", models: [], api_key: "sk-updated" }] } as unknown as AppConfig;
    expect(markClearedProviderApiKeys(unchanged, saved)).toBe(unchanged);
  });
});

/**
 * K6/K7 (方案 20260905_02):凭据池账号辅助。``accounts`` 是后端密钥通道
 * (secret_store 写入/注水)唯一处理的键(2026-11-07 W4:旧 ``credentials``
 * 读取别名同步退役)。
 */
describe("providerCredentialAccounts", () => {
  it("reads the canonical accounts key", () => {
    const provider = {
      accounts: [{ id: "a", enabled: true }]
    } as unknown as GatewayProviderConfig;
    expect(providerCredentialAccounts(provider)).toEqual([{ id: "a", enabled: true }]);
  });

  it("ignores the retired credentials alias", () => {
    const provider = { credentials: [{ id: "legacy" }] } as unknown as GatewayProviderConfig;
    expect(providerCredentialAccounts(provider)).toEqual([]);
  });

  it("returns an empty array for missing or non-array values", () => {
    expect(providerCredentialAccounts(undefined)).toEqual([]);
    expect(providerCredentialAccounts({} as GatewayProviderConfig)).toEqual([]);
    expect(providerCredentialAccounts({ accounts: "nope" } as unknown as GatewayProviderConfig)).toEqual([]);
  });
});

describe("createProviderAccountId / createProviderAccount", () => {
  it("generates ids that do not collide with existing ones", () => {
    const used = new Set(Array.from({ length: 50 }, (_, index) => `cred-${index}`));
    const id = createProviderAccountId(used);
    expect(id).toBeTruthy();
    expect(used.has(id)).toBe(false);
  });

  it("seeds backend-default account fields (enabled, priority 0, weight 1)", () => {
    const account = createProviderAccount([]);
    expect(account.enabled).toBe(true);
    expect(account.priority).toBe(0);
    expect(account.weight).toBe(1);
    expect(account.id).toMatch(/^cred-/);
  });
});

describe("formatCooldownRemaining", () => {
  it("formats sub-minute, minute and hour tiers", () => {
    expect(formatCooldownRemaining(59_000)).toBe("0:59");
    expect(formatCooldownRemaining(60_000)).toBe("1:00");
    expect(formatCooldownRemaining(65_000)).toBe("1:05");
    expect(formatCooldownRemaining(3_600_000)).toBe("1:00:00");
  });

  it("clamps non-positive and invalid values to zero", () => {
    expect(formatCooldownRemaining(0)).toBe("0:00");
    expect(formatCooldownRemaining(-1)).toBe("0:00");
    expect(formatCooldownRemaining(Number.NaN)).toBe("0:00");
  });
});

describe("createProviderDraft (K1 default)", () => {
  it("defaults the credential strategy to first (方案 §3.7)", () => {
    expect(createProviderDraft([]).credentialStrategy).toBe("first");
  });
});

describe("URL 模式(20261014_01)", () => {
  it("新草稿默认 base;编辑路径按存量播种(漏播种会在提交时被固定键字面量抹掉)", () => {
    expect(createProviderDraft([]).urlMode).toBe("base");
    expect(createProviderDraftFromProvider({ models: [], name: "P", urlMode: "full" }).urlMode).toBe("full");
    // 存量供应商无该键:宽容回落 base(与后端 UrlMode::parse 口径一致)。
    expect(createProviderDraftFromProvider({ models: [], name: "P" }).urlMode).toBe("base");
  });

  it("providerProbeCandidates 仅 full 模式透传 urlMode(base 不写键)", () => {
    expect(providerProbeCandidates(createProviderDraft([]))[0]?.urlMode).toBeUndefined();
    expect(providerProbeCandidates({ ...createProviderDraft([]), urlMode: "full" })[0]?.urlMode).toBe("full");
  });
});

describe("providerKeyMode(两模式映射)", () => {
  it("first/缺省/pinned → fixed;weighted/sticky/balanced → balanced", () => {
    expect(providerKeyMode(undefined)).toBe("fixed");
    expect(providerKeyMode("first")).toBe("fixed");
    expect(providerKeyMode("pinned")).toBe("fixed");
    expect(providerKeyMode("weighted")).toBe("balanced");
    expect(providerKeyMode("sticky")).toBe("balanced");
    expect(providerKeyMode("balanced")).toBe("balanced");
  });
});

describe("enforceSingleEnabledKey + enabledAccountIds + applyEnabledIds(模式切换)", () => {
  const rows = [
    { enabled: true, id: "a1" },
    { enabled: true, id: "a2" },
    { enabled: false, id: "a3" }
  ];

  it("只保留指定行启用,其余一律停用(不原地改)", () => {
    const next = enforceSingleEnabledKey(rows, "a3");
    expect(next.map((account) => account.enabled)).toEqual([false, false, true]);
    expect(rows[0].enabled).toBe(true);
  });

  it("采集启用 id 集合;按集合恢复开关", () => {
    expect(enabledAccountIds(rows)).toEqual(["a1", "a2"]);
    const restored = applyEnabledIds(rows, ["a2", "a3"]);
    expect(restored.map((account) => account.enabled)).toEqual([false, true, true]);
  });

  it("nextProviderAccountLabel:从 密钥数+1 起取第一个未占用号(不复用空隙)", () => {
    expect(nextProviderAccountLabel([], "密钥")).toBe("密钥 1");
    expect(nextProviderAccountLabel([{ id: "a", label: "密钥 1" }], "密钥")).toBe("密钥 2");
    // 已占 1/2/4:从 4(数+1)起,4 被占 → 5(3 号空隙不复用,保持单调)。
    const rows = [
      { id: "a", label: "密钥 1" },
      { id: "b", label: "密钥 2" },
      { id: "c", label: "密钥 4" }
    ];
    expect(nextProviderAccountLabel(rows, "密钥")).toBe("密钥 5");
    // 候选撞名继续跳。
    const gap = [
      { id: "a", label: "密钥 1" },
      { id: "b", label: "密钥 3" }
    ];
    expect(nextProviderAccountLabel(gap, "密钥")).toBe("密钥 4");
  });
});

describe("fillDefaultAccountLabels(历史数据兼容)", () => {
  it("为无名称行按位置补「{base} N」,撞占用名顺延;有名称行不动", () => {
    const { accounts, changed } = fillDefaultAccountLabels(
      [
        { enabled: true, id: "a1" },
        { enabled: true, id: "a2", label: "主 key" },
        { enabled: true, id: "a3" }
      ],
      "密钥"
    );
    expect(changed).toBe(true);
    expect(accounts[0].label).toBe("密钥 1");
    expect(accounts[1].label).toBe("主 key");
    expect(accounts[2].label).toBe("密钥 3");
  });

  it("位置号被占用时顺延到下一个空闲号", () => {
    const { accounts } = fillDefaultAccountLabels(
      [
        { enabled: true, id: "a1", label: "密钥 1" },
        { enabled: true, id: "a2" }
      ],
      "密钥"
    );
    expect(accounts[1].label).toBe("密钥 2");
  });

  it("全部有名称时不产生新数组", () => {
    const rows = [{ enabled: true, id: "a1", label: "x" }];
    const { accounts, changed } = fillDefaultAccountLabels(rows, "密钥");
    expect(changed).toBe(false);
    expect(accounts).toBe(rows);
  });
});

/**
 * isReservedProviderName: "wrouter" is the aggregate-model namespace
 * (`wrouter/<group>` prefix calling + owned_by in /v1/models). Providers
 * must never occupy it, or the prefix becomes ambiguous.
 */
describe("isReservedProviderName", () => {
  it("blocks the namespace in any casing", () => {
    expect(isReservedProviderName("wrouter")).toBe(true);
    expect(isReservedProviderName("WRouter")).toBe(true);
    expect(isReservedProviderName("  WROUTER  ")).toBe(true);
  });

  it("blocks names whose slug would collide with the namespace id", () => {
    expect(isReservedProviderName("wrouter-")).toBe(true);
  });

  it("allows names that merely contain or extend it", () => {
    expect(isReservedProviderName("WRouter Pro")).toBe(false);
    expect(isReservedProviderName("wrouter-like")).toBe(false);
    expect(isReservedProviderName("my-wrouter")).toBe(false);
    expect(isReservedProviderName("千问")).toBe(false);
  });
});

/**
 * createRouteModelOptions: enabled groups are offered as
 * `wrouter/<group>` targets so the aggregate namespace is selectable
 * in fallback/rule dropdowns; disabled groups stay out.
 */
describe("createRouteModelOptions", () => {
  const provider = {
    id: "openai",
    name: "OpenAI",
    models: ["gpt-4o"]
  } as GatewayProviderConfig;

  it("appends wrouter/<group> options for enabled groups", () => {
    const groups = [
      { id: "g1", name: "smart-router", enabled: true, strategy: "first", targets: [] },
      { id: "g2", name: "off-group", enabled: false, strategy: "first", targets: [] }
    ] as never;
    const options = createRouteModelOptions([provider], groups);
    expect(options.map((option) => option.value)).toEqual([
      "OpenAI/gpt-4o",
      "wrouter/smart-router"
    ]);
  });

  it("returns provider-only options without groups", () => {
    const options = createRouteModelOptions([provider]);
    expect(options.map((option) => option.value)).toEqual(["OpenAI/gpt-4o"]);
  });
});
