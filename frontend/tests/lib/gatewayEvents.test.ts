// F2b.8: SSE 事件链路测试。
//  - createSseFrameParser：帧切分 / 多行 data / 无 event 名缺省 message
//  - EventInvalidationCoalescer：去抖合并 + status 组节流（F2b.6 事件风暴防护）
//  - createGatewayEventsClient：断线指数退避重连状态机 + 重连成功回调（F2b.1）
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  createGatewayEventsClient, createSseFrameParser, EventInvalidationCoalescer
} from "@/lib/gateway-events";

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("createSseFrameParser", () => {
  it("按空行切帧并解析 event/data", () => {
    const frames: Array<{ event: string; data: string }> = [];
    const parse = createSseFrameParser((frame) => frames.push(frame));
    parse('event: config-changed\ndata: {"scope":"providers"}\n\n');
    parse("event: gateway-status\ndata: {\"ok\":true}\n\ndata: {\"partial\":true}");
    expect(frames[0]).toEqual({ event: "config-changed", data: '{"scope":"providers"}' });
    expect(frames[1]).toEqual({ event: "gateway-status", data: '{"ok":true}' });
    // 未完帧残留 buffer，不产生半帧
    expect(frames).toHaveLength(2);
  });

  it("跨 chunk 的帧在补齐后解析", () => {
    const frames: string[] = [];
    const parse = createSseFrameParser((frame) => frames.push(frame.event));
    parse("event: log-appended\nda");
    parse("ta: {\"id\":1}\n\n");
    expect(frames).toEqual(["log-appended"]);
  });

  it("无 event 名的帧缺省 message", () => {
    const frames: string[] = [];
    const parse = createSseFrameParser((frame) => frames.push(frame.event));
    parse("data: {}\n\n");
    expect(frames).toEqual(["message"]);
  });
});

describe("EventInvalidationCoalescer (F2b.6)", () => {
  it("log-appended 事件风暴合并为一次失效", () => {
    const flushed: Array<ReadonlyArray<readonly unknown[]>> = [];
    const coalescer = new EventInvalidationCoalescer((prefixes) => flushed.push(prefixes));

    coalescer.add("log-appended");
    coalescer.add("log-appended");
    coalescer.add("log-appended");
    vi.advanceTimersByTime(299);
    expect(flushed).toHaveLength(0);
    vi.advanceTimersByTime(1);
    expect(flushed).toHaveLength(1);
    expect(flushed[0]).toEqual([["callLogs"]]);
  });

  it("gateway-status 心跳被节流到 3s 最小间隔", () => {
    const flushed: number[] = [];
    const coalescer = new EventInvalidationCoalescer(() => flushed.push(Date.now()));

    // 心跳 1/s：t=0 / 1000 / 2000 / 3000 / 4000
    coalescer.add("gateway-status");
    vi.advanceTimersByTime(0);
    expect(flushed).toHaveLength(1); // 首次触达立即失效
    for (const _ of [1, 2, 3, 4]) {
      vi.advanceTimersByTime(1000);
      coalescer.add("gateway-status");
    }
    vi.advanceTimersByTime(4000);
    // t=0 / 3000 / 6000 → 三次（1s 心跳不会把状态查询打回 1s 轮询）
    expect(flushed).toHaveLength(3);
  });

  it("不同组各自独立触发", () => {
    const flushed: Array<ReadonlyArray<readonly unknown[]>> = [];
    const coalescer = new EventInvalidationCoalescer((prefixes) => flushed.push(prefixes));
    coalescer.add("config-changed");
    coalescer.add("usage-updated");
    vi.advanceTimersByTime(300);
    expect(flushed).toHaveLength(2);
  });

  it("dispose 后不再触发", () => {
    const flushed: Array<ReadonlyArray<readonly unknown[]>> = [];
    const coalescer = new EventInvalidationCoalescer((prefixes) => flushed.push(prefixes));
    coalescer.add("log-appended");
    coalescer.dispose();
    vi.advanceTimersByTime(1000);
    expect(flushed).toHaveLength(0);
  });
});

describe("createGatewayEventsClient 重连状态机 (F2b.1)", () => {
  function sseResponse(frames: string): Response {
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(new TextEncoder().encode(frames));
        controller.close();
      }
    });
    return new Response(stream, { status: 200, headers: { "content-type": "text/event-stream" } });
  }

  it("失败→退避→重连成功：状态序列与 onReconnected 事件刷新", async () => {
    const statuses: string[] = [];
    const events: Array<{ type: string; data: unknown }> = [];
    let reconnected = 0;
    let calls = 0;

    const client = createGatewayEventsClient({
      onEvent: (event) => events.push(event),
      onStatus: (status) => statuses.push(status),
      onReconnected: () => {
        reconnected += 1;
      },
      fetchImpl: vi.fn(() => {
        calls += 1;
        if (calls <= 2) {
          return Promise.resolve(new Response("unauthorized", { status: 401 }));
        }
        return Promise.resolve(
          sseResponse('event: gateway-status\ndata: {"ok":true,"name":"WRouter"}\n\n')
        );
      })
    });

    client.start();
    await vi.advanceTimersByTimeAsync(0);
    // 第 1 次失败 → reconnecting → 1s 退避
    expect(statuses[0]).toBe("connecting");
    await vi.advanceTimersByTimeAsync(1000);
    // 第 2 次失败 → 2s 退避
    await vi.advanceTimersByTimeAsync(2000);
    // 第 3 次成功 → live + onReconnected + 事件解析
    await vi.advanceTimersByTimeAsync(0);

    expect(statuses).toContain("live");
    expect(reconnected).toBe(1);
    expect(events).toEqual([{ type: "gateway-status", data: { ok: true, name: "WRouter" } }]);

    client.stop();
  });

  it("指数退避按 1s/2s/4s 递增", async () => {
    let calls = 0;
    let liveCount = 0;
    const client = createGatewayEventsClient({
      onEvent: () => undefined,
      onStatus: (status) => {
        if (status === "live") {
          liveCount += 1;
        }
      },
      onReconnected: () => undefined,
      fetchImpl: vi.fn(() => {
        calls += 1;
        return Promise.resolve(new Response("down", { status: 503 }));
      })
    });

    client.start();
    await vi.advanceTimersByTimeAsync(0); // call 1 fails
    await vi.advanceTimersByTimeAsync(999);
    expect(calls).toBe(1); // 1s 退避未到
    await vi.advanceTimersByTimeAsync(1);
    expect(calls).toBe(2); // 1s 到点
    await vi.advanceTimersByTimeAsync(1999);
    expect(calls).toBe(2); // 2s 退避未到
    await vi.advanceTimersByTimeAsync(1);
    expect(calls).toBe(3); // 2s 到点

    client.stop();
    expect(liveCount).toBe(0);
  });

  it("stop() 之后彻底取消订阅:不再发起任何请求(进入停止态即调 stop)", async () => {
    let calls = 0;
    const client = createGatewayEventsClient({
      onEvent: () => undefined,
      onStatus: () => undefined,
      onReconnected: () => undefined,
      fetchImpl: vi.fn(() => {
        calls += 1;
        return Promise.resolve(new Response("down", { status: 503 }));
      })
    });

    client.start();
    await vi.advanceTimersByTimeAsync(0);
    expect(calls).toBe(1);

    // 页面进入「正在停止」→ hook cleanup 调 stop() → 必须**停止重连**。
    client.stop();
    await vi.advanceTimersByTimeAsync(60_000);
    expect(calls).toBe(1);
  });

  it("收到 service-stopping 停机通告后通知上层且**不再重连**(2026-11-03)", async () => {
    let calls = 0;
    let stoppingCalls = 0;
    const events: Array<{ type: string; data: unknown }> = [];
    const client = createGatewayEventsClient({
      onEvent: (event) => events.push(event),
      onStatus: () => undefined,
      onReconnected: () => undefined,
      onServiceStopping: () => {
        stoppingCalls += 1;
      },
      fetchImpl: vi.fn(() => {
        calls += 1;
        return Promise.resolve(
          sseResponse('event: service-stopping\ndata: {"reason":"shutdown"}\n\n')
        );
      })
    });

    client.start();
    await vi.advanceTimersByTimeAsync(0);
    expect(stoppingCalls).toBe(1);
    // 停机通告不是业务事件 → 不进事件流。
    expect(events).toEqual([]);

    // 关键断言:后端正在退出 → 不再重连(否则只会徒劳地刷失败请求)。
    await vi.advanceTimersByTimeAsync(60_000);
    expect(calls).toBe(1);

    client.stop();
  });
});
