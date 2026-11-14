import type {
  AppConfig,
} from "@/contracts/app";
import {
  type AppCopy
} from "./i18n";

import { MetricTone } from "./components/controls";
import { ResolvedLanguage, ResolvedTheme } from "./types";
// R8: read/persist/normalize/detect language helpers live in @/lib/language;
// re-export here so the home barrel stays intact.
export {
  normalizeLanguagePreference,
  readLanguagePreference,
  persistLanguagePreference,
  detectSystemLanguage
} from "@/lib/language";

export function cloneConfig(config: AppConfig): AppConfig {
  return JSON.parse(JSON.stringify(config)) as AppConfig;
}

export function isPlainRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export function stringValue(value: unknown): string | undefined {
  return typeof value === "string" && value.trim() ? value.trim() : undefined;
}

export function normalizeProviderModelSelector(value: string | undefined): string {
  const trimmed = value?.trim() ?? "";
  if (!trimmed) {
    return "";
  }
  const commaIndex = trimmed.indexOf(",");
  if (commaIndex > 0 && commaIndex < trimmed.length - 1) {
    const provider = trimmed.slice(0, commaIndex).trim();
    const model = trimmed.slice(commaIndex + 1).trim();
    return provider && model ? `${provider}/${model}` : trimmed;
  }
  return trimmed;
}

export function uniqueStrings(values: string[]): string[] {
  const seen = new Set<string>();
  const result: string[] = [];
  for (const value of values) {
    const item = value.trim();
    if (!item || seen.has(item)) {
      continue;
    }
    seen.add(item);
    result.push(item);
  }
  return result;
}

export function isMacPlatform(platform: string): boolean {
  const normalized = platform.toLowerCase();
  return normalized === "darwin" || normalized.includes("mac");
}

/** 20261101_03:Windows 走隐藏式标题栏(titleBarOverlay),与 macOS 同享应用内拖拽区。 */
export function isWindowsPlatform(platform: string): boolean {
  return platform.toLowerCase().includes("win");
}

export function detectSystemTheme(): ResolvedTheme {
  return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

export function normalizeThemePreference(value: unknown): AppConfig["theme"] {
  return value === "light" || value === "dark" || value === "system" ? value : "system";
}

export function formatSystemOption(label: string, value: string): string {
  return `${label} (${value})`;
}

export function themeDisplayName(theme: ResolvedTheme, copy: AppCopy): string {
  return theme === "dark" ? copy.settings.themeDark : copy.settings.themeLight;
}

export function languageDisplayName(language: ResolvedLanguage, copy: AppCopy): string {
  return language === "zh" ? copy.settings.languageChinese : copy.settings.languageEnglish;
}

export function metricToneBar(tone: MetricTone) {
  if (tone === "teal") return "bg-teal-500";
  if (tone === "blue") return "bg-blue-500";
  if (tone === "indigo") return "bg-indigo-500";
  if (tone === "amber") return "bg-amber-500";
  if (tone === "slate") return "bg-slate-500";
  return "bg-rose-500";
}

export function formatEditableJson(value: unknown): string {
  return JSON.stringify(value, null, 2);
}
