"""Capture golden snapshots for specific case ids (minimal helper).

Reuses replay_golden's gateway plumbing to (re)capture individual cases into
testdata/golden; 请求构造走 cases.explicit_request(显式 selector 契约)。
Usage::

    python _capture_cases.py [--command "<wr.exe serve ...>"] case_id [case_id ...]

--command 缺省时用 replay_golden.DEFAULT_COMMAND(target/replay 构建,需先 cargo build)。

**上游字节样本(B-5)**:本脚本同时采集「网关发给厂商的原始字节」——
每次采集会写下 ``<case>/upstream_request.json``(方法/路径/语义头/body 原文+sha256)
与 ``<case>/upstream_request.sha256``(样本文件自身的 sha256)。
mock 上游由本脚本拉起并在结束时回收(采集前会清空该 case 的记录)。
"""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import replay_golden as rg
from cases import ALL_CASES, GOLDEN_DIR, explicit_request
from seed_config import seed_config


def _write_upstream_sample(case_dir: Path, case_id: str, spec_ref: str, requests: list[dict], note: str = "") -> None:
    """写下上游字节样本 + 样本自身的 sha256 校验文件。"""
    sample = {
        "case_id": case_id,
        "spec_ref": spec_ref,
        "captured_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S"),
        "requests": requests,
    }
    if note:
        sample["note"] = note
    payload = (json.dumps(sample, ensure_ascii=False, indent=2) + "\n").encode("utf-8")
    (case_dir / "upstream_request.json").write_bytes(payload)
    digest = hashlib.sha256(payload).hexdigest()
    (case_dir / "upstream_request.sha256").write_text(
        f"{digest}  upstream_request.json\n", encoding="utf-8"
    )


def capture_one(case_id: str, command: str) -> dict:
    case = next(c for c in ALL_CASES if c.id == case_id)
    case_dir = GOLDEN_DIR / "cases" / case_id
    case_dir.mkdir(parents=True, exist_ok=True)

    request = explicit_request(case)
    (case_dir / "request.json").write_text(
        json.dumps(request, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )

    tmp = Path(tempfile.mkdtemp(prefix=f"wr-capture-{case_id}-"))
    gateway = None
    try:
        seed_config(tmp, [p.to_config() for p in case.providers], list(case.api_keys or []), case.router_fallback)
        gateway = rg.RustGatewayProc(command, tmp, tmp / "ready.flag", env=rg.gateway_env(tmp))
        rg.reset_upstream_log(case_id)
        live = rg._request(case_dir, case)
        upstream_requests = rg.fetch_upstream_log(case_id)
    finally:
        if gateway is not None:
            gateway.stop()
        shutil.rmtree(tmp, ignore_errors=True)

    meta = {
        "case_id": case.id,
        "description": case.description,
        "spec_ref": case.spec_ref,
        "client_protocol": case.client_protocol,
        "stream": case.stream,
        "captured_status": live["status"],
        "captured_content_type": "text/event-stream" if live["kind"] == "sse" else "application/json",
        "response_kind": live["kind"],
        "event_count": len(live.get("events", [])) if live["kind"] == "sse" else None,
        "upstream_request_count": len(upstream_requests),
        "extra_volatile_keys": list(case.extra_volatile_keys),
        "captured_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S"),
    }
    if live["kind"] == "json":
        (case_dir / "response.json").write_text(
            json.dumps(live["json"], ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
        )
    else:
        (case_dir / "response.sse.txt").write_text(
            "\n".join(live["events"]) + "\n", encoding="utf-8"
        )
    _write_upstream_sample(case_dir, case.id, case.spec_ref, upstream_requests, case.note)
    print(
        f"[captured] {case_id}: status={live['status']} kind={live['kind']} "
        f"upstream_requests={len(upstream_requests)}"
    )
    return meta


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--command", default=rg.DEFAULT_COMMAND, help="缺省用 target/replay 构建(先 cargo build)")
    parser.add_argument("--gateway-port", type=int, default=None, help="网关端口(默认 3460;被占用时换一组)")
    parser.add_argument("--web-port", type=int, default=None, help="管理面端口(默认 3462)")
    parser.add_argument("--proxy-port", type=int, default=None, help="代理端口(默认 3463)")
    parser.add_argument("case_ids", nargs="+")
    args = parser.parse_args()

    port_note = rg.resolve_ports(args.gateway_port, args.web_port, args.proxy_port)
    if port_note:
        print(f"[ports] {port_note}")
    print(f"[ports] gateway={rg.GATEWAY_PORT} web={rg.WEB_PORT} proxy={rg.PROXY_PORT}")

    manifest_path = GOLDEN_DIR / "manifest.json"
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    known = {meta.get("case_id") for meta in manifest["cases"]}

    mock = rg.MockUpstream()
    try:
        fresh: dict[str, dict] = {}
        for case_id in args.case_ids:
            fresh[case_id] = capture_one(case_id, args.command)
    finally:
        mock.stop()

    kept = [meta for meta in manifest["cases"] if meta.get("case_id") not in fresh]
    manifest["cases"] = kept + list(fresh.values())
    manifest["ok_count"] = sum(1 for meta in manifest["cases"] if "error" not in meta)
    manifest_path.write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    print(f"manifest updated: {len(manifest['cases'])} cases")


if __name__ == "__main__":
    main()
