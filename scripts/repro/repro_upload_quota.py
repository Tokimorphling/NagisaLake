#!/usr/bin/env python3
"""复现：只申请上传、不传任何字节即可占满组织存储配额。

POST /artifacts/uploads 会在签发预签名 URL 前就预留完整配额。客户端拿到 URL
后如果从不 PUT、也不调用 complete，artifact 会永久停留在 pending_upload：
没有过期回收、没有取消上传接口、没有 artifact 删除接口。

默认 dry-run，只报告是否可能占满。加 --commit 才真正申请配额——注意在修复前
这部分配额无法释放。
"""

import sys
import uuid
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from _common import connect, error_code, not_reproduced, reproduced

COMMIT = "--commit" in sys.argv
GIB = 1024**3

hub = connect()
used, limit = hub.quota()
print(f"\n起始配额 已用 {used / GIB:.2f} GiB / 上限 {limit / GIB:.2f} GiB")

_status, settings = hub.call("GET", "/settings/public")
max_artifact = settings["max_artifact_bytes"]
headroom = limit - used
print(f"单文件上限 {max_artifact / GIB:.2f} GiB，剩余配额 {headroom / GIB:.2f} GiB")
print(f"→ 占满剩余配额需要约 {-(-headroom // max_artifact)} 次申请，且无需上传任何字节")

if not COMMIT:
    print("\ndry-run：未实际申请。加 --commit 执行真实验证。")
    print("（修复前这些配额无法释放，只能直接改数据库）")
    sys.exit(0)

print("\n开始申请（不上传任何字节）")
chunk = min(max_artifact, max(headroom // 4, 1))
reserved = 0
created = []
for attempt in range(1, 33):
    status, body = hub.call(
        "POST",
        "/artifacts/uploads",
        {
            "name": f"never-uploaded-{uuid.uuid4().hex[:8]}.bin",
            "content_type": "application/octet-stream",
            "size_bytes": chunk,
            "sha256": "0" * 64,
        },
    )
    if status != 201:
        print(f"  第 {attempt} 次被拒 [{status}] {error_code(body)}")
        break
    created.append(body["artifact"]["id"])
    reserved += chunk
    used, limit = hub.quota()
    print(f"  第 {attempt} 次成功 -> 配额已用 {used / GIB:.2f} GiB")
    if used >= limit:
        break

used, limit = hub.quota()
print(f"\n共预留 {reserved / GIB:.2f} GiB，实际上传 0 字节")
print(f"当前配额 {used / GIB:.2f} GiB / {limit / GIB:.2f} GiB")

# 确认是否存在任何释放手段。
if created:
    status, _ = hub.call("DELETE", f"/artifacts/{created[0]}")
    print(f"DELETE /artifacts/{{id}} -> {status}（404/405 表示没有删除接口）")

status, body = hub.call(
    "POST",
    "/artifacts/uploads",
    {
        "name": "probe.bin",
        "content_type": "application/octet-stream",
        "size_bytes": 1024 * 1024,
        "sha256": "1" * 64,
    },
)
print(f"再申请 1 MiB -> [{status}] {error_code(body) if status != 201 else 'created'}")

if status != 201:
    reproduced(
        "已复现：零字节上传即占满组织存储配额。",
        f"  预留 {reserved / GIB:.2f} GiB，对象存储中没有对应数据。",
        "  artifact 永久停留在 pending_upload：无过期回收、无取消上传、无删除接口。",
    )
not_reproduced("配额未被占满（可能已实现过期回收）")
