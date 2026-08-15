# Worker 接入指南

本文介绍如何把一台运行 ComfyUI 的机器接入 Nagisalake Hub 并开始执行作业。适用于真实设备接入，
也兼容本地联调；局域网场景的最小步骤见 [LAN_TESTING_CN.md](LAN_TESTING_CN.md)，生产部署见
[DEPLOYMENT_CN.md](DEPLOYMENT_CN.md)。

## 架构：Worker 是出站连接

Worker 不开放任何入站端口。它主动从边缘机器发起 WebSocket 连接
`ws(s)://<hub>/v1/worker/connect`，通过 SMUX 多路复用控制面（注册、心跳、dispatch、事件 ACK）
和数据流（输入下载、输出上传）。媒体字节不经过 Hub 请求体：

```text
浏览器/SDK ──HTTPS/JSON──> Hub ──WSS/SMUX 控制+数据流──> Worker ──localhost──> ComfyUI
                              │                          │
                              └── presigned PUT/GET ─────┘
                                     (私有对象存储)
```

因此边缘机器不需要公网 IP、端口映射或反转代理；只有 Hub 需要对外可达。Worker 只执行
`[[workflows]]` 里显式 allowlist 的 ComfyUI API-format 工作流，消费者永远不能提交任意
workflow JSON。

## 前置条件

- Hub 已经部署并可访问（地址、服务端可用）。
- 边缘机器上有 ComfyUI，且端口默认 `127.0.0.1:8188`（可与 Worker 同机）。
- 一个由组织浏览器 session 创建的 `nwk_` Worker 凭据（明文只显示一次）。
- 一个 API-format 的 ComfyUI workflow JSON 文件。ComfyUI 保存的文件是 editor graph，
  生产执行应使用 `/prompt` 所需的 API-format 导出。

## 第一步：创建 Worker 凭据

浏览器在控制台（或直接调用 API）：

```http
POST /api/v1/organizations/{org_id}/worker-credentials
```

响应中的 `nwk_...` 明文只返回一次，之后列表只显示前缀。可以同时限制：

- `allowed_namespace`：配置里 `worker.namespace` 必须与它一致，否则注册被拒绝；
- 过期时间：到期后 Worker 无法重连。

`nwk_` 决定 organization、owner 和可选 namespace，Worker 注册消息不能改租户。同一组织内
相同 `namespace + node_name` 不能被另一用户的凭据接管。

## 第二步：写 Worker 配置

从仓库复制模板再改：

```bash
cp examples/nagisalake-worker.toml worker.toml
```

最小场景只需要四个关键点：

| 配置 | 说明 |
| --- | --- |
| `hub.url` | 必须是 `ws://` 或 `wss://` + `/v1/worker/connect` |
| `hub.token` | `nwk_` 明文；生产用 `NAGISALAKE_WORKER_TOKEN` 环境变量，别写进文件 |
| `worker.namespace` / `node_name` | 设备身份，与凭据的 `allowed_namespace` 一致 |
| `workflows[].file` | API-format workflow JSON 路径 |

启动命令：

```bash
RUST_LOG=info ./nagisalake-worker --config worker.toml

# 或指定 CONFIG_PATH 时可用环境变量传配置路径
NAGISALAKE_WORKER_CONFIG=worker.toml ./nagisalake-worker
```

看到 `worker registered with Hub`（日志含 `worker_id` 和 `session_id`）即成功，控制台设备页
会显示「在线」。启动失败会在启动阶段直接以明确错误退出，不会进入重连循环。

### 相对路径的解析基准

`work_dir`、`workflows[].file`、`state.sqlite_url` 和 `hub.tls.ca_certificates` 这四个相对路径
都按 **worker.toml 所在目录** 解析（不是进程启动目录）。把配置文件拷到别的机器时，相对的
workflow 和 TLS 文件要一起带去。

## 完整配置参考

```toml
# 执行时的临时目录（输入下载、输出缓冲）。相对 worker.toml 解析。
work_dir = "./worker-state/work"

[hub]
url = "wss://hub.example.com/v1/worker/connect"
# nwk_ 明文；为空时回退到环境变量 NAGISALAKE_WORKER_TOKEN
token = "..."
# 可选：通过 HTTP CONNECT 代理建立隧道，见下文「代理」
# proxy = "http://proxy.example.com:3128"
reconnect_max_seconds = 60        # 重连退避上限
connect_timeout_seconds = 15      # 连接/注册超时
max_frame_bytes = 1048576         # 单帧上限 1 MiB

[worker]
namespace = "home-gpu"            # 与凭据 allowed_namespace 一致
node_name = "comfyui-01"          # namespace + node_name 决定设备身份
parallelism = 1                   # 同时执行几个作业
queue_depth = 4                   # 除执行中之外的等待位
[worker.labels]                   # 展示用标签，可选
gpu = "rtx-4090"

[state]
sqlite_url = "sqlite://worker-state/worker.db"   # 执行 journal，持久化断线恢复

[comfyui]
base_url = "http://127.0.0.1:8188"
poll_interval_ms = 1000           # 轮询 /history 的间隔，最小 100
request_timeout_seconds = 60
max_output_bytes = 5368709120     # 单输出上限，最大 5 GiB

[[workflows]]
id = "sdxl-txt2img"
version = "v1"
file = "./workflows/sdxl-txt2img-api.json"
output_types = ["image/png"]

# 公共参数名 → workflow JSON 里的 JSON Pointer。
[workflows.parameters]
prompt = "/6/inputs/text"
seed = "/3/inputs/seed"

# 需要输入文件时声明。index 从 0 开始且必须连续：
# 提交作业时 input_artifact_ids 是位置数组，第 N 个 ID 对应 index = N 的绑定。
# [[workflows.inputs]]
# index = 0
# name = "source_image"
# content_type = "image/*"
# pointer = "/10/inputs/image"
```

### 配置校验要点

以下情况 Worker 启动即报错，不会带病重连：

- `hub.url` 空、或不是 `ws://` / `wss://`（把 `https://` 贴进来会立刻被拒绝）；
- `ws://` 且配置了 `hub.tls.ca_certificates`（那样什么都没加密）；
- 没有 `hub.token` 也没有 `NAGISALAKE_WORKER_TOKEN`；
- `worker.namespace` / `node_name` 为空；
- `parallelism = 0`；
- `queue_depth > 1024`；
- `connect_timeout_seconds` 或 `max_frame_bytes` 为 0；
- 没有任何 `[[workflows]]`；
- `poll_interval_ms < 100`；
- `max_output_bytes` 为 0 或超过 5 GiB。

## 环境变量

| 变量 | 用途 |
| --- | --- |
| `NAGISALAKE_WORKER_CONFIG` | CLI 的 `--config` 的等价物；ComfyUI 节点也会读它 |
| `NAGISALAKE_WORKER_TOKEN` | 当 `hub.token` 为空或为空串时作为 nwk_ 来源 |
| `NAGISALAKE_WORKER_PROXY` | 当 `hub.proxy` 为空时作为代理地址来源 |

`RUST_LOG` 控制日志级别；不设置时默认 `info`（容器里也不会因为空 filter 静默）。

## 三种运行方式

### 1. 独立 CLI（推荐用于无人值守设备）

```bash
./nagisalake-worker --config /etc/nagisalake/worker.toml
```

推荐用 systemd/任务计划守护，`restart=on-failure`。断线后 Worker 自会后端退避重连
（从 1 秒起倍增，上限 `reconnect_max_seconds`），不需要手动拉起。

### 2. ComfyUI 自定义节点

见 [integrations/comfyui_nagisalake/README.md](../integrations/comfyui_nagisalake/README.md)。
把 `integrations/comfyui_nagisalake` 装到 ComfyUI 的 `custom_nodes`，在工作流里放
`Nagisalake Hub Worker` 节点：节点第一次被排队执行时在后台启动进程级 Worker 并注册 Hub。
节点**不**创建 `DispatchJob`，所以它不能解决「Hub 要派第一条任务时节点还没执行过」的冷启动
循环——无人值守机器应改用独立 CLI，或用 ComfyUI 启动脚本先排队一次 bootstrap workflow。

### 3. Python 嵌入

`start_worker(config_path)` 返回一个句柄，提供 `status()`（starting/running/stopped/failed）、
`last_error()`、`stop()`。嵌入初始化有 30 秒超时。PyO3 必须与 ComfyUI 实际使用的解释器一致。

## 代理

从 2026-08 起支持通过 HTTP CONNECT 代理建立出站隧道（用于必须走公司代理才能出网的环境）：

```toml
[hub]
proxy = "http://proxy.example.com:3128"
```

- 只接受 `http://` scheme，默认端口 80；HTTPS 代理地址会被拒绝。
- 代理负责 CONNECT 隧道，TLS 依旧在 Hub 主机名上终止。所以 `wss://` 可以安全地穿过
  HTTP 代理，凭据不会暴露给代理。
- 不传 `hub.proxy`、也不设 `NAGISALAKE_WORKER_PROXY` 时直连。

## 容量：并行度与队列深度

**两个独立数字，不要混用：**

- `parallelism`：同时执行几个作业，由显存和引擎决定，调大真的会并发跑。
- `queue_depth`：除执行中外的等待位，调大只增加容纳量，不增加并发。

Hub 准入规则是 `执行中 + 排队中 < parallelism + queue_depth`。上面的示例（1 并发 + 4 排队）
能接 5 个作业，第 6 个才返回 `unavailable`。`queue_depth = 0` 等价于旧版「满载即拒绝」。

排队发生在 Worker 侧信号量之后，所以 ComfyUI 同时最多收到 `parallelism` 个 prompt。只有
`parallelism > 1` 时 ComfyUI 自身队列才会非空（默认串行执行 prompt），作业详情里的 ComfyUI
队列位置也只在那种配置下出现。

## 断线恢复与拉起未完成任务

执行状态写入 SQLite journal。Worker 启动时：

1. 恢复非终态记录并重新执行；
2. 把非终态 job id 清单随 `Register` 发给 Hub，让 Hub 知道哪些任务需要它的 cleanup 帮投。

journal 里的非终态任务超过 1024 个时 Worker 拒绝启动（保护恢复上限）。断线后任务等待新连接，
重连后重投未确认事件；取消只调用 ComfyUI `/queue` 删除队列项，进入 `uploading` 后拒绝取消。

注册被 Hub 拒绝（包括 `session_replaced`——同一身份被另一个 session 顶替）时，Worker 以
`registration failed` 退出本次连接并进入重连退避，不会恢复任务执行。

## 故障排查

| 现象 | 原因 |
| --- | --- |
| 启动即报 `hub.url must be a ws:// or wss://` | `hub.url` 写成了 `https://` |
| 启动即报 `worker.parallelism must be greater than zero` | `parallelism = 0` |
| 启动即报 `at least one workflow` | `[[workflows]]` 为空 |
| 日志反复 `registration failed` | nwk_ 过期、`namespace` 与  凭据不一致、或身份被顶替 |
| 一直 `hub control connection failed` | 网络不通、Hub 没起、代理地址写错 |
| 控制台设备离线 | Worker 进程没在跑，或重连退避中 |
| 作业 `failed`，错误里出现 `127.0.0.1:8188` | 那台设备上 ComfyUI 没启动 |
| workflow 没出现在目录里 | manifest 漂移（`approval_state = drifted`），或设备离线 |
| 作业卡 `uploading` 后失败 | 对象存储 `endpoint_url` 指向了客户端不可达地址（常见于 `127.0.0.1`） |

manifest 漂移排查：

```sql
SELECT workflow_id, version, approval_state FROM workflow_versions;
```

## 安全提示

- 生产必须 `wss://`；`ws://` 只用于可信局域网。
- 私有 CA 场景：`[hub.tls] ca_certificates` 必须放 **CA 证书**，自签名的服务器证书没有
  `CA:TRUE` 不会被信任。证书文件每次重连都会重新读取，轮换不用重启。
- `nwk_` 明文只显示一次，放 secret manager 或受限环境文件，不要提交到 Git。
- 吊销凭据会阻止重连并断开当前 session。