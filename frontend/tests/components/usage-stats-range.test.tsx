// 用量统计的时间范围选择器:四个预设 + 自定义日历面板。
//
// 20261104_01:「24 小时」下线,只留 今天 / 7 天 / 30 天 / 全部,另加
// 「自定义」(带日历图标的独立段 + 选日面板)。
import { waitFor } from "@testing-library/react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { UsageStatsRangeSelector } from "@/pages/home/components/dashboard/usage-stats-range";
import { AppI18nContext, appCopy } from "@/pages/home/shared/i18n";
import type { UsageStatsRange } from "@/contracts/app";

let root: Root | null = null;

function mountSelector(range: UsageStatsRange, setRange: (value: UsageStatsRange) => void) {
  const container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  root.render(
    <AppI18nContext.Provider value={appCopy.zh}>
      <UsageStatsRangeSelector range={range} setRange={setRange} />
    </AppI18nContext.Provider>
  );
  return container;
}

async function waitForRender(container: HTMLElement) {
  await waitFor(() => {
    expect(container.querySelector('[role="group"]')).not.toBeNull();
  });
  return container;
}

function buttonByText(scope: ParentNode, text: string) {
  const button = Array.from(scope.querySelectorAll<HTMLButtonElement>("button")).find(
    (element) => element.textContent?.trim() === text
  );
  expect(button, `button "${text}" not found`).toBeTruthy();
  return button!;
}

/** 面板里的「某一天」按钮(纯数字文本)。 */
function dayButtons(scope: ParentNode) {
  return Array.from(scope.querySelectorAll<HTMLButtonElement>("button")).filter((element) =>
    /^\d{1,2}$/.test(element.textContent?.trim() ?? "")
  );
}

afterEach(() => {
  root?.unmount();
  root = null;
  document.body.innerHTML = "";
});

describe("UsageStatsRangeSelector", () => {
  it("预设只剩 今天 / 7 天 / 30 天 / 全部,「24 小时」已下线", async () => {
    const container = await waitForRender(mountSelector("today", () => {}));
    const group = container.querySelector('[role="group"]')!;
    const labels = Array.from(group.querySelectorAll("button")).map((element) =>
      element.textContent?.trim()
    );
    expect(labels).toEqual(["今天", "7 天", "30 天", "全部", "自定义"]);
    expect(container.textContent ?? "").not.toContain("24");
  });

  it("点「全部」把 range 切到 all", async () => {
    const setRange = vi.fn();
    const container = await waitForRender(mountSelector("today", setRange));
    buttonByText(container, "全部").click();
    expect(setRange).toHaveBeenCalledWith("all");
  });

  it("自定义:点开日历面板,连选两天 → range 变成 from..to 窗口串", async () => {
    const setRange = vi.fn();
    const container = await waitForRender(mountSelector("today", setRange));
    buttonByText(container, "自定义").click();
    // 20261104_03:面板走 portal,挂在 document.body 上,不在容器内。
    await waitFor(() => {
      expect(document.querySelector('[role="dialog"]')).not.toBeNull();
    });
    const dialog = document.querySelector('[role="dialog"]')!;
    const days = dayButtons(dialog);
    expect(days.length).toBeGreaterThan(1);
    days[0].click();
    // 第一下只是记下开始日:等面板提示变成「再选最后一天」再点第二下
    // (React 的状态更新是异步的,不等就会用旧处理器再记一次开始日)。
    await waitFor(() => {
      expect(document.body.textContent ?? "").toContain("再选最后一天");
    });
    dayButtons(document.querySelector('[role="dialog"]')!)[1].click();
    expect(setRange).toHaveBeenCalledTimes(1);
    expect(setRange.mock.calls[0][0]).toMatch(/^\d{4}-\d{2}-\d{2}\.\.\d{4}-\d{2}-\d{2}$/);
  });

  it("自定义:快捷区间「昨天」直接给出当天的窗口串", async () => {
    const setRange = vi.fn();
    const container = await waitForRender(mountSelector("today", setRange));
    buttonByText(container, "自定义").click();
    await waitFor(() => {
      expect(document.querySelector('[role="dialog"]')).not.toBeNull();
    });
    const dialog = document.querySelector('[role="dialog"]')!;
    buttonByText(dialog, "昨天").click();
    expect(setRange).toHaveBeenCalledTimes(1);
    const value = setRange.mock.calls[0][0] as string;
    const [from, to] = value.split("..");
    expect(from).toBe(to);
    const yesterday = new Date();
    yesterday.setDate(yesterday.getDate() - 1);
    const expected = `${yesterday.getFullYear()}-${String(yesterday.getMonth() + 1).padStart(2, "0")}-${String(yesterday.getDate()).padStart(2, "0")}`;
    expect(from).toBe(expected);
  });

  // 20261104_03:图标与文字必须同行 —— Tailwind v4 preflight 把 svg 设成
  // display:block,按钮不建成 flex 容器时图标独占一行、「自定义」被挤到下方
  // (浏览器实测:按钮宽 53px,内容需 67px,文字换行)。
  // jsdom 不做布局,只能钉住「按钮是 flex 容器 + 图标在按钮内」这两个前提。
  it("自定义段:按钮是 flex 容器,日历图标与文字在同一个按钮里", async () => {
    const container = await waitForRender(mountSelector("today", () => {}));
    const custom = buttonByText(container, "自定义");
    expect(custom.className.split(/\s+/)).toContain("inline-flex");
    expect(custom.querySelector("svg")).not.toBeNull();
  });

  // 20261104_03:面板不能留在分段控件里 —— 主内容区是 overflow-auto 的滚动
  // 容器,面板 absolute 挂在内会被裁掉左缘(浏览器实测:面板左缘 x=237,
  // 容器左界 x=248,左侧 11px 连边框一起消失)。
  it("自定义段:选日面板走 portal 挂在 body 上,并用 fixed 定位", async () => {
    const container = await waitForRender(mountSelector("today", () => {}));
    buttonByText(container, "自定义").click();
    await waitFor(() => {
      expect(document.querySelector('[role="dialog"]')).not.toBeNull();
    });
    const dialog = document.querySelector('[role="dialog"]')!;
    expect(dialog.parentElement).toBe(document.body);
    const classes = dialog.className.split(/\s+/);
    expect(classes).toContain("fixed");
    expect(classes).not.toContain("absolute");
  });

  it("已选自定义窗口时:段上写所选日期区间,且处于选中态", async () => {
    const container = await waitForRender(mountSelector("2026-10-01..2026-10-07", () => {}));
    const custom = buttonByText(container, "10/1 – 10/7");
    expect(custom.getAttribute("aria-pressed")).toBe("true");
    // 预设段此时都不是选中态。
    expect(buttonByText(container, "今天").getAttribute("aria-pressed")).toBe("false");
  });
});
