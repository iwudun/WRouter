"""Replay golden cases against the Rust gateway (20261016_02).

样本即真相:``testdata/golden`` 的样本就是现行 Rust 行为的防回归基线,
直接回放,无需任何前置改写。一条命令完成 构建 → mock 上游 → 逐 case
重放 → 逐字段 diff。JSON bodies are deep-diffed after dropping volatile
keys; SSE streams are compared event-by-event after dropping volatile keys
inside each ``data:`` payload (chunk boundaries and ordering are exact).

Usage::

    python scripts/verify/replay_golden.py                  # 全量 22 case
    python scripts/verify/replay_golden.py --only openai-chat-basic
    python scripts/verify/replay_golden.py --skip-build     # 跳过 cargo build

Exit code 0 iff every case matches. Report lands in
testdata/golden/parity-report.json (gitignored runtime data).
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))

import httpx

from cases import (
    ALL_CASES,
    GOLDEN_DIR,
    MOCK_UPSTREAM_PORT,
    REPO_ROOT,
    auth_headers,
    GLOBAL_VOLATILE_HEADERS,
    GLOBAL_VOLATILE_JSON_KEYS,
)
from seed_config import seed_config

# 网关监听端口 —— 默认可用 `--gateway-port/--web-port/--proxy-port` 覆盖。
#
# ⚠️ 为什么要覆盖:回放会真的起一个网关进程。本机若已有 WRouter 在跑
# (桌面壳 debug 版就占 3460/3462),回放**不会**报「端口被占用」,而是把请求
# 发到**别人的实例**上 —— 表现为满屏 401 之类莫名其妙的 diff,极难定位。
# 所以:① 端口可覆盖;② 启动前预检占用,占用即**快速失败**并说清原因。
GATEWAY_PORT = 3460
WEB_PORT = 3462
PROXY_PORT = 3463

DEFAULT_COMMAND = (
    f"backend/target/replay/debug/wr{'.exe' if os.name == 'nt' else ''} "
    "serve --data-dir {data_dir} --ready-file {ready_file}"
)

# --- 上游字节取证(B-5)---------------------------------------------------
# mock 上游把「网关发给厂商的原始字节」按 case 记录下来,这里负责取回与比对。
UPSTREAM_LOG_PATH = "/__upstream_log"

# 断网用:指向 discard 端口,任何走环境代理的出站请求都会被立刻拒掉。
# ⚠️ 网关的直连分支显式调用 `.no_proxy()`(upstream.rs),**不读环境代理**,
# 所以真正的「不准碰网」保证来自 `_assert_loopback_upstreams()`(所有 case 的上游
# base_url 必须是回环地址)+ 上游字节清单本身(外发请求会让清单与 golden 不符)。
BLOCKED_PROXY = "http://127.0.0.1:9"
LOOPBACK_HOSTS = ("127.0.0.1", "localhost", "::1")


def build_gateway() -> None:
    print(f"[build] cargo build --target-dir target/replay -p wr-cli (cwd={REPO_ROOT / 'backend'})")
    proc = subprocess.run(
        ["cargo", "build", "--target-dir", "target/replay", "-p", "wr-cli"],
        cwd=REPO_ROOT / "backend",
    )
    if proc.returncode != 0:
        sys.exit(f"cargo build failed with exit code {proc.returncode}")


def _mock_alive() -> bool:
    try:
        return httpx.get(f"http://127.0.0.1:{MOCK_UPSTREAM_PORT}/healthz", timeout=1.0).status_code == 200
    except Exception:
        return False


class MockUpstream:
    """探活复用已运行的 mock_upstream(3999),否则 spawn 并在结束时清理。"""

    def __init__(self) -> None:
        self.proc: subprocess.Popen | None = None
        if _mock_alive():
            print(f"[mock] reusing running mock_upstream on port {MOCK_UPSTREAM_PORT}")
            return
        self.proc = subprocess.Popen(
            [sys.executable, str(Path(__file__).resolve().parent / "mock_upstream.py")],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.STDOUT,
        )
        deadline = time.monotonic() + 15.0
        while time.monotonic() < deadline:
            if self.proc.poll() is not None:
                raise RuntimeError(f"mock_upstream exited early with code {self.proc.returncode}")
            if _mock_alive():
                print(f"[mock] spawned mock_upstream on port {MOCK_UPSTREAM_PORT}")
                return
            time.sleep(0.2)
        raise TimeoutError("mock upstream did not become ready in time")

    def stop(self) -> None:
        if self.proc is not None and self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.proc.kill()


def reset_upstream_log(case_id: str) -> None:
    """清空 mock 上游里该 case 的请求记录(每个 case 回放前先清)。"""
    httpx.delete(
        f"http://127.0.0.1:{MOCK_UPSTREAM_PORT}{UPSTREAM_LOG_PATH}/{case_id}",
        timeout=5.0,
    )


def fetch_upstream_log(case_id: str) -> list[dict]:
    """取回 mock 上游收到的该 case 的上游请求(按到达顺序)。"""
    resp = httpx.get(
        f"http://127.0.0.1:{MOCK_UPSTREAM_PORT}{UPSTREAM_LOG_PATH}/{case_id}",
        timeout=5.0,
    )
    resp.raise_for_status()
    return list(resp.json().get("requests", []))


def _assert_loopback_upstreams() -> None:
    """硬门禁:任何 golden case 的上游 base_url 必须是回环地址。

    golden 回放会真的起网关打真实 HTTP —— 上游必须是本机 mock,绝不允许指向
    公网(那会让「跑一次回放」变成一次真的外联)。环境变量式断网对网关的直连
    分支无效(直连显式 `.no_proxy()`),所以这道门禁才是真正的保证。
    """
    offenders: list[str] = []
    for case in ALL_CASES:
        for provider in case.providers:
            host = provider.base_url.split("//", 1)[-1].split("/", 1)[0].split(":", 1)[0]
            if host not in LOOPBACK_HOSTS:
                offenders.append(f"{case.id} → {provider.base_url}")
    if offenders:
        sys.exit(
            "golden case 的上游必须是回环地址(禁止外联):\n  " + "\n  ".join(offenders)
        )


def gateway_env(data_dir: Path) -> dict[str, str]:
    """网关子进程的环境:HOME/APPDATA 指向临时目录 + 断网环境变量 + 端口。"""
    appdata = data_dir / "appdata"
    localappdata = data_dir / "localappdata"
    appdata.mkdir(parents=True, exist_ok=True)
    localappdata.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()
    env.update({
        "HOME": str(data_dir),
        "USERPROFILE": str(data_dir),
        "APPDATA": str(appdata),
        "LOCALAPPDATA": str(localappdata),
        "HTTP_PROXY": BLOCKED_PROXY,
        "HTTPS_PROXY": BLOCKED_PROXY,
        "ALL_PROXY": BLOCKED_PROXY,
        "NO_PROXY": ",".join(LOOPBACK_HOSTS),
        "WROUTER_GATEWAY_PORT": str(GATEWAY_PORT),
        "WROUTER_WEB_PORT": str(WEB_PORT),
        "WROUTER_PROXY_PORT": str(PROXY_PORT),
    })
    return env


def apply_port_overrides(gateway_port: int | None, web_port: int | None, proxy_port: int | None) -> None:
    global GATEWAY_PORT, WEB_PORT, PROXY_PORT
    if gateway_port:
        GATEWAY_PORT = gateway_port
    if web_port:
        WEB_PORT = web_port
    if proxy_port:
        PROXY_PORT = proxy_port


def _port_in_use(port: int) -> bool:
    """该端口有进程在监听。"""
    import socket

    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.settimeout(0.5)
        return sock.connect_ex(("127.0.0.1", port)) == 0


def _port_bindable(port: int) -> bool:
    """该端口能被本进程绑定(Windows 的保留端口区间会直接拒绝)。"""
    import socket

    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        try:
            sock.bind(("127.0.0.1", port))
        except OSError:
            return False
    return True


def _port_usable(port: int) -> bool:
    # 两个条件都要查:「有人监听」用 connect 判定;「Windows 保留区间」用 bind 判定。
    # (Windows 的 SO_REUSEADDR 允许与既有监听端口重复绑定,不能只看 bind。)
    return not _port_in_use(port) and _port_bindable(port)


def preflight_ports() -> None:
    """启动前预检端口占用 —— 占用即快速失败(否则会把请求打到别人的实例上)。"""
    busy = [port for port in (GATEWAY_PORT, WEB_PORT, PROXY_PORT) if not _port_usable(port)]
    if busy:
        sys.exit(
            "回放端口不可用: " + ", ".join(str(p) for p in busy)
            + " —— 已被别的进程占用、或落在 Windows 保留端口区间(netsh interface ipv4 show excludedportrange protocol=tcp)。"
            + "用 --gateway-port/--web-port/--proxy-port 换一组空闲端口,"
            + "否则回放会打到别人的实例上(表现为满屏无意义的 diff)。"
        )


def resolve_ports(gateway_port: int | None, web_port: int | None, proxy_port: int | None) -> str:
    """定端口:显式指定的按指定的(不可用即报错);没指定的自动挑一组空闲端口。

    自动挑的原因是实测:本机 3460-3565 整段被 Windows 保留,默认端口根本绑不上;
    而「端口被别人占着」若不预检,回放会静默打到别人的实例上(见 preflight_ports)。
    """
    global GATEWAY_PORT, WEB_PORT, PROXY_PORT
    if gateway_port or web_port or proxy_port:
        apply_port_overrides(gateway_port, web_port, proxy_port)
        preflight_ports()
        return ""
    default = (3460, 3462, 3463)
    if all(_port_usable(port) for port in default):
        GATEWAY_PORT, WEB_PORT, PROXY_PORT = default
        return ""
    for base in range(3990, 4990, 3):
        candidate = (base, base + 1, base + 2)
        if MOCK_UPSTREAM_PORT in candidate:
            continue  # 3999 是 mock 上游的端口,不能占用
        if all(_port_usable(port) for port in candidate):
            GATEWAY_PORT, WEB_PORT, PROXY_PORT = candidate
            return (
                f"默认端口 {default} 不可用(被占用或在 Windows 保留区间),"
                f"自动改用 {candidate}"
            )
    sys.exit("找不到一组三个都空闲的端口(试了 3990-4990);用 --gateway-port 手动指定")


class RustGatewayProc:
    """wr.exe process; {data_dir}/{ready_file} placeholders in the command."""

    def __init__(self, command: str, data_dir: Path, ready_file: Path, env: dict[str, str] | None = None) -> None:
        self.data_dir = data_dir
        cmd = [part.format(data_dir=str(data_dir), ready_file=str(ready_file)) for part in shlex.split(command)]
        self.proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT, env=env)
        self._wait_ready()

    def _wait_ready(self, timeout_s: float = 30.0) -> None:
        deadline = time.monotonic() + timeout_s
        while time.monotonic() < deadline:
            if self.proc.poll() is not None:
                raise RuntimeError(f"rust gateway exited early with code {self.proc.returncode}")
            try:
                if httpx.get(f"http://127.0.0.1:{GATEWAY_PORT}/health", timeout=1.0).status_code == 200:
                    return
            except Exception:
                time.sleep(0.2)
        raise TimeoutError("rust gateway did not become ready in time")

    def stop(self) -> None:
        if self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait(timeout=5)


def _strip_volatile(node: Any, volatile: set[str]) -> Any:
    if isinstance(node, dict):
        return {k: _strip_volatile(v, volatile) for k, v in node.items() if k not in volatile}
    if isinstance(node, list):
        return [_strip_volatile(item, volatile) for item in node]
    return node


def _diff(golden: Any, live: Any, path: str, out: list[str]) -> None:
    """方向永远以 golden 为基准:live 缺/多/值不同,分别报告。"""
    if type(golden) is not type(live):
        out.append(f"{path}: type golden={type(golden).__name__} live={type(live).__name__} ({golden!r} vs {live!r})")
        return
    if isinstance(golden, dict):
        for key in sorted(set(golden) | set(live)):
            if key not in live:
                out.append(f"live.missing: {path}.{key} (golden={golden[key]!r})")
            elif key not in golden:
                out.append(f"live.extra: {path}.{key} {live[key]!r}")
            else:
                _diff(golden[key], live[key], f"{path}.{key}", out)
    elif isinstance(golden, list):
        if len(golden) != len(live):
            out.append(f"{path}: length live={len(live)} != golden={len(golden)}")
        for i, (g, l) in enumerate(zip(golden, live)):
            _diff(g, l, f"{path}[{i}]", out)
    elif golden != live:
        out.append(f"value: {path} {live!r} != golden {golden!r}")


def _sse_events(text: str) -> list[str]:
    """Golden SSE is stored as a raw line sequence (one stored line = one
    ``event:``/``data:`` line in arrival order), so the diff is line-based —
    no reliance on blank-line event framing. Blank lines (e.g. the trailing
    newline of an empty stream) carry no meaning and are dropped."""
    return [line for line in text.splitlines() if line.strip()]


def compare_upstream(golden_dir: Path, case_id: str, live: list[dict]) -> list[str]:
    """比对「网关发给厂商的字节」与 golden。

    三层断言:
      ① golden 文件自身完整性 —— ``upstream_request.sha256`` 必须等于
         ``upstream_request.json`` 的 sha256(防止有人手改样本却不更新校验);
      ② 请求条数一致(顺序 = 到达顺序,回退链的 -a/-b 上游按序可比);
      ③ 每条请求的方法 / 路径 / 语义头 / **body 逐字节** + body sha256 一致。

    body 用「原始字节的 UTF-8 文本」保存(协议请求体是 JSON,单行紧凑);
    非 UTF-8 时退化为 base64,比对仍走字节。
    """
    diffs: list[str] = []
    golden_path = golden_dir / "upstream_request.json"
    sha_path = golden_dir / "upstream_request.sha256"
    if not golden_path.exists():
        return [f"upstream: golden file missing ({golden_path.name}) —— 用 _capture_cases.py 重采"]
    raw = golden_path.read_bytes()
    if not sha_path.exists():
        diffs.append(f"upstream: sha256 file missing ({sha_path.name})")
    else:
        recorded = sha_path.read_text(encoding="utf-8").split()[0].strip()
        actual = hashlib.sha256(raw).hexdigest()
        if recorded != actual:
            diffs.append(f"upstream.sha256: file={actual} != recorded={recorded}")

    golden = json.loads(raw.decode("utf-8"))
    golden_reqs = list(golden.get("requests", []))
    if len(golden_reqs) != len(live):
        diffs.append(f"upstream: request count live={len(live)} != golden={len(golden_reqs)}")
    for i, (g, l) in enumerate(zip(golden_reqs, live)):
        for key in ("method", "upstream", "path", "content_type"):
            if g.get(key) != l.get(key):
                diffs.append(f"upstream[{i}].{key}: live={l.get(key)!r} != golden={g.get(key)!r}")
        if g.get("headers", {}) != l.get("headers", {}):
            diffs.append(f"upstream[{i}].headers: live={l.get('headers')!r} != golden={g.get('headers')!r}")
        if "body_base64" in g or "body_base64" in l:
            if g.get("body_base64") != l.get("body_base64"):
                diffs.append(f"upstream[{i}].body(base64): live 与 golden 字节不同")
        elif (g.get("body") or "") != (l.get("body") or ""):
            diffs.append(f"upstream[{i}].body: live={l.get('body')!r} != golden={g.get('body')!r}")
        if g.get("body_sha256") != l.get("body_sha256"):
            diffs.append(
                f"upstream[{i}].body_sha256: live={l.get('body_sha256')!r} != golden={g.get('body_sha256')!r}"
            )
    return diffs


def compare(case_meta: dict, golden_dir: Path, live: dict) -> list[str]:
    diffs: list[str] = []
    if live["status"] != case_meta["captured_status"]:
        diffs.append(f"status: live={live['status']} != golden={case_meta['captured_status']}")
        return diffs
    if live["kind"] != case_meta["response_kind"]:
        diffs.append(f"response_kind: live={live['kind']} != golden={case_meta['response_kind']}")
        return diffs

    volatile = set(GLOBAL_VOLATILE_JSON_KEYS) | set(case_meta.get("extra_volatile_keys") or [])
    if live["kind"] == "json":
        golden = json.loads((golden_dir / "response.json").read_text(encoding="utf-8"))
        _diff(_strip_volatile(golden, volatile), _strip_volatile(live["json"], volatile), "body", diffs)
    else:
        golden_lines = _sse_events((golden_dir / "response.sse.txt").read_text(encoding="utf-8"))
        live_lines = [line for line in live["events"] if line.strip()]
        if len(golden_lines) != len(live_lines):
            diffs.append(f"sse: line count live={len(live_lines)} != golden={len(golden_lines)}")
        for i, (golden_line, live_line) in enumerate(zip(golden_lines, live_lines)):
            if golden_line.startswith("data:") and live_line.startswith("data:"):
                golden_payload, live_payload = golden_line[5:].strip(), live_line[5:].strip()
                try:
                    _diff(
                        _strip_volatile(json.loads(golden_payload), volatile),
                        _strip_volatile(json.loads(live_payload), volatile),
                        f"sse[{i}].data",
                        diffs,
                    )
                except json.JSONDecodeError:
                    if golden_payload != live_payload:
                        diffs.append(f"sse[{i}].data(raw): live={live_payload!r} != golden={golden_payload!r}")
            elif golden_line != live_line:
                diffs.append(f"sse[{i}]: live={live_line!r} != golden={golden_line!r}")
    return diffs


def _request(case_golden_dir: Path, case) -> dict:
    req = json.loads((case_golden_dir / "request.json").read_text(encoding="utf-8"))
    timeout = httpx.Timeout(90.0 if case.upstream_delay_s else 60.0)
    headers = auth_headers(case)
    if req["path"].endswith("streamGenerateContent") or case.stream:
        chunks: list[str] = []
        with httpx.stream("POST", f"http://127.0.0.1:{GATEWAY_PORT}{req['path']}", json=req["body"], headers=headers, timeout=timeout) as resp:
            status = resp.status_code
            for line in resp.iter_lines():
                if line:
                    chunks.append(line)
        return {"kind": "sse", "status": status, "events": chunks}
    resp = httpx.post(f"http://127.0.0.1:{GATEWAY_PORT}{req['path']}", json=req["body"], headers=headers, timeout=timeout)
    try:
        parsed: Any = resp.json()
    except Exception:
        parsed = resp.text
    return {"kind": "json", "status": resp.status_code, "json": parsed}


def main() -> None:
    parser = argparse.ArgumentParser(description="Build → mock → replay golden cases → diff.")
    parser.add_argument("--command", default=DEFAULT_COMMAND, help="Rust binary command line; {data_dir}/{ready_file} placeholders")
    parser.add_argument("--golden", default=str(GOLDEN_DIR))
    parser.add_argument("--out", default=str(GOLDEN_DIR / "parity-report.json"))
    parser.add_argument("--only", default=None, metavar="CASE_ID", help="replay a single case")
    parser.add_argument("--skip-build", action="store_true", help="skip `cargo build` (纯调试;默认强制新鲜构建)")
    parser.add_argument("--gateway-port", type=int, default=None, help="网关端口(默认 3460;被占用时换一组)")
    parser.add_argument("--web-port", type=int, default=None, help="管理面端口(默认 3462)")
    parser.add_argument("--proxy-port", type=int, default=None, help="代理端口(默认 3463)")
    args = parser.parse_args()

    port_note = resolve_ports(args.gateway_port, args.web_port, args.proxy_port)
    if port_note:
        print(f"[ports] {port_note}")
    print(f"[ports] gateway={GATEWAY_PORT} web={WEB_PORT} proxy={PROXY_PORT}")

    if not args.skip_build:
        build_gateway()
    elif not Path(shlex.split(args.command)[0].format(data_dir="", ready_file="")).exists():
        sys.exit(f"--skip-build: binary not found at {shlex.split(args.command)[0]!r}; drop --skip-build or run cargo build first")

    mock = MockUpstream()
    try:
        _assert_loopback_upstreams()
        golden_root = Path(args.golden)
        manifest = json.loads((golden_root / "manifest.json").read_text(encoding="utf-8"))
        cases_meta = manifest["cases"]
        if args.only:
            cases_meta = [meta for meta in cases_meta if meta.get("case_id") == args.only]
            if not cases_meta:
                known = [meta.get("case_id") for meta in manifest["cases"]]
                sys.exit(f"unknown case {args.only!r}; known cases: {known}")
        results: dict[str, dict] = {}

        for meta in cases_meta:
            case_id = meta["case_id"]
            if "error" in meta:
                results[case_id] = {"match": False, "diffs": ["golden capture failed; re-capture via _capture_cases.py"]}
                continue
            case_golden_dir = golden_root / "cases" / case_id
            case = next(c for c in ALL_CASES if c.id == case_id)
            tmp = Path(tempfile.mkdtemp(prefix=f"wr-replay-{case_id}-"))
            gateway: Any = None
            try:
                seed_config(tmp, [p.to_config() for p in case.providers], list(case.api_keys or []), case.router_fallback)
                gateway = RustGatewayProc(args.command, tmp, tmp / "ready.flag", env=gateway_env(tmp))
                reset_upstream_log(case_id)
                live = _request(case_golden_dir, case)
                diffs = compare(meta, case_golden_dir, live)
                diffs += compare_upstream(case_golden_dir, case_id, fetch_upstream_log(case_id))
            except Exception as exc:
                diffs = [f"replay error: {exc!r}"]
            finally:
                if gateway is not None:
                    gateway.stop()
                shutil.rmtree(tmp, ignore_errors=True)
            results[case_id] = {"match": not diffs, "diffs": diffs}
            print(f"[{'MATCH' if not diffs else 'DIFF ':5}] {case_id}: " + ("" if not diffs else diffs[0] + (f" (+{len(diffs)-1} more)" if len(diffs) > 1 else "")))

        report = {
            "tool": "replay_golden",
            "run_at": time.strftime("%Y-%m-%dT%H:%M:%S"),
            "cases": results,
            "match_count": sum(1 for r in results.values() if r["match"]),
            "total": len(results),
        }
        Path(args.out).write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
        print(f"\n{report['match_count']}/{report['total']} cases match → {args.out}")
        sys.exit(0 if report["match_count"] == report["total"] else 1)
    finally:
        mock.stop()


if __name__ == "__main__":
    main()
