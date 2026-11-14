"""真实 SDK 冒烟(P5-4 / C14)。

启动本地 wr-cli + 回环 mock 上游,分别用 OpenAI / Anthropic Python SDK
走四条链路:同协议/跨协议 × 非流/流,验证客户端解析、工具调用与终态。

依赖:``pip install openai anthropic``;先 ``cargo build --target-dir
target/replay -p wr-cli``(与 golden 工具链共用二进制)。
"""

import json
import os
import sys
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

os.environ["NO_PROXY"] = "127.0.0.1,localhost"
for key in ("HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"):
    os.environ.pop(key, None)
sys.path.insert(0, str(Path(__file__).resolve().parent))

import anthropic
import openai
import replay_golden as rg
from seed_config import seed_config


class Mock(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def _body(self):
        length = int(self.headers.get("content-length", "0"))
        return json.loads(self.rfile.read(length) or b"{}")

    def _json(self, obj):
        data = json.dumps(obj).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def _sse(self, chunks):
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("connection", "close")
        self.end_headers()
        for chunk in chunks:
            self.wfile.write(chunk.encode())
            self.wfile.flush()

    def do_POST(self):
        body = self._body()
        if self.path.endswith("/v1/chat/completions"):
            if body.get("stream"):
                self._sse(
                    [
                        'data: {"id":"chatcmpl_sdk","object":"chat.completion.chunk","created":1,"model":"m-test","choices":[{"index":0,"delta":{"role":"assistant","content":"hello sdk"},"finish_reason":null}]}\n\n',
                        'data: {"id":"chatcmpl_sdk","object":"chat.completion.chunk","created":1,"model":"m-test","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_sdk","type":"function","function":{"name":"lookup","arguments":""}}]},"finish_reason":null}]}\n\n',
                        'data: {"id":"chatcmpl_sdk","object":"chat.completion.chunk","created":1,"model":"m-test","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\\"q\\":\\"x\\"}"}}]},"finish_reason":null}]}\n\n',
                        'data: {"id":"chatcmpl_sdk","object":"chat.completion.chunk","created":1,"model":"m-test","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}\n\n',
                        "data: [DONE]\n\n",
                    ]
                )
            else:
                self._json(
                    {
                        "id": "chatcmpl_sdk",
                        "object": "chat.completion",
                        "created": 1,
                        "model": "m-test",
                        "choices": [
                            {
                                "index": 0,
                                "message": {
                                    "role": "assistant",
                                    "content": "hello sdk",
                                    "tool_calls": [
                                        {
                                            "id": "call_sdk",
                                            "type": "function",
                                            "function": {
                                                "name": "lookup",
                                                "arguments": '{"q":"x"}',
                                            },
                                        }
                                    ],
                                },
                                "finish_reason": "tool_calls",
                            }
                        ],
                        "usage": {
                            "prompt_tokens": 2,
                            "completion_tokens": 3,
                            "total_tokens": 5,
                        },
                    }
                )
        elif self.path.endswith("/v1/messages"):
            if body.get("stream"):
                self._sse(
                    [
                        'event: message_start\ndata: {"type":"message_start","message":{"id":"msg_sdk","type":"message","role":"assistant","model":"claude-test","content":[],"stop_reason":null,"usage":{"input_tokens":2,"output_tokens":0}}}\n\n',
                        'event: content_block_start\ndata: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}\n\n',
                        'event: content_block_delta\ndata: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello sdk"}}\n\n',
                        'event: content_block_stop\ndata: {"type":"content_block_stop","index":0}\n\n',
                        'event: content_block_start\ndata: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_sdk","name":"lookup","input":{}}}\n\n',
                        'event: content_block_delta\ndata: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\\"q\\":\\"x\\"}"}}\n\n',
                        'event: content_block_stop\ndata: {"type":"content_block_stop","index":1}\n\n',
                        'event: message_delta\ndata: {"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"input_tokens":2,"output_tokens":3}}\n\n',
                        'event: message_stop\ndata: {"type":"message_stop"}\n\n',
                    ]
                )
            else:
                self._json(
                    {
                        "id": "msg_sdk",
                        "type": "message",
                        "role": "assistant",
                        "model": "claude-test",
                        "content": [
                            {"type": "text", "text": "hello sdk"},
                            {
                                "type": "tool_use",
                                "id": "toolu_sdk",
                                "name": "lookup",
                                "input": {"q": "x"},
                            },
                        ],
                        "stop_reason": "tool_use",
                        "usage": {"input_tokens": 2, "output_tokens": 3},
                    }
                )
        else:
            self.send_response(404)
            self.send_header("content-length", "0")
            self.end_headers()


def check(condition, message):
    if not condition:
        raise AssertionError(message)


def openai_stream(client, model):
    content = ""
    tools = {}
    finish = None
    for chunk in client.chat.completions.create(
        model=model,
        messages=[{"role": "user", "content": "hi"}],
        stream=True,
    ):
        if not chunk.choices:
            continue
        choice = chunk.choices[0]
        if choice.delta.content:
            content += choice.delta.content
        for call in choice.delta.tool_calls or []:
            entry = tools.setdefault(call.index, {"name": "", "args": ""})
            if call.function and call.function.name:
                entry["name"] = call.function.name
            if call.function and call.function.arguments:
                entry["args"] += call.function.arguments
        if choice.finish_reason:
            finish = choice.finish_reason
    return content, tools, finish


def anthropic_stream(client, model):
    content = ""
    tools = {}
    stop = None
    with client.messages.stream(
        model=model,
        max_tokens=64,
        messages=[{"role": "user", "content": "hi"}],
    ) as stream:
        for event in stream:
            if event.type == "content_block_delta":
                text = getattr(event.delta, "text", None)
                partial = getattr(event.delta, "partial_json", None)
                if text:
                    content += text
                if partial:
                    tools[event.index] = tools.get(event.index, "") + partial
            if event.type == "message_delta":
                stop = event.delta.stop_reason
    return content, tools, stop


mock = ThreadingHTTPServer(("127.0.0.1", 0), Mock)
threading.Thread(target=mock.serve_forever, daemon=True).start()
mock_url = f"http://127.0.0.1:{mock.server_port}"

rg.resolve_ports(None, None, None)
tmp = Path(tempfile.mkdtemp(prefix="wr-sdk-smoke-"))
providers = [
    {
        "id": "prov-openai",
        "name": "prov-openai",
        "api_base_url": mock_url,
        "protocol": "openai",
        "models": ["m-test"],
        "max_retries": 0,
        "enabled": True,
    },
    {
        "id": "prov-anthropic",
        "name": "prov-anthropic",
        "api_base_url": mock_url,
        "protocol": "anthropic",
        "models": ["claude-test"],
        "max_retries": 0,
        "enabled": True,
    },
]
seed_config(tmp, providers, [{"key": "sk-golden-1", "enabled": True, "name": "sdk"}])
gateway = rg.RustGatewayProc(
    rg.DEFAULT_COMMAND, tmp, tmp / "ready.flag", env=rg.gateway_env(tmp)
)
try:
    oa = openai.OpenAI(
        api_key="sk-golden-1",
        base_url=f"http://127.0.0.1:{rg.GATEWAY_PORT}/v1",
        timeout=15,
        max_retries=0,
    )
    an = anthropic.Anthropic(
        api_key="sk-golden-1",
        base_url=f"http://127.0.0.1:{rg.GATEWAY_PORT}",
        timeout=15,
        max_retries=0,
    )

    response = oa.chat.completions.create(
        model="prov-openai/m-test", messages=[{"role": "user", "content": "hi"}]
    )
    check(response.choices[0].message.content == "hello sdk", "openai native nonstream text")
    check(
        response.choices[0].message.tool_calls[0].function.name == "lookup",
        "openai native nonstream tool",
    )

    content, tools, finish = openai_stream(oa, "prov-openai/m-test")
    check(
        content == "hello sdk"
        and tools[0]["name"] == "lookup"
        and "q" in tools[0]["args"]
        and finish == "tool_calls",
        f"openai native stream {content} {tools} {finish}",
    )

    response = oa.chat.completions.create(
        model="prov-anthropic/claude-test",
        messages=[{"role": "user", "content": "hi"}],
    )
    check(response.choices[0].message.content == "hello sdk", "openai->anthropic nonstream text")
    check(
        response.choices[0].message.tool_calls[0].function.name == "lookup",
        "openai->anthropic nonstream tool",
    )

    content, tools, finish = openai_stream(oa, "prov-anthropic/claude-test")
    check(
        content == "hello sdk"
        and tools[0]["name"] == "lookup"
        and "q" in tools[0]["args"]
        and finish == "tool_calls",
        f"openai->anthropic stream {content} {tools} {finish}",
    )

    response = an.messages.create(
        model="prov-anthropic/claude-test",
        max_tokens=64,
        messages=[{"role": "user", "content": "hi"}],
    )
    check(response.content[0].text == "hello sdk", "anthropic native nonstream text")
    check(
        any(block.type == "tool_use" and block.name == "lookup" for block in response.content),
        "anthropic native nonstream tool",
    )

    content, tools, stop = anthropic_stream(an, "prov-anthropic/claude-test")
    check(
        content == "hello sdk"
        and "q" in list(tools.values())[0]
        and stop == "tool_use",
        f"anthropic native stream {content} {tools} {stop}",
    )

    response = an.messages.create(
        model="prov-openai/m-test",
        max_tokens=64,
        messages=[{"role": "user", "content": "hi"}],
    )
    check(response.content[0].text == "hello sdk", "anthropic->openai nonstream text")
    check(
        any(block.type == "tool_use" and block.name == "lookup" for block in response.content),
        "anthropic->openai nonstream tool",
    )

    content, tools, stop = anthropic_stream(an, "prov-openai/m-test")
    check(
        content == "hello sdk"
        and "q" in list(tools.values())[0]
        and stop == "tool_use",
        f"anthropic->openai stream {content} {tools} {stop}",
    )

    print(
        "SDK_SMOKE_OK openai/anthropic x native/cross protocol x nonstream/stream"
    )
finally:
    gateway.stop()
    mock.shutdown()
