import { translateErrorMessage } from "@/contracts/i18n";

import { copyZh } from "./copy.zh";
import type { AppCopy } from "./types";

export function translateText(copy: AppCopy, value: string): string {
  return copy.text[value] ?? value;
}

export function translateAppErrorMessage(copy: AppCopy, value: string): string {
  return translateErrorMessage(copy === copyZh ? "zh" : "en", translateText(copy, value));
}

export function formatAppError(copy: AppCopy, error: unknown): string {
  return translateAppErrorMessage(copy, error instanceof Error ? error.message : String(error));
}

export function translateOptions<T extends { label: string; value: string }>(options: T[], t: (value: string) => string): T[] {
  return options.map((option) => ({ ...option, label: t(option.label) }));
}
