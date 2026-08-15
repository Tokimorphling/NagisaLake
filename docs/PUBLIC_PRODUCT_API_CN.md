# 公开服务与前端 API

本文描述仓库当前已经实现的公共控制面，以及真正上线前仍需补齐的能力。接口实现位于
`apps/nagisalake-hub/src/product_api.rs`，统一前缀为 `/api/v1`。旧版 `/v1` token API
仅用于兼容和本地 mock，不应作为新前端的接口。

机器可读契约位于 [`openapi.yaml`](openapi.yaml)。SDK 应基于该文件生成或维护兼容层；
`web/src/api` 是本仓库控制台的内部 client，不是对外 SDK。

## 是否需要前端

作为内部执行引擎，Nagisalake 不依赖前端，CLI、SDK 和 ComfyUI 节点已经可以完成接入。作为
允许多个账户注册、管理自己的设备并分享算力的公共产品，则需要一个前端。它不是执行链路的
前置条件，而是账户、设备、workflow、job、配额和审计的操作台。

建议第一版只做一个工作台，不先做营销站：

- 登录/注册和组织切换。
- 我的设备、Worker 凭据、设备邀请码和共享对象。
- workflow 目录，以及由 manifest 驱动的 job 表单。
- job 列表、状态、输出下载和取消。
- API Key、配额、成员角色和审计。

## 从 `ref/sub2api` 借鉴的边界

`ref/sub2api` 中值得保留的是前后端边界：统一 HTTP client、路由权限、浏览器认证与程序
API Key 分离、refresh 并发协调、结构化错误、敏感动作审计。Nagisalake 没有照搬余额、支付、
渠道账号或复杂分组定价。

Nagisalake 还做了两处不同选择：

- refresh token 使用 `Secure`、`HttpOnly`、`SameSite=Lax` cookie；access token 只应放在
  前端内存，不放 `localStorage`。
- CSRF cookie 是可读的根路径 cookie，前端在 refresh 时把它复制到 `X-CSRF-Token`；refresh cookie
  仍限制在 `/api/v1/auth` 且为 HttpOnly。
- Worker 使用独立的 `nwk_` enrollment token；它既不是浏览器 session，也不是消费者 API Key。

前端实现位于 `web/`，可以用 `--features embed-web` 静态编译进 Hub 二进制，此时控制台与 API 同源。
Hub 自身不发送 CORS 响应头，独立托管的前端必须通过同源反向代理接入，或在代理层配置 CORS。

## 认证契约

凭据类型不能混用：

| 前缀 | 用途 | 保存位置 |
| --- | --- | --- |
| `nss_` | 短期浏览器 access token | 前端内存，使用 Bearer header |
| `nsr_` | 可轮换 refresh token | HttpOnly cookie，仅发往 `/api/v1/auth` |
| `nsc_` | refresh 的 CSRF token | 响应体和可读 cookie |
| `nsk_` | 程序 API Key | 调用方 secret store，明文只返回一次 |
| `nwk_` | Worker enrollment token | 边缘设备 secret store，明文只返回一次 |
| `ndi_` | 设备邀请码 | 通过受信渠道交给被邀请用户 |

登录、注册和 refresh 成功时返回：

```json
{
  "access_token": "nss_...",
  "token_type": "Bearer",
  "access_expires_at": 1786000000000,
  "refresh_expires_at": 1788500000000,
  "csrf_token": "nsc_...",
  "user": {
    "id": "...",
    "email": "user@example.com",
    "status": "active",
    "email_verified": true,
    "created_at": 1785000000000
  },
  "current_organization_id": "..."
}
```

公开账户采用 OAuth-only：新用户通过已配置的 Google、GitHub 或 OIDC provider 创建账户，邮箱
验证和账户恢复由 provider 负责，Hub 不发送注册验证邮件，也不提供密码找回/改密码流程。密码
登录和注册路由只用于 loopback 或受控本地兼容环境；公网配置应保持
`password_auth_enabled = false`。

普通请求使用 `Authorization: Bearer nss_...`。浏览器切换组织时增加
`X-Organization-ID: <organization_id>`；服务端会再次检查 membership。API Key 固定绑定一个
organization，不能用该 header 跨租户。

`POST /api/v1/auth/refresh` 必须同时满足：

- 浏览器以 `credentials: "include"` 发送 refresh 和 CSRF cookie。
- `X-CSRF-Token` 与 CSRF cookie 相同。
- `Origin` 位于 Hub 的 `browser.allowed_origins`，或与请求的 `Host` 同源。同源请求本身不构成
  CSRF，因此嵌入式控制台无需登记；比较只取 authority（`host[:port]`），以适配代理终止 TLS 时
  Hub 看到 `http` 而浏览器发送 `https` 的情况。
- refresh token 尚未使用、过期或撤销；每次 refresh 都原子轮换，旧 token 重放返回 401。

前端收到 401 时只允许一个 tab/请求执行 refresh，成功后重试原请求一次。程序调用始终使用
`nsk_`，不调用 refresh，也不复用浏览器 token。

错误格式稳定为：

```json
{
  "error": {
    "code": "forbidden",
    "message": "missing permission devices:read",
    "request_id": "..."
  }
}
```

响应同时返回 `X-Request-ID`。当前 code 包括 `unauthorized`、`forbidden`、`not_found`、
`invalid_request`、`conflict`、`quota_exceeded`、`unavailable`、`upstream_error` 和
`internal_error`。

## 已实现 API

除公开设置、OAuth start/callback、兼容注册/登录和 refresh 外，所有接口都要求 Bearer token。表中的“浏览器”表示明确
拒绝 API Key；“两者”表示浏览器 session 或拥有对应 scope 的 API Key。

### 账户与组织

| Method | Path | 调用者与用途 |
| --- | --- | --- |
| `GET` | `/api/v1/settings/public` | 公开；注册开关、上传上限和认证类型 |
| `GET` | `/api/v1/openapi.yaml` | 公开；返回当前 Hub 使用的机器可读 OpenAPI 3.1 契约 |
| `GET` | `/api/v1/auth/oauth/providers` | 公开；列出可用 OAuth provider |
| `GET` | `/api/v1/auth/oauth/{provider}/start` | 公开；启动授权码 + PKCE 登录 |
| `GET` | `/api/v1/auth/oauth/{provider}/callback` | 公开；完成 OAuth 登录或创建账户 |
| `POST` | `/api/v1/auth/register` | 仅本地兼容；密码注册，公网 OAuth-only 配置会禁用 |
| `POST` | `/api/v1/auth/login` | 仅本地兼容；密码登录，公网 OAuth-only 配置会禁用 |
| `POST` | `/api/v1/auth/refresh` | refresh cookie + CSRF + Origin；轮换 session |
| `POST` | `/api/v1/auth/logout` | 浏览器；撤销当前 session 并清 cookie |
| `POST` | `/api/v1/auth/revoke-all-sessions` | 浏览器；撤销该用户所有 session |
| `GET` | `/api/v1/auth/me` | 浏览器；用户、全部 membership、当前组织 |
| `DELETE` | `/api/v1/auth/me` | 浏览器；删除账户；仍是组织 owner 时拒绝，需先转移或删除组织 |
| `GET` | `/api/v1/organizations` | 浏览器；列出当前用户的组织 |
| `POST` | `/api/v1/organizations` | 浏览器；创建组织并成为 owner |
| `GET` | `/api/v1/organizations/{org}/members` | 两者；admin/owner |
| `PATCH` | `/api/v1/organizations/{org}/members/{user}` | 两者；修改 role，禁止删除最后一个 owner |
| `DELETE` | `/api/v1/organizations/{org}/members/{user}` | 浏览器；admin/owner 移除成员并撤销其 session、Key、Worker 凭据 |
| `GET` | `/api/v1/organizations/{org}/member-invites` | 浏览器；admin/owner 列出组织邀请码 |
| `POST` | `/api/v1/organizations/{org}/member-invites` | 浏览器；admin/owner 创建一次性成员邀请码 |
| `DELETE` | `/api/v1/organizations/{org}/member-invites/{id}` | 浏览器；撤销未使用的邀请码 |
| `POST` | `/api/v1/organization-invitations/accept` | 浏览器；接受 `noi_` 成员邀请码并加入组织 |
| `POST` | `/api/v1/organizations/{org}/owner-transfer` | 浏览器；owner 原子转移给现有成员 |
| `GET` | `/api/v1/organizations/{org}/export` | 浏览器；admin/owner 导出组织控制面 JSON，不含 secret 明文 |
| `DELETE` | `/api/v1/organizations/{org}` | 浏览器；owner，JSON body 的 `confirm` 必须等于组织 ID |
| `GET` | `/api/v1/organizations/{org}/quota` | 两者；`quota:read` |
| `PATCH` | `/api/v1/organizations/{org}/quota` | 浏览器；admin/owner 调整配额策略 |
| `GET` | `/api/v1/organizations/{org}/audit-logs` | 两者；admin/owner，keyset 分页 `?limit=&cursor=` |

成员邀请码只授予组织 membership；设备邀请码仍只授予特定设备的使用权，不会把用户加入设备所属组织。

### API Key 与 Worker 凭据

| Method | Path | 调用者与用途 |
| --- | --- | --- |
| `GET` | `/api/v1/organizations/{org}/api-keys` | 浏览器；member 看自己的，admin/owner 看全部，keyset 分页 |
| `POST` | `/api/v1/organizations/{org}/api-keys` | 浏览器；创建 `nsk_`，明文只返回一次 |
| `DELETE` | `/api/v1/organizations/{org}/api-keys/{id}` | 浏览器；本人或 admin/owner 撤销 |
| `GET` | `/api/v1/organizations/{org}/worker-credentials` | 浏览器；本人或 operator 以上查看，keyset 分页 |
| `POST` | `/api/v1/organizations/{org}/worker-credentials` | 浏览器；创建 `nwk_`，可限制 namespace/过期时间 |
| `DELETE` | `/api/v1/organizations/{org}/worker-credentials/{id}` | 浏览器；撤销并主动断开当前 Worker session |

API Key 请求体为 `{"name":"sdk","scopes":["workflows:read","jobs:write"],"expires_in_seconds":...}`。
可用 scope 为：

```text
workflows:read  workflows:write  jobs:read       jobs:write
jobs:cancel     artifacts:read   artifacts:write workers:manage
members:manage  api_keys:manage  quota:read      quota:manage
audit:read      devices:read     devices:use     devices:register
devices:share
```

scope 不能提升创建者的组织角色；请求必须同时通过 role 和 scope。workflow 发布仍是后续能力，
quota 策略已可由组织 admin/owner 通过上述 `PATCH` 路由管理。

quota 请求体：

```json
{
  "max_concurrent_jobs": 4,
  "max_storage_bytes": 10737418240,
  "max_jobs_per_period": 1000,
  "period_seconds": 86400
}
```

组织删除请求：

```json
{"confirm":"organization-id"}
```

删除会先删除组织 artifact 对象，再删除 PostgreSQL 控制面记录；对象存储删除失败时返回错误并
保留组织元数据，客户端可以重试。导出是同步 JSON 响应，包含成员、作业、事件、artifact 元数据、
workflow、设备、凭据元数据、配额和审计记录；API Key/Worker token 只包含 prefix，不包含 hash 或
明文。账户删除会删除该用户的 OAuth identity、session、自有设备和凭据；其加入的共享组织数据不
会因账户删除而删除。

Hub 会定期从 `jobs` 表对账 `quota_usage.active_jobs`。长时间没有 Worker heartbeat 的非终态 job
会标记为 failed 并释放并发占用；对账结果通过 `/metrics` 暴露。

### 设备注册与分享

| Method | Path | 调用者与用途 |
| --- | --- | --- |
| `GET` | `/api/v1/devices` | 浏览器 member 以上；列出本人拥有和被分享的设备，keyset 分页，返回连接状态和脱敏 workflow 摘要 |
| `POST` | `/api/v1/device-invites` | 浏览器；设备 owner 创建邀请码 |
| `DELETE` | `/api/v1/device-invites/{id}` | 浏览器；设备 owner 撤销邀请码 |
| `POST` | `/api/v1/device-invitations/accept` | 浏览器；使用 `{"code":"ndi_..."}` 接受邀请 |
| `POST` | `/api/v1/devices/shares/revoke` | 浏览器；owner 按设备和 grantee 撤销授权 |

邀请码创建请求：

```json
{
  "device_organization_id": "owner-org",
  "device_id": "worker-id",
  "max_uses": 1,
  "expires_in_seconds": 86400
}
```

完整交互如下：

1. owner 创建 `nwk_` Worker 凭据并配置到 CLI 或 ComfyUI 节点。
2. Worker 反向连接 Hub，注册稳定的 `device_id` 和 workflow manifest。
3. owner 从 `GET /api/v1/devices` 取得设备坐标，创建 `ndi_` 邀请码。
4. 另一账户登录后接受邀请码。
5. 被邀请账户的设备和 workflow 列表出现该设备；离线 workflow 仍可见，但
   `available=false`。
6. 被邀请账户提交 job 时指定 `device_organization_id` 和 `device_id`，Hub 再校验有效 grant。

同一 organization 内相同 Worker identity 不能被另一用户的凭据接管。邀请码有过期时间、最大使用
次数和撤销状态；重复接受同一有效邀请返回同一 grant，不重复计数。

### Workflow、Artifact 与 Job

| Method | Path | 调用者与用途 |
| --- | --- | --- |
| `GET` | `/api/v1/workflows` | 两者；已审核且当前用户可访问的 workflow 目录，keyset 分页 |
| `POST` | `/api/v1/artifacts/uploads` | 两者；申请输入对象的预签名 PUT |
| `POST` | `/api/v1/artifacts/uploads/{id}/complete` | 两者；HEAD/大小/hash 校验后完成上传 |
| `GET` | `/api/v1/artifacts/{id}/download` | 两者；授权后返回短期预签名 GET |
| `GET` | `/api/v1/jobs` | 两者；分页，`?limit=&cursor=`，返回 `{"items":[...],"next_cursor":...}` |
| `POST` | `/api/v1/jobs` | 两者；提交 job，建议带 `Idempotency-Key` |
| `GET` | `/api/v1/jobs/{id}` | 两者；job、事件和输出 |
| `GET` | `/api/v1/jobs/{id}/events?after={sequence}` | 两者；认证 SSE，按 sequence 增量推送事件，终态后关闭 |
| `DELETE` | `/api/v1/jobs/{id}` | 两者；member 取消自己创建的 job，operator 以上取消任意 job |

上传请求和完成请求：

```json
{"name":"source.png","content_type":"image/png","size_bytes":123,"sha256":"<64 hex>"}
{"artifact_id":"...","size_bytes":123,"sha256":"<64 hex>"}
```

客户端必须严格使用服务端返回的 `upload.method/url/headers` 直传对象存储。媒体内容不经过 Hub
JSON 请求体。

job 请求：

```json
{
  "workflow_id": "sdxl-txt2img",
  "workflow_version": "v1",
  "parameters": {"prompt": "portrait", "seed": 42},
  "input_artifact_ids": ["..."],
  "device_organization_id": "owner-org",
  "device_id": "worker-id"
}
```

共享设备场景中，job、输入输出对象、幂等记录和配额均属于调用者当前 organization；
`worker_organization_id` 只标识实际执行设备。也就是说当前计量向提交 job 的租户记账，不向设备
owner 的租户记账。以后若加入市场结算，应另建 usage/settlement ledger，不能改变这条 ownership。

## RBAC

| 能力 | viewer | member | operator | admin | owner |
| --- | --- | --- | --- | --- | --- |
| workflow、组织 job、quota 只读 | 是 | 是 | 是 | 是 | 是 |
| artifact、提交 job、取消自己的 job | 否 | 是 | 是 | 是 | 是 |
| 注册/使用/分享自己的设备、管理自己的 Key | 否 | 是 | 是 | 是 | 是 |
| 管理 Worker、发布 workflow、取消任意 job | 否 | 否 | 是 | 是 | 是 |
| 管理成员、全部 Key、quota、审计 | 否 | 否 | 否 | 是 | 是 |
| 删除 organization | 否 | 否 | 否 | 否 | 是 |

workflow 发布仍未开放；organization 删除和 quota 策略写路由已开放，但都经过 owner/admin 授权。

## 前端实现约定

- 应用启动调用 `/auth/me` 恢复用户与 membership；组织切换只改变内存中的当前 org 和
  `X-Organization-ID`，不改写 API Key。
- 不根据角色隐藏数据后就假设安全。菜单可按 role 做可用性提示，但所有动作以服务端 403 为准。
- workflow 表单只读取公开 manifest 字段。Hub 已移除 JSON Pointer、ComfyUI node id/type/field 和
  Worker session/labels；设备列表也不会返回原始 `capabilities_json`，前端不应依赖这些内部字段。
- `available=false` 表示 manifest 可浏览但当前没有在线 Worker，提交按钮应禁用。
- 所有一次性 secret 使用“仅显示一次”对话框；之后列表只显示 prefix、创建时间、last used、过期和
  revoked 状态。
- `GET /jobs` 是 keyset 分页：`limit` 默认 50、上限 200，把返回的 `next_cursor` 原样回传即可取下一页，
  `null` 表示到底。cursor 是不透明串，不要解析它。列表行不含 `events`，事件时间线只在
  `GET /jobs/{id}` 返回。
- 审计日志、设备、workflow、API Key 和 Worker 凭据也使用相同的
  `{"items":[...],"next_cursor":...}` 分页 envelope；默认 `limit` 为 50、上限 200。
  各资源的 cursor 只允许回传给原资源 endpoint，不要解析、拼接或跨 endpoint 复用。
- `/jobs/{id}/events` 使用 fetch + ReadableStream 消费，不能用原生 `EventSource` 代替，因为浏览器
  access token 只在内存中并通过 Authorization header 发送。客户端保存最后 sequence，断线后带
  `after` 重连；协议终态包括 completed、failed、cancelled。

## 对公网开放前仍需完成

当前实现适合单 Hub、受控用户群的公共 MVP。OAuth-only 已解决 Hub 侧邮件验证/密码恢复依赖，
但公开注册仍需要以下运营和规模化工作：

- MFA/passkey（若产品需要超出 OAuth provider 本身的账户保护）。
- 多实例共享的限流状态、账户级失败锁定和机器人防护；当前 limiter 是单 Hub 进程内状态。
- 按 `openapi.yaml` 生成并发布带版本的外部 SDK。
- 用户/组织数据导出与删除流程，以及明确的保留期和对象存储清理策略。
- usage ledger 的展示、设备 owner 分成/结算产品；当前 job 配额只记提交方组织。
- 多 Hub 的共享 Worker session 路由、跨实例 ACK 和 outbox 共享消费语义。
- multipart 上传（大文件需要时）以及更完整的业务指标、告警和审计归档。

部署限制与安全配置见 [DEPLOYMENT_CN.md](DEPLOYMENT_CN.md)，数据库租户边界见
[DATABASE_SCHEMA_CN.md](DATABASE_SCHEMA_CN.md)。
