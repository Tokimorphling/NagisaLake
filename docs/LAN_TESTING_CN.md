# 局域网联调指南

在一台机器上跑 Hub，让局域网其他设备访问控制台、接入 ComfyUI 设备、提交作业。
仅用于可信网络的开发联调，不是生产部署方案，生产配置见
[DEPLOYMENT_CN.md](DEPLOYMENT_CN.md)。

## 三个必须对外可达的服务

局域网联调最常见的失败是只把 Hub 对外开放，忘了另外两个：

| 服务 | 端口 | 谁访问它 | 绑定要求 |
| --- | --- | --- | --- |
| Hub | 9091 | 浏览器、SDK、Worker | `0.0.0.0` |
| 对象存储 | 9000 | 浏览器、SDK、Worker **直连** | `0.0.0.0` |
| ComfyUI | 8188 | 只有同机的 Worker | 回环即可 |

对象存储必须对外可达，因为媒体不经过 Hub：Hub 只签发预签名 URL，客户端和 Worker
直连对象存储收发字节。如果 `object_store.endpoint_url` 写成 `127.0.0.1`，局域网设备
拿到的 URL 会指向它自己，症状是作业卡在 `uploading` 后失败，而 Hub 本身完全正常。

## Hub 侧

```toml
[server]
listen = "0.0.0.0:9091"

[browser]
# 局域网 HTTP，必须为 false
cookie_secure = false
# 内嵌控制台与 API 同源，无需登记自己的 origin
allowed_origins = ["http://localhost:3000"]

[object_store]
# 必须是局域网可达地址
endpoint_url = "http://192.168.31.102:9000"
```

启动：

```bash
export NAGISALAKE_S3_ACCESS_KEY_ID=minioadmin
export NAGISALAKE_S3_SECRET_ACCESS_KEY=minioadmin
./target/release/nagisalake-hub --config nagisalake-hub.local.toml
```

日志里的 `console=true` 表示控制台已编译进二进制，直接访问
`http://<Hub 地址>:9091` 即可，不需要单独跑 Vite。

> **DHCP 陷阱**：`endpoint_url` 里的地址是硬编码的。本机 IP 变化后（续约、换网络）
> 上传下载会突然失效，而 Hub 健康检查依然正常。用 `ipconfig getifaddr en0`
> （macOS）或 `hostname -I`（Linux）核对，或给这台机器配固定 IP。

MinIO 的端口映射也要绑到 `0.0.0.0`：

```bash
docker run -d --name nagisalake-minio \
  -p 0.0.0.0:9000:9000 -p 0.0.0.0:9001:9001 \
  -v nagisalake-minio-data:/data \
  -e MINIO_ROOT_USER=minioadmin -e MINIO_ROOT_PASSWORD=minioadmin \
  quay.io/minio/minio:latest server /data --console-address ":9001"

docker exec nagisalake-minio mc alias set local http://127.0.0.1:9000 minioadmin minioadmin
docker exec nagisalake-minio mc mb --ignore-existing local/nagisalake-private
```

浏览器直传还需要 bucket 允许该前端 origin 的 CORS `PUT/GET/HEAD`。用 SDK 或
本仓库的脚本调用时不受 CORS 限制，只有浏览器上传会受影响。

### 非安全上下文的降级

局域网 HTTP origin 不是安全上下文，浏览器会关掉一部分 API。实测 Chrome 访问
`http://192.168.x.x`：

| API | 局域网 HTTP | 控制台的处理 |
| --- | --- | --- |
| `crypto.subtle` | 不可用 | 改用分块 SHA-256（`api/hash.ts`） |
| `crypto.randomUUID` | 不可用 | 用 `getRandomValues` 构造 v4 UUID |
| `crypto.getRandomValues` | 可用 | 直接使用，回退不损失随机性质量 |
| `navigator.clipboard` | 不可用 | 退回 `execCommand('copy')` |

这些降级只为让局域网联调可用，**不会让 HTTP 变安全**。正式部署仍必须 HTTPS，
届时全部走原生实现。相关回退都有单元测试（`api/hash.test.ts`、
`lib/platform.test.ts`）。

## 接入一台 ComfyUI 设备

设备只建立出站连接，所以那台机器不需要开放任何入站端口。完整配置参考与校验规则见
[WORKER_ONBOARDING_CN.md](WORKER_ONBOARDING_CN.md)。

1. 在控制台「凭据 → Worker 凭据」创建 `nwk_` 凭据。明文只显示一次。
   填了 `allowed_namespace` 的话，Worker 配置里的 `namespace` 必须与它一致。
2. 把 [`examples/nagisalake-worker-lan.toml`](../examples/nagisalake-worker-lan.toml)
   拷到那台机器，改三处：`hub.url` 指向 Hub 的局域网地址、`hub.token` 填 `nwk_` 明文、
   `worker.namespace` / `node_name` 标识这台设备。
3. 启动：

   ```bash
   RUST_LOG=info ./nagisalake-worker --config worker.toml
   ```

   看到 `worker registered with Hub` 即成功，控制台的设备页会变成「在线」。

`namespace + node_name` 决定设备身份，改名等于换一台新设备。同一组织内相同身份
不能被另一个用户的凭据接管。

只有 `[[workflows]]` 里显式列出的 workflow 会注册给 Hub。调用方永远不能提交任意
workflow JSON，公共契约就是配置里声明的参数与输入位。

### 并行度与队列深度

设备的容量由两个独立的数字决定，不要混用：

```toml
[worker]
# 同时执行几个作业。由显存和引擎决定，调大会真的并发跑。
parallelism = 1
# 除执行中的之外，还能接收几个等待项。调大只增加等待位，不增加并发。
queue_depth = 4
```

Hub 的准入规则是 `执行中 + 排队中 < parallelism + queue_depth`。所以上面的配置能接 5 个作业：
1 个在跑、4 个排队，第 6 个才返回 `unavailable`。

`queue_depth = 0` 等价于旧行为：满载即拒绝提交。

排队发生在 Worker 侧的信号量之后，因此 ComfyUI 同时最多收到 `parallelism` 个 prompt——调大
`queue_depth` 不会把 ComfyUI 压垮。也因为这样，只有 `parallelism > 1` 时 ComfyUI 自身的队列才会
非空（它默认串行执行 prompt），作业详情里的 ComfyUI 队列位置也只在那种配置下才会出现。

控制台的 workflow 卡片会显示 `执行中 a/p · 排队 q/d`，四种状态含义：

| 状态 | 含义 | 能否提交 |
| --- | --- | --- |
| 可用 | 有空闲执行槽 | 能，立即执行 |
| 可排队 | 执行槽满但队列有位 | 能，进队列等待 |
| 忙碌 | 执行槽和队列都满 | 不能，等当前作业结束 |
| 离线 | 没有在线设备 | 不能 |

### manifest 漂移

同一个 `(id, version)` 如果上报了不同的 manifest，Hub 会把该版本的
`approval_state` 置为 `drifted`，它会从目录中消失，防止调用方拿到不稳定的契约。
契约变化应发布新 `version`，不要原地改。

排查：

```sql
SELECT workflow_id, version, approval_state FROM workflow_versions;
```

## 跑冒烟测试

[`scripts/smoke_test.py`](../scripts/smoke_test.py) 只用 Python 3 标准库，可以直接
拷到任何局域网设备上运行，不需要装依赖。它走的是 SDK 的真实路径：认证、列目录、
上传输入、提交作业、轮询、下载输出并校验 sha256。

先在控制台创建一个 `nsk_` API Key，勾选
`workflows:read`、`jobs:read`、`jobs:write`、`artifacts:read`、`artifacts:write`：

```bash
# 完整链路
./scripts/smoke_test.py --base-url http://192.168.31.102:9091 \
    --api-key nsk_... --output-dir ./outputs

# 指定 workflow 和参数
./scripts/smoke_test.py --base-url http://192.168.31.102:9091 --api-key nsk_... \
    --workflow sdxl-txt2img --param prompt "a cat" --param seed 42

# 只查连通性和目录，不提交
./scripts/smoke_test.py --base-url http://192.168.31.102:9091 --api-key nsk_... --no-submit
```

退出码：`0` 全部通过，`1` 有失败，`2` 前置条件不满足（例如没有在线设备）。

`--api-key` 无法读取 `/devices`，那是浏览器专用接口，脚本会跳过并继续。

## 常见故障对照

| 现象 | 原因 |
| --- | --- |
| 局域网打不开控制台，本机正常 | `listen` 还是 `127.0.0.1`；或本机 IP 已变；或防火墙 |
| 登录成功，十几分钟后被登出 | refresh 被拒。内嵌控制台是同源的应当自动通过；独立托管的前端需要把 origin 写进 `allowed_origins` |
| 作业停在 `uploading` 然后失败 | `endpoint_url` 指向 `127.0.0.1`，或对象存储没绑 `0.0.0.0` |
| 作业 `failed`，错误提到 `127.0.0.1:8188` | 那台设备上 ComfyUI 没启动 |
| 提交返回 `unavailable` | 没有在线且有余量的设备 |
| workflow 在目录里消失 | manifest 漂移，`approval_state` 变成 `drifted` |
| 浏览器上传报 CORS | bucket 没允许该前端 origin |

## 安全边界

`registration_enabled = true` 配合 `0.0.0.0` 意味着同网络的任何人都能注册账号。
当前实现还没有邮箱验证、密码找回、MFA 和登录限流，所以：

- 只在可信网络里开着注册，否则改成 `false` 由 owner 手动建账号。
- 不要把这套配置暴露到公网。公网开放前需要补齐的能力见
  [PUBLIC_PRODUCT_API_CN.md](PUBLIC_PRODUCT_API_CN.md) 末尾。
- `cookie_secure = false` 仅限局域网 HTTP。走 HTTPS 时必须改回 `true`。
