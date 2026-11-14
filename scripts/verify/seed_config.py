"""Standalone sqlite config seeder for the golden toolchain (R7.4).

镜像已退役 Python 后端 ``SqliteStore.save_config()`` 的播种行为——payload
模板于 2026-08-30 逐字节采集自 Python 实现(R7.5 随 ``backend/`` 删除)——
使 ``replay_golden`` / ``_capture_cases`` 播种与基线一致的配置形态。Only
``app_config`` is created here: the Rust gateway builds every other table on
boot (``schema.rs`` is all ``IF NOT EXISTS``) and reads the same single
payload row (C5 零迁移).
"""

from __future__ import annotations

import json
import sqlite3
from pathlib import Path

_CONFIG_PAYLOAD_TEMPLATE = {
    "version": 2,
    "preferredProvider": "",
    "Providers": [],
    "APIKEYS": [],
    "routingRules": [],
    "policies": {"enabled": True, "defaultEffect": "allow", "rules": []},
    "reliability": {
        "circuit_breaker": {
            "enabled": True,
            "failure_threshold": 3,
            "cooldown_ms": 30000,
            "failure_status_codes": [408, 429, 500, 502, 503, 504],
        },
        "concurrency": {"enabled": True, "max_in_flight_per_provider": 8, "queue_timeout_ms": 0},
        "idempotency": {
            "enabled": True,
            "ttl_ms": 300000,
            "max_entries": 1000,
            "header_name": "idempotency-key",
            "cache_error_responses": False,
        },
        "health": {"enabled": True, "initial_delay_ms": 5000, "interval_ms": 30000, "timeout_ms": 5000},
        "precheck": {"enabled": False, "rate_limit_rules": [], "budget_max_cost_usd": 0.0},
        "metrics": {"enabled": True, "include_provider_health": True},
    },
    "theme": "system",
    "proxy": {},
    "observability": {},
    "Router": {"rules": [], "fallback": {}},
}


def seed_config(
    data_dir: Path,
    providers: list[dict],
    api_keys: list[dict] | None = None,
    router_fallback: dict | None = None,
) -> None:
    """Create ``<data_dir>/config.sqlite`` holding the config the gateway reads.

    ``router_fallback`` 缺省为空表(不回退);给了则写进 ``Router.fallback``
    (全局失败回退,20261101_02 T3 后是回退的唯一来源)。
    """
    data_dir.mkdir(parents=True, exist_ok=True)
    # dev-open 逃生门已移除:数据面强制密钥。空/未指定 api_keys 时种入黄金
    # 默认密钥(必须与 cases.DEFAULT_CLIENT_KEY 一致),使注册表非空;
    # cases.auth_headers 会带上同一个密钥。
    keys = list(api_keys) if api_keys else [{"key": "sk-golden-1", "enabled": True, "name": "golden"}]
    router = {"rules": [], "fallback": dict(router_fallback or {})}
    payload = {**_CONFIG_PAYLOAD_TEMPLATE, "Providers": list(providers), "APIKEYS": keys, "Router": router}
    conn = sqlite3.connect(data_dir / "config.sqlite")
    try:
        conn.execute(
            "CREATE TABLE IF NOT EXISTS app_config"
            " (id INTEGER PRIMARY KEY CHECK (id = 1), payload TEXT NOT NULL)"
        )
        conn.execute(
            "INSERT INTO app_config (id, payload) VALUES (1, ?)",
            (json.dumps(payload, ensure_ascii=False),),
        )
        conn.commit()
    finally:
        conn.close()
