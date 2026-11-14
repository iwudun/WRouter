// Unit tests for call-log filter option builders (调用日志密钥筛选).
// Regression: the credential Select previously used logSelectOptions (value ===
// label == display name), so the backend credential filter (api_key_id
// equality) never matched — filtering by a single key returned zero rows.
import { describe, expect, it } from "vitest";
import { credentialSelectOptions } from "@/pages/home/shared/usage";

describe("credentialSelectOptions", () => {
  const credentials = [
    { id: "key_1", name: "Key One" },
    { id: "key_2", name: "Key Two" }
  ];

  it("option value 为凭据 id（而非显示名称），后端按 api_key_id 过滤才能匹配", () => {
    expect(credentialSelectOptions("全部密钥", credentials, undefined)).toEqual([
      { label: "全部密钥", value: "" },
      { label: "Key One", value: "key_1" },
      { label: "Key Two", value: "key_2" }
    ]);
  });

  it("选中的 id 已不在当前窗口时保留合成选项，控件不静默回退为全部密钥", () => {
    const options = credentialSelectOptions("全部密钥", credentials, "key_stale");
    expect(options).toContainEqual({ label: "key_stale", value: "key_stale" });
    expect(options).toHaveLength(4);
  });

  it("名称缺失时回退到 id 作为显示文本", () => {
    const options = credentialSelectOptions("全部密钥", [{ id: "key_3", name: "" }], undefined);
    expect(options).toContainEqual({ label: "key_3", value: "key_3" });
  });
});
