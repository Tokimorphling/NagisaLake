#!/usr/bin/env python3
"""复现：取消排队中的作业后 queued_jobs 永不回落。

Worker 在 register_job 时把 queued_jobs +1，但只有取得并发许可后才通过
activate() 把它 -1。作业在等待许可期间被取消会直接返回，跳过 activate()，
而 finish_job 只移除 cancellation token，不动计数。

Hub 用 active_jobs + queued_jobs < concurrency 判断容量，所以 concurrency = 1
的设备取消一次排队作业后就永久判定已满，直到 Worker 重启。

前置：一个在线 Worker（concurrency = 1）、组织 max_concurrent_jobs >= 2、
ComfyUI 响应足够慢。
"""

import sys
import time
import uuid
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from _common import TERMINAL_STATES, connect, error_code, fail, not_reproduced, reproduced

SETTLE_SECONDS = 6  # 留出一个心跳周期，让 Hub 收到最新计数


def submit(hub, label):
    status, body = hub.call(
        "POST",
        "/jobs",
        {
            "workflow_id": WORKFLOW,
            "workflow_version": VERSION,
            "parameters": {},
            "input_artifact_ids": [],
        },
        idempotency_key=f"repro-queued-{uuid.uuid4()}",
    )
    if status == 202:
        print(f"  作业 {label} 已受理 {body['id'][:8]}")
        return body["id"]
    print(f"  作业 {label} 被拒 [{status}] {error_code(body)}")
    return None


def state_of(hub, job_id):
    _status, body = hub.call("GET", f"/jobs/{job_id}")
    return body["state"] if body else "?"


def wait_for(hub, job_id, wanted, limit=30):
    deadline = time.monotonic() + limit
    while time.monotonic() < deadline:
        current = state_of(hub, job_id)
        if current in wanted:
            return current
        time.sleep(0.5)
    return state_of(hub, job_id)


def cancel(hub, job_id, label):
    status, body = hub.call("DELETE", f"/jobs/{job_id}")
    if status != 200:
        fail(f"取消{label}未成功 [{status}] {error_code(body)}，无法据此判断是否泄漏")
    return status


hub = connect()

# 选一个不需要上传文件的可用 workflow，避免牵扯对象存储。
_status, workflows = hub.call("GET", "/workflows")
candidates = [
    w
    for w in (workflows or [])
    if w["available"]
    and w["manifest_consistent"]
    and not [i for i in (w.get("manifest") or {}).get("inputs", []) if i["kind"] == "artifact"]
]
if not candidates:
    fail("没有可用且无需输入文件的 workflow，需要一台在线 Worker")
WORKFLOW, VERSION = candidates[0]["id"], candidates[0]["version"]
print(f"使用 workflow {WORKFLOW}@{VERSION}")

print("\n0. 清理残留的未终态作业")
_status, page = hub.call("GET", "/jobs")
leftover = [j for j in (page or {}).get("items", []) if j["state"] not in TERMINAL_STATES]
for job in leftover:
    hub.call("DELETE", f"/jobs/{job['id']}")
    print(f"  已请求取消 {job['id'][:8]} ({job['state']})")
if leftover:
    time.sleep(SETTLE_SECONDS)
active, queued, concurrency = hub.worker_counts()
print(f"  起始计数 active={active} queued={queued} concurrency={concurrency}")
if concurrency != 1:
    print(f"  注意：concurrency={concurrency}，需要提交 {concurrency} 个作业才能占满")
if queued:
    fail(f"起始 queued={queued} 已非零，先重启 Worker 再跑（说明上一轮已泄漏）")

print("\n1. 提交作业 A，占住并发 slot")
job_a = submit(hub, "A")
if not job_a:
    fail("作业 A 无法提交")
state_a = wait_for(hub, job_a, {"running", "uploading"} | TERMINAL_STATES)
print(f"  A 状态 {state_a}")
if state_a in TERMINAL_STATES:
    fail(f"作业 A 过快到达 {state_a}，无法占住 slot；让 ComfyUI 响应更慢再试")

print("\n2. 提交作业 B，它应当卡在并发许可等待上")
job_b = submit(hub, "B")
if not job_b:
    fail("作业 B 无法提交（可能是组织 max_concurrent_jobs 太低）")
time.sleep(2)
print(f"  B 状态 {state_of(hub, job_b)}（accepted 表示已计入 queued 但未取得许可）")
active, queued, _c = hub.worker_counts()
print(f"  计数 active={active} queued={queued}")

print("\n3. 取消排队中的作业 B")
cancel(hub, job_b, "B")
print(f"  B 状态 {wait_for(hub, job_b, TERMINAL_STATES)}")

print("\n4. 取消作业 A，释放 slot")
cancel(hub, job_a, "A")
print(f"  A 状态 {wait_for(hub, job_a, TERMINAL_STATES)}")
time.sleep(SETTLE_SECONDS)

_status, page = hub.call("GET", "/jobs")
live = [j for j in (page or {}).get("items", []) if j["state"] not in TERMINAL_STATES]
active, queued, concurrency = hub.worker_counts()
print(f"\n5. 全部作业已终态（未终态数 {len(live)}）")
print(f"   计数 active={active} queued={queued} concurrency={concurrency}")

if queued:
    reproduced(
        "已复现：Worker 完全空闲，但 queued_jobs 未回落。",
        f"  active={active} queued={queued} concurrency={concurrency}",
        "  Hub 以 active + queued < concurrency 判断容量，因此该设备永久判定已满。",
        "  取消排队作业时跳过了 activate()，而 finish_job 不递减 queued_jobs。",
        "  仅重启 Worker 可恢复。",
    )
not_reproduced("queued_jobs 已正确回落到 0")
