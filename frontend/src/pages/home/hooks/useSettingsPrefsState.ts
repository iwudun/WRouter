// R7 split: settings prefs domain — per-preference change handlers + settings dialog.
// Language/system-language/theme state stays in App (they feed copy/t/formatError).
import { type Dispatch, type SetStateAction, useState } from "react";
import {
  AppConfig, AppLanguagePreference, normalizeLanguagePreference, normalizeObservabilityConfig,
  normalizeProxyConfig, normalizeThemePreference,
  persistLanguagePreference, ResolvedLanguage, ResolvedTheme, SettingsPageId,
} from "../shared/index";

export type SettingsPrefsStateOptions = {
  draftConfig: AppConfig;
  updateConfig: (mutator: (config: AppConfig) => AppConfig) => void;
  setLanguagePreference: Dispatch<SetStateAction<AppLanguagePreference>>;
  setSystemLanguage: Dispatch<SetStateAction<ResolvedLanguage>>;
  setSystemTheme: Dispatch<SetStateAction<ResolvedTheme>>;
};

export function useSettingsPrefsState({
  draftConfig, updateConfig, setLanguagePreference, setSystemLanguage, setSystemTheme
}: SettingsPrefsStateOptions) {
  // 2026-09-10:设置视图化后不再有弹窗开关,只保留「初始分区」——深链
  // (供应商页「去设置代理」/启动错误横幅)先设分区,再由 App 切到 settings 视图。
  // 2026-09-13:通用页签上移到第一位,设置页默认落在通用。
  const [settingsInitialPage, setSettingsInitialPage] = useState<SettingsPageId>("general");

  function changeThemePreference(value: string) {
    const theme = normalizeThemePreference(value);
    updateConfig((config) => ({
      ...config,
      theme
    }));
  }

  function changeLaunchAtLogin(launchAtLogin: boolean) {
    updateConfig((config) => ({
      ...config,
      launchAtLogin
    }));
  }

  function changeObservabilityConfig(patch: Partial<AppConfig["observability"]>) {
    updateConfig((config) => ({
      ...config,
      observability: normalizeObservabilityConfig({
        ...config.observability,
        ...patch
      })
    }));
  }

  function changeProxyConfig(patch: Partial<AppConfig["proxy"]>) {
    updateConfig((config) => ({
      ...config,
      proxy: normalizeProxyConfig({
        ...config.proxy,
        ...patch,
        // 20260906_01:upstream 只剩 mode + presetId,浅合并即可;presets 由
        // 上面的展开覆盖,normalizeProxyConfig 会拿它校验 presetId 引用是否
        // 还存在(指向已删预设 → 清空,UI 回到「未选择」)。
        upstream: {
          ...config.proxy.upstream,
          ...(patch.upstream ?? {})
        }
      })
    }));
  }

  function openSettingsPage(page: SettingsPageId) {
    setSettingsInitialPage(page);
  }

  function changeLanguagePreference(value: string) {
    const language = normalizeLanguagePreference(value);
    setLanguagePreference(language);
    persistLanguagePreference(language);
  }

  return {
    settingsInitialPage,
    changeThemePreference,
    changeLaunchAtLogin,
    changeObservabilityConfig,
    changeProxyConfig,
    openSettingsPage,
    changeLanguagePreference
  };
}