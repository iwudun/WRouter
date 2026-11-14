// R8 (2026-08-01): cross-page language preference types + helpers.
//
// Previously AppLanguagePreference / ResolvedLanguage were duplicated in
// pages/home/shared/types.ts; languagePreferenceStorageKey in
// pages/home/shared/i18n.tsx; and the read/persist/normalize/detect helpers in
// pages/home/shared/common.ts. They now have a single source here; the home
// barrel re-exports them so downstream consumers are unchanged.

export type AppLanguagePreference = "system" | "en" | "zh";
export type ResolvedLanguage = "en" | "zh";

export const languagePreferenceStorageKey = "wr.ui.language";

export function normalizeLanguagePreference(value: unknown): AppLanguagePreference {
  return value === "en" || value === "zh" || value === "system" ? value : "system";
}

export function readLanguagePreference(): AppLanguagePreference {
  try {
    return normalizeLanguagePreference(window.localStorage.getItem(languagePreferenceStorageKey));
  } catch {
    return "system";
  }
}

export function persistLanguagePreference(language: AppLanguagePreference) {
  try {
    if (language === "system") {
      window.localStorage.removeItem(languagePreferenceStorageKey);
      return;
    }
    window.localStorage.setItem(languagePreferenceStorageKey, language);
  } catch {
    // Language preference is a UI enhancement; ignore unavailable storage.
  }
}

export function detectSystemLanguage(): ResolvedLanguage {
  const languages = navigator.languages?.length ? navigator.languages : [navigator.language];
  return languages.some((language) => language.toLowerCase().startsWith("zh")) ? "zh" : "en";
}
