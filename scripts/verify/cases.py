"""Golden-sample case definitions (rebuild plan 14, R0.2/R0.3).

Single source of truth for the golden capture / parity replay toolchain. Each
case describes:

* the client request sent to the WRouter gateway (protocol + body + headers),
* the provider configuration the gateway is given (mock upstream base URL),
* what the mock upstream answers with (JSON body / SSE line list / error /
  delay / truncated stream), so the mock server stays stateless and
  deterministic,
* which response fields are volatile (parity diffs exclude them).

Streaming upstream bodies reuse ``testdata/fixtures/*.json`` (their
``upstream_sse`` line lists; R7.4 migrated from the retired ``backend/tests/fixtures``) so capture exercises the exact byte sequences the
unit tests already treat as canonical — including the tool-call leading-`{`
fragment case (C7).

Case ids map into provider base URLs (``http://127.0.0.1:3999/<case-id>``),
so the mock upstream needs no request matching: the path carries the case.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
FIXTURES_DIR = REPO_ROOT / "testdata" / "fixtures"

MOCK_UPSTREAM_PORT = 3999
GATEWAY_PORT = 3460
MANAGEMENT_PORT = 3462
MANAGEMENT_TOKEN = "r0-capture-token"
GOLDEN_DIR = REPO_ROOT / "testdata" / "golden"

# Client-visible api key configured for capture runs (auth-failed case
# exercises the wrong-key path; every other case sends this one).
DEFAULT_CLIENT_KEY = "sk-golden-1"

# JSON keys dropped before parity diffing (volatile across runs / backends).
GLOBAL_VOLATILE_JSON_KEYS = ["id", "created", "created_at", "request_id", "requestId", "ts", "item_id", "system_fingerprint", "service_tier"]
# SSE data payload keys dropped the same way.
GLOBAL_VOLATILE_SSE_KEYS = GLOBAL_VOLATILE_JSON_KEYS
# Response headers excluded from diff.
GLOBAL_VOLATILE_HEADERS = ["date", "x-wrouter-request-id", "server", "content-length", "x-request-id"]


def _load_fixture(name: str) -> dict:
    with open(FIXTURES_DIR / name, encoding="utf-8") as fh:
        return json.load(fh)


def _sse_lines(name: str) -> list[str]:
    return list(_load_fixture(name)["upstream_sse"])


@dataclass
class ProviderCfg:
    """One provider entry for the gateway config (AppConfig.Providers shape)."""

    id: str
    base_url: str
    protocol: str = "openai"
    models: list[str] = field(default_factory=lambda: ["m-golden"])

    def to_config(self, api_key: str = "") -> dict:
        return {
            "id": self.id,
            "name": self.id,
            "api_base_url": self.base_url,
            "protocol": self.protocol,
            "models": list(self.models),
            "api_key": api_key,
        }


@dataclass
class Case:
    id: str
    description: str
    client_protocol: str  # openai | anthropic | gemini | gemini_stream | openai_responses | openai_embeddings
    request: dict
    providers: list[ProviderCfg]
    api_keys: list[dict] | None = None  # None/[] → seed_config 种入黄金默认密钥(auth_headers 携带它)
    client_headers: dict = field(default_factory=dict)
    # 全局失败回退(Router.fallback)。缺省 → 空表 = 不回退。
    # 形如 {"mode": "model-chain", "retryCount": 0, "models": ["<provider>/<model>"]}。
    router_fallback: dict | None = None
    # 写进上游字节样本的备注(样本即真相 → 已知缺陷也要写在样本里,
    # 免得后来人把「当前行为」误读成「期望行为」)。
    note: str = ""
    # Mock upstream behaviour
    upstream_status: int = 200
    upstream_json: dict | None = None
    upstream_sse: list[str] | None = None
    upstream_delay_s: float = 0.0
    upstream_truncate_after_lines: int | None = None  # send N SSE lines then close early
    # base_url 后缀 → 该上游的状态码(如 {"-a": 503} 表示候选链第一个上游报 503、
    # 第二个正常)。没有它,同一 case 的多个上游行为完全相同 → 「回退」类 case
    # 永远只打一次上游,退化成假覆盖。
    upstream_status_suffix: dict[str, int] = field(default_factory=dict)
    # Echo picked request-body fields back inside a valid response text, so
    # golden snapshots pin the OUTBOUND (converted) shape (thinking matrix,
    # 2026-10-14). Non-stream only.
    upstream_reflect: list[str] | None = None
    extra_volatile_keys: list[str] = field(default_factory=list)

    @property
    def stream(self) -> bool:
        return bool(self.request.get("stream")) or self.client_protocol == "gemini_stream"

    @property
    def spec_ref(self) -> str:
        """规格锚点(B-5):本 case 在协议转换矩阵里的位置。

        相当于 magpie 的「issue 号」—— 但 WRouter 没有 issue tracker(gitee),
        故用「客户端协议 × 上游协议 × 面向」的矩阵格子做可回溯锚点,见 SPEC_REFS。
        """
        return SPEC_REFS.get(self.id, "")

    def gateway_path(self) -> str:
        return {
            "openai": "/v1/chat/completions",
            "anthropic": "/v1/messages",
            "openai_responses": "/v1/responses",
            "openai_embeddings": "/v1/embeddings",
            "gemini": f"/v1beta/models/{self.request.get('model', 'm-golden')}:generateContent",
            "gemini_stream": f"/v1beta/models/{self.request.get('model', 'm-golden')}:streamGenerateContent",
            "gemini_interactions": "/v1/interactions",
        }[self.client_protocol]


def _mock_base(case_id: str, suffix: str = "") -> str:
    return f"http://127.0.0.1:{MOCK_UPSTREAM_PORT}/{case_id}{suffix}"


def selector_for(case: Case) -> str:
    """显式 ``provider/model`` 选择器 —— 客户端侧选择器的**唯一构造点**。

    裸模型名已不被解析(见 docs/reference/03-网关协议与路由.md §3.1),
    所以 golden 的客户端请求必须带显式选择器。
    """
    provider = case.providers[0]
    model = provider.models[0] if provider.models else "m-golden"
    return f"{provider.id}/{model}"


def explicit_request(case: Case) -> dict:
    """客户端请求契约:显式选择器 + 该 case 的请求体。

    产出形状与 ``testdata/golden/cases/<case>/request.json`` 一致:
    ``{"method", "path", "headers", "body"}``。
    """
    selector = selector_for(case)
    body = {**case.request, "model": selector}
    path = case.gateway_path()
    raw_model = str(case.request.get("model", "m-golden"))
    if raw_model and raw_model in path:
        # gemini 形态的路径把模型名放在路径段里 → 一并换成显式选择器
        path = path.replace(raw_model, selector)
    return {"method": "POST", "path": path, "headers": {}, "body": body}


def _one_provider(case_id: str, protocol: str = "openai", models: list[str] | None = None) -> list[ProviderCfg]:
    return [ProviderCfg(id=f"prov-{case_id}", base_url=_mock_base(case_id), protocol=protocol, models=models or ["m-golden"])]


# ---------------------------------------------------------------------------
# Minimal valid client request bodies per protocol
# ---------------------------------------------------------------------------

_OPENAI_MSGS = [{"role": "user", "content": "Say hi"}]


def _openai_request(**extra: object) -> dict:
    body: dict = {"model": "m-golden", "messages": list(_OPENAI_MSGS)}
    body.update(extra)
    return body


def _anthropic_request(**extra: object) -> dict:
    body: dict = {"model": "m-golden", "max_tokens": 256, "messages": list(_OPENAI_MSGS)}
    body.update(extra)
    return body


def _gemini_request(**extra: object) -> dict:
    body: dict = {
        "model": "m-golden",
        "contents": [{"role": "user", "parts": [{"text": "Say hi"}]}],
    }
    body.update(extra)
    return body


def _openai_ok_chat() -> dict:
    return {
        "id": "up-ok",
        "object": "chat.completion",
        "created": 1,
        "model": "m-golden",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "hello from upstream"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 3, "completion_tokens": 5, "total_tokens": 8},
    }


def _anthropic_ok_messages() -> dict:
    return {
        "id": "msg-up-ok",
        "type": "message",
        "role": "assistant",
        "model": "m-golden",
        "content": [{"type": "text", "text": "hola desde upstream"}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 3, "output_tokens": 4},
    }


def _upstream_error(status: int, message: str) -> tuple[int, dict]:
    return status, {"error": {"message": message, "type": "upstream_error", "code": str(status)}}


def _err_case(case_id: str, status: int, message: str) -> Case:
    status_code, body = _upstream_error(status, message)
    return Case(
        id=case_id,
        description=f"Upstream returns HTTP {status}; gateway must rewrite it into the client protocol envelope.",
        client_protocol="openai",
        request=_openai_request(),
        providers=_one_provider(case_id),
        upstream_status=status_code,
        upstream_json=body,
    )


def _case_openai_stream(name: str, fixture: str, description: str) -> Case:
    return Case(
        id=name,
        description=description,
        client_protocol="openai",
        request=_openai_request(stream=True),
        providers=_one_provider(name),
        upstream_sse=_sse_lines(fixture),
    )


# ---------------------------------------------------------------------------
# The case catalogue (order = capture order; keep ids stable — they are
# directory names under testdata/golden/cases/ and parity report keys)
# ---------------------------------------------------------------------------

ALL_CASES: list[Case] = [
    # --- pass-through / same-protocol ---
    Case(
        id="openai-chat-basic",
        description="OpenAI chat, non-stream, same-protocol pass-through.",
        client_protocol="openai",
        request=_openai_request(),
        providers=_one_provider("openai-chat-basic"),
        upstream_json=_openai_ok_chat(),
    ),
    Case(
        id="openai-chat-stream-text",
        description="OpenAI chat, streaming text-only chunks with finish_reason and [DONE].",
        client_protocol="openai",
        request=_openai_request(stream=True),
        providers=_one_provider("openai-chat-stream-text"),
        upstream_sse=[
            'data: {"id":"c1","object":"chat.completion.chunk","created":1,"model":"m-golden","choices":[{"index":0,"delta":{"role":"assistant","content":"Hel"},"finish_reason":null}]}',
            'data: {"id":"c1","object":"chat.completion.chunk","created":1,"model":"m-golden","choices":[{"index":0,"delta":{"content":"lo"},"finish_reason":null}]}',
            'data: {"id":"c1","object":"chat.completion.chunk","created":1,"model":"m-golden","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}',
            "data: [DONE]",
        ],
    ),
    _case_openai_stream(
        "openai-chat-stream-tool-first-fragment",
        "chat_stream_args_in_opening_chunk.json",
        "C7 regression: tool-call arguments opening fragment ('{') relayed from the first chunk (fixture: chat_stream_args_in_opening_chunk).",
    ),
    _case_openai_stream(
        "openai-chat-stream-usage-tail",
        "usage_in_stream.json",
        "Streaming chat carrying a usage block in the stream tail (fixture: usage_in_stream).",
    ),
    Case(
        id="openai-embeddings",
        description="Embeddings pass-through (openai_embeddings client, openai upstream).",
        client_protocol="openai_embeddings",
        request={"model": "m-golden", "input": "hello"},
        providers=_one_provider("openai-embeddings"),
        upstream_json={"object": "list", "data": [{"object": "embedding", "index": 0, "embedding": [0.1, 0.2, 0.3]}], "model": "m-golden", "usage": {"prompt_tokens": 2, "total_tokens": 2}},
    ),
    Case(
        id="openai-responses-basic",
        description="OpenAI Responses non-stream client → chat-completions upstream (A2 conversion; the gateway rejects protocol=openai_responses providers, see openai-responses-native-provider).",
        client_protocol="openai_responses",
        request={"model": "m-golden", "input": "Say hi"},
        providers=[ProviderCfg(id="prov-openai-responses-basic", base_url=_mock_base("openai-responses-basic"), protocol="openai")],
        upstream_json=_openai_ok_chat(),
    ),
    Case(
        id="openai-responses-native-provider",
        description="Responses client against a protocol=openai_responses provider: current Python gateway rejects with 502 'Unsupported provider protocol' — golden pins this behaviour so the Rust rewrite must match it (or this case updates when semantics change).",
        client_protocol="openai_responses",
        request={"model": "m-golden", "input": "Say hi"},
        providers=[ProviderCfg(id="prov-openai-responses-native-provider", base_url=_mock_base("openai-responses-native-provider"), protocol="openai_responses")],
        upstream_json=_openai_ok_chat(),
    ),
    Case(
        id="openai-responses-stream",
        description="OpenAI Responses streaming client → Anthropic streaming upstream (fixture: responses_stream — client_protocol openai_responses, upstream_protocol anthropic).",
        client_protocol="openai_responses",
        request={"model": "m-golden", "input": "Say hi", "stream": True},
        providers=[ProviderCfg(id="prov-openai-responses-stream", base_url=_mock_base("openai-responses-stream"), protocol="anthropic")],
        upstream_sse=_sse_lines("responses_stream.json"),
    ),
    Case(
        id="anthropic-native-stream-tool-use",
        description="Anthropic Messages streaming tool-use, same-protocol pass-through (fixture: anthropic_stream_tool_use).",
        client_protocol="anthropic",
        request=_anthropic_request(stream=True),
        providers=[ProviderCfg(id="prov-anthropic-native-stream-tool-use", base_url=_mock_base("anthropic-native-stream-tool-use"), protocol="anthropic")],
        upstream_sse=_sse_lines("anthropic_stream_tool_use.json"),
    ),
    # --- cross-protocol conversion (gateway translates both directions) ---
    Case(
        id="anthropic-client-openai-upstream-stream",
        description="Anthropic Messages client → OpenAI upstream, streaming: SSE event translation.",
        client_protocol="anthropic",
        request=_anthropic_request(stream=True),
        providers=_one_provider("anthropic-client-openai-upstream-stream"),
        upstream_sse=[
            'data: {"id":"c1","object":"chat.completion.chunk","created":1,"model":"m-golden","choices":[{"index":0,"delta":{"role":"assistant","content":"bon"},"finish_reason":null}]}',
            'data: {"id":"c1","object":"chat.completion.chunk","created":1,"model":"m-golden","choices":[{"index":0,"delta":{"content":"jour"},"finish_reason":null}]}',
            'data: {"id":"c1","object":"chat.completion.chunk","created":1,"model":"m-golden","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}',
            "data: [DONE]",
        ],
    ),
    Case(
        id="openai-client-anthropic-upstream",
        description="OpenAI chat client → Anthropic upstream (reverse conversion).",
        client_protocol="openai",
        request=_openai_request(),
        providers=[ProviderCfg(id="prov-openai-client-anthropic-upstream", base_url=_mock_base("openai-client-anthropic-upstream"), protocol="anthropic")],
        upstream_json=_anthropic_ok_messages(),
    ),
    Case(
        id="gemini-client-openai-upstream",
        description="Gemini generateContent client → OpenAI-protocol provider. Pins observed behaviour: URL is built in gemini shape regardless of provider protocol, and the openai-shaped upstream JSON is relayed to the client without conversion.",
        client_protocol="gemini",
        request=_gemini_request(),
        providers=_one_provider("gemini-client-openai-upstream"),
        upstream_json=_openai_ok_chat(),
    ),
    Case(
        id="gemini-native-stream",
        description="Gemini client → Gemini upstream, streaming pass-through (fixture: gemini_stream_generate_content).",
        client_protocol="gemini_stream",
        request=_gemini_request(),
        providers=[ProviderCfg(id="prov-gemini-native-stream", base_url=_mock_base("gemini-native-stream"), protocol="gemini")],
        upstream_sse=_sse_lines("gemini_stream_generate_content.json"),
    ),
    # --- gemini interactions (R0 inventory gap: protocol face uncovered;
    #     captured at R3 per 内部盘点记录 r0-inventory §1.1 note) ---
    Case(
        id="gemini-interactions-basic",
        description="Gemini Interactions client → gemini provider (R3-added coverage for the gemini.interactions protocol face).",
        client_protocol="gemini_interactions",
        request={"model": "m-golden", "input": "Say hi"},
        providers=[ProviderCfg(id="prov-gemini-interactions-basic", base_url=_mock_base("gemini-interactions-basic"), protocol="gemini")],
        upstream_json={
            "id": "int-up-1",
            "status": "completed",
            "steps": [
                {"type": "message", "text": "hello from interaction"},
                {"type": "function_call", "id": "call-1", "name": "lookup", "arguments": {"q": "hi"}},
            ],
            "usageMetadata": {"promptTokenCount": 2, "candidatesTokenCount": 3, "totalTokenCount": 5},
        },
    ),
    # --- upstream error paths (envelope rewrite into client protocol) ---
    _err_case("upstream-401", 401, "bad upstream key"),
    _err_case("upstream-429", 429, "rate limited"),
    Case(
        id="upstream-503-fallback",
        description="First provider 503 (FALLBACK_STATUS_CODES) → candidate chain falls back to second provider which succeeds.",
        client_protocol="openai",
        request=_openai_request(),
        providers=[
            ProviderCfg(id="prov-upstream-503-fallback-a", base_url=_mock_base("upstream-503-fallback", "-a"), models=["m-golden"]),
            ProviderCfg(id="prov-upstream-503-fallback-b", base_url=_mock_base("upstream-503-fallback", "-b"), models=["m-golden"]),
        ],
        # 第一个上游报 503、第二个正常 —— 没有这条,两个上游行为相同,
        # 网关拿到 200 就不回退,这个 case 会退化成「只打一次上游」的假覆盖。
        upstream_status_suffix={"-a": 503},
        # 回退链(model-chain):主 attempt 打 -a,503 后切到 -b。
        # 没有这条,配置里没有回退链 → 网关只会原地重试 -a 然后把 503 透给客户端,
        # 依然测不到「候选链回退」这层语义。
        router_fallback={
            "mode": "model-chain",
            "retryCount": 0,
            "models": ["prov-upstream-503-fallback-b/m-golden"],
        },
        # ✅ 2026-11-05 已修:回退那一环原先把**内部选择器**
        # ("prov-upstream-503-fallback-b/m-golden")当模型名发给厂商 —— 真实厂商会回
        # model not found → 回退在生产里等于失效;mock 不校验 model 字段 →
        # 客户端可见响应看不出来,是上游字节基线把它抓出来的。
        # 修复:候选的 model 取 target 解析出的裸模型名
        # (`forward/routing.rs` 的 `upstream_model_name`)。
        # 回归用例:`backend/crates/wr-server/tests/gateway/`
        # `model_chain_fallback_sends_bare_model_to_upstream`。
        # 本样本记录的是**修复后**的行为(第 5 条的 body.model = "m-golden")。
        note=(
            "回退环的 body.model 是裸模型名 —— 修复前这里曾是内部选择器"
            "(prov-upstream-503-fallback-b/m-golden),真实厂商会回 model not found。"
            "回归用例:wr-server/tests/gateway/ model_chain_fallback_sends_bare_model_to_upstream"
        ),
    ),
    Case(
        id="upstream-400-no-retry",
        description="Upstream 400 (NO_RETRY_STATUS_CODES): must not retry nor fallback; 400 envelope returned as-is to client.",
        client_protocol="openai",
        request=_openai_request(),
        providers=[
            ProviderCfg(id="prov-upstream-400-no-retry", base_url=_mock_base("upstream-400-no-retry"), models=["m-golden"]),
            ProviderCfg(id="prov-upstream-400-no-retry-b", base_url=_mock_base("upstream-400-no-retry", "-b"), models=["m-golden"]),
        ],
        upstream_status=400,
        upstream_json={"error": {"message": "bad request upstream", "type": "invalid_request_error", "code": "400"}},
    ),
    Case(
        id="thinking-openai-to-anthropic",
        description="Reasoning effort crosses to Anthropic upstream: thinking derived from effort, custom sampling dropped, tool name sanitized, parallel_tool_calls mapped to disable_parallel_tool_use. Mock reflects the picked outbound fields back as text.",
        client_protocol="openai",
        request=_openai_request(
            reasoning_effort="high",
            temperature=0.5,
            top_p=0.9,
            max_tokens=2048,
            parallel_tool_calls=False,
            tool_choice="auto",
            tools=[{"type": "function", "function": {"name": "mcp.demo.tool", "description": "d", "parameters": {"type": "object"}}}],
        ),
        providers=_one_provider("thinking-openai-to-anthropic", protocol="anthropic"),
        upstream_reflect=["thinking", "temperature", "top_p", "tool_choice", "tools", "max_tokens"],
    ),
    Case(
        id="thinking-gemini3-thinking-level",
        description="Gemini 3 generation split: effort medium maps to thinkingLevel (flash supports medium) and thinkingBudget must not appear. Mock reflects generationConfig back.",
        client_protocol="openai",
        request={"model": "gemini-3-flash-golden", "messages": list(_OPENAI_MSGS), "reasoning_effort": "medium"},
        providers=_one_provider("thinking-gemini3-thinking-level", protocol="gemini", models=["gemini-3-flash-golden"]),
        upstream_reflect=["generationConfig"],
    ),
    Case(
        id="stream-midway-disconnect",
        description="Upstream stream ends after 2 of 5 events (no [DONE]): pins the observed behaviour that the Python gateway synthesizes a normal finish_reason=stop chunk + [DONE] instead of failing the stream.",
        client_protocol="openai",
        request=_openai_request(stream=True),
        providers=_one_provider("stream-midway-disconnect"),
        upstream_sse=[
            'data: {"id":"c1","object":"chat.completion.chunk","created":1,"model":"m-golden","choices":[{"index":0,"delta":{"role":"assistant","content":"par"},"finish_reason":null}]}',
            'data: {"id":"c1","object":"chat.completion.chunk","created":1,"model":"m-golden","choices":[{"index":0,"delta":{"content":"tial"},"finish_reason":null}]}',
            'data: {"id":"c1","object":"chat.completion.chunk","created":1,"model":"m-golden","choices":[{"index":0,"delta":{"content":"truncated"},"finish_reason":null}]}',
            'data: {"id":"c1","object":"chat.completion.chunk","created":1,"model":"m-golden","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}',
            "data: [DONE]",
        ],
        upstream_truncate_after_lines=2,
    ),
    # --- gateway-side auth ---
    Case(
        id="auth-failed",
        description="Client presents an unknown API key while keys are configured → 401 authentication_error; audited in request_logs.",
        client_protocol="openai",
        request=_openai_request(),
        providers=_one_provider("auth-failed"),
        api_keys=[{"key": DEFAULT_CLIENT_KEY, "enabled": True, "name": "golden"}],
        client_headers={"Authorization": f"Bearer sk-wrong-key"},
        upstream_json=_openai_ok_chat(),
    ),
]


def case_by_id(case_id: str) -> Case:
    for case in [*ALL_CASES, *PERF_CASES]:
        if case.id == case_id:
            return case
    raise KeyError(f"unknown golden case: {case_id}")


# ---------------------------------------------------------------------------
# 规格锚点(B-5)
#
# magpie 用「测试名带 issue 号」让「测试名即规格」;WRouter 没有 issue tracker,
# 等价物是**协议转换矩阵里的格子** —— 每个 case 锁定哪一格,写在这里一处可审。
#
# ⚠️ 锚点只标「**格子**(客户端协议 × 上游协议 × 面向)」,**不标行为**:
# 矩阵的「当前行为」列冻结于 2026-10-12(20261012_01 阶段 A),而随后的语义管线
# 重写(20261012_01 阶段 B/C)已改变实际行为(如 gemini × openai 由「静默透传」
# 变成「语义转换」)。锚点若写成行为断言,会随实现漂移而失真。
#
# 锚点来源:
#   20261012_01 = 内部设计记录《统一语义模型 阶段A 基线与契约》
#                 (§1.1 协议空间 / §1.2 请求侧 / §1.4 流式 relay / §3.2 reasoning-thinking
#                  / §3.7 usage)
#   03-网关协议与路由 = docs/reference/03-网关协议与路由.md
#                 (§1 网关端点 / §4 三层重试与回退)
# ---------------------------------------------------------------------------
SPEC_REFS: dict[str, str] = {
    # --- openai 同协议 ---
    "openai-chat-basic": "20261012_01 §1.2 请求侧 · openai × openai",
    "openai-chat-stream-text": "20261012_01 §1.4 流式 relay · openai × openai",
    "openai-chat-stream-tool-first-fragment": "20261012_01 §1.4 流式 relay · openai × openai tool_call 首段分片",
    "openai-chat-stream-usage-tail": "20261012_01 §1.4 流式 relay · openai × openai usage 尾块",
    "openai-embeddings": "20261012_01 §1.2 请求侧 · openai_embeddings × openai",
    # --- openai 家族互转 ---
    "openai-responses-basic": "20261012_01 §1.2 请求侧 · openai_responses × openai",
    "openai-responses-native-provider": "20261012_01 §1.1 协议空间 · provider protocol 拒绝(502)",
    "openai-responses-stream": "20261012_01 §1.4 流式 relay · openai_responses × anthropic",
    # --- anthropic ---
    "anthropic-native-stream-tool-use": "20261012_01 §1.4 流式 relay · anthropic × anthropic",
    "anthropic-client-openai-upstream-stream": "20261012_01 §1.4 流式 relay · anthropic × openai",
    "openai-client-anthropic-upstream": "20261012_01 §1.2 请求侧 · openai × anthropic",
    # --- gemini ---
    "gemini-client-openai-upstream": "20261012_01 §1.2 请求侧 · gemini × openai",
    "gemini-native-stream": "20261012_01 §1.4 流式 relay · gemini × gemini",
    "gemini-interactions-basic": "20261012_01 §1.2 请求侧 · gemini_interactions × gemini",
    # --- reasoning / thinking ---
    "thinking-openai-to-anthropic": "20261012_01 §3.2 reasoning / thinking · openai → anthropic thinking 映射",
    "thinking-gemini3-thinking-level": "20261012_01 §3.2 reasoning / thinking · gemini thinkingLevel 分代",
    # --- 错误 / 韧性 ---
    "upstream-401": "03-网关协议与路由 §4 三层重试与回退 · 上游 401 改写",
    "upstream-429": "03-网关协议与路由 §4 三层重试与回退 · 上游 429 改写",
    "upstream-400-no-retry": "03-网关协议与路由 §4 三层重试与回退 · 400 不重试不回退",
    "upstream-503-fallback": "03-网关协议与路由 §4 三层重试与回退 · 503 候选链回退",
    "stream-midway-disconnect": "20261012_01 §1.4 流式 relay · 缺 terminal 的 EOF",
    "auth-failed": "03-网关协议与路由 §1 网关端点 · 客户端密钥校验(401)",
}


# ---------------------------------------------------------------------------
# Perf-only cases (R0.5): never captured as golden samples; the mock upstream
# serves them so the baseline measures real HTTP end-to-end through the
# gateway. Sizes are chosen so per-request overhead dominates client-side
# transfer noise on loopback.
# ---------------------------------------------------------------------------

_PERF_CHUNK_BYTES = 1024
_PERF_CHUNK_COUNT = 100
_PERF_JSON_BYTES = 65536


def _perf_sse() -> list[str]:
    lines = []
    for i in range(_PERF_CHUNK_COUNT):
        payload = json.dumps({
            "id": "perf", "object": "chat.completion.chunk", "created": 1, "model": "m-golden",
            "choices": [{"index": 0, "delta": {"content": "x" * _PERF_CHUNK_BYTES}, "finish_reason": None}],
        }, separators=(",", ":"))
        lines.append(f"data: {payload}")
    lines.append('data: {"id":"perf","object":"chat.completion.chunk","created":1,"model":"m-golden","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}')
    lines.append("data: [DONE]")
    return lines


PERF_CASES: list[Case] = [
    Case(
        id="perf-stream-large",
        description=f"Perf: streaming chat, {_PERF_CHUNK_COUNT} chunks x {_PERF_CHUNK_BYTES}B payload.",
        client_protocol="openai",
        request=_openai_request(stream=True),
        providers=_one_provider("perf-stream-large"),
        upstream_sse=_perf_sse(),
    ),
    Case(
        id="perf-json-large",
        description=f"Perf: non-stream chat with {_PERF_JSON_BYTES}B completion body.",
        client_protocol="openai",
        request=_openai_request(),
        providers=_one_provider("perf-json-large"),
        upstream_json={
            "id": "perf", "object": "chat.completion", "created": 1, "model": "m-golden",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "x" * _PERF_JSON_BYTES}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 3, "completion_tokens": _PERF_JSON_BYTES // 4, "total_tokens": _PERF_JSON_BYTES // 4 + 3},
        },
    ),
]


def auth_headers(case: Case) -> dict:
    """Headers the capture/replay client sends to the gateway.

    dev-open 已移除(数据面强制密钥):无显式认证头时恒发 DEFAULT_CLIENT_KEY
    —— seed_config 对空/未指定 api_keys 也种入同一密钥,故注册表非空且命中。
    显式带头的用例(如 auth-failed 的 wrong-key)保持原样以触发 401。
    """
    headers = dict(case.client_headers)
    if "Authorization" not in headers and "x-api-key" not in headers:
        headers["Authorization"] = f"Bearer {DEFAULT_CLIENT_KEY}"
    return headers
