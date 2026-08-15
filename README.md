# Nagisalake

Nagisalake 将云端 Hub 与 NAT 后的 ComfyUI Worker 连接起来。控制面使用 Tokio、Axum、
WebSocket/SMUX 和类型化 JSON 协议；PostgreSQL 保存公共控制面的多租户元数据，多媒体数据面使用
私有 S3-compatible 对象存储。

进一步文档：

- [架构与状态机](docs/ARCHITECTURE_CN.md)
- [Worker 接入指南](docs/WORKER_ONBOARDING_CN.md)
- [本地全链路环境](scripts/local-stack/README.md)
- [局域网联调](docs/LAN_TESTING_CN.md)
- [Workflow Manifest 设计](docs/WORKFLOW_MANIFEST_CN.md)
- [部署指南](docs/DEPLOYMENT_CN.md)
- [公开服务、账户与前端 API](docs/PUBLIC_PRODUCT_API_CN.md)
- [OpenAPI 契约](docs/openapi.yaml)
- [PostgreSQL 数据模型与租户边界](docs/DATABASE_SCHEMA_CN.md)

## 本地启动

要求 Rust 1.88+、一个 S3-compatible bucket（例如 MinIO）和本机 ComfyUI。公开账户 API 还要求
PostgreSQL；不配置数据库时仅保留旧版 `/v1` 兼容 API。

```bash
export NAGISALAKE_S3_ACCESS_KEY_ID=minioadmin
export NAGISALAKE_S3_SECRET_ACCESS_KEY=minioadmin
export NAGISALAKE_DATABASE_URL='postgres://postgres@127.0.0.1:5432/nagisalake'

cargo run -p nagisalake-hub -- --config examples/nagisalake-hub.toml
cargo run -p nagisalake-worker-cli --bin nagisalake-worker -- \
  --config examples/nagisalake-worker.toml
```

Worker 也可以编译为 Python 扩展并作为 ComfyUI 自定义节点运行：

```bash
cd integrations/comfyui_nagisalake
python -m pip install 'maturin>=1.9.4,<2'
python -m maturin develop --release
```

这里的 `python` 必须是 ComfyUI 实际使用的解释器。

把该目录安装到 ComfyUI 的 `custom_nodes/comfyui_nagisalake` 后，在工作流中加入
`Nagisalake Hub Worker`。这个节点执行时会在后台 Tokio runtime 中启动一次进程级 Worker，
连接并注册 Hub；它本身不创建或提交 `DispatchJob`。完整步骤见
[`integrations/comfyui_nagisalake/README.md`](integrations/comfyui_nagisalake/README.md)。

公开模式由浏览器账户创建一次可见的 `nwk_` Worker 凭据和 `nsk_` API Key；示例配置中的静态
worker/consumer token 只用于旧版兼容。生产连接必须使用 `wss://`，并将所有 secret 放入 secret
manager 或受限环境文件。

## 浏览器控制台

`web/` 是 `/api/v1` 的前端工作台。开发时用 Vite dev server，它把 `/api` 代理到 Hub：

```bash
cd web && pnpm install && pnpm dev   # http://localhost:3000
```

发布时可以把控制台静态编译进 Hub 二进制，部署只需一个文件：

```bash
cd web && pnpm install && pnpm build
cargo build --profile release-lto -p nagisalake-hub --features embed-web
```

此时 Hub 在自己的 origin 上同时提供 `/api/v1` 和控制台，`allowed_origins` 不需要为它添加条目。
`embed-web` 默认关闭，因此 `cargo build`、`cargo test` 和 `cargo clippy` 不依赖 Node 工具链；
未开启该 feature 时 Hub 只提供 API，未命中的路径继续返回 JSON 404。详见
[`web/README.md`](web/README.md)。

## 公共控制面

`/api/v1` 已实现浏览器 session、organization RBAC、API Key、Worker 凭据、设备邀请码、quota、
audit、workflow、artifact 和 job API。用户可以注册自己的 ComfyUI 设备，生成 `ndi_` 邀请码；另一
账户兑换后即可看到该设备及其已审核 workflow，并把 job 定向到该设备。

浏览器 access token、程序 API Key 和 Worker 凭据相互隔离。具体请求、权限矩阵、前端状态管理和
设备分享流程见 [公开服务与前端 API](docs/PUBLIC_PRODUCT_API_CN.md)。
机器可读的 `/api/v1` 契约见 [`docs/openapi.yaml`](docs/openapi.yaml)；SDK 应由该文件生成并按
契约版本发布，不要从前端内部 client 复制类型。

## 旧版消费者 API

以下 `/v1` 接口为兼容入口。新 SDK 和前端应使用 `/api/v1`。

申请输入上传：

```http
POST /v1/artifacts/uploads
Authorization: Bearer <api-token>
Content-Type: application/json

{"name":"source.png","content_type":"image/png","size_bytes":123,"sha256":"<64 hex>"}
```

客户端严格按响应的 `upload.method`、`upload.url`、`upload.headers` PUT 文件，然后完成上传：

```http
POST /v1/artifacts/uploads/{artifact_id}/complete
Authorization: Bearer <api-token>
Content-Type: application/json

{"artifact_id":"...","size_bytes":123,"sha256":"<64 hex>"}
```

创建作业：

```http
POST /v1/jobs
Authorization: Bearer <api-token>
Idempotency-Key: portrait-001
Content-Type: application/json

{
  "workflow_id":"sdxl-txt2img",
  "workflow_version":"v1",
  "parameters":{"prompt":"portrait photo","seed":42,"steps":24,"width":1024,"height":1024},
  "input_artifact_ids":[]
}
```

查询、取消和获取输出签名：

```text
GET    /v1/jobs/{job_id}
DELETE /v1/jobs/{job_id}
GET    /v1/artifacts/{artifact_id}/download
GET    /v1/workflows
GET    /v1/workers
GET    /healthz
```

## 验证

```bash
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo check -p nagisalake-worker --features python
```

`--all-features` 包含 `embed-web`，因此需要先执行 `cd web && pnpm build`，否则构建脚本会明确
提示缺少 `web/dist`。只验证 Rust 时省略该 feature 即可。开启后会额外运行控制台的静态资源、SPA
回退和缓存头测试：

```bash
cargo test -p nagisalake-hub --features embed-web
```

端到端 mock 会启动真实 Hub 和 Worker，并使用模拟 ComfyUI 与内存 S3-compatible 服务验证输入
上传、反向派发、执行、输出回传和下载；不会运行 `test_workflows` 中的真实工作流：

```bash
cargo test -p nagisalake-hub \
  consumer_job_completes_through_hub_worker_and_mock_comfyui -- --nocapture

NAGISALAKE_TEST_DATABASE_URL='postgres://postgres@127.0.0.1:5432/nagisalake_test' \
  cargo test -p nagisalake-hub-store --test postgres -- --nocapture

NAGISALAKE_TEST_DATABASE_URL='postgres://postgres@127.0.0.1:5432/nagisalake_test' \
  cargo test -p nagisalake-hub \
  shared_device_flow_uses_distinct_browser_api_and_worker_credentials -- --nocapture
```
