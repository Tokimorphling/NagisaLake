# Nagisalake 部署指南

本文对应仓库当前实现。生产公共服务必须启用 PostgreSQL；不配置数据库的模式只用于旧版
`/v1` API 的兼容和本地 mock，不提供账户、多租户或持久恢复能力。

## 拓扑

```text
浏览器 / SDK -- HTTPS + JSON --> 反向代理 --> nagisalake-hub -- private S3
                                      ^                         ^
                                      | WSS + SMUX              | presigned PUT/GET
                              边缘 nagisalake-worker -- localhost --> ComfyUI
```

Worker 只建立出站连接，因此边缘机器不需要入站端口。控制面传 JSON/协议元数据；图片、音频、视频
走对象存储短期预签名 URL，不经过 Hub 请求体。

当前 Hub 仍建议单实例：PostgreSQL 保存 durable metadata，在线 WebSocket session、ACK waiter 和
本地 hydrate cache 在进程内。单实例已经运行 durable dispatch outbox consumer；不要直接部署多个
Hub 副本，扩容前仍需共享 Worker session 路由、跨实例 ACK 和统一 outbox 消费协调。

## 生产镜像一键更新

仓库根目录的 `deploy.sh` 封装了当前 AWS 生产环境的安全更新流程。SSH alias `binance-test-3`
可用时，在仓库根目录直接执行：

```bash
./deploy.sh
```

它默认拉取 `ghcr.io/tokimorphling/nagisalake-hub:latest`，并且只在拉取成功后停止旧容器。新容器
必须同时通过远端 loopback 的 `/healthz`、`/readyz`，随后还会检查公网 HTTPS；启动失败时脚本会
恢复旧容器。部署使用 AWS 上已有的 `/home/ubuntu/nagisalake/.env` 和 `hub.toml`，不会读取、打印
或上传本地 `.env`，也不会修改 Caddy、DNS、数据库或对象存储。

常用命令：

```bash
# 只做只读预检，不拉镜像或重启容器
./deploy.sh --dry-run

# 发布指定 tag
./deploy.sh --image ghcr.io/tokimorphling/nagisalake-hub:v0.2.0

# 推荐用于严格复现的不可变 digest
./deploy.sh \
  --image ghcr.io/tokimorphling/nagisalake-hub@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa

# 使用未写入 SSH config 的私钥
./deploy.sh --identity-file /path/to/id_ed25519
```

如果 GHCR 需要认证，先在本地环境设置 `GITHUB_PAT`。脚本会先尝试使用服务器已有 Docker 登录；
只有 GHCR 明确返回未授权时，才通过 `docker login --password-stdin` 发送 token，并在同一个远端部署锁
下重新拉取、锁定镜像 ID 和切换容器，token 不会出现在命令行中。完整参数见 `./deploy.sh --help`。

脚本会对比仓库中的 `deploy/prod/hub.toml` 与远端配置并报告 `in-sync` 或 `drifted`，但镜像更新不会
自动覆盖远端配置；确认配置变更后应单独、显式地同步。若 SSH 连接被强制杀死而未能执行 shell trap，
下次部署会在检测到 `nagisalake-hub-previous` 时停止。先通过
`docker ps -a --filter name=nagisalake-hub` 确认两个容器状态；若新容器不可用，就删除失败的新容器、
把 `nagisalake-hub-previous` 改回原名并启动，再验证 `/healthz` 与 `/readyz`。不能未经检查就删除旧容器。

## 前置条件

- Rust 1.88+（或使用发布产物）。
- PostgreSQL 14+，Hub 数据库用户需要建表和运行 migration 的权限。
- private S3-compatible bucket（AWS S3、R2、MinIO 等），允许 `PutObject`、`GetObject`、
  `HeadObject`，以及按部署策略清理对象的权限。
- 能代理 WebSocket upgrade 的 HTTPS 反向代理。
- 边缘机器上的 ComfyUI，及 API-format workflow JSON。

## 构建与检查

```bash
cargo build --profile release-lto -p nagisalake-hub
cargo build --profile release-lto -p nagisalake-worker-cli
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo check -p nagisalake-worker --features python
```

### 把控制台编译进 Hub

`embed-web` feature 把 `web/dist` 静态嵌入二进制，部署产物只有一个文件，不需要另外托管前端或
配置 CORS：

```bash
cd web && pnpm install && pnpm build && cd ..
cargo build --profile release-lto -p nagisalake-hub --features embed-web
```

该 feature 默认关闭，因此纯 Rust 的构建和 CI 不需要 Node 工具链。前端未构建时开启它会由
`build.rs` 直接报错并提示需要执行的命令，而不是产生难以定位的宏错误。`--all-features` 也包含它，
所以在跑 `cargo clippy --all-features` 前需要先构建前端。

嵌入后的行为：

- `/` 和所有非 API 路径返回控制台；`/jobs/{id}` 这类前端路由在硬刷新时由 SPA 回退处理。
- `/api/`、`/v1/`、`/healthz`、`/readyz` 和 `/metrics` 永远返回非 HTML 响应，未命中的接口保持 `not_found` 错误信封，不会把
  HTML 返回给 SDK。
- `assets/` 下的文件名带内容 hash，返回 `Cache-Control: immutable` 和 ETag；`index.html` 返回
  `no-cache`，避免客户端锁定旧的资源图。缺失的 `assets/` 文件返回 404 而不是 HTML。
- 控制台与 API 同源，因此 `browser.allowed_origins` 不需要为 Hub 自身添加条目。

Python 节点构建必须使用 ComfyUI 实际运行的 Python：

```bash
cd integrations/comfyui_nagisalake
python -m pip install 'maturin>=1.9.4,<2'
python -m maturin build --release
```

## PostgreSQL 与 Hub

复制 `examples/nagisalake-hub.toml`，取消 `[database]` 示例块或只通过环境变量提供 URL：

```bash
export NAGISALAKE_DATABASE_URL='postgres://nagisalake:change-me@postgres:5432/nagisalake'
export NAGISALAKE_S3_ACCESS_KEY_ID='...'
export NAGISALAKE_S3_SECRET_ACCESS_KEY='...'
```

`NAGISALAKE_DATABASE_URL` 会创建/覆盖数据库配置，启动时自动运行 `sqlx` migrations。正式环境不要
把密码、`nwk_`、`nsk_`、S3 secret 或 legacy token 写入 Git；使用 secret manager 或受限的
`EnvironmentFile`。数据库连接字符串不会出现在 Hub 的 Debug 输出中。

```bash
/opt/nagisalake/bin/nagisalake-hub --config /etc/nagisalake/hub.toml
```

公共模式的关键设置：

- `[browser].cookie_secure = true`，只在本地 HTTP 测试设为 false。
- `allowed_origins` 填写额外的前端 origin（含 scheme、host、port）；refresh 会校验 `Origin` 和
  双提交 CSRF token。Hub 只接受与请求 `Host` 同源的 `Origin`，或该列表中的条目；因此用
  `--features embed-web` 编译进来的控制台无需配置，独立托管的前端才需要在这里登记，或在代理层
  正确配置 CORS（Hub 自身不发送 CORS 响应头）。
- 公开注册使用 OAuth-only：配置 `[oauth]` provider、保持 `password_auth_enabled = false`，由 Google、
  GitHub 或 OIDC provider 负责邮箱验证和账户恢复。`/auth/register`、`/auth/login` 仅保留 loopback
 兼容用途，公网不要把它们作为账户入口。
- `registration_enabled` 控制 OAuth 是否允许创建新账户。Hub 内置 IP + 账户/组织维度的进程内限流，
  生产多副本仍需在反向代理/WAF 和共享存储层提供跨实例限流、账户级失败锁定和机器人防护。
- `browser.cookie_secure = false` 在非 loopback listen 地址会拒绝启动，除非显式设置
  `allow_insecure_cookies = true`；生产应使用 HTTPS/WSS 并保持默认 `true`。
- `transport.max_artifact_bytes` 当前最大为 5 GiB；单次 PUT，不是 multipart。

### Linux.do Connect 登录

在 Linux.do Connect 的“申请接入”表单中填写：

- 应用名：`Nagisalake`
- 应用主页：`https://nagisalake.tokilake.abrdns.com`
- 应用描述：`连接云端 Hub 与 NAT 后的 ComfyUI 设备`
- 回调地址：`https://nagisalake.tokilake.abrdns.com/api/v1/auth/oauth/linuxdo/callback`
- 应用图标：`https://nagisalake.tokilake.abrdns.com/nagisalake.svg`
- 最低等级：按开放范围选择；`0` 允许最低等级用户，设为 `1` 或更高可减少新账号范围。

保存后，把签发的 client id 写进配置；client secret 只写入受限的环境文件：

```toml
[oauth.providers.linuxdo]
kind = "linuxdo"
client_id = "签发的-client-id"
client_secret_env = "NAGISALAKE_OAUTH_LINUXDO_SECRET"
```

```bash
NAGISALAKE_OAUTH_LINUXDO_SECRET='签发的-client-secret'
```

Linux.do Connect 使用 OAuth2 端点 `/oauth2/authorize`、`/oauth2/token` 和 `/api/user`，默认 scope
为 `user`。Hub 使用 Linux.do 的稳定用户 id 建立身份绑定，不使用上游未验证邮箱自动关联已有账户。

运行状态端点：

- `/healthz` 是存活检查，会报告数据库、对象存储、连接 Worker 和 `ready` 字段；依赖暂时不可达时
  返回 `degraded`。
- `/readyz` 是严格就绪检查，数据库或对象存储不可达时返回 HTTP 503。
- `/metrics` 返回 Prometheus text format，至少包含连接 Worker、过期上传回收和 quota 对账计数器。

反向代理必须：

- 为 `/v1/worker/connect` 保留 WebSocket upgrade、`Sec-WebSocket-Protocol` 和长空闲超时（建议
  120 秒以上）。
- 对外只提供 HTTPS/WSS，TLS 1.2+，不要缓冲或压缩 WebSocket。
- 只信任代理自己生成的 `X-Forwarded-*`；客户端提供的 request id 会被截断并回显为 `X-Request-ID`。

最小 systemd 示例：

```ini
[Unit]
Description=Nagisalake Hub
After=network-online.target
Wants=network-online.target

[Service]
User=nagisalake
Group=nagisalake
EnvironmentFile=/etc/nagisalake/hub.env
ExecStart=/opt/nagisalake/bin/nagisalake-hub --config /etc/nagisalake/hub.toml
Restart=on-failure
RestartSec=3
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true

[Install]
WantedBy=multi-user.target
```

## Worker 与 ComfyUI

Worker 的完整接入步骤、配置字段、环境变量、代理支持和断线恢复见
[WORKER_ONBOARDING_CN.md](WORKER_ONBOARDING_CN.md)。以下只讲生产特有的注意事项。

### 独立 CLI

每个边缘设备先由组织浏览器 session 调用：

```text
POST /api/v1/organizations/{org_id}/worker-credentials
```

响应中的 `nwk_...` 明文只返回一次。将它放入 Worker 配置或 secret store，设置 namespace、node
name、持久 SQLite URL、work directory 和 ComfyUI `base_url`：

```bash
/opt/nagisalake/bin/nagisalake-worker --config /etc/nagisalake/worker.toml
```

Worker 注册时只能声明 capability；凭据决定 organization、owner 和可选 namespace，不能由注册
消息改变租户。相同 organization 内已有设备 identity 也不能被另一用户凭据接管。吊销凭据会阻止
重连并关闭当前在线 session。

### Worker 侧 TLS

`hub.url` 只接受 `ws://` 和 `wss://`，其它 scheme 在启动时就报错，不会带着退避重试循环下去。
生产用 `wss://`：bearer token 和 dispatch 全部走这条连接，明文 `ws://` 只适用于局域网测试。

证书来自公共 CA 时不需要任何额外配置，内置公共根证书就能校验。私有 CA 才需要：

```toml
[hub]
url = "wss://hub.example.com/v1/worker/connect"

[hub.tls]
# 相对路径按 worker.toml 所在目录解析。
ca_certificates = ["./tls/hub-ca.pem"]
```

- 这里放的必须是 **CA 证书**。自签名的服务器证书没有 `basicConstraints: CA:TRUE`，不是可用的
  信任根，webpki 会以 unknown issuer 拒绝整条链。要自签就自己签一个 CA，再用它签服务器证书。
- 私有 CA 是 **追加** 到公共根上的，混合部署（一部分 Hub 用私有 CA、另一部分用公共证书）不会
  因此失效。
- 每次连接尝试都会重新读取这些文件，轮换证书在下一次重连生效，不用重启 Worker。文件读不到
  或解析不出证书会直接让本次尝试失败，而不是静默退回只信任公共根。
- 在 `ws://` 上配置 `ca_certificates` 会被拒绝：那种组合下什么都没有被加密。
- 没有提供跳过证书校验的开关。Worker 凭据在握手时就发出去了，关掉校验等于把它交给任何能做
  中间人的一方。

### ComfyUI 节点

将 `integrations/comfyui_nagisalake` 安装到 ComfyUI `custom_nodes`。把
`Nagisalake Hub Worker` 节点加入 workflow 后，节点第一次被 ComfyUI 排队执行时在后台启动进程级
Tokio Worker 并注册 Hub；它不创建 `DispatchJob`。因此节点不能单独解决“Hub 远程触发第一条任务时
节点还没有执行”的冷启动循环。无人值守机器应运行独立 CLI，或由 ComfyUI 启动脚本先排队一次
bootstrap workflow。

## 对象存储与数据安全

- bucket 必须 private；预签名 URL 只在短 TTL 内有效。
- key 以 `organizations/{organization_id}/inputs|outputs/...` 开头，Hub 在签发 URL 前检查租户。
- 配置 bucket CORS 时只允许明确的前端 origins、`PUT/GET/HEAD` 和签名所需 headers，不要把
  `*` 与凭据一起使用。
- 浏览器合成参数卡会优先用短期预签名 GET 直接读取对象存储；视频还会发送 `Range`。生产 R2 的
  Dashboard CORS JSON 模板见 `deploy/prod/r2-cors.json`。应用前先读取并备份 bucket 的完整现有
  CORS，再合并规则；Cloudflare 的设置操作会整份替换配置。变更后最多等待约 30 秒，并分别验证
  GET/Range 与 PUT 预检。缺少 GET CORS 时前端会安全回退到登录态 Hub `/content`，但大媒体流量会
  再次经过 Hub，因此不能把回退当作生产常态。
- Worker 和 Hub 都校验大小、content type、SHA-256；不要依赖文件名作为安全边界。
- Hub 定时回收过期 `pending_upload`，同时释放组织 storage quota；对象删除失败和回收数量通过
  `/metrics` 暴露。bucket 仍应配置 lifecycle 作为第二道清理措施。

## 配额、审计与恢复

当前 PostgreSQL 配额口径是：组织最大并发 job、周期 job 数、storage bytes。job 创建事务会锁定
配额行、绑定输入 artifact、写 job 与 dispatch outbox；终态事件使用幂等 usage ledger 释放并发。
协议目前没有可靠 GPU 秒字段，因此不要把当前 `period_jobs` 宣称为 GPU 用量。`quota:read` 可读
快照，组织 admin/owner 可通过 `PATCH /api/v1/organizations/{org_id}/quota` 调整策略。Hub 定期
从 job 状态和 worker heartbeat 对账 active job 占用，避免外部删除或 Worker 永久失联造成配额泄漏。

重要控制面和执行动作写入 organization-scoped audit log；日志不应包含 token、预签名 URL 或完整
workflow 参数。生产环境应把审计表导出到不可变日志系统。

备份和恢复建议：

1. 使用 PostgreSQL PITR/每日快照备份整个数据库，单独保留对象存储版本或生命周期策略。
2. 恢复时先恢复数据库，再启动 Hub；Hub 会重新运行兼容 migrations 并 hydrate durable state。
3. Worker SQLite 是边缘执行 journal，也要按设备备份；升级前停止 Worker 或使用 SQLite online
   backup，不要复制正在写入的单文件。
4. 验证 Hub 重启后 job、artifact、共享 workflow 仍可查询，Worker 重连能取得新的 presigned URL。

## 本地完整 mock

不运行 `test_workflows` 中的真实工作流；仓库测试会用 mock ComfyUI 和内存 S3 验证真实 Hub、Worker、
对象上传、事件 ACK、输出下载链路。启用 PostgreSQL 后再运行账户与设备分享流程：

```bash
NAGISALAKE_TEST_DATABASE_URL='postgres://postgres@127.0.0.1:5432/nagisalake_test' \
  cargo test -p nagisalake-hub-store --test postgres -- --nocapture

NAGISALAKE_TEST_DATABASE_URL='postgres://postgres@127.0.0.1:5432/nagisalake_test' \
  cargo test -p nagisalake-hub shared_device_flow_uses_distinct_browser_api_and_worker_credentials \
  -- --nocapture

cargo test -p nagisalake-hub consumer_job_completes_through_hub_worker_and_mock_comfyui -- --nocapture
```

## 已知边界

当前版本适合作为单 Hub、受控用户群的公共 MVP。正式 Hosted Product 仍需要跨实例限流/账户锁定、
外部 SDK 的生成与版本化、数据导出删除、usage ledger 产品和多副本 session/outbox 路由。OAuth-only、
SSE、成员闭环、配额对账、ready/metrics 和过期上传回收已在当前实现中提供。接口和数据模型已为
剩余能力预留 organization、role、scope、audit 和 idempotency 边界；见
[PUBLIC_PRODUCT_API_CN.md](PUBLIC_PRODUCT_API_CN.md)。
