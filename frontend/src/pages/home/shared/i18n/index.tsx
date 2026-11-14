import { createContext, useContext, useMemo } from "react";

// R8: language preference types + storage key live in @/lib/language.
import { languagePreferenceStorageKey, type ResolvedLanguage } from "@/lib/language";

import { copyEn } from "./copy.en";
import { copyZh } from "./copy.zh";
import { formatAppError, translateText } from "./translate";
import type { AppCopy } from "./types";

export type { AppCopy };
export type { ResolvedLanguage };
export { languagePreferenceStorageKey };
export { formatAppError, translateAppErrorMessage, translateOptions, translateText } from "./translate";

export const appCopy: Record<ResolvedLanguage, AppCopy> = {
  en: copyEn,
  zh: copyZh
};

export const AppI18nContext = createContext<AppCopy>(appCopy.en);

export function useAppText() {
  const copy = useContext(AppI18nContext);
  return useMemo(() => (value: string) => translateText(copy, value), [copy]);
}

export function useAppNumberLocale(): Intl.LocalesArgument {
  const copy = useContext(AppI18nContext);
  return copy === appCopy.zh ? "zh-CN" : "en-US";
}

export function useAppErrorText() {
  const copy = useContext(AppI18nContext);
  return useMemo(() => (error: unknown) => formatAppError(copy, error), [copy]);
}
