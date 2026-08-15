"""复现脚本共用的最小 Hub 客户端。只用标准库。"""

from __future__ import annotations

import json
import os
import sys
import urllib.error
import urllib.request

TERMINAL_STATES = {"completed", "failed", "cancelled"}


class Hub:
    """浏览器 session 客户端。

    这些脚本用登录而非 API Key，因为取消作业需要 jobs:cancel scope，而 Key
    默认不带；用 Key 会拿到 403 并让判定失真。
    """

    def __init__(self, base_url: str):
        self.base = base_url.rstrip("/")
        self.token: str | None = None
        self.organization_id: str | None = None

    def call(self, method: str, path: str, body=None, idempotency_key: str | None = None):
        request = urllib.request.Request(
            f"{self.base}/api/v1{path}",
            data=json.dumps(body).encode() if body is not None else None,
            method=method,
        )
        if body is not None:
            request.add_header("Content-Type", "application/json")
        if self.token:
            request.add_header("Authorization", f"Bearer {self.token}")
        if self.organization_id:
            request.add_header("X-Organization-ID", self.organization_id)
        if idempotency_key:
            request.add_header("Idempotency-Key", idempotency_key)
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                raw = response.read()
                return response.status, (json.loads(raw) if raw else None)
        except urllib.error.HTTPError as error:
            raw = error.read()
            try:
                return error.code, json.loads(raw)
            except Exception:
                return error.code, {"raw": raw.decode(errors="replace")[:200]}
        except urllib.error.URLError as error:
            return 0, {"error": {"code": "network_error", "message": str(error.reason)}}

    def login(self, email: str, password: str) -> None:
        status, body = self.call("POST", "/auth/login", {"email": email, "password": password})
        if status != 200:
            fail(f"登录失败 [{status}] {error_code(body)}")
        self.token = body["access_token"]
        self.organization_id = body["current_organization_id"]

    def worker_counts(self):
        """返回第一台 Worker 的 (active, queued, concurrency)。"""
        _status, body = self.call("GET", "/workflows")
        for workflow in body or []:
            for worker in workflow["workers"]:
                return worker["active_jobs"], worker["queued_jobs"], worker["concurrency"]
        return None, None, None

    def quota(self):
        _status, body = self.call("GET", f"/organizations/{self.organization_id}/quota")
        return body["storage_bytes"], body["max_storage_bytes"]


def post_json(url: str, body: dict):
    """无认证的裸 POST，用于计时测量。忽略 4xx，只关心耗时。"""
    request = urllib.request.Request(
        url, data=json.dumps(body).encode(), method="POST"
    )
    request.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return response.status, response.read()
    except urllib.error.HTTPError as error:
        return error.code, error.read()


def error_code(body) -> str:
    if isinstance(body, dict) and "error" in body:
        return f"{body['error'].get('code', '?')}: {body['error'].get('message', '')}"
    return str(body)


def env_config():
    base = os.environ.get("NAGISALAKE_BASE_URL", "http://127.0.0.1:9091")
    email = os.environ.get("NAGISALAKE_EMAIL")
    password = os.environ.get("NAGISALAKE_PASSWORD")
    if not email or not password:
        fail("需要 NAGISALAKE_EMAIL 和 NAGISALAKE_PASSWORD 环境变量")
    return base, email, password


def connect() -> Hub:
    base, email, password = env_config()
    hub = Hub(base)
    status, _ = hub.call("GET", "/settings/public")
    if status != 200:
        fail(f"无法访问 {base}，确认 Hub 已启动")
    hub.login(email, password)
    print(f"已连接 {base}，组织 {hub.organization_id}")
    return hub


def fail(message: str):
    """前置条件不满足：中止且不给复现结论。"""
    print(f"ABORT: {message}")
    sys.exit(3)


def reproduced(*lines: str):
    print()
    for line in lines:
        print(line)
    sys.exit(2)


def not_reproduced(message: str):
    print(f"\n未复现：{message}")
    sys.exit(0)
