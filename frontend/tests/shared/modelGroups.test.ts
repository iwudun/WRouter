// 20260905_04 M6:聚合模型共享工具单测 —— 归一化(与后端 parse_model_groups
// 同口径)、命名冲突检查、跨供应商同名快捷带入、顺序即优先级序列化/迁移。
import { describe, expect, it } from "vitest";
import type { GatewayProviderConfig, ModelGroupConfig } from "@/contracts/app";
import {
  distinctProviderModelNames,
  migrateTargetsByOrder,
  modelGroupNameConflict,
  normalizeModelGroups,
  providersServingModel,
  serializeTargetsByOrder
} from "@/pages/home/shared/model-groups";

function provider(id: string, name: string, models: string[]): GatewayProviderConfig {
  return { id, models, name } as GatewayProviderConfig;
}

describe("normalizeModelGroups", () => {
  it("keeps valid groups, skips invalid items and dedupes names (first wins)", () => {
    const groups = normalizeModelGroups([
      { id: "g1", name: " Agg ", strategy: "weighted", targets: [{ providerId: "p1", model: "m1", weight: 2 }] },
      { name: "agg", targets: [{ providerId: "p2", model: "m2" }] },
      { name: "", targets: [{ providerId: "p3", model: "m3" }] },
      { name: "empty", targets: [] },
      { name: "bad-targets", targets: [{ providerId: "", model: "m" }, "junk"] },
      "junk",
      null
    ]);
    expect(groups).toHaveLength(1);
    expect(groups[0].name).toBe("Agg");
    expect(groups[0].strategy).toBe("weighted");
    expect(groups[0].targets[0]).toEqual({ model: "m1", providerId: "p1", weight: 2 });
  });

  it("returns [] for missing/non-array input (zero-migration for old configs)", () => {
    expect(normalizeModelGroups(undefined)).toEqual([]);
    expect(normalizeModelGroups("x")).toEqual([]);
    expect(normalizeModelGroups({})).toEqual([]);
  });

  it("generates a stable id when missing and normalizes weight/priority defaults", () => {
    const [group] = normalizeModelGroups([
      { name: "g", targets: [{ model: "m", providerId: "p", priority: 2.7, weight: -3 }] }
    ]);
    expect(group.id).toMatch(/^mg-/);
    // 负权重被丢弃(读取侧回退 1);priority 截断为整数。
    expect(group.targets[0].weight).toBeUndefined();
    expect(group.targets[0].priority).toBe(2);
  });
});

describe("modelGroupNameConflict", () => {
  const groups: ModelGroupConfig[] = [
    { id: "g1", name: "smart-router", targets: [{ model: "m1", providerId: "p1" }] }
  ];

  it("flags duplicate group names (case-insensitive, excluding self)", () => {
    expect(modelGroupNameConflict("SMART-ROUTER", groups)).toBe("smart-router");
    expect(modelGroupNameConflict("SMART-ROUTER", groups, "g1")).toBeUndefined();
  });

  it("returns undefined for a fresh unique name", () => {
    expect(modelGroupNameConflict("brand-new", groups)).toBeUndefined();
    expect(modelGroupNameConflict("  ", groups)).toBeUndefined();
  });
});

describe("cross-provider helpers", () => {
  const providers = [
    provider("p1", "Primary", ["gpt-4o", "solo-a"]),
    provider("p2", "Secondary", ["GPT-4o", "solo-b"]),
    provider("p3", "Third", [])
  ];

  it("providersServingModel matches case-insensitively", () => {
    expect(providersServingModel(providers, "gpt-4o").map((p) => p.id)).toEqual(["p1", "p2"]);
    expect(providersServingModel(providers, "nope")).toEqual([]);
  });

  it("distinctProviderModelNames dedupes and sorts", () => {
    expect(distinctProviderModelNames(providers)).toEqual(["gpt-4o", "solo-a", "solo-b"]);
  });

  it("serializeTargetsByOrder: first/sticky 列表序 → 唯一递增 priority,统一 enabled", () => {
    const targets = [
      { model: "a", priority: 9, providerId: "p1", enabled: false },
      { model: "b", providerId: "p2" }
    ];
    expect(serializeTargetsByOrder(targets, "first")).toEqual([
      { model: "a", priority: 0, providerId: "p1", enabled: true },
      { model: "b", priority: 1, providerId: "p2", enabled: true }
    ]);
    // sticky 同 first(命中置顶由后端处理)。
    expect(serializeTargetsByOrder(targets, "sticky")[0].priority).toBe(0);
  });

  it("serializeTargetsByOrder: weighted 全体同层(priority 0)保证层内加权作用于全部目标", () => {
    const targets = [
      { model: "a", providerId: "p1" },
      { model: "b", providerId: "p2" }
    ];
    expect(serializeTargetsByOrder(targets, "weighted")).toEqual([
      { model: "a", priority: 0, providerId: "p1", enabled: true },
      { model: "b", priority: 0, providerId: "p2", enabled: true }
    ]);
  });

  it("serializeTargetsByOrder: 未变化的 target 保留原引用(避免行无谓重挂载)", () => {
    const target = { model: "a", priority: 0, providerId: "p1", enabled: true };
    expect(serializeTargetsByOrder([target], "first")[0]).toBe(target);
  });

  it("migrateTargetsByOrder: 旧 priority 升序稳定排序后按新语义序列化", () => {
    const group: ModelGroupConfig = {
      id: "g",
      name: "g",
      strategy: "first",
      targets: [
        { model: "slow", priority: 5, providerId: "p1" },
        { model: "fast", priority: 0, providerId: "p2" },
        { model: "mid", priority: 0, providerId: "p3" }
      ]
    };
    // 同 priority 保持配置序(fast 在 mid 前),5 排最后,重编 0..n-1。
    expect(migrateTargetsByOrder(group).map((target) => target.model)).toEqual(["fast", "mid", "slow"]);
    expect(migrateTargetsByOrder(group).map((target) => target.priority)).toEqual([0, 1, 2]);
  });
});
