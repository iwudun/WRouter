// 模型指纹检测弹窗的前端测试(方案 20261030_01 F6;2026-11-01 检测入口
// 从供应商详情页集中到「模型指纹」页,供应商改为弹窗内自选)。
//
// 覆盖:
// 1. 供应商自选:默认选第一个供应商、模型全选,请求带上 providerId;
//    切换供应商会重置模型清单与结果;
// 2. 探针档位选择(快速 / 标准 / 完整)确实传到后端请求里;
// 3. 跨套件比对的拒绝理由(方案 X9)在弹窗里**逐字**展示 —— 不弹 toast,
//    用户必须看到「分两次检测」的完整上下文;
// 4. 三段结论(参考库 / 全局候选 / 供应商锚点)并列渲染;
// 5. `insufficient` 档**不显示具体模型名**(方案 X4 显式弃权)。
import { waitFor } from "@testing-library/react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { FingerprintDetectDialog } from "@/pages/home/components/fingerprint-detect-dialog";
import type {
  FingerprintAttribution,
  FingerprintDetectRequest,
  FingerprintDetectResponse,
  GatewayProviderConfig
} from "@/contracts/app";

function attribution(
  overrides: Partial<FingerprintAttribution> = {}
): FingerprintAttribution {
  return {
    beta: 11.5,
    calibration_queries: "3",
    diagnostics: [],
    family_probabilities: [],
    margin: 0.99,
    probability: 0.97,
    results: [
      {
        display_name: "claude-sonnet-4-6",
        model: "claude-sonnet-4-6",
        nuisance_score: 0.1,
        probability: 0.97,
        score: 3.2
      }
    ],
    strength: "strong",
    used_outputs: 12,
    ...overrides
  };
}

function response(overrides: Partial<FingerprintDetectResponse> = {}): FingerprintDetectResponse {
  return {
    probeSuite: "wrouter-v1",
    probeTier: "standard",
    results: [
      {
        conclusions: {
          anchor: attribution({ prediction: "anchor-model", prediction_name: "Anchor Model" }),
          global: attribution({ prediction: "global-model", prediction_name: "Global Model" })
        },
        diagnostics: { accepted: 12, durationMs: 4321.5, gaps: 1, parseFailures: 0, probes: 12 },
        model: "model-a",
        probeDiagnostics: [],
        probeSeed: "wrouter-p1",
        probeSuite: "wrouter-v1",
        probeTier: "standard",
        providerId: "p1",
        skipped: []
      }
    ],
    ...overrides
  };
}

const providers: GatewayProviderConfig[] = [
  { id: "p1", models: ["model-a"], name: "Provider One" },
  { id: "p2", models: ["model-b1", "model-b2"], name: "Provider Two" }
];

let root: Root | null = null;
let container: HTMLElement | null = null;

function mountDialog(
  onDetect: (request: FingerprintDetectRequest) => Promise<FingerprintDetectResponse>
) {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  root.render(
    <FingerprintDetectDialog
      onClose={() => {}}
      onDetect={onDetect}
      open
      providers={providers}
    />
  );
  return container;
}

async function clickButtonByText(text: string) {
  await waitFor(() => {
    const button = Array.from(container!.querySelectorAll<HTMLButtonElement>("button"))
      .find((element) => element.textContent?.trim() === text && !element.disabled);
    expect(button, `enabled button "${text}" not found`).toBeTruthy();
    button!.click();
  });
}

function selectByLabel(label: string) {
  const select = container!.querySelector<HTMLSelectElement>(`select[aria-label="${label}"]`);
  expect(select, `select "${label}" not found`).toBeTruthy();
  return select!;
}

function changeSelect(label: string, value: string) {
  const select = selectByLabel(label);
  select.value = value;
  select.dispatchEvent(new Event("change", { bubbles: true }));
  return select;
}

afterEach(() => {
  root?.unmount();
  root = null;
  container?.remove();
  container = null;
  document.body.innerHTML = "";
});

describe("FingerprintDetectDialog", () => {
  it("默认选第一个供应商并全选其模型;档位选择会传到检测请求里", async () => {
    const onDetect = vi.fn(async (_request: FingerprintDetectRequest) => response());
    const c = mountDialog(onDetect);

    // 默认供应商 = 第一个;模型清单 = 该供应商全部模型(默认全选)。
    await waitFor(() => {
      expect((selectByLabel("Provider") as HTMLSelectElement).value).toBe("p1");
      expect(c.textContent).toContain("model-a");
      expect(c.textContent).not.toContain("model-b1");
    });

    await clickButtonByText("Start detection");
    await waitFor(() => {
      expect(onDetect).toHaveBeenCalledTimes(1);
    });
    expect(onDetect.mock.calls[0][0]).toMatchObject({
      modelNames: ["model-a"],
      probeTier: "standard",
      providerId: "p1"
    });

    // 切到「快速(3 条探针)」后再跑一次。
    const tierSelect = changeSelect("Probe tier", "quick");
    await waitFor(() => {
      expect(tierSelect.value).toBe("quick");
    });

    await clickButtonByText("Start detection");
    await waitFor(() => {
      expect(onDetect).toHaveBeenCalledTimes(2);
    });
    expect(onDetect.mock.calls[1][0]).toMatchObject({ probeTier: "quick" });
    expect(c.textContent).toContain("Start detection");
  });

  it("切换供应商会重置模型清单与结果", async () => {
    const onDetect = vi.fn(async (_request: FingerprintDetectRequest) => response());
    const c = mountDialog(onDetect);

    await waitFor(() => {
      expect(selectByLabel("Provider")).toBeTruthy();
    });
    changeSelect("Provider", "p2");
    await waitFor(() => {
      expect(c.textContent).toContain("model-b1");
      expect(c.textContent).toContain("model-b2");
      expect(c.textContent).not.toContain("model-a");
    });

    await clickButtonByText("Start detection");
    await waitFor(() => {
      expect(onDetect).toHaveBeenCalledTimes(1);
    });
    expect(onDetect.mock.calls[0][0]).toMatchObject({
      modelNames: ["model-b1", "model-b2"],
      providerId: "p2"
    });
  });

  it("库为空时在弹窗内展示跳过理由", async () => {
    const reason = "用户自建库至少需要 2 个模型才能做闭集归因。";
    const onDetect = vi.fn(async (_request: FingerprintDetectRequest) =>
      response({
        results: [
          {
            ...response().results[0],
            skipped: [{ library: "global", reason }]
          }
        ]
      })
    );
    const c = mountDialog(onDetect);

    await clickButtonByText("Start detection");
    await waitFor(() => {
      expect(c.textContent).toContain("Skipped libraries");
    });
    expect(c.textContent).toContain(reason);
  });

  it("两段结论并列渲染", async () => {
    const onDetect = vi.fn(async (_request: FingerprintDetectRequest) => response());
    const c = mountDialog(onDetect);

    await clickButtonByText("Start detection");
    await waitFor(() => {
      expect(c.textContent).toContain("Detection results");
    });
    expect(c.textContent).toContain("Global candidate conclusion");
    expect(c.textContent).toContain("Provider anchor conclusion");
    // 两段结论各自的模型名。
    expect(c.textContent).toContain("Global Model");
    expect(c.textContent).toContain("Anchor Model");
    // 诊断行。
    expect(c.textContent).toContain("Valid probes");
    expect(c.textContent).toContain("12/12");
    expect(c.textContent).toContain("wrouter-v1");
  });

  it("证据不足(insufficient)时不显示具体模型名", async () => {
    const onDetect = vi.fn(async (_request: FingerprintDetectRequest) =>
      response({
        results: [
          {
            ...response().results[0],
            conclusions: {
              global: attribution({
                prediction: undefined,
                prediction_name: undefined,
                results: [
                  {
                    display_name: "secret-model-x",
                    model: "secret-model-x",
                    nuisance_score: 0.1,
                    probability: 0.42,
                    score: 0.1
                  }
                ],
                strength: "insufficient"
              })
            }
          }
        ]
      })
    );
    const c = mountDialog(onDetect);

    await clickButtonByText("Start detection");
    await waitFor(() => {
      expect(c.textContent).toContain("Evidence insufficient to attribute");
    });
    // 弃权态不得泄露任何具体模型名(方案 X4)。
    expect(c.textContent).not.toContain("secret-model-x");
    expect(c.textContent).toContain("Insufficient evidence");
  });
});
