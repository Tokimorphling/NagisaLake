# Nagisalake Studio 与控制台

`/api/v1` 公共控制面的前端工作台：创作、Agent 文本助手、账户与组织、设备与凭据、workflow 目录、
由 manifest 驱动的作业表单、作业状态与输出下载、配额和审计。

技术栈为 React 19、TypeScript、Vite 8、Tailwind CSS 4 和 TanStack Query。

## Studio 与 Agent 工作台

- `/studio/video`、`/studio/image`、`/studio/audio`、`/studio/avatar`：窄侧栏、双栏创作区、大提示词
  编辑器和固定的参数/提交操作栏。右侧提供灵感、任务历史和助手输出；手机端切换创作/结果面板。
- `/studio/agent`：独立文本 Skill 执行，展示真实调用记录、流式预览、失败/取消状态以及最终文本。
  完成后可复制，或显式带回指定媒体的创作区，不会自动提交媒体任务。
- 工作流选项直接来自 Hub 的已发布目录，不把某个品牌标签匹配到另一个模型。已选工作流消失时
  禁止提交并要求重新选择；提示词不会被自动填入负面提示词字段。
- `配置并生成` 复用原有 `JobForm` 的 manifest 校验、输入顺序、设备选择和直传流程。Studio 提交后
  留在右侧结果区；其他调用者仍默认跳转作业详情。
- 历史搜索和状态筛选只作用于已加载的页。只有选中的输出申请媒体票据，不为每个历史条目预取大文件。
- Gallery 参数只按目标 manifest 的公开参数名预填，不复用他人的输入 artifact。
- 提示词和 Agent 结果不写入 URL/localStorage；跨页面传递使用带用户与组织标记的 router state，
  导入草稿后即从当前历史记录中移除传递内容。
  切换组织会清空创作/执行视图、取消旧请求；上传的 reserve/complete 与 job submit 固定使用发起时的组织。
- 浅色/深色主题沿用用户偏好。没有复制参考界面的商标、媒体素材或虚构积分价格，也没有无实现的画布按钮。

相关模块：`features/studio` 管理创作视图与 manifest 映射，`features/agent/useAgentRun.ts` 是内嵌助手
和完整 Agent 页共用的执行控制器；`features/studio/studio.css` 只影响 Studio，不重绘控制台。

Agent 后端配置及事件契约见 [`docs/AGENT_API_CN.md`](../docs/AGENT_API_CN.md)。

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
Hub 保留 legacy API 及显式启用的 Agent 接口，但本控制台仍需要账户登录。

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
- UI 区分可用、可排队、忙碌和离线；`available=false` 不等于没有在线 Worker。
  manifest 缺失/不一致或执行与队列容量已满时，提交仍被禁用。
- `input_artifact_ids` 是位置数组，第 N 个 ID 对应 Worker 的第 N 个输入绑定，数量必须完全匹配。
  作业表单严格按 manifest 顺序上传。
- 作业详情使用 SSE 更新；列表在有进行中作业时按 3 秒轮询，终态后退避。Agent 使用认证 POST SSE，
  收到终态便停止读取；中途失败不自动重放 POST，最终文本替换预览而非重复追加。
- `GET /jobs` 为 keyset 分页，前端用 `useInfiniteQuery` 加「加载更多」；客户端筛选只作用于已加载的页。
- 工作流、设备和 Gallery 等列表同样遵循各自的 cursor 契约；Studio 不把已加载集合当成全站搜索结果。
