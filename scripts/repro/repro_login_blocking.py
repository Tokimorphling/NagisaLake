#!/usr/bin/env python3
"""复现：同步 Argon2 占满 Tokio 执行线程，拖慢无关请求。

登录 handler 直接在 async 上下文里跑 Argon2，没有 spawn_blocking。
并发登录会占住执行线程，连只读内存计数的 /healthz 都被拖慢。

/healthz 不碰数据库、不做加密，它变慢只能来自执行线程被占满。
"""

import os
import statistics
import sys
import threading
import time
import urllib.error
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from _common import env_config, fail, not_reproduced, post_json, reproduced

BASE, EMAIL, _password = env_config()
CONCURRENCY = max(8, (os.cpu_count() or 4) * 3)
DURATION_SECONDS = 6.0
WRONG_PASSWORD = "definitely-not-the-right-password"


def timed_healthz() -> float:
    start = time.perf_counter()
    try:
        urllib.request.urlopen(f"{BASE}/healthz", timeout=30).read()
    except urllib.error.HTTPError as error:
        error.read()
    except urllib.error.URLError as error:
        fail(f"无法连接 {BASE}: {error.reason}")
    return (time.perf_counter() - start) * 1000


def hammer_login(stop: threading.Event) -> None:
    while not stop.is_set():
        try:
            post_json(f"{BASE}/api/v1/auth/login", {"email": EMAIL, "password": WRONG_PASSWORD})
        except Exception:  # 401 是预期结果，只关心它消耗的 CPU
            pass


print(f"Tokio 阻塞测量 -> {BASE}")
print(f"本机 {os.cpu_count()} 逻辑核，登录并发 {CONCURRENCY}\n")

for _ in range(5):
    timed_healthz()

baseline = [timed_healthz() for _ in range(40)]
baseline_median = statistics.median(baseline)
baseline_p95 = sorted(baseline)[int(len(baseline) * 0.95)]
print(f"基线   /healthz: 中位数 {baseline_median:6.2f} ms  p95 {baseline_p95:6.2f} ms")

stop = threading.Event()
threads = [threading.Thread(target=hammer_login, args=(stop,), daemon=True) for _ in range(CONCURRENCY)]
for thread in threads:
    thread.start()

time.sleep(1.0)  # 让登录压力先铺满
loaded = []
deadline = time.time() + DURATION_SECONDS
while time.time() < deadline:
    loaded.append(timed_healthz())

stop.set()
for thread in threads:
    thread.join(timeout=5)

if not loaded:
    fail("压测期间没有采到样本")

loaded_median = statistics.median(loaded)
loaded_p95 = sorted(loaded)[int(len(loaded) * 0.95)]
print(
    f"压测中 /healthz: 中位数 {loaded_median:6.2f} ms  p95 {loaded_p95:6.2f} ms  "
    f"最大 {max(loaded):6.2f} ms  (样本 {len(loaded)})"
)

ratio = loaded_median / max(baseline_median, 0.001)
print(f"\n中位数放大 {ratio:.1f}x")

if ratio > 2.5:
    reproduced(
        "已复现：同步 Argon2 占满执行线程，无关请求同样被拖慢。",
        "  修复需要把密码哈希移到受限的 spawn_blocking / 专用线程池。",
    )
not_reproduced(f"未观察到显著放大（{ratio:.1f}x）")
