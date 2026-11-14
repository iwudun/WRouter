// 回归测试:RPC 错误信封解包。
// 此前桥接层只读顶层 error.message,导致 FastAPI 的 {detail:{...}} 信封
// (501 NOT_IMPLEMENTED / 401)一律显示成 "Unknown error"(web 桥甚至抛 TypeError)。
import { describe, expect, it } from "vitest";
import { rpcErrorMessage } from "@/lib/rpc-error";

describe("rpcErrorMessage", () => {
  it("解包 501 NOT_IMPLEMENTED 的 detail 信封(本次回归点)", () => {
    const payload = {
      detail: {
        error: {
          code: "NOT_IMPLEMENTED",
          message: "Method 'testApiEndpoints' is not implemented in WRouter. Tracked for P13/P14.",
          method: "testApiEndpoints"
        },
        ok: false
      }
    };
    expect(rpcErrorMessage(payload, 501)).toBe(
      "Method 'testApiEndpoints' is not implemented in WRouter. Tracked for P13/P14."
    );
  });

  it("解包 detail 为字符串的信封(如 401)", () => {
    expect(rpcErrorMessage({ detail: "Unauthorized" }, 401)).toBe("Unauthorized");
  });

  it("解包顶层 error.message(普通 RPC 错误)", () => {
    const payload = { error: { message: "boom" }, ok: false };
    expect(rpcErrorMessage(payload, 500)).toBe("boom");
  });

  it("detail 为无 message 的对象时序列化兜底", () => {
    expect(rpcErrorMessage({ detail: { code: "X" } }, 400)).toBe('{"code":"X"}');
  });

  it("非 JSON 响应体回退到 HTTP 状态描述", () => {
    expect(rpcErrorMessage(undefined, 500)).toBe("WRouter web API failed with HTTP 500");
  });
});
