#!/usr/bin/env python3
"""Nagisalake 控制面端到端冒烟测试。

只用 Python 3 标准库，可以直接拷到任何局域网设备上运行，不需要装依赖。

它走的是消费者路径，和 SDK 调用方式一致：认证 -> 列目录 -> 上传输入对象
-> 提交作业 -> 轮询到终态 -> 下载输出。缺少的前置条件会明确报出来，而不是
静默跳过。

用法:

    # 用浏览器账户（会拿到一个短期 access token）
    ./scripts/smoke_test.py --base-url http://192.168.31.108:9091 \\
        --email demo@nagisalake.dev --password '...'

    # 用程序 API Key（推荐，SDK 的真实用法）
    ./scripts/smoke_test.py --base-url http://192.168.31.108:9091 --api-key nsk_...

    # 只检查连通性和目录，不提交作业
    ./scripts/smoke_test.py --base-url http://192.168.31.108:9091 --api-key nsk_... --no-submit

退出码 0 表示全部通过，1 表示有失败，2 表示前置条件不满足（例如没有在线设备）。
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
import time
import urllib.error
import urllib.request
import uuid
from typing import Any

TERMINAL_STATES = {"completed", "failed", "cancelled"}


# ----------------------------------------------------------------- 输出

class Out:
    """带颜色的分级输出。非 TTY 时自动降级为纯文本。"""

    def __init__(self) -> None:
        self.color = sys.stdout.isatty() and os.environ.get("NO_COLOR") is None
        self.failures = 0

    def _c(self, code: str, text: str) -> str:
        return f"\033[{code}m{text}\033[0m" if self.color else text

    def step(self, text: str) -> None:
        print(f"\n{self._c('1', text)}")

    def ok(self, text: str) -> None:
        print(f"  {self._c('32', 'OK')}   {text}")

    def fail(self, text: str) -> None:
        self.failures += 1
        print(f"  {self._c('31', 'FAIL')} {text}")

    def info(self, text: str) -> None:
        print(f"  {self._c('90', '·')}    {text}")

    def warn(self, text: str) -> None:
        print(f"  {self._c('33', 'WARN')} {text}")


out = Out()


# ------------------------------------------------------------- HTTP 客户端

class ApiError(Exception):
    def __init__(self, status: int, code: str, message: str, request_id: str | None):
        super().__init__(f"{code}: {message}")
        self.status = status
        self.code = code
        self.message = message
        self.request_id = request_id


class Client:
    def __init__(self, base_url: str, timeout: float = 30.0):
        self.base = base_url.rstrip("/")
        self.timeout = timeout
        self.token: str | None = None
        self.organization_id: str | None = None
        # API Key 固定绑定一个组织，不能用 X-Organization-ID 跨租户切换。
        self.is_api_key = False

    def request(
        self,
        method: str,
        path: str,
        body: Any = None,
        idempotency_key: str | None = None,
    ) -> Any:
        url = f"{self.base}/api/v1{path}"
        data = json.dumps(body).encode() if body is not None else None
        request = urllib.request.Request(url, data=data, method=method)
        if data is not None:
            request.add_header("Content-Type", "application/json")
        if self.token:
            request.add_header("Authorization", f"Bearer {self.token}")
        # 浏览器 session 才用这个 header；API Key principal 会被服务端拒绝。
        if self.organization_id and not self.is_api_key:
            request.add_header("X-Organization-ID", self.organization_id)
        if idempotency_key:
            request.add_header("Idempotency-Key", idempotency_key)

        try:
            with urllib.request.urlopen(request, timeout=self.timeout) as response:
                raw = response.read()
                return json.loads(raw) if raw else None
        except urllib.error.HTTPError as error:
            raw = error.read()
            code, message, request_id = "unknown", error.reason, None
            try:
                payload = json.loads(raw)["error"]
                code = payload.get("code", code)
                message = payload.get("message", message)
                request_id = payload.get("request_id")
            except Exception:
                message = raw.decode(errors="replace")[:200] or str(error.reason)
            raise ApiError(error.code, code, message, request_id) from None
        except urllib.error.URLError as error:
            raise ApiError(0, "network_error", f"无法连接 {url}: {error.reason}", None) from None

    def get(self, path: str) -> Any:
        return self.request("GET", path)

    def post(self, path: str, body: Any = None, idempotency_key: str | None = None) -> Any:
        return self.request("POST", path, body, idempotency_key)


# ------------------------------------------------------------------ 步骤

def check_reachable(client: Client) -> bool:
    out.step("1. 连通性")
    try:
        with urllib.request.urlopen(f"{client.base}/healthz", timeout=10) as response:
            health = json.loads(response.read())
        out.ok(f"{client.base}/healthz -> {health.get('status')}")
        out.info(f"Hub 当前在线 Worker: {health.get('connected_workers', 0)}")
    except Exception as error:
        out.fail(f"无法访问 {client.base}/healthz: {error}")
        out.info("确认 Hub 的 listen 是 0.0.0.0 而不是 127.0.0.1，并检查防火墙")
        return False

    try:
        settings = client.get("/settings/public")
        out.ok(
            "公开设置: 注册="
            f"{'开' if settings['registration_enabled'] else '关'}, "
            f"单文件上限={settings['max_artifact_bytes'] / 1024 ** 3:.1f} GiB"
        )
    except ApiError as error:
        out.fail(f"读取公开设置失败: {error}")
        return False
    return True


def authenticate(client: Client, args: argparse.Namespace) -> bool:
    out.step("2. 认证")
    if args.api_key:
        client.token = args.api_key
        client.is_api_key = True
        try:
            # API Key 不能调 /auth/me（那是浏览器专用），用 jobs 探活并确认 scope。
            client.get("/jobs")
            out.ok(f"API Key 可用 (前缀 {args.api_key[:12]}…)")
            return True
        except ApiError as error:
            out.fail(f"API Key 认证失败: {error}")
            if error.code == "forbidden":
                out.info("这个 Key 可能缺少 jobs:read scope")
            return False

    if not (args.email and args.password):
        out.fail("需要 --api-key，或者同时给出 --email 和 --password")
        return False

    try:
        auth = client.post(
            "/auth/login", {"email": args.email, "password": args.password}
        )
    except ApiError as error:
        out.fail(f"登录失败: {error}")
        return False

    client.token = auth["access_token"]
    client.organization_id = auth["current_organization_id"]
    out.ok(f"已登录 {auth['user']['email']}")

    me = client.get("/auth/me")
    current = next(
        (m for m in me["memberships"] if m["organization_id"] == client.organization_id),
        None,
    )
    out.info(
        f"组织 {current['organization_name'] if current else '?'} · 角色 "
        f"{current['role'] if current else '?'}"
    )
    return True


def list_catalog(client: Client) -> tuple[list[dict], list[dict]]:
    out.step("3. 设备与 Workflow 目录")

    devices: list[dict] = []
    try:
        devices = client.get("/devices")
        online = [d for d in devices if d["connected"]]
        out.ok(f"设备 {len(devices)} 台，在线 {len(online)} 台")
        for device in devices:
            mark = "在线" if device["connected"] else "离线"
            out.info(
                f"[{mark}] {device['node_name']} ({device['device_id']}) "
                f"namespace={device['namespace']} "
                f"workflow={len(device['workflows'])} "
                f"来源={'自有' if device['access_kind'] == 'owner' else '共享'}"
            )
    except ApiError as error:
        # API Key 默认没有 devices:read，这不算失败。
        out.warn(f"读取设备列表失败（{error.code}），跳过：{error.message}")

    workflows = client.get("/workflows")
    available = [w for w in workflows if w["available"]]
    out.ok(f"Workflow {len(workflows)} 个，可用 {len(available)} 个")
    for workflow in workflows:
        manifest = workflow.get("manifest") or {}
        inputs = manifest.get("inputs", [])
        artifacts = [i for i in inputs if i["kind"] == "artifact"]
        params = [i for i in inputs if i["kind"] == "parameter"]
        mark = "可用" if workflow["available"] else "离线"
        flag = "" if workflow["manifest_consistent"] else "  [manifest 不一致]"
        out.info(
            f"[{mark}] {workflow['id']}@{workflow['version']} "
            f"输入文件={len(artifacts)} 参数={len(params)} "
            f"输出={','.join(workflow['output_types']) or '未声明'}{flag}"
        )
    return devices, workflows


def pick_workflow(workflows: list[dict], wanted: str | None) -> dict | None:
    candidates = [w for w in workflows if w["available"] and w["manifest_consistent"]]
    if wanted:
        candidates = [w for w in candidates if w["id"] == wanted]
        if not candidates:
            out.fail(f"没有可用且 manifest 一致的 workflow 叫 {wanted}")
            return None
    if not candidates:
        return None
    # 优先选不需要上传文件的，这样在没有对象存储时也能跑。
    candidates.sort(
        key=lambda w: len(
            [i for i in (w.get("manifest") or {}).get("inputs", []) if i["kind"] == "artifact"]
        )
    )
    return candidates[0]


def upload_artifact(client: Client, name: str, content: bytes, content_type: str) -> str:
    """预留 -> 直传对象存储 -> 完成校验。媒体不经过 Hub 的 JSON body。"""
    digest = hashlib.sha256(content).hexdigest()
    reserved = client.post(
        "/artifacts/uploads",
        {
            "name": name,
            "content_type": content_type,
            "size_bytes": len(content),
            "sha256": digest,
        },
    )
    upload = reserved["upload"]
    artifact_id = reserved["artifact"]["id"]

    # 必须严格使用服务端返回的 method/url/headers，多一个或少一个都会破坏签名。
    request = urllib.request.Request(
        upload["url"], data=content, method=upload["method"]
    )
    for key, value in upload["headers"].items():
        request.add_header(key, value)
    try:
        with urllib.request.urlopen(request, timeout=120) as response:
            if response.status not in (200, 204):
                raise ApiError(response.status, "upstream_error", "对象存储拒绝上传", None)
    except urllib.error.HTTPError as error:
        raise ApiError(
            error.code,
            "upstream_error",
            f"直传对象存储失败: {error.read().decode(errors='replace')[:200]}",
            None,
        ) from None
    except urllib.error.URLError as error:
        host = upload["url"].split("/")[2] if "//" in upload["url"] else upload["url"]
        raise ApiError(
            0,
            "network_error",
            f"无法连接对象存储 {host}: {error.reason}。"
            "如果这里是 127.0.0.1，说明 Hub 的 object_store.endpoint_url "
            "需要改成局域网可达地址",
            None,
        ) from None

    client.post(
        f"/artifacts/uploads/{artifact_id}/complete",
        {"artifact_id": artifact_id, "size_bytes": len(content), "sha256": digest},
    )
    return artifact_id


def png_pixel() -> bytes:
    """一张 1x1 PNG，用作占位输入。"""
    import base64

    return base64.b64decode(
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8"
        "DwHwAFAAH/q842iQAAAABJRU5ErkJggg=="
    )


def build_parameters(manifest: dict, overrides: dict[str, str]) -> dict[str, Any]:
    """按 manifest 声明的类型构造参数，未指定的用默认值。"""
    parameters: dict[str, Any] = {}
    for item in manifest.get("inputs", []):
        if item["kind"] != "parameter":
            continue
        name = item["name"]
        if name in overrides:
            raw = overrides[name]
            kind = item["type"]
            if kind == "integer":
                parameters[name] = int(raw)
            elif kind == "number":
                parameters[name] = float(raw)
            elif kind == "boolean":
                parameters[name] = raw.lower() in ("1", "true", "yes")
            else:
                parameters[name] = raw
        elif item["required"]:
            if item.get("default") is not None:
                parameters[name] = item["default"]
            elif item["type"] == "string":
                parameters[name] = "nagisalake smoke test"
            elif item["type"] == "integer":
                parameters[name] = 1
            elif item["type"] == "number":
                parameters[name] = 1.0
            elif item["type"] == "boolean":
                parameters[name] = False
    return parameters


def submit_and_wait(
    client: Client,
    workflow: dict,
    args: argparse.Namespace,
) -> bool:
    out.step("4. 提交作业")
    manifest = workflow.get("manifest") or {}
    artifacts = [i for i in manifest.get("inputs", []) if i["kind"] == "artifact"]

    # input_artifact_ids 是位置数组：第 N 个 ID 对应 Worker 的第 N 个输入绑定，
    # 数量必须完全一致，所以严格按 manifest 顺序上传。
    artifact_ids: list[str] = []
    if artifacts:
        out.info(f"该 workflow 需要 {len(artifacts)} 个输入文件，按 manifest 顺序上传")
        for index, item in enumerate(artifacts):
            try:
                artifact_id = upload_artifact(
                    client,
                    f"smoke-input-{index}.png",
                    png_pixel(),
                    item.get("content_type") or "image/png",
                )
                artifact_ids.append(artifact_id)
                out.ok(f"输入位 {index + 1} ({item['name']}) -> {artifact_id[:12]}…")
            except ApiError as error:
                out.fail(f"上传输入位 {index + 1} 失败: {error.message}")
                return False

    parameters = build_parameters(manifest, dict(args.param or []))
    payload: dict[str, Any] = {
        "workflow_id": workflow["id"],
        "workflow_version": workflow["version"],
        "parameters": parameters,
        "input_artifact_ids": artifact_ids,
    }
    if args.device_id and args.device_org:
        payload["device_organization_id"] = args.device_org
        payload["device_id"] = args.device_id
        out.info(f"定向到设备 {args.device_id}")

    out.info(f"参数: {json.dumps(parameters, ensure_ascii=False)}")
    idempotency_key = f"smoke-{uuid.uuid4()}"
    try:
        job = client.post("/jobs", payload, idempotency_key=idempotency_key)
    except ApiError as error:
        out.fail(f"提交失败: {error}")
        if error.code == "unavailable":
            out.info("没有在线且有余量的设备。需要启动一个连到本机 ComfyUI 的 Worker")
        elif error.code == "quota_exceeded":
            out.info("配额已用尽，检查 /quota")
        return False

    out.ok(f"作业已受理 {job['id']}")

    out.step("5. 轮询到终态")
    deadline = time.monotonic() + args.timeout
    last_state, last_progress = None, None
    while time.monotonic() < deadline:
        job = client.get(f"/jobs/{job['id']}")
        state, progress = job["state"], job["progress"]
        if (state, progress) != (last_state, last_progress):
            percent = f" {progress * 100:.0f}%" if progress is not None else ""
            out.info(f"{state}{percent}")
            last_state, last_progress = state, progress
        if state in TERMINAL_STATES:
            break
        time.sleep(args.poll_interval)
    else:
        out.fail(f"{args.timeout}s 内未到终态，当前 {job['state']}")
        out.info(f"手动查看: {client.base}/jobs/{job['id']}")
        return False

    for event in job["events"]:
        percent = f" ({event['progress'] * 100:.0f}%)" if event["progress"] is not None else ""
        out.info(f"事件 {event['sequence']} {event['kind']}{percent}: {event['message']}")

    if job["state"] != "completed":
        out.fail(f"作业终态为 {job['state']}")
        if job.get("error"):
            out.info(f"错误: {job['error']}")
        return False
    out.ok(f"作业完成，输出 {len(job['output_artifact_ids'])} 个")

    out.step("6. 下载输出")
    if not job["output_artifact_ids"]:
        out.warn("这个 workflow 没有产生输出对象")
        return True
    for index, artifact_id in enumerate(job["output_artifact_ids"]):
        try:
            resolved = client.get(f"/artifacts/{artifact_id}/download")
            url = resolved["download"]["url"]
            request = urllib.request.Request(url, method=resolved["download"]["method"])
            for key, value in resolved["download"]["headers"].items():
                request.add_header(key, value)
            with urllib.request.urlopen(request, timeout=120) as response:
                content = response.read()
            digest = hashlib.sha256(content).hexdigest()
            expected = resolved["artifact"]["sha256"]
            if digest == expected:
                out.ok(
                    f"输出 {index + 1} {resolved['artifact']['name']} "
                    f"{len(content)} 字节，sha256 校验通过"
                )
            else:
                out.fail(f"输出 {index + 1} sha256 不匹配: 期望 {expected[:16]}… 实际 {digest[:16]}…")
            if args.output_dir:
                os.makedirs(args.output_dir, exist_ok=True)
                target = os.path.join(args.output_dir, resolved["artifact"]["name"])
                with open(target, "wb") as handle:
                    handle.write(content)
                out.info(f"已保存到 {target}")
        except ApiError as error:
            out.fail(f"下载输出 {index + 1} 失败: {error.message}")
    return True


def show_quota(client: Client) -> None:
    if not client.organization_id:
        return
    out.step("7. 配额")
    try:
        quota = client.get(f"/organizations/{client.organization_id}/quota")
    except ApiError as error:
        out.warn(f"读取配额失败（{error.code}），跳过")
        return
    out.info(f"并发作业 {quota['active_jobs']}/{quota['max_concurrent_jobs']}")
    out.info(f"周期作业 {quota['period_jobs']}/{quota['max_jobs_per_period']}")
    out.info(
        f"存储 {quota['storage_bytes'] / 1024 ** 2:.1f} MiB / "
        f"{quota['max_storage_bytes'] / 1024 ** 3:.1f} GiB"
    )


# -------------------------------------------------------------------- main

def main() -> int:
    parser = argparse.ArgumentParser(
        description="Nagisalake 控制面端到端冒烟测试",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument(
        "--base-url",
        default=os.environ.get("NAGISALAKE_BASE_URL", "http://127.0.0.1:9091"),
        help="Hub 地址，例如 http://192.168.31.108:9091",
    )
    parser.add_argument("--api-key", default=os.environ.get("NAGISALAKE_API_KEY"),
                        help="nsk_ 程序 API Key")
    parser.add_argument("--email", default=os.environ.get("NAGISALAKE_EMAIL"))
    parser.add_argument("--password", default=os.environ.get("NAGISALAKE_PASSWORD"))
    parser.add_argument("--workflow", help="指定 workflow id，默认自动选一个可用的")
    parser.add_argument("--device-id", help="把作业定向到某台设备")
    parser.add_argument("--device-org", help="该设备所属组织 id，与 --device-id 配对使用")
    parser.add_argument(
        "--param",
        nargs=2,
        action="append",
        metavar=("NAME", "VALUE"),
        help="覆盖某个参数，可重复，例如 --param prompt 'a cat'",
    )
    parser.add_argument("--timeout", type=float, default=300.0, help="等待终态的秒数")
    parser.add_argument("--poll-interval", type=float, default=2.0)
    parser.add_argument("--output-dir", help="把输出对象保存到这个目录")
    parser.add_argument("--no-submit", action="store_true",
                        help="只检查连通性和目录，不提交作业")
    args = parser.parse_args()

    print(f"Nagisalake 冒烟测试 -> {args.base_url}")

    client = Client(args.base_url)
    if not check_reachable(client):
        return 1
    if not authenticate(client, args):
        return 1

    _devices, workflows = list_catalog(client)

    if args.no_submit:
        show_quota(client)
        out.step("结果")
        out.info("--no-submit：跳过作业提交")
        print()
        return 1 if out.failures else 0

    workflow = pick_workflow(workflows, args.workflow)
    if workflow is None:
        out.step("4. 提交作业")
        out.warn("没有可用的 workflow，跳过提交")
        out.info("需要一台在线设备：启动 Worker 连接 Hub 并注册 workflow manifest")
        out.info("离线设备的 workflow 可以浏览，但不能提交作业")
        show_quota(client)
        out.step("结果")
        out.info("前置条件不满足：没有在线设备")
        print()
        return 2

    out.info(f"选中 {workflow['id']}@{workflow['version']}")
    submit_and_wait(client, workflow, args)
    show_quota(client)

    out.step("结果")
    if out.failures:
        out.fail(f"{out.failures} 项失败")
        print()
        return 1
    out.ok("全部通过")
    print()
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        print("\n已中断")
        sys.exit(130)
