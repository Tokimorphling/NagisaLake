# PostgreSQL 数据模型与租户边界

公共控制面启用 `NAGISALAKE_DATABASE_URL` 后，Hub 启动时运行
`crates/nagisalake-hub-store/migrations` 中的 migrations。数据库保存账户、租户、执行元数据和
恢复状态；图片、音频、视频仍只保存在私有对象存储。

## 表分组

| 领域 | 表 | 作用 |
| --- | --- | --- |
| 账户 | `users`、`organizations`、`memberships` | 用户、租户根和 RBAC role |
| 凭据 | `browser_sessions`、`api_keys`、`worker_credentials` | 三种互相隔离的认证主体，只保存 hash/prefix |
| 设备 | `workers`、`worker_workflows`、`workflow_versions` | 稳定设备 identity、设备与 workflow 映射、manifest 版本 |
| 分享 | `device_share_invites`、`device_grants` | 有期限邀请码和跨账户设备授权 |
| 执行 | `jobs`、`job_events`、`dispatch_outbox`、`idempotency_records` | job 状态、事件、待派发记录和请求幂等 |
| 数据 | `artifacts`、`artifact_upload_requests` | 对象元数据、上传和 Worker 输出请求恢复 |
| 配额 | `quota_policies`、`quota_usage`、`usage_ledger` | 并发、周期 job、存储预占与幂等释放 |
| 审计 | `audit_logs` | actor、request、动作、资源、结果与脱敏 metadata |

标识符使用文本 UUID，时间为 Unix 毫秒。数据库迁移是前向兼容的；部署时不要手工改表代替 migration。
已经进入 `main` 或在任何环境执行过的 migration 文件必须保持字节不变（包括注释和空白），因为
SQLx 会校验完整文件 checksum。任何后续说明或 schema 调整都要写入文档或新增 forward migration，
不能回改旧 migration。已经上线的 migration 还应登记到 migrations 目录的 `SHA384SUMS`，由 CI
阻止意外漂移。

## Organization ownership

`organizations.id` 是业务租户根。job、artifact、workflow、Worker、API Key、配额和大部分审计
记录都显式带 `organization_id`。Store 查询先使用 organization 限定，再使用资源 id；不存在与
跨租户访问对外都应表现为 404，减少 IDOR 枚举。

对象存储 key 使用：

```text
organizations/{organization_id}/inputs/...
organizations/{organization_id}/outputs/...
```

签发预签名 URL 前还要检查 principal、artifact organization 和状态。数据库不保存可恢复的 API
Key、Worker token、refresh token或邀请码明文。

共享设备保留两个 ownership：

- `jobs.organization_id` 是调用者和计费租户。
- `jobs.worker_organization_id + worker_id` 是实际执行设备。

`device_grants` 只授予 grantee 使用指定设备，不创建 membership，也不让 grantee 查询设备
organization 的其他资源。workflow 目录通过 `worker_workflows` 精确关联设备；未授权设备的
manifest 不会因 workflow id 相同而泄露。
owner 设备的每次读取、workflow 聚合和直连使用还会重新检查 owner 在设备 organization 的
membership；因此即使 legacy 兼容字段没有外键，移除 membership 也不会留下 owner 访问。

## 关键事务与不变量

### 注册和角色

注册在一个事务内创建 user、默认 organization、owner membership 以及 quota policy/usage。
角色修改锁定 organization 的 membership；最后一个 owner 不能被降级。只有 owner 可以授予 owner
或修改另一个 owner。

### Session rotation

refresh 使用“session id + 当前 refresh hash + 未撤销”条件更新 access/refresh hash。并发 refresh
只有一个成功，旧 token 重放不会生成第二组凭据。revoke-all 按 user 撤销所有 session family。

### Worker registration

`nwk_` 凭据决定 organization、owner 和可选 namespace，Worker 注册消息不能覆盖这些字段。
同一 organization 下已有的 Worker identity 只有相同 owner 可以更新。每次注册 upsert
`worker_workflows`；相同 workflow id/version 的 content hash 变化时版本进入 `drifted`，从公共
目录隐藏，等待以后加入审核接口。

### Device invite

接受邀请码会锁定 invite，检查 owner、过期、撤销和最大使用次数，在同一事务内增加 use count 并
upsert grant。相同 grantee 重复兑换返回原 grant，不重复消耗次数。撤销 grant 后，后续 job 选择
设备会失败。

### Job 与配额

创建 job 的事务会锁定 quota 行，并执行：

1. 检查 organization + actor + endpoint + `Idempotency-Key`。
2. 校验并绑定 ready 输入 artifact。
3. 检查并预占 active job、周期 job 和已有 storage。
4. 写入 job、dispatch outbox 和 idempotency record。

相同 key 和相同请求返回原 job；相同 key 不同请求返回 conflict。job 到达 completed、failed 或
cancelled 后，通过唯一 usage ledger 释放 active job，因此重复事件和 Hub 重启不会重复扣减。

当前 `dispatch_outbox` 已持久化，但在线 Worker session 和 ACK waiter 仍在单 Hub 内存中，所以
数据库本身并不能让多个 Hub 副本安全共享同一 Worker。多副本前需要 outbox consumer、session
owner 租约和跨实例消息路由。

## 启动恢复

Hub 启动后从 PostgreSQL hydrate artifact、upload request、workflow、job 和 job event 到本地路由
缓存。完成状态、输出请求幂等和离线 workflow 因此能跨重启恢复；Worker 重连后重新建立在线 session。

备份应把 PostgreSQL PITR/快照与对象存储版本策略作为一组恢复点。数据库恢复但对象缺失时，artifact
metadata 仍存在但下载/校验会失败；对象恢复但数据库缺失时，对象会成为孤立数据。详细步骤见
[DEPLOYMENT_CN.md](DEPLOYMENT_CN.md)。
