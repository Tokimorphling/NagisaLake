# Workflow Manifest 设计

## 目标

ComfyUI workflow 的内部图通常包含几十个节点和大量 widget。直接把整张图暴露给消费者会带来三个问题：

- 内部节点名称和 JSON Pointer 不稳定，升级自定义节点后容易破坏调用方。
- 大部分 widget 是实现细节，不应成为远程可修改参数。
- 仅靠 workflow JSON 无法完整得到 `COMBO` 选项、数值范围、步长和自定义节点语义。

Nagisalake 因此把“可执行图”和“公共调用契约”分开。Worker 保存完整模板，只把显式 allowlist
生成的 manifest 注册给 Hub。消费者读取 manifest 构造请求，但永远不能提交任意 workflow JSON。

## 当前流程

1. Worker 加载 `[[workflows]]` 配置和 workflow JSON。
2. API-format workflow 直接作为执行模板；editor-format workflow 只做 best-effort 归一化。
3. `workflows.parameters` 和 `workflows.inputs` 决定哪些字段属于公共输入。
4. Worker 从 JSON 默认值和 editor metadata 推断基础类型，生成 schema version 1 manifest。
5. manifest 随 `Register` 消息发送到 Hub。
6. Hub 按 `(id, version)` 聚合所有在线 Worker，通过 `GET /v1/workflows` 返回契约和可用容量。
7. 同一 `(id, version)` 的 Worker 如果上报不同 manifest，Hub 将
   `manifest_consistent` 置为 `false`。此时前端不应允许新建作业。

当前 manifest 形状如下：

```json
{
  "schema_version": 1,
  "display_name": "image-edit",
  "description": null,
  "inputs": [
    {
      "name": "prompt",
      "kind": "parameter",
      "type": "string",
      "content_type": null,
      "pointer": "/6/inputs/text",
      "required": false,
      "default": "a portrait",
      "options": [],
      "node_id": "6",
      "node_type": "CLIPTextEncode",
      "field": "text"
    },
    {
      "name": "source_image",
      "kind": "artifact",
      "type": "image",
      "content_type": "image/*",
      "pointer": "/10/inputs/image",
      "required": true,
      "default": null,
      "options": [],
      "node_id": "10",
      "node_type": "LoadImage",
      "field": "image"
    }
  ],
  "outputs": [
    {"name": "output_0", "content_type": "image/png"}
  ],
  "warnings": []
}
```

Worker 配置示例：

```toml
[[workflows]]
id = "image-edit"
version = "v1"
file = "./workflows/image-edit-api.json"
output_types = ["image/png"]

[workflows.parameters]
prompt = "/6/inputs/text"
seed = "/3/inputs/seed"

[[workflows.inputs]]
index = 0
name = "source_image"
content_type = "image/*"
pointer = "/10/inputs/image"
```

`pointer`、`node_id` 和 `node_type` 当前用于诊断。未来公开服务应在普通用户响应中隐藏这些内部字段，
只在 Worker 管理或调试接口返回。

## Editor JSON 与 API JSON

ComfyUI 的普通保存文件是 editor workflow：顶层包含 `nodes`、`links` 和 `widgets_values`，主要用于
恢复画布。ComfyUI `/prompt` 接受的是 API-format prompt：以 node id 为 key，并包含
`class_type` 和 `inputs`。

Nagisalake 可以读取 editor workflow 并生成 mock manifest，但 widget 到输入字段的映射只能
best-effort 推断，因此这类文件会携带 warning。具名 `widgets_values` 对象按字段精确匹配；数组形式
会按 `INT`、`FLOAT`、`BOOLEAN`、`STRING`/`COMBO` 类型跳过附加控件值，例如 seed 后面的
`control_after_generate`。但自定义节点仍可能有无法静态识别的 widget，因此生产执行应使用 ComfyUI
的 API-format 导出；后续的动态补全功能应优先用本机 `/object_info` 校验契约。显式配置的媒体 MIME 是公共契约的
权威来源，例如内部节点把视频解码为 `IMAGE`，`content_type = "video/*"` 仍会向消费者声明
`type = "video"`。本地 `test_workflows` 下的 8 个复杂 editor workflow 已全部用于纯解析测试，
不会被提交到 ComfyUI；该目录也被 `.gitignore` 排除，不随公开源码发布。

参考：

- [ComfyUI Workflow JSON schema](https://docs.comfy.org/specs/workflow_json)
- [ComfyUI `/object_info` 实现](https://github.com/Comfy-Org/ComfyUI/blob/master/server.py)

### `test_workflows` 观察结果

本地 8 张样本图合计有 352 个节点、34 种节点类型、475 个已连接输入，以及 792 个未连接且带
widget 的候选输入。280 个节点使用数组形式的 `widgets_values`，25 个使用对象形式。若把 792 个候选项
全部自动公开，模型路径、节点开关、内部 ID 和插件实现细节都会进入用户 API；这也是 manifest 必须
以显式 allowlist/契约节点为边界，而静态扫描只能提供候选项和 warning 的直接依据。

## 推荐的统一方式

长期方案应采用四级信息来源，优先级从高到低：

1. **显式公共契约**：workflow 作者决定字段名、说明、是否必填、媒体类型和输出名称。这是唯一权威来源。
2. **Nagisalake 契约节点**：提供 `Nagisalake Parameter`、`Nagisalake Artifact Input` 和
   `Nagisalake Artifact Output` ComfyUI 节点。作者在画布中填写稳定公共名称，Worker 扫描这些节点
   自动生成 manifest，避免手写 JSON Pointer。这是最方便且最稳定的后续实现。
3. **ComfyUI `/object_info` 补全**：Worker 在本机 ComfyUI 可用时，用节点类的 `INPUT_TYPES()`
   补全 enum options、min/max/step、tooltip 和 output metadata。
4. **静态推断兜底**：从默认 JSON 值和 editor slot type 推断 `string`、`integer`、`number`、
   `boolean`、`image`、`audio`、`video` 或 `artifact`。

不建议自动公开所有未连接 widget。自动扫描适合生成候选项和 warning，不适合决定远程安全边界。

## 版本与缓存

- `workflow id + version` 是不可变公共契约；契约变化应发布新 version。
- manifest 自身使用独立 `schema_version`，便于协议向后兼容。
- 下一版可加入规范化 manifest hash 和 workflow template hash。Hub 用 hash 快速发现同版本漂移，
  前端也可用它做表单缓存键。
- Hub 当前只聚合在线 Worker。公开服务应把已审核的 manifest 持久化到 PostgreSQL，在线 Worker
  只贡献可用容量，不决定目录是否存在。

## 输出的特殊性

ComfyUI 输出经常由自定义节点动态产生，静态图不能可靠确定文件个数和格式。manifest 应描述公共
输出槽位和允许的 MIME 类型，实际作业结果仍以 `output_artifact_ids` 为准。未来需要支持一个槽位
多个文件时，应给输出增加 `cardinality`，例如 `one`、`optional` 或 `many`，而不是依赖数组位置。
