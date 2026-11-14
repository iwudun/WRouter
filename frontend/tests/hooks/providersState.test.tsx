// 临时供应商条目生命周期测试:唯一内存草稿 + 面板可见性由 selection 驱动。
// 锁定行为:重复添加幂等不重置、取消丢弃且不改配置、保存转正、失败保留。
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, renderHook } from "@testing-library/react";
import { useState, type ReactNode } from "react";
import { describe, expect, it, vi } from "vitest";
import type { AppConfig } from "@/contracts/app";
import { useProvidersState } from "@/pages/home/hooks/useProvidersState";
import { createProviderDraftFromProvider } from "@/pages/home/shared/providers";
import { fallbackConfig } from "@/pages/home/shared/fallbacks";
import { appCopy } from "@/pages/home/shared/i18n";
import { mockWindowWr } from "../helpers/mockWindowWr";

type PersistImpl = (config: AppConfig, setError: (message: string) => void) => Promise<boolean>;

function cloneConfig(config: AppConfig): AppConfig {
  return JSON.parse(JSON.stringify(config)) as AppConfig;
}

function useHarness(persist?: PersistImpl) {
  const [config, setConfig] = useState<AppConfig>(() => cloneConfig(fallbackConfig));
  const state = useProvidersState({
    draftConfig: config,
    configLoaded: true,
    copy: appCopy.zh,
    showToast: () => undefined,
    t: (value) => value,
    formatError: (error) => String(error),
    buildConfigUpdate: (mutator) => mutator(cloneConfig(config)),
    setConfigDraft: (next) => {
      setConfig(cloneConfig(next));
      return next;
    },
    persistConfig: persist ?? (async (next: AppConfig) => {
      setConfig(cloneConfig(next));
      return true;
    }),
    setActionError: () => undefined
  });
  return { config, ...state };
}

function makeWrapper() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: Infinity } }
  });
  return ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={client}>{children}</QueryClientProvider>
  );
}

describe("useProvidersState 临时供应商条目", () => {
  it("重复打开新增只保留一个临时条目,已填草稿不重置", () => {
    mockWindowWr();
    const { result } = renderHook(() => useHarness(), { wrapper: makeWrapper() });
    act(() => result.current.openAddProviderDialog());
    act(() => result.current.updateProviderDraft({ name: "P1", baseUrl: "https://api.example.com" }));

    expect(result.current.providerTemporary).toBe(true);

    act(() => result.current.openAddProviderDialog());
    expect(result.current.providerDraft.name).toBe("P1");
    expect(result.current.providerDraft.baseUrl).toBe("https://api.example.com");
    expect(result.current.providerTemporary).toBe(true);
  });

  it("取消丢弃临时条目且不修改配置", () => {
    mockWindowWr();
    const { result } = renderHook(() => useHarness(), { wrapper: makeWrapper() });
    act(() => result.current.openAddProviderDialog());
    act(() => result.current.updateProviderDraft({ name: "P1", modelsText: "m1\nm2" }));
    act(() => result.current.closeProviderDraft());

    expect(result.current.providerTemporary).toBe(false);
    expect(result.current.providerDraft.name).toBe("");
    expect(result.current.config.Providers).toHaveLength(0);
  });

  it("保存成功后临时态转正并写入配置一次", async () => {
    mockWindowWr();
    const persist = vi.fn(async (next: AppConfig) => {
      void next;
      return true;
    });
    const { result } = renderHook(() => useHarness(persist), { wrapper: makeWrapper() });
    act(() => result.current.openAddProviderDialog());
    act(() => result.current.updateProviderDraft({
      baseUrl: "https://api.example.com",
      modelsText: "m1\nm2",
      name: "P1"
    }));

    let saved = false;
    await act(async () => {
      saved = await result.current.submitProviderDraft();
    });

    expect(saved).toBe(true);
    expect(persist).toHaveBeenCalledTimes(1);
    expect(result.current.providerTemporary).toBe(false);
    expect(result.current.config.Providers).toHaveLength(1);
    expect(result.current.config.Providers[0]?.name).toBe("P1");
    expect(result.current.providerCreatedIndex).toBe(0);
  });

  it("保存失败保留临时条目、输入与错误", async () => {
    mockWindowWr();
    const persist = vi.fn(async (_next: AppConfig, setError: (message: string) => void) => {
      setError("save failed");
      return false;
    });
    const { result } = renderHook(() => useHarness(persist), { wrapper: makeWrapper() });
    act(() => result.current.openAddProviderDialog());
    act(() => result.current.updateProviderDraft({
      baseUrl: "https://api.example.com",
      modelsText: "m1",
      name: "P1"
    }));

    let saved: boolean | undefined;
    await act(async () => {
      saved = await result.current.submitProviderDraft();
    });

    expect(saved).toBe(false);
    expect(result.current.providerTemporary).toBe(true);
    expect(result.current.providerProbeError).toBe("save failed");
    expect(result.current.providerDraft.name).toBe("P1");
    // 配置回滚由 useConfigState 承载(queryState.test 已覆盖),此处只断言
    // 草稿面板侧的保留语义。
  });

  it("详情面板检测不写共享探测状态,临时草稿探测结果不受正式条目检测影响", async () => {
    const { wr } = mockWindowWr();
    wr.checkProviderConnectivity = vi.fn(() => Promise.resolve({
      failed: [],
      passed: [],
      results: [],
      probe: undefined
    })) as never;
    const { result } = renderHook(() => useHarness(), { wrapper: makeWrapper() });
    // 造一个正式供应商用于详情面板检测
    act(() => result.current.openAddProviderDialog());
    act(() => result.current.updateProviderDraft({
      baseUrl: "https://api.example.com",
      modelsText: "m1",
      name: "P1"
    }));
    await act(async () => {
      await result.current.submitProviderDraft();
    });
    act(() => result.current.openAddProviderDialog());
    act(() => result.current.updateProviderDraft({ name: "Temp", modelsText: "tm" }));

    await act(async () => {
      await result.current.checkProviderConnectivity(0);
    });
    // 详情路径(trackState=false)不写草稿面板的探测状态
    expect(result.current.providerConnectivityProbe).toBeUndefined();
    expect(result.current.providerConnectivityLoading).toBe(false);
    expect(result.current.providerDraft.name).toBe("Temp");
  });

  // 2026-10-14 回归:用户把 API 格式换成 Anthropic Messages 后,检测可用性
  // 仍按 capabilities 里旧探测遗留的 openai_chat_completions 拼端点
  // (https://api.deepseek.com/anthropic + /v1/chat/completions)恒失败。
  // 探测协议必须以表单当前所选格式为唯一权威。
  it("检测可用性按当前所选 API 格式探测,不被旧 capabilities 播种的格式劫持", async () => {
    const { wr } = mockWindowWr();
    let captured: {
      candidates?: Array<{ baseUrl?: string; protocols?: string[] }>;
      protocols?: string[];
    } | undefined;
    wr.checkProviderConnectivity = vi.fn((request: never) => {
      captured = request as unknown as typeof captured;
      return Promise.resolve({ failed: [], passed: [], results: [], probe: undefined });
    }) as never;

    const { result } = renderHook(() => useHarness(), { wrapper: makeWrapper() });
    // 存量供应商:保存的格式与 capabilities 都是旧探测遗留的
    // openai_chat_completions,base URL 已换成 DeepSeek 的 anthropic 前缀。
    act(() => {
      result.current.reorderProviders([{
        api_base_url: "https://api.deepseek.com/anthropic",
        capabilities: [{ baseUrl: "https://api.deepseek.com", type: "openai_chat_completions" }],
        models: ["deepseek-flash"],
        name: "DeepSeek",
        protocol: "openai",
        type: "openai_chat_completions"
      }]);
    });
    // 编辑路径播种草稿后,用户把格式换成 Anthropic Messages。
    act(() => {
      result.current.setProviderDraft(createProviderDraftFromProvider(result.current.config.Providers[0]!));
    });
    act(() => result.current.updateProviderDraft({ protocol: "anthropic_messages" }));

    await act(async () => {
      await result.current.checkProviderDraft();
    });

    expect(captured?.protocols).toEqual(["anthropic_messages"]);
    expect(captured?.candidates?.[0]?.protocols).toEqual(["anthropic_messages"]);
    expect(captured?.candidates?.[0]?.baseUrl).toBe("https://api.deepseek.com/anthropic");
  });

  // 20261014_01:URL 模式保存链 —— full 只 trim 不归一化(原样落库并写
  // urlMode 键);base 维持归一化且摘键(等于默认值不落库)。
  it("full URL 模式保存跳过归一化并落库 urlMode;base 模式不写键", async () => {
    const { result } = renderHook(() => useHarness(), { wrapper: makeWrapper() });
    act(() => result.current.openAddProviderDialog());
    act(() => result.current.updateProviderDraft({
      baseUrl: "https://api.example.com/v1/",
      modelsText: "m1",
      name: "Base"
    }));
    await act(async () => {
      await result.current.submitProviderDraft();
    });
    const baseProvider = result.current.config.Providers[0]!;
    expect(baseProvider.api_base_url).toBe("https://api.example.com/v1");
    expect(baseProvider.urlMode).toBeUndefined();

    // 编辑路径:播种草稿 → 切 TAB 到 full → 原样地址(自定义路径+尾斜杠
    // +query)不被改写。播种漏了 urlMode 时这里会静默退回 base。
    act(() => result.current.setProviderEditIndex(0));
    act(() => {
      result.current.setProviderDraft(createProviderDraftFromProvider(result.current.config.Providers[0]!));
    });
    act(() => result.current.updateProviderDraft({
      baseUrl: "https://gw.example.com/llm/claude/?edition=us",
      urlMode: "full"
    }));
    await act(async () => {
      await result.current.submitProviderDraft();
    });
    const fullProvider = result.current.config.Providers[0]!;
    expect(fullProvider.urlMode).toBe("full");
    expect(fullProvider.api_base_url).toBe("https://gw.example.com/llm/claude/?edition=us");
  });

  // 20261014_01 详情面板 TAB:setProviderUrlMode 即时落库 —— full 写键,
  // base 摘键;已保存地址不重写(切 full 后的后续编辑才按原样落库)。
  it("详情面板切换 URL 模式:full 落库、base 摘键,地址不动", async () => {
    const { result } = renderHook(() => useHarness(), { wrapper: makeWrapper() });
    act(() => result.current.openAddProviderDialog());
    act(() => result.current.updateProviderDraft({
      baseUrl: "https://api.example.com/v1",
      modelsText: "m1",
      name: "P1"
    }));
    await act(async () => {
      await result.current.submitProviderDraft();
    });
    expect(result.current.config.Providers[0]!.urlMode).toBeUndefined();

    act(() => result.current.setProviderUrlMode(0, "full"));
    expect(result.current.config.Providers[0]!.urlMode).toBe("full");
    // 切模式不改写已保存地址。
    expect(result.current.config.Providers[0]!.api_base_url).toBe("https://api.example.com/v1");

    act(() => result.current.setProviderUrlMode(0, "base"));
    expect(result.current.config.Providers[0]!.urlMode).toBeUndefined();
  });

  // 20261017_01:批量添加(获取模型面板多选)——单次落库、跳过已存在
  // (不覆盖既有元数据)、元数据值作为批量默认值。
  it("addModels 批量落库:跳过已存在,元数据批量生效,persist 一次", async () => {
    const persist = vi.fn(async (next: AppConfig) => {
      void next;
      return true;
    });
    const { result } = renderHook(() => useHarness(persist), { wrapper: makeWrapper() });
    act(() => result.current.openAddProviderDialog());
    act(() => result.current.updateProviderDraft({
      baseUrl: "https://api.example.com/v1",
      modelsText: "m1",
      name: "P1"
    }));
    await act(async () => {
      await result.current.submitProviderDraft();
    });
    persist.mockClear();

    act(() => result.current.addModels(0, ["m1", "m2", "m3"], {
      contextWindow: 200000,
      inputTypes: ["text", "image"],
      maxOutputTokens: 64000
    }));

    const provider = result.current.config.Providers[0]!;
    expect(provider.models).toEqual(["m1", "m2", "m3"]);
    // 已存在的 m1 不覆盖,元数据只落在新增的两个。
    expect(provider.modelContextWindow).toEqual({ m2: 200000, m3: 200000 });
    expect(provider.modelMaxOutputTokens).toEqual({ m2: 64000, m3: 64000 });
    expect(provider.modelInputTypes).toEqual({ m2: ["text", "image"], m3: ["text", "image"] });
    expect(persist).toHaveBeenCalledTimes(1);

    // 全部已存在 → 配置不动、不再落库。
    act(() => result.current.addModels(0, ["m2"], {
      contextWindow: 1,
      inputTypes: ["text"],
      maxOutputTokens: 1
    }));
    expect(persist).toHaveBeenCalledTimes(1);
    expect(result.current.config.Providers[0]!.modelContextWindow).toEqual({ m2: 200000, m3: 200000 });
  });
});
