import type { Device } from '@/api/types'

/**
 * Coarse worker runtime for the device list.
 *
 * Inferred UI-side, same honest-heuristic pattern as workflow-tags.ts: the Hub
 * device payload carries no engine field (registration labels are even
 * stripped by sanitize_workflow_catalog), so this reads what the worker did
 * publish — node name and workflow ids. `unknown` is the fallback when
 * nothing matches, never a claim about the real runtime.
 */
export type WorkerRuntime = 'sglang' | 'comfyui' | 'unknown'

export type DeviceOutputKind = 'image' | 'video' | 'audio'

export interface DeviceRuntimeInfo {
  kind: WorkerRuntime
  label: string
  /** Coarse media types the device's registered workflows produce. */
  outputs: DeviceOutputKind[]
}

export const WORKER_RUNTIME_LABELS: Record<WorkerRuntime, string> = {
  sglang: 'SGLang',
  comfyui: 'ComfyUI',
  unknown: '未声明',
}

export const WORKER_RUNTIME_TONES: Record<
  WorkerRuntime,
  'accent' | 'violet' | 'neutral'
> = {
  sglang: 'violet',
  comfyui: 'accent',
  unknown: 'neutral',
}

export const DEVICE_OUTPUT_LABELS: Record<DeviceOutputKind, string> = {
  image: '图片',
  video: '视频',
  audio: '音频',
}

/** SGLang-compatible servers expose OpenAI-style image endpoints; ids usually carry the prefix. */
function mentionsSgLang(text: string): boolean {
  return /sglang|sg_lang/i.test(text)
}

function mentionsComfyUi(text: string): boolean {
  return /comfy/i.test(text)
}

function outputKinds(device: Device): DeviceOutputKind[] {
  const kinds = new Set<DeviceOutputKind>()
  for (const workflow of device.workflows) {
    for (const type of workflow.output_types) {
      const media = type.toLowerCase().split('/')[0]
      if (media === 'image' || media === 'video' || media === 'audio') kinds.add(media)
    }
  }
  const order: DeviceOutputKind[] = ['image', 'video', 'audio']
  return order.filter((kind) => kinds.has(kind))
}

/**
 * Reads one device's runtime and coarse output capability.
 *
 * Workflow ids beat the node name: a node may be renamed freely, while a
 * registered workflow id like `sglang-sd3-txt2img` can only come from a worker
 * configured for that engine.
 */
export function deviceRuntime(device: Device): DeviceRuntimeInfo {
  const workflowText = device.workflows.map((workflow) => workflow.id).join(' ')
  const nodeText = `${device.node_name} ${device.namespace}`
  const kind: WorkerRuntime = mentionsSgLang(`${workflowText} ${nodeText}`)
    ? 'sglang'
    : mentionsComfyUi(workflowText) || mentionsComfyUi(nodeText)
      ? 'comfyui'
      : 'unknown'
  return { kind, label: WORKER_RUNTIME_LABELS[kind], outputs: outputKinds(device) }
}
