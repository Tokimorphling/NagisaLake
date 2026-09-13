# Agent / OpenCode 接入

## 边界与组合方式

`crates/nagisalake-agent` 是独立的运行时库，不依赖 Axum、Hub 状态或 PostgreSQL。

```text
Studio / API client
  ├─ POST run / run stream
  └─ POST cancel
       ↓ Hub：认证、组织速率/并发限制、执行记录
AgentService<OpenCode> : Service<RunRequest>
       ↓ Backend::execute(Execution) -> impl Future + Send
OpenCode HTTP client + session event router + persisted-message reconciler
```

- `AgentService<B>` 用泛型静态派发实现 workspace 现有的 `service_async::Service`。
- `Backend` 在 Tokio task 边界要求 `Send` future。现有 `service_async::Service` 本身不要求
  `Send`，因此不通过 `async_trait`、`Box<dyn Future>` 或 `dyn Backend` 弥补这个差异。
- 后续接入其他 agent API，只需实现 `Backend::execute`，返回 `Completion`，通过
  `Execution.events.send(Progress::...)` 上报进度。鉴权、前端事件和业务数据库无需跟着上游 schema 改动。
- “零开销”指抽象层没有动态分发/额外 boxed future，不代表网络、任务、channel 或 JSON 没有分配。
- 当前是独立的 **文本 skill 执行**：前端发送任务、接收进度、取消任务、显式应用结果。
  不开放任意 shell/MCP、任意 agent/system prompt、工具授权弹窗或共享多轮会话。

## 配置

`examples/nagisalake-hub.toml` 已有本地示例。省略整个 `[opencode]` 不影响原有 Hub。
仅构造 client，不在 Hub 启动时强制访问 OpenCode。

```toml
[opencode]
base_url = "http://127.0.0.1:4096"
allowed_skills = ["h3-prompt-writing"]
agent = "build"
skill_version = "1"
max_concurrent_runs = 16
max_runs_per_organization = 2
run_timeout_seconds = 120
poll_interval_ms = 2000
request_timeout_seconds = 15
event_buffer = 256
upstream_buffer = 256
cleanup_sessions = true
# 非 loopback 地址必须配置：
# password_env = "OPENCODE_SERVER_PASSWORD"
# directory = "/workspace/skills"
[opencode.model]
provider_id = "opencode"
model_id = "mimo-v2.5-free"
```

provider/model 应使用 OpenCode `/provider` 返回的实际 ID；例如 OpenCode Zen 对应 `opencode`，
MiMo V2.5 Free 对应 `mimo-v2.5-free`。`agent = "build"` 不是模型选择；省略 model 时由服务端决定。
caller 日志记录 `requested_provider` / `requested_model`，省略时明确标为 `server_default`，不把
请求配置当成服务端实际模型的独立回执。`llm.runtime=ai-sdk` 是 OpenCode 内部信息，不属于 caller 配置。

`allowed_skills` 默认空列表，**拒绝全部 skill**。`GET /skill` 的发现结果不自动成为授权列表。
名称必须是字母/数字/`-`/`_`，不接受 glob 或路径。`skill_version` 是部署管理的业务版本标记，
不是从上游 skill 文本自动推导的版本。

生产建议使用受限的专用 `enhance` agent，并固定 OpenCode 镜像版本。每次执行还会在新会话上设置
`* → deny`、`skill:<批准的名称> → allow`，不把用户 options 当作权限或 provider 配置。
如果某个 skill 必须运行命令或生成媒体，这个文本接口会拒绝相关工具；不要为了“跑通”移除默认拒绝规则。

默认输入上限 64 KiB、输出上限 1 MiB、单条上游 SSE 上限 1 MiB、单次上游 JSON 响应上限 8 MiB。
超限失败而不是无限增长内存。密码只从环境变量读取；base URL 不接受 userinfo、query 或 fragment。

## HTTP 契约

| 路径 | 行为 |
| --- | --- |
| `GET /api/v1/agent/skills` | 返回 `{ enabled, items: [{ id, version }] }` |
| `POST /api/v1/agent/runs` | 等待终态，返回统一的 completed/error/cancelled 事件 |
| `POST /api/v1/agent/runs/stream` | 同一请求格式，返回 SSE；不自动重放 POST |
| `POST /api/v1/agent/runs/{id}/cancel` | 取消自己的执行；已结束的持久化任务可幂等返回 204 |
| `GET /api/v1/agent/runs/{id}` | 查询自己的业务执行记录，需要 PostgreSQL |
| `POST /v1/enhance`、`POST /v1/enhance/stream` | 任务文档中的兼容入口，复用相同认证和执行逻辑 |

创建/目录接口使用现有 `jobs:write` 权限，历史读取使用 `jobs:read`，取消使用 `jobs:cancel`。
公开模式接受浏览器 session/API key，检查组织与 owner；Worker 凭据不接受。
未配置 PostgreSQL 的本地模式仍须提供 legacy consumer bearer token，绝不匿名执行。
浏览器 token 继续只在内存中，组织通过 `X-Organization-ID` 传入并由 Hub 验证。

请求：

```json
{"skill":"h3-prompt-writing","input":"一只纸船漂过池塘；仅编写提示词","options":{}}
```

允许的公共事件：

```text
started         { execution_id, skill }
stage_started   { call_id, name }
stage_finished  { call_id, name }
stage_failed    { call_id, name }
text_delta      { text }
warning         { code }
completed       { execution_id, text }
error           { execution_id, code, message }
cancelled       { execution_id }
```

JSON 中另有 `type` 字段，值与 SSE event 名称一致。`completed.text` 是权威最终结果，应替换之前的
预览，不要再追加一次。中途断流且没有终态是未完成，不能当作成功，也不能自动重发 POST。
当前不支持 SSE 游标重放；PostgreSQL 模式可用执行 ID 查询结果。

Studio 的提示词区域提供 skill 选择、流式预览、取消和“将结果应用到提示词”。结果不会自动覆盖用户
在生成过程中继续编辑的草稿；切换组织或卸载组件会中止该次请求。

## 可靠性与安全

1. 每次执行新建 OpenCode 会话，订阅 session 路由后才发送 `prompt_async`。
2. 每个 backend 实例仅一条上游 `/event` 连接，按 session ID 路由到有界队列，不向每个任务广播全站事件。
3. SSE 断连会退避重连；即使初次 SSE 连接失败，持久化消息轮询仍可完成任务。
4. `session.idle` 只触发核对。必须看到最终 assistant 完成标记；`finish=tool-calls` 不算完成，
   provider error 不会被转换成空字符串成功。还必须看到批准的 skill 工具调用成功。
5. 工具状态单调去重；只输出已确认的 assistant text part，不转发 reasoning、工具参数、输出、标题或原始错误体。
6. 整个执行 future（包括建会话、HTTP、轮询、发送进度）在统一取消/超时范围内。独立终态通道不受满进度队列阻塞。
7. 丢弃 `RunHandle` 请求取消；已有 session 的 guard 会发出有界 abort/cleanup。短生命周期 CLI 退出前调用
   `OpenCodeClient::wait_for_cleanup()`。网络不可达时 abort 是 best effort，并不承诺远端已停止计费。
8. JSON/请求/事件错误只报告代码或 HTTP 状态，不把 URL、密码、prompt、工具输出写入日志。
   tracing 记录 execution ID、session ID（仅内部）、skill、状态和耗时。

OpenCode 会话创建请求发出后、收到 session ID 前发生断连，可能留下一个尚未提交 prompt 的空会话；
OpenCode API 没有本地可以依赖的创建幂等键。不要复用或扫描其他用户的会话来“补救”。

## PostgreSQL 与单实例边界

前向迁移 `0016_agent_executions.sql` 保存输入、options、结果、业务 skill 版本、owner 和时间戳。
工具内容和 reasoning 不保存；session ID 也不是业务主键。
读取同时限定组织和 owner；组织/用户删除有级联清理。终态条件更新防止晚到的取消覆盖结果。

结果观察器异步持久化终态，和 SSE 交付并非一个原子事务。持久化失败会记录错误；进程中断遗留的
running 记录在 deadline 过期后读取为 `failed/interrupted`，而不是永久假装运行。
不配数据库时没有跨请求/重启历史保存。

并发限制与取消路由目前是**单 Hub 实例**语义，和现有 Worker session 目录一致。横向扩容前需增加
共享 admission/执行 owner 租约与跨实例取消路由，不应直接部署多个副本来扩大同一组织额度。

## 验证

```bash
cargo test -p nagisalake-agent
cargo test -p nagisalake-hub --lib
# 设置专用测试库后再跑真实 PostgreSQL 用例：
cargo test -p nagisalake-hub-store --test agent_executions

# 只探测服务与列出 skills，不调用模型：
cargo run -p nagisalake-agent --example opencode -- \
  --config examples/nagisalake-hub.toml --list-skills

# 显式真实调用；会消耗已配置 provider 的额度：
cargo run -p nagisalake-agent --example opencode -- \
  --config examples/nagisalake-hub.toml --skill h3-prompt-writing \
  --input 'Write a short text-only H3 prompt for an origami boat on a pond.'
```

最初对本地 `1.18.15` 的未指定模型请求，先发现并修复了 user `summary` 为对象而非 bool 的兼容问题，
随后模型执行分别返回 provider error、120 秒 timeout。此前的失败日志没有足够信息还原服务端实际选择的
模型，因此不能将根因直接断定为某个 provider 或 caller 故障。

后续按 `/provider` 核对并显式指定 `opencode / mimo-v2.5-free`，真实 `h3-prompt-writing` 调用已成功：
执行 ID `418ba285-de3a-4bf4-ba8a-917fe7b36f6e`，耗时约 32.811 秒，观察到四个不同 call ID 的 skill
完成事件、连续 `text_delta` 和最终 `completed`。最终成功还经过了“批准的 skill 名称 + 工具完成状态”
的内部校验。此结果确认调用链路可工作，但不代表模型一定满足提示词中的每项内容/格式约束。
