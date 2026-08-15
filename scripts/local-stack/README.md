# 本地全链路环境

一条命令拉起 Hub（含内嵌控制台）、PostgreSQL、MinIO、mock ComfyUI 和一个 Worker，
并验证「上传输入 → 派发 → 执行 → 回传输出 → 下载校验」整条链路。

```bash
./scripts/local-stack/stack.sh up       # 构建并启动，注册一个 Worker
./scripts/local-stack/stack.sh test     # 提交作业并校验输出 sha256
./scripts/local-stack/stack.sh status   # 进程、端点、设备容量、配额漂移
./scripts/local-stack/stack.sh logs worker
./scripts/local-stack/stack.sh down     # 停进程，保留数据
./scripts/local-stack/stack.sh reset    # 停进程并删除数据库、bucket、Worker journal
```

`up` 可重复执行。生成的配置和日志都在 `.local-stack/`（已 gitignore）。

要求 PostgreSQL、Docker、`pnpm`、`python3`。不需要 GPU 和模型权重：mock ComfyUI 返回一张
固定的 8×8 PNG。

## 常用变量

```bash
NAGISALAKE_LOCAL_PARALLELISM=2 NAGISALAKE_LOCAL_QUEUE_DEPTH=4 ./scripts/local-stack/stack.sh up
MOCK_PENDING_POLLS=400 ./scripts/local-stack/stack.sh up   # 让作业长期占住执行槽，便于观察排队
NAGISALAKE_LOCAL_HOST=192.168.1.20 ./scripts/local-stack/stack.sh up   # 指定对外地址
NAGISALAKE_LOCAL_MINIO_PORT=9002 ./scripts/local-stack/stack.sh up     # 9000 被占用时
```

## 三个最容易踩的坑

**改了 `web/` 但没重新构建 Hub。** 控制台被编译进二进制，只跑 `pnpm build` 不生效。
对比正在服务的 bundle 与磁盘上的：

```bash
curl -s http://127.0.0.1:9091/ | grep -o 'src="/assets/[^"]*"'
ls web/dist/assets/*.js
```

`stack.sh up` 两个都会重建。

**`pkill -f worker.toml` 匹配不到 Worker。** 命令行是 `--config worker.toml`（相对路径）。
每次「重启」都会留下旧进程，多个 Worker 共用同一身份会互相顶替会话，看起来像 Hub 的 bug。
用 `pkill -9 -f 'release/nagisalake-worker'`，`status` 会在发现多个时警告。

**上传失败但 Hub 一切正常。** 媒体不经过 Hub，客户端直连对象存储。所以
`object_store.endpoint_url` 必须是客户端可达的地址，且 MinIO 要绑 `0.0.0.0`。
本机 IP 变化后旧地址会失效，`up` 会按当前地址重新生成配置。

完整清单和排查顺序见
[`.claude/skills/nagisalake-local-stack/SKILL.md`](../../.claude/skills/nagisalake-local-stack/SKILL.md)。
