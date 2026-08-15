# Nagisalake 控制台

`/api/v1` 公共控制面的前端工作台：账户与组织、设备与凭据、workflow 目录、由 manifest 驱动的
作业表单、作业状态与输出下载、配额和审计。

技术栈为 React 19、TypeScript、Vite 7、Tailwind CSS 4 和 TanStack Query。

## 开发

```bash
pnpm install
pnpm dev            # http://localhost:3000
```

Hub 默认代理到 `http://127.0.0.1:9091`，可用 `NAGISALAKE_HUB_URL` 覆盖：

```bash
NAGISALAKE_HUB_URL=http://127.0.0.1:9091 pnpm dev
```

其他命令：

```bash
pnpm typecheck
pnpm test
pnpm build          # 输出到 dist/
pnpm preview
```

## 为什么必须走代理

Hub 目前不返回任何 CORS 响应头，`browser.allowed_origins` 只用于 refresh 的 `Origin` 校验。
因此前端不能跨 origin 直连 Hub。Vite dev server 把 `/api` 和 `/healthz` 代理到 Hub，使浏览器
始终处于同源状态，refresh cookie（`Path=/api/v1/auth`、`HttpOnly`、`SameSite=Lax`）、CSRF 双提交
和 `Origin` 校验才能按生产语义工作。

生产部署有两种方式，推荐第一种。

### 静态编译进 Hub（推荐）

```bash
pnpm build
cd .. && cargo build --release -p nagisalake-hub --features embed-web
```

产物是单个二进制，同时提供 `/api/v1` 和控制台。因为同源，不需要 CORS，也不需要在
`allowed_origins` 里登记 Hub 自己的 origin。Hub 会处理 SPA 深链接回退，并给带 hash 的资源发送
`immutable` 缓存头、给 `index.html` 发送 `no-cache`。

`embed-web` 默认关闭，所以纯 Rust 构建不需要 Node。开启但未执行 `pnpm build` 时，Hub 的
`build.rs` 会直接报错并说明要跑哪条命令。

### 独立托管

把 `dist/` 交给 Nginx、Caddy 或 CDN，并把 `/api` 反向代理到 Hub 的同一 origin。若前端与 API 不同源，
需要在代理层配置 CORS，并把前端 origin 写入 Hub 的 `allowed_origins`。

## Hub 侧前置条件

```toml
[browser]
registration_enabled = true
cookie_secure = false                        # 本地 HTTP 才设为 false
allowed_origins = ["http://localhost:3000"]  # 必须与前端 origin 完全一致
```

公共控制面还要求配置 PostgreSQL（`NAGISALAKE_DATABASE_URL` 或 `[database]`）。未配置数据库时
Hub 只保留旧版 `/v1` 兼容 API，本控制台无法登录。

输入文件通过预签名 PUT 直传对象存储，因此 bucket 的 CORS 必须允许该前端 origin 的
`PUT/GET/HEAD` 以及签名所需的 header。否则上传会在浏览器侧被拦截。

## 实现约定

这些约定来自 `docs/PUBLIC_PRODUCT_API_CN.md`，修改代码时请保持一致：

- access token 只存在内存中，不写 `localStorage`。刷新页面时用 refresh cookie 恢复会话。
- refresh 单飞：并发 401 只触发一次轮换，成功后重试原请求一次。
- 组织切换只改变内存中的 `X-Organization-ID`；服务端会重新校验 membership。API Key 固定绑定
  自己的组织，不受切换影响。
- 一次性 secret（`nsk_`、`nwk_`、`ndi_`）使用“仅显示一次”对话框，之后列表只显示前缀与状态。
- 菜单按角色做可用性提示，但真正的边界是服务端 403。前端不假设隐藏即安全。
- `available=false` 表示 manifest 可浏览但没有在线 Worker，提交按钮禁用；manifest 不一致时同样
  禁止提交。
- `input_artifact_ids` 是位置数组，第 N 个 ID 对应 Worker 的第 N 个输入绑定，数量必须完全匹配。
  作业表单严格按 manifest 顺序上传。
- 当前没有 SSE，作业详情按 2 秒轮询，列表在有进行中作业时按 3 秒轮询，终态后退避。
- `GET /jobs` 为 keyset 分页，前端用 `useInfiniteQuery` 加「加载更多」；客户端筛选只作用于已加载的页。
- 其余列表接口仍无 cursor 分页，按完整列表处理。
