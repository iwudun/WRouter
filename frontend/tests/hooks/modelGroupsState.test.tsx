// 临时条目生命周期测试(聚合模型):新建 = 唯一内存临时草稿(点「添加」才
// 落库);编辑既有组 = 即时落库(对齐供应商详情交互)。锁定行为:重复添加
// 幂等、切换互不覆盖、保存转正、失败保留、取消语义、名称校验、目标去重。
import { act, renderHook } from "@testing-library/react";
import { useState } from "react";
import { describe, expect, it, vi } from "vitest";
import type { AppConfig, ModelGroupConfig } from "@/contracts/app";
import { useModelGroupsState } from "@/pages/home/hooks/useModelGroupsState";
import { fallbackConfig } from "@/pages/home/shared/fallbacks";

type PersistImpl = (config: AppConfig, setError: (message: string) => void) => Promise<boolean>;

function cloneConfig<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T;
}

function groupWithTargets(id: string, name: string): ModelGroupConfig {
  return {
    enabled: true,
    id,
    name,
    strategy: "first",
    targets: [{ model: "m1", providerId: "p1" }]
  };
}

function useHarness(options: { groups?: ModelGroupConfig[]; persist?: PersistImpl } = {}) {
  const [config, setConfig] = useState<AppConfig>(() => {
    const seeded = cloneConfig(fallbackConfig);
    if (options.groups) {
      seeded.modelGroups = cloneConfig(options.groups);
    }
    return seeded;
  });
  const persist = options.persist ?? (async (next: AppConfig) => {
    setConfig(cloneConfig(next));
    return true;
  });
  return useModelGroupsState({
    draftConfig: config,
    buildConfigUpdate: (mutator) => mutator(cloneConfig(config)),
    setConfigDraft: (next) => {
      setConfig(cloneConfig(next));
      return next;
    },
    persistConfig: persist,
    setActionError: () => undefined,
    t: (value) => value
  });
}

describe("useModelGroupsState 临时聚合条目(新建)", () => {
  it("重复点击添加复用同一临时草稿,已填内容不重置", () => {
    const { result } = renderHook(() => useHarness());
    act(() => result.current.openAddModelGroup());
    act(() => result.current.updateModelGroupDraft({ name: "router-x", strategy: "weighted" }));
    act(() => result.current.openAddModelGroup());

    expect(result.current.temporaryModelGroupDraft?.name).toBe("router-x");
    expect(result.current.temporaryModelGroupDraft?.strategy).toBe("weighted");
    expect(result.current.groupDraft.draft?.name).toBe("router-x");
    expect(result.current.groupDraft.editId).toBeUndefined();
    expect(result.current.modelGroups).toHaveLength(0);
  });

  it("临时草稿编辑不写正式配置;切换到正式组后临时内容保留", () => {
    const { result } = renderHook(() => useHarness({ groups: [groupWithTargets("g1", "alpha")] }));
    act(() => result.current.openAddModelGroup());
    act(() => {
      result.current.updateModelGroupDraft({ name: "temp-1" });
      result.current.addModelGroupTarget("p2", "m9");
    });
    // 临时草稿不进正式配置
    expect(result.current.modelGroups.some((group) => group.name === "temp-1")).toBe(false);
    expect(result.current.modelGroups).toHaveLength(1);

    act(() => result.current.openEditModelGroup("g1"));
    expect(result.current.groupDraft.editId).toBe("g1");

    act(() => result.current.selectTemporaryModelGroup());
    expect(result.current.groupDraft.draft?.name).toBe("temp-1");
    expect(result.current.groupDraft.draft?.targets).toHaveLength(1);
    expect(result.current.groupDraft.editId).toBeUndefined();
  });

  it("临时聚合保存成功后转为正式条目并保持编辑上下文", async () => {
    const persist = vi.fn(async (next: AppConfig) => {
      void next;
      return true;
    });
    const { result } = renderHook(() => useHarness({ persist }));
    act(() => result.current.openAddModelGroup());
    act(() => {
      result.current.updateModelGroupDraft({ name: "agg-1" });
      result.current.addModelGroupTarget("p1", "m1");
    });

    let saved = false;
    await act(async () => {
      saved = await result.current.submitModelGroupDraft();
    });

    expect(saved).toBe(true);
    expect(persist).toHaveBeenCalledTimes(1);
    const created = result.current.modelGroups.find((group) => group.name === "agg-1");
    expect(created).toBeDefined();
    expect(result.current.temporaryModelGroupDraft).toBeUndefined();
    expect(result.current.modelGroupCreatedId).toBe(created?.id);
    expect(result.current.groupDraft.editId).toBe(created?.id);
    expect(result.current.groupDraft.draft?.name).toBe("agg-1");
  });

  it("保存失败保留临时草稿、输入与错误,可修正后重试", async () => {
    const persist = vi.fn(async (_next: AppConfig, setError: (message: string) => void) => {
      if (persist.mock.calls.length === 1) {
        setError("save failed");
        return false;
      }
      return true;
    });
    const { result } = renderHook(() => useHarness({ persist }));
    act(() => result.current.openAddModelGroup());
    act(() => {
      result.current.updateModelGroupDraft({ name: "agg-x" });
      result.current.addModelGroupTarget("p1", "m1");
    });

    let saved: boolean | undefined;
    await act(async () => {
      saved = await result.current.submitModelGroupDraft();
    });

    expect(saved).toBe(false);
    expect(result.current.groupDraftError).toBe("save failed");
    expect(result.current.temporaryModelGroupDraft?.name).toBe("agg-x");
    expect(result.current.temporaryModelGroupDraft?.targets).toHaveLength(1);
    expect(result.current.modelGroupCreatedId).toBeUndefined();

    act(() => result.current.updateModelGroupDraft({ name: "agg-y" }));
    let retried = false;
    await act(async () => {
      retried = await result.current.submitModelGroupDraft();
    });
    expect(retried).toBe(true);
  });

  it("取消临时条目:丢弃草稿且不改正式配置", () => {
    const { result } = renderHook(() => useHarness({ groups: [groupWithTargets("g1", "alpha")] }));
    act(() => result.current.openAddModelGroup());
    act(() => result.current.updateModelGroupDraft({ name: "temp-1" }));
    act(() => result.current.closeModelGroupDraft());

    expect(result.current.temporaryModelGroupDraft).toBeUndefined();
    expect(result.current.groupDraft.draft).toBeUndefined();
    expect(result.current.modelGroups).toHaveLength(1);
    expect(result.current.modelGroups[0]?.name).toBe("alpha");
  });

  it("快捷带入与手动添加目标均幂等去重", () => {
    const { result } = renderHook(() => useHarness());
    act(() => result.current.openAddModelGroup());
    act(() => result.current.addModelGroupTargetsForModelName([{ model: "m1", providerId: "p1" }]));
    act(() => result.current.addModelGroupTargetsForModelName([{ model: "m1", providerId: "p1" }]));
    act(() => result.current.addModelGroupTarget("p1", "M1"));

    expect(result.current.groupDraft.draft?.targets).toHaveLength(1);
  });
});

describe("useModelGroupsState 编辑既有组(即时落库)", () => {
  it("名称修改即时写配置并持久化", async () => {
    const persist = vi.fn(async () => true);
    const { result } = renderHook(() => useHarness({ groups: [groupWithTargets("g1", "alpha")], persist }));
    act(() => result.current.openEditModelGroup("g1"));
    act(() => result.current.updateModelGroupDraft({ name: "alpha-edited" }));

    expect(result.current.modelGroups[0]?.name).toBe("alpha-edited");
    // 打开即迁移目标序到「顺序即优先级」(1 次)+ 名称落库(1 次)。
    expect(persist).toHaveBeenCalledTimes(2);
    // 编辑上下文即实时配置对象
    expect(result.current.groupDraft.draft?.name).toBe("alpha-edited");
    expect(result.current.groupDraft.editId).toBe("g1");
  });

  it("非法名称不落库:空名/斜杠/重名各自报错", () => {
    const { result } = renderHook(() => useHarness({
      groups: [groupWithTargets("g1", "alpha"), groupWithTargets("g2", "beta")]
    }));
    act(() => result.current.openEditModelGroup("g1"));

    act(() => result.current.updateModelGroupDraft({ name: "   " }));
    expect(result.current.groupDraftError).toBe("Enter a name.");
    act(() => result.current.updateModelGroupDraft({ name: "a/b" }));
    expect(result.current.groupDraftError).toBe("Name cannot contain a slash.");
    act(() => result.current.updateModelGroupDraft({ name: "BETA" }));
    expect(result.current.groupDraftError).toBe("Model group name already exists.");
    expect(result.current.modelGroups[0]?.name).toBe("alpha");

    act(() => result.current.updateModelGroupDraft({ name: "gamma" }));
    expect(result.current.groupDraftError).toBe("");
    expect(result.current.modelGroups[0]?.name).toBe("gamma");
  });

  it("目标增删改序即时写配置;启停开关已移除,落库统一为启用", () => {
    const { result } = renderHook(() => useHarness({ groups: [groupWithTargets("g1", "alpha")] }));
    act(() => result.current.openEditModelGroup("g1"));

    act(() => result.current.updateModelGroupTarget(0, { weight: 3, enabled: false }));
    expect(result.current.modelGroups[0]?.targets[0]?.weight).toBe(3);
    // 启停开关从 UI 移除:序列化强制 enabled: true。
    expect(result.current.modelGroups[0]?.targets[0]?.enabled).toBe(true);

    act(() => result.current.addModelGroupTarget("p2", "m2"));
    act(() => result.current.addModelGroupTarget("p2", "m2"));
    expect(result.current.modelGroups[0]?.targets).toHaveLength(2);

    // 拖拽整序替换:顺序即优先级(m2 提到最前 → priority 0)。
    act(() => result.current.reorderModelGroupTargets(
      [...(result.current.modelGroups[0]?.targets ?? [])].reverse()
    ));
    expect(result.current.modelGroups[0]?.targets[0]?.model).toBe("m2");
    expect(result.current.modelGroups[0]?.targets[0]?.priority).toBe(0);
    expect(result.current.modelGroups[0]?.targets[1]?.priority).toBe(1);

    act(() => result.current.removeModelGroupTarget(0));
    expect(result.current.modelGroups[0]?.targets).toHaveLength(1);
    expect(result.current.modelGroups[0]?.targets[0]?.model).toBe("m1");
    expect(result.current.modelGroups[0]?.targets[0]?.priority).toBe(0);
  });

  it("打开旧配置即迁移:旧 priority 升序稳定排序后按列表序重编", () => {
    const { result } = renderHook(() => useHarness({
      groups: [{
        enabled: true,
        id: "g1",
        name: "alpha",
        strategy: "first",
        targets: [
          { model: "slow", priority: 5, providerId: "p1" },
          { model: "fast", priority: 0, providerId: "p2" }
        ]
      }]
    }));

    act(() => result.current.openEditModelGroup("g1"));
    expect(result.current.modelGroups[0]?.targets.map((target) => target.model)).toEqual(["fast", "slow"]);
    expect(result.current.modelGroups[0]?.targets.map((target) => target.priority)).toEqual([0, 1]);
    expect(result.current.modelGroups[0]?.targets.every((target) => target.enabled)).toBe(true);
  });

  it("weighted 策略迁移后全体同层(priority 0),层内加权才作用于全部目标", () => {
    const { result } = renderHook(() => useHarness({
      groups: [{
        enabled: true,
        id: "g1",
        name: "alpha",
        strategy: "weighted",
        targets: [
          { model: "a", priority: 0, providerId: "p1" },
          { model: "b", priority: 3, providerId: "p2" }
        ]
      }]
    }));

    act(() => result.current.openEditModelGroup("g1"));
    expect(result.current.modelGroups[0]?.targets.map((target) => target.priority)).toEqual([0, 0]);
  });

  it("启用切换即时落库;删除正式组后取消激活且临时草稿保留", async () => {
    const { result } = renderHook(() => useHarness({ groups: [groupWithTargets("g1", "alpha")] }));
    act(() => result.current.openAddModelGroup());
    act(() => result.current.updateModelGroupDraft({ name: "temp-1" }));
    act(() => result.current.openEditModelGroup("g1"));

    act(() => result.current.setModelGroupEnabled("g1", false));
    expect(result.current.modelGroups[0]?.enabled).toBe(false);

    await act(async () => {
      await result.current.removeModelGroup("g1");
    });
    expect(result.current.modelGroups).toHaveLength(0);
    expect(result.current.groupDraft.draft).toBeUndefined();
    expect(result.current.temporaryModelGroupDraft?.name).toBe("temp-1");
  });
});
