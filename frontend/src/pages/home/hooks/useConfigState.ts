// R7 split: config draft + persistence core. Owns draftConfig/savedConfig and the
// shared update/persist primitives that every domain hook depends on.
//
// F2a.2: 配置加载改走 Query（staleTime Infinity 的一次性拉取）；显式保存
// 走 useMutation 并带乐观回滚——persistConfig 前的草稿快照在失败时恢复
// （动作前已乐观应用删除/编辑），避免"删除失败但列表仍显示已删"；
// 400ms 防抖自动保存保持旧语义：失败保留草稿仅报错（持续输入而非动作，
// 回滚会毁掉用户正在编辑的内容）。请求层统一走 lib/api（F2a.6）。
import { useEffect, useRef, useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import {
  ApiKeyConfig, AppConfig, AppSaveConfigOptions, cloneConfig, fallbackConfig, markClearedProviderApiKeys, normalizeConfig
} from "../shared/index";
import { api, hasBridge } from "@/lib/api";
import { queryKeys } from "@/lib/queryKeys";
import { syncLaunchAtLogin } from "@/lib/tauri-window";

export type ConfigStateOptions = {
  formatError: (error: unknown) => string;
  setActionError: (message: string) => void;
  t: (value: string) => string;
};

export function useConfigState({ formatError, setActionError, t }: ConfigStateOptions) {
  const [draftConfig, setDraftConfig] = useState<AppConfig>(fallbackConfig);
  const [configLoaded, setConfigLoaded] = useState(() => !hasBridge());
  const [savedConfig, setSavedConfig] = useState<AppConfig>(fallbackConfig);
  const autoSaveRequestId = useRef(0);

  const dirty = draftConfig !== savedConfig;

  // 一次性配置拉取（原 App.tsx getConfig → syncConfigState → setConfigLoaded）。
  const configQuery = useQuery({
    queryKey: queryKeys.config,
    queryFn: () => api.getConfig(),
    enabled: hasBridge(),
    staleTime: Infinity,
    retry: false
  });

  useEffect(() => {
    if (configQuery.data) {
      syncConfigState(configQuery.data);
      setConfigLoaded(true);
    }
  }, [configQuery.data]);

  useEffect(() => {
    // 加载失败时也放行 UI：沿用旧 catch 分支（回落打包默认值继续渲染）。
    if (configQuery.isError) {
      setConfigLoaded(true);
    }
  }, [configQuery.isError]);

  function syncConfigState(config: AppConfig) {
    const normalized = normalizeConfig(config);
    setSavedConfig(normalized);
    setDraftConfig(normalized);
    // 20261102_01 P4-3:开机自启与 OS 登录项保持一致。
    // 配置一落盘就同步(不必等下次启动)—— 非 Tauri 壳里是空操作
    // (桌面壳在启动时对账,见 desktop/src-tauri/src/lib.rs::sync_startup_state)。
    void syncLaunchAtLogin(Boolean(normalized.launchAtLogin));
  }

  function updateConfig(mutator: (config: AppConfig) => AppConfig) {
    setDraftConfig((current) => {
      const next = normalizeConfig(mutator(cloneConfig(current)));
      return next;
    });
  }

  function buildConfigUpdate(mutator: (config: AppConfig) => AppConfig): AppConfig {
    return normalizeConfig(mutator(cloneConfig(draftConfig)));
  }

  function setConfigDraft(config: AppConfig): AppConfig {
    const normalized = normalizeConfig(config);
    setDraftConfig(normalized);
    return normalized;
  }

  // 显式保存 mutation：乐观更新（草稿已先应用）+ 失败回滚到动作前快照。
  const saveMutation = useMutation({
    mutationFn: async (input: {
      config: AppConfig;
      options?: AppSaveConfigOptions;
      requestId: number;
      previous?: AppConfig;
      setError?: (message: string) => void;
    }) => {
      const saved = await api.saveConfig(markClearedProviderApiKeys(input.config, savedConfig), input.options);
      return saved;
    },
    onSuccess: (saved, input) => {
      // 仅当本次仍是最新在途保存时才同步响应（旧 autoSaveRequestId 防串写语义）。
      if (autoSaveRequestId.current === input.requestId) {
        syncConfigState(saved);
        setActionError("");
      }
    },
    onError: (error, input) => {
      // 乐观回滚：恢复动作前的草稿引用（快照即动作前的 draftConfig 本体；
      // 仅当本次仍是最新在途请求时执行，避免覆盖更晚的自动保存同步）。
      if (input.previous && autoSaveRequestId.current === input.requestId) {
        setDraftConfig(input.previous);
      }
      if (autoSaveRequestId.current === input.requestId) {
        input.setError?.(formatError(error));
      }
    }
  });

  // 400ms 防抖自动保存：草稿变化即持久化；失败保留草稿 + 错误横幅
  // （不回滚——回滚会丢弃用户正在输入的内容，与迁移前语义一致）。
  useEffect(() => {
    if (!hasBridge() || !dirty) {
      return;
    }

    const requestId = autoSaveRequestId.current + 1;
    autoSaveRequestId.current = requestId;
    const configToSave = markClearedProviderApiKeys(draftConfig, savedConfig);
    const timer = window.setTimeout(() => {
      api.saveConfig(configToSave)
        .then((saved) => {
          if (autoSaveRequestId.current === requestId) {
            syncConfigState(saved);
            setActionError("");
          }
        })
        .catch((error) => {
          if (autoSaveRequestId.current === requestId) {
            setActionError(formatError(error));
          }
        });
    }, 400);

    return () => window.clearTimeout(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [dirty, draftConfig]);

  async function persistConfig(config: AppConfig, setError: (message: string) => void, options?: AppSaveConfigOptions): Promise<boolean> {
    const requestId = autoSaveRequestId.current + 1;
    autoSaveRequestId.current = requestId;
    if (!hasBridge()) {
      syncConfigState(config);
      return true;
    }

    // 动作前快照：直接持有当前草稿引用（回滚时恢复同引用；draftConfig 在
    // 静止态是不可变约定——所有更新都先克隆）。
    const previous = draftConfig;
    try {
      await saveMutation.mutateAsync({ config, options, requestId, previous, setError });
      return true;
    } catch {
      // 错误呈现 + 回滚已在 onError 完成；仅向调用方报告失败。
      return false;
    }
  }

  async function persistApiKeys(apiKeys: ApiKeyConfig[], setError: (message: string) => void): Promise<boolean> {
    if (!hasBridge()) {
      setError(t("API key persistence is only available in the desktop app."));
      return false;
    }

    try {
      const saved = await api.saveApiKeys(apiKeys);
      syncConfigState(saved);
      setError("");
      return true;
    } catch (error) {
      setError(formatError(error));
      return false;
    }
  }

  return {
    draftConfig,
    savedConfig,
    configLoaded,
    setConfigLoaded,
    syncConfigState,
    updateConfig,
    buildConfigUpdate,
    setConfigDraft,
    persistConfig,
    persistApiKeys
  };
}
