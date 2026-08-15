# 缺陷复现脚本

每个脚本复现一个已确认的缺陷，只用 Python 3 标准库。约定退出码：

- `0` 未复现（缺陷已修复，或环境不满足触发条件）
- `2` 已复现
- `1` / `3` 脚本本身无法完成判定（前置条件缺失、权限不足）

区分 `0` 和 `3` 很重要：一次失败的前置操作会让流程看起来"复现"了，而实际上
什么都没发生。这些脚本在前置步骤失败时中止，不会继续给结论。

## 前置条件

Hub 需连接 PostgreSQL 并可访问。`repro_queued_leak.py` 还需要：

- 一个在线 Worker，`concurrency = 1`
- 组织配额 `max_concurrent_jobs >= 2`，否则 Hub 会先以 `quota_exceeded`
  拒绝第二个作业，根本无法构造出排队状态
- ComfyUI（或 mock）响应足够慢，让第一个作业稳定占住并发 slot

凭据通过环境变量传入，不要写进命令行历史：

```bash
export NAGISALAKE_BASE_URL=http://127.0.0.1:9091
export NAGISALAKE_EMAIL=you@example.com
export NAGISALAKE_PASSWORD='...'
```

取消作业需要 `jobs:cancel`，`nsk_` API Key 默认不带这个 scope，所以这些脚本
使用浏览器登录而非 API Key。

## 脚本

| 脚本 | 缺陷 |
| --- | --- |
| `repro_queued_leak.py` | 取消排队作业后 `queued_jobs` 永不回落，设备永久判定已满 |
| `repro_upload_quota.py` | 只申请上传、不传字节即可占满组织存储配额，且无回收手段 |
| `repro_login_timing.py` | 登录对未注册邮箱跳过 Argon2，形成可枚举邮箱的计时边信道 |
| `repro_login_blocking.py` | 同步 Argon2 占满 Tokio 执行线程，拖慢无关请求 |

```bash
./scripts/repro/repro_queued_leak.py
./scripts/repro/repro_upload_quota.py
./scripts/repro/repro_login_timing.py
./scripts/repro/repro_login_blocking.py
```

`repro_upload_quota.py` 会占用配额且**无法自动释放**（这正是缺陷本身）。它默认
以 dry-run 模式运行，只报告是否可能占满；加 `--commit` 才真正申请。修复后应改用
`--commit` 验证过期回收是否生效。
