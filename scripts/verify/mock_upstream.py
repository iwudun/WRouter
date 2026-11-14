"""Stateless mock LLM upstream for golden capture / replay (20261016_02).

Route convention: the provider base_url carries the case id
(``http://127.0.0.1:3999/<case-id>[-suffix]``), so this server needs no
request matching beyond a case lookup. Response behaviour (status / JSON /
SSE line list / delay / truncated stream) comes from the Case catalogue in
``cases.py``. Run standalone::

    python scripts/verify/mock_upstream.py [--port 3999]

**上游请求记录(2026-11-05 B-5)**:本服务同时是「网关发给厂商的字节」的取证点 ——
每次收到的上游请求按**原始字节**记录(方法 / 路径 / 语义头子集 / body 原文 + sha256),
按 case id 归组,经 ``GET /__upstream_log/<case_id>`` 暴露给回放脚本比对。
没有这层记录时,协议转换的错误在客户端侧往往看不出来(客户端只看得到被改写后的响应)。
"""

from __future__ import annotations

import argparse
import asyncio
import base64
import hashlib
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import json
import uvicorn
from fastapi import FastAPI, Request
from fastapi.responses import JSONResponse, Response, StreamingResponse

from cases import ALL_CASES, Case

app = FastAPI(title="WRouter golden mock upstream")

# 上游请求记录:case_id -> 请求条目列表(按到达顺序)。
# 同一 case 的多个上游(base_url 带 -a/-b 后缀)按序记录,回放时逐条比对。
_UPSTREAM_LOG: dict[str, list[dict]] = {}

# 只记录**语义头**(不含凭据、request id、UA 等易变或敏感字段)。
# anthropic-version 是 Anthropic 上游的硬性要求,mock 不校验头 → 只能靠这里钉住。
_SEMANTIC_HEADERS = ("anthropic-version", "accept")


def _record(case_id: str, upstream: str, request: Request, raw: bytes) -> None:
    entry: dict = {
        "method": request.method,
        "upstream": upstream,
        "path": request.url.path,
        "content_type": request.headers.get("content-type"),
        "headers": {name: request.headers[name] for name in _SEMANTIC_HEADERS if name in request.headers},
        "body_bytes": len(raw),
        "body_sha256": hashlib.sha256(raw).hexdigest(),
    }
    try:
        entry["body"] = raw.decode("utf-8")
    except UnicodeDecodeError:
        # 非 UTF-8 body(理论上不该出现)→ 存 base64,比对走字节流
        entry["body_base64"] = base64.b64encode(raw).decode("ascii")
    _UPSTREAM_LOG.setdefault(case_id, []).append(entry)


def _case_for(prefix: str) -> Case:
    """按路径前缀找 case —— **必须取最长匹配**。

    case id 之间存在前缀包含关系(``anthropic-client-openai-upstream`` 是
    ``anthropic-client-openai-upstream-stream`` 的前缀)。若按声明顺序取第一个命中,
    流式 case 会被短 id 的非流式 case 抢答 —— mock 返回 JSON 而非 SSE,
    回放表现为「SSE 行数对不上」,看起来像网关行为漂移,其实是 mock 路由错。
    """
    best: Case | None = None
    for case in ALL_CASES:
        if prefix == case.id or prefix.startswith(case.id + "-"):
            if best is None or len(case.id) > len(best.id):
                best = case
    if best is None:
        raise KeyError(f"no case for mock path prefix {prefix!r}")
    return best


@app.get("/healthz")
async def healthz() -> dict:
    return {"ok": True}


@app.get("/__upstream_log/{case_id}")
async def upstream_log(case_id: str) -> dict:
    """回放/采集脚本取该 case 收到的上游请求(按到达顺序)。"""
    return {"case_id": case_id, "requests": _UPSTREAM_LOG.get(case_id, [])}


@app.delete("/__upstream_log/{case_id}")
async def upstream_log_reset(case_id: str) -> dict:
    """清空该 case 的记录 —— 每次回放/采集一个 case 前先调用,避免跨轮次串味。"""
    _UPSTREAM_LOG.pop(case_id, None)
    return {"case_id": case_id, "requests": []}


def _status_for(case: Case, prefix: str) -> int:
    """该上游的状态码 —— 候选链里不同上游可以有不同的状态(见 upstream_status_suffix)。"""
    for suffix, status in case.upstream_status_suffix.items():
        if prefix.endswith(suffix):
            return status
    return case.upstream_status


@app.post("/{prefix}/{rest:path}")
async def upstream(prefix: str, rest: str, request: Request) -> Response:
    case = _case_for(prefix)
    raw = await request.body()
    _record(case.id, prefix, request, raw)
    body = _safe_json(raw)
    status = _status_for(case, prefix)
    if case.upstream_delay_s:
        await asyncio.sleep(case.upstream_delay_s)

    if case.upstream_reflect is not None:
        picked = {key: body.get(key) for key in case.upstream_reflect}
        text = json.dumps({"reflected": picked}, sort_keys=True, ensure_ascii=False)
        protocol = case.providers[0].protocol
        if protocol == "anthropic":
            return JSONResponse({
                "id": "up-reflect", "type": "message", "role": "assistant",
                "model": body.get("model", "m-golden"),
                "content": [{"type": "text", "text": text}],
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 10, "output_tokens": 5},
            })
        if protocol == "gemini":
            return JSONResponse({
                "candidates": [
                    {"content": {"parts": [{"text": text}], "role": "model"}, "finishReason": "STOP"},
                ],
                "usageMetadata": {"promptTokenCount": 10, "candidatesTokenCount": 5, "totalTokenCount": 15},
                "modelVersion": body.get("model", "m-golden"),
            })
        return JSONResponse({
            "id": "up-reflect", "object": "chat.completion", "created": 0,
            "model": body.get("model", "m-golden"),
            "choices": [{"index": 0, "message": {"role": "assistant", "content": text}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15},
        })

    if status != 200 or case.upstream_json is not None:
        return JSONResponse(case.upstream_json or {"error": {"message": "mock error"}}, status_code=status)

    body_is_stream = bool(body.get("stream")) or ":streamGenerateContent" in rest
    if case.upstream_sse is not None and body_is_stream:
        lines = case.upstream_sse
        if case.upstream_truncate_after_lines is not None:
            lines = lines[: case.upstream_truncate_after_lines]

        async def sse() -> StreamingResponse:
            for line in lines:
                yield line + "\n\n"
                await asyncio.sleep(0)

        return StreamingResponse(sse(), media_type="text/event-stream")

    # SSE-shaped case but client asked non-stream (or vice versa): answer with
    # a generic JSON completion so the gateway sees a well-formed upstream.
    return JSONResponse(
        {"id": "mock-fallback", "object": "chat.completion", "created": 0, "model": "m-golden",
         "choices": [{"index": 0, "message": {"role": "assistant", "content": "mock"}, "finish_reason": "stop"}]},
    )


def _safe_json(raw: bytes) -> dict:
    try:
        parsed = json.loads(raw.decode("utf-8"))
    except Exception:
        return {}
    return parsed if isinstance(parsed, dict) else {}


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=3999)
    args = parser.parse_args()
    uvicorn.run(app, host="127.0.0.1", port=args.port, log_level="warning")


if __name__ == "__main__":
    main()
