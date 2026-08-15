# Nagisalake 架构

## 组件边界

- `nagisalake-hub` 部署在云端，提供消费者 HTTP API，并维护单实例 Worker 会话目录。
- `nagisalake-worker` 从边缘节点主动建立 WebSocket/SMUX 连接，不要求 NAT 入站端口。
- Worker 可作为独立 CLI 运行，也可通过 PyO3 扩展嵌入 ComfyUI。嵌入模式下，工作流中的
  `Nagisalake Hub Worker` 节点执行时启动进程级 Tokio Worker 并注册 Hub；节点不自行创建
  `DispatchJob`。
- Worker 当前只执行本机 ComfyUI 的 allowlist workflow，不接受消费者提交任意 workflow JSON。
- 普通参数在 HTTP/JSON 请求体与控制消息中传输；图片、音频和视频只通过私有
  S3-compatible 对象存储的短期预签名 GET/PUT 传输。

```text
消费者 ──Bearer/JSON──> Hub ──JSON control over WSS/SMUX──> Worker
   │                    │                                      │
   │ presigned PUT/GET  │ presign + HEAD                       │ localhost HTTP
   └────────────────> 私有对象存储 <───────────────────────────┤
                                                              └──> ComfyUI
```

控制协议不承载 base64 或二进制媒体。Hub 只保存对象键和元数据，预签名 URL 不持久化。

## 作业流程

1. 消费者调用 `POST /api/v1/artifacts/uploads` 申请输入对象 PUT。
2. 消费者按响应的 method、URL、headers 上传，再调用 complete 接口。
3. 消费者调用 `POST /api/v1/jobs`，请求体包含 workflow、参数和输入 artifact ID。
4. Hub 按 workflow capability 和当前并发量选择在线 Worker，发送 `DispatchJob` 并等待
   `CommandAck`。
5. Worker 把 dispatch 写入 SQLite journal，流式下载输入并校验大小与 SHA-256，再上传到
   ComfyUI、渲染 allowlist JSON Pointer、提交 `/prompt` 并轮询 `/history`。
6. Worker 从 `/view` 流式读取输出，向 Hub 请求 PUT 票据，上传并等待 HEAD 校验和 ACK。
7. 所有输出确认后 Worker 才发送 `completed`。消费者查询 job 并申请输出对象 GET。

状态事件使用单调 sequence 和 journal pending outbox。Worker 断线时作业任务等待新连接，重连后
继续重投未确认事件。取消只调用 ComfyUI `/queue` 删除队列项；进入 `uploading` 后拒绝取消。

## Workflow 契约

Worker 加载 workflow 时只对显式 allowlist 的 parameters 和 artifact inputs 生成 manifest，随注册
消息发送给 Hub。Hub 通过 `GET /api/v1/workflows` 聚合同一 `(id, version)` 的 manifest、Worker labels
和实时容量，并标记不同 Worker 之间的版本漂移。消费者依据 manifest 构造请求，但不接触或提交
任意 workflow JSON。

普通 ComfyUI 保存文件是 editor graph，不等同于 `/prompt` 所需的 API-format JSON。Worker 可以对
editor graph 做 best-effort 静态解析并生成 manifest warning，但生产执行应使用 API-format 导出。
契约字段、限制和后续 Nagisalake 输入/输出节点方案见
[WORKFLOW_MANIFEST_CN.md](WORKFLOW_MANIFEST_CN.md)。

## 安全边界

- Worker 与消费者使用不同 Bearer token，token 不接受 query 参数。
- Bucket 必须为 private；S3 凭据只应通过环境变量注入 Hub。
- workflow 模板只保存在 Worker，配置加载时验证所有 RFC 6901 pointer，重叠 pointer 会被拒绝。
- 输入在 Worker 端重算 SHA-256；输出由 Hub 用 HEAD 校验大小和签名时绑定的 SHA metadata。
- 单个对象当前使用单次 PUT，上限 5 GiB；更大媒体需要增加 multipart 协议。

## 当前部署约束

启用 `[database]` 后，Hub 会在启动时运行版本化 PostgreSQL migrations，并将用户、组织成员关系、
浏览器 session、API Key、Worker 凭据、设备分享、workflow manifest、artifact、job、事件、幂等键、
配额和审计日志持久化。Hub 仍把这些记录 hydrate 到本地内存以服务在线请求，因此当前部署仍是单个
Hub replica；Worker session、ACK waiter 和发送队列也只存在于进程内。Hub 重启后可恢复 job/artifact
视图和离线共享 workflow，Worker 重连时会重新绑定未完成任务并重新生成短期对象 URL。

要水平扩容，需要把 `SessionRegistry` 和 dispatch outbox 消费者拆成共享路由/队列（例如 Redis/NATS），
并为对象生命周期、过期上传和后台重试增加独立 worker；不能仅增加 Hub replica。

ComfyUI 当前通过 `/history` 轮询，只上报关键状态，不包含节点级实时进度。对象存储目前没有
multipart、生命周期清理或孤立产物回收任务。
