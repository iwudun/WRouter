// Unit tests for the routing-key quota window (配额统计窗口).
// Regression (20261105 C-9): the backend `quotaWindowMs` accepts ANY millisecond
// value (limits.rs adopts the written value as-is), but the UI only knew three
// presets (minute/hour/day). Any other window — e.g. a weekly quota written by
// hand into config.json — was mapped to the "day" fallback on load and then
// written back as 86_400_000 on the next edit+save: the weekly quota was
// silently downgraded to a daily one. The UI must now express week/month and
// round-trip unknown windows verbatim.
import { describe, expect, it } from "vitest";
import type { ApiKeyConfig } from "@/contracts/app";
import {
  QUOTA_WINDOW_MS,
  apiKeyLimitsFromDraft,
  createApiKeyEditDraft,
  formatQuotaWindowDuration,
  quotaWindowMsFromPreset,
  quotaWindowPresetFromMs
} from "@/pages/home/shared/api-keys";

function keyWithQuotaWindow(quotaWindowMs: number | undefined): ApiKeyConfig {
  return {
    createdAt: "2026-11-05T00:00:00Z",
    id: "key_1",
    key: "sk-test",
    name: "Key One",
    limits: { maxTokens: 1000, quotaWindowMs }
  };
}

describe("quotaWindowPresetFromMs", () => {
  it("已知档位返回对应 preset", () => {
    expect(quotaWindowPresetFromMs(QUOTA_WINDOW_MS.minute)).toBe("minute");
    expect(quotaWindowPresetFromMs(QUOTA_WINDOW_MS.hour)).toBe("hour");
    expect(quotaWindowPresetFromMs(QUOTA_WINDOW_MS.day)).toBe("day");
    expect(quotaWindowPresetFromMs(QUOTA_WINDOW_MS.week)).toBe("week");
    expect(quotaWindowPresetFromMs(QUOTA_WINDOW_MS.month)).toBe("month");
  });

  it("不认识的窗口值返回 undefined，不回落到 day", () => {
    expect(quotaWindowPresetFromMs(7_200_000)).toBeUndefined();
    expect(quotaWindowPresetFromMs(90_000)).toBeUndefined();
    expect(quotaWindowPresetFromMs(undefined)).toBeUndefined();
  });
});

describe("quotaWindowMsFromPreset", () => {
  it("preset 与毫秒值一一对应", () => {
    expect(quotaWindowMsFromPreset("minute")).toBe(60_000);
    expect(quotaWindowMsFromPreset("hour")).toBe(3_600_000);
    expect(quotaWindowMsFromPreset("day")).toBe(86_400_000);
    expect(quotaWindowMsFromPreset("week")).toBe(604_800_000);
    expect(quotaWindowMsFromPreset("month")).toBe(2_592_000_000);
  });
});

describe("配额窗口的编辑往返", () => {
  it("已知档位:week 原样往返", () => {
    const draft = createApiKeyEditDraft(keyWithQuotaWindow(QUOTA_WINDOW_MS.week));
    expect(draft.quotaWindow).toBe("week");
    expect(draft.quotaWindowCustomMs).toBeUndefined();
    expect(apiKeyLimitsFromDraft(draft)?.quotaWindowMs).toBe(604_800_000);
  });

  it("已知档位:month 原样往返", () => {
    const draft = createApiKeyEditDraft(keyWithQuotaWindow(QUOTA_WINDOW_MS.month));
    expect(draft.quotaWindow).toBe("month");
    expect(apiKeyLimitsFromDraft(draft)?.quotaWindowMs).toBe(2_592_000_000);
  });

  it("未知窗口值:进 custom 并保留原毫秒数", () => {
    const draft = createApiKeyEditDraft(keyWithQuotaWindow(7_200_000));
    expect(draft.quotaWindow).toBe("custom");
    expect(draft.quotaWindowCustomMs).toBe(7_200_000);
  });

  it("回归:未知窗口值编辑保存后不被静默改写成日配额", () => {
    const draft = createApiKeyEditDraft(keyWithQuotaWindow(7_200_000));
    const limits = apiKeyLimitsFromDraft(draft);
    expect(limits?.quotaWindowMs).toBe(7_200_000);
    expect(limits?.quotaWindowMs).not.toBe(QUOTA_WINDOW_MS.day);
    expect(limits?.maxTokens).toBe(1000);
  });

  it("用户显式切到某个档位时,自定义值被覆盖为该档位", () => {
    const draft = createApiKeyEditDraft(keyWithQuotaWindow(7_200_000));
    draft.quotaWindow = "week";
    draft.quotaWindowCustomMs = undefined;
    expect(apiKeyLimitsFromDraft(draft)?.quotaWindowMs).toBe(604_800_000);
  });

  it("未设配额窗口时回落 day，且不产生自定义值", () => {
    const draft = createApiKeyEditDraft(keyWithQuotaWindow(undefined));
    expect(draft.quotaWindow).toBe("day");
    expect(draft.quotaWindowCustomMs).toBeUndefined();
    expect(apiKeyLimitsFromDraft(draft)?.quotaWindowMs).toBe(86_400_000);
  });
});

describe("formatQuotaWindowDuration", () => {
  it("整小时/整分给可读时长，其余给毫秒", () => {
    expect(formatQuotaWindowDuration(7_200_000)).toBe("2h");
    expect(formatQuotaWindowDuration(5_400_000)).toBe("90m");
    expect(formatQuotaWindowDuration(90_000)).toBe("90000ms");
    expect(formatQuotaWindowDuration(1_500)).toBe("1500ms");
    expect(formatQuotaWindowDuration(undefined)).toBe("");
  });
});
