import type { Workflow } from '@/api/types'

export type StudioMedia = 'video' | 'image' | 'audio' | 'avatar'

export interface StudioMode {
  id: string
  label: string
  description: string
  aliases: string[]
}

export interface StudioModel {
  id: string
  name: string
  description: string
  media: StudioMedia
  modes: StudioMode[]
}

/**
 * Presentation-only catalog. Workflow ids and manifest fields remain owned by
 * the Hub; aliases simply help the friendly Studio labels find a matching
 * workflow when a Worker publishes it.
 */
export const STUDIO_MODELS: StudioModel[] = [
  {
    id: 'minimax-h3',
    name: 'MiniMax H3',
    description: '高质量文生与参考图生视频',
    media: 'video',
    modes: [
      { id: 't2v', label: '文生视频', description: '从文字描述生成视频', aliases: ['t2v', 'text-to-video', 'text2video'] },
      { id: 'i2v', label: '图生视频', description: '让一张图片动起来', aliases: ['i2v', 'image-to-video', 'image2video'] },
      { id: 'first-last', label: '首尾帧', description: '用首帧和尾帧控制转场', aliases: ['first-last', 'flf', 'start-end', 'keyframe'] },
    ],
  },
  {
    id: 'wan-22',
    name: 'Wan 2.2',
    description: '开源高质量视频生成模型',
    media: 'video',
    modes: [
      { id: 't2v', label: '文生视频', description: '从文字描述生成视频', aliases: ['t2v', 'text-to-video', 'text2video'] },
      { id: 'i2v', label: '图生视频', description: '基于参考图生成动态视频', aliases: ['i2v', 'image-to-video', 'image2video'] },
    ],
  },
  {
    id: 'image',
    name: '图片生成',
    description: '把想法变成可用图片',
    media: 'image',
    modes: [{ id: 'txt2img', label: '文生图片', description: '从文字描述生成图片', aliases: ['txt2img', 'text-to-image', 'text2image', 'image'] }],
  },
  {
    id: 'audio',
    name: '音频生成',
    description: '为作品生成声音和配乐',
    media: 'audio',
    modes: [{ id: 'tts', label: '文本转语音', description: '从文字生成语音', aliases: ['tts', 'text-to-speech', 'audio'] }],
  },
  {
    id: 'avatar',
    name: '数字人',
    description: '用脚本和声音生成数字人视频',
    media: 'avatar',
    modes: [{ id: 'avatar', label: '数字人视频', description: '从脚本生成口播视频', aliases: ['avatar', 'digital-human', 'talking-head'] }],
  },
]

function searchableWorkflowText(workflow: Workflow): string {
  return [workflow.id, workflow.manifest?.display_name, workflow.manifest?.description]
    .filter(Boolean)
    .join(' ')
    .toLowerCase()
}

function mediaMatches(workflow: Workflow, media: StudioMedia): boolean {
  const text = searchableWorkflowText(workflow)
  if (media === 'avatar') return /avatar|digital-human|talking-head|数字人/i.test(text)
  const outputs = workflow.manifest?.outputs.map((output) => output.content_type) ?? workflow.output_types
  if (media === 'video') return outputs.some((value) => value.toLowerCase().includes('video'))
  if (media === 'image') return outputs.some((value) => value.toLowerCase().includes('image'))
  return outputs.some((value) => value.toLowerCase().includes('audio'))
}

/** Resolve a friendly model/mode to the best currently published workflow. */
export function resolveStudioWorkflow(
  workflows: Workflow[],
  model: StudioModel,
  mode: StudioMode,
): Workflow | null {
  const candidates = workflows.filter((workflow) => workflow.manifest_consistent && mediaMatches(workflow, model.media))
  if (candidates.length === 0) return null
  const modeMatch = candidates.find((workflow) => {
    const text = searchableWorkflowText(workflow)
    return mode.aliases.some((alias) => text.includes(alias))
  })
  if (modeMatch) return modeMatch
  return candidates.find((workflow) => workflow.available) ?? candidates[0] ?? null
}

export function modelsForMedia(media: StudioMedia): StudioModel[] {
  return STUDIO_MODELS.filter((model) => model.media === media)
}
