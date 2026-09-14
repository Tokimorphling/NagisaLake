import type { GalleryItem, JobSummary, Workflow } from '@/api/types'

export type StudioMedia = 'video' | 'image' | 'audio' | 'avatar'
export type StudioInputMode = 'all' | 'text' | 'reference' | 'motion'
export const MEDIA_LABELS: Record<StudioMedia, string> = { video: '视频生成', image: '图片生成', audio: '音频生成', avatar: '数字人' }
export const STUDIO_MEDIA: StudioMedia[] = ['avatar', 'video', 'image', 'audio']

export function isStudioMedia(value: string | undefined): value is StudioMedia {
  return STUDIO_MEDIA.includes(value as StudioMedia)
}
export function workflowKey(workflow: Pick<Workflow, 'id' | 'version'>) { return JSON.stringify([workflow.id, workflow.version]) }
export function workflowTitle(workflow: Workflow) { return workflow.manifest?.display_name || workflow.id }

/** Only published Hub metadata defines capabilities; never substitute another model. */
export function workflowsForMedia(workflows: Workflow[], media: StudioMedia, mode: StudioInputMode = 'all') {
  return workflows.filter((workflow) => {
    const outputs = workflow.manifest?.outputs.map((output) => output.content_type) ?? workflow.output_types
    const text = `${workflow.id} ${workflow.manifest?.display_name ?? ''} ${workflow.manifest?.description ?? ''}`
    const matches = media === 'avatar' ? /avatar|digital[-_ ]human|talking[-_ ]head|数字人/i.test(text)
      : outputs.some((type) => type.toLowerCase().startsWith(`${media}/`))
    if (!matches) return false
    if (mode === 'all') return true
    if (!workflow.manifest) return false
    const hasReference = workflow.manifest.inputs.some((input) => input.kind === 'artifact')
    if (mode === 'motion') {
      // 动作迁移需要视频参考素材驱动：只保留声明了 video artifact 输入的工作流。
      return workflow.manifest.inputs.some(
        (input) => input.kind === 'artifact' && (input.content_type ?? '').toLowerCase().startsWith('video/'),
      )
    }
    return mode === 'reference' ? hasReference : !hasReference
  })
}

const PROMPT_NAMES = ['prompt', 'positive_prompt', 'positive', 'text', 'description', 'instruction', '提示词']
const negativeName = (name: string) => /negative|负面|反向/i.test(name)
export function promptField(workflow: Workflow | null): string | null {
  const fields = workflow?.manifest?.inputs.filter((input) => input.kind === 'parameter' && input.type === 'string' && !negativeName(input.name)) ?? []
  for (const name of PROMPT_NAMES) {
    const field = fields.find((field) => field.name.toLowerCase() === name)
    if (field) return field.name
  }
  return fields.find((field) => /prompt|text|description|提示词/i.test(field.name))?.name ?? null
}

export function promptText(parameters: Record<string, unknown> | null | undefined): string {
  if (!parameters || typeof parameters !== 'object' || Array.isArray(parameters)) return ''
  const fields = Object.entries(parameters).filter(([key, value]) => !negativeName(key) && typeof value === 'string')
  for (const name of PROMPT_NAMES) {
    const value = fields.find(([key]) => key.toLowerCase() === name)?.[1]
    if (typeof value === 'string') return value
  }
  return fields.find(([key]) => /prompt|text|description|提示词/i.test(key))?.[1] as string ?? ''
}

export function compatibleParameters(workflow: Workflow, parameters: Record<string, unknown> | null | undefined) {
  return Object.fromEntries((workflow.manifest?.inputs ?? []).filter((field) => field.kind === 'parameter' && parameters && Object.hasOwn(parameters, field.name)).map((field) => [field.name, parameters![field.name]]))
}

export function filterHistory(jobs: JobSummary[], search: string, status: 'all' | 'active' | 'completed' | 'failed') {
  const query = search.trim().toLocaleLowerCase()
  return jobs.filter((job) => {
    if (status === 'active' && ['completed', 'failed', 'cancelled'].includes(job.state)) return false
    if (status === 'completed' && job.state !== 'completed') return false
    if (status === 'failed' && job.state !== 'failed') return false
    return !query || `${job.id} ${job.workflow_id} ${promptText(job.parameters)}`.toLocaleLowerCase().includes(query)
  })
}

export function galleryForMedia(items: GalleryItem[], media: StudioMedia, search: string) {
  const query = search.trim().toLocaleLowerCase()
  return items.filter((item) => item.media_kind === (media === 'avatar' ? 'video' : media)
    && (!query || `${item.display_name} ${item.workflow_id} ${promptText(item.parameters)}`.toLocaleLowerCase().includes(query)))
}

/** One-shot router transfer; consumed state is cleared after importing the draft. */
export interface PromptTransfer { organizationId: string; userId: string; prompt: string }
export function transferredPrompt(state: unknown, organizationId: string, userId: string): string {
  if (!state || typeof state !== 'object' || !('studioPrompt' in state)) return ''
  const transfer = state.studioPrompt as Partial<PromptTransfer> | null
  return userId !== '' && transfer?.organizationId === organizationId && transfer.userId === userId && typeof transfer.prompt === 'string' ? transfer.prompt : ''
}
