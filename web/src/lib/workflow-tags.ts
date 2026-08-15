import type { Workflow, WorkflowManifest } from '@/api/types'

/**
 * Coarse category for the catalog filter.
 *
 * Derived from the manifest rather than a server field: the Hub has no notion of
 * workflow taxonomy, so this is a UI-side heuristic over declared inputs and
 * outputs. `other` is the honest fallback when nothing matches.
 */
export type WorkflowTag = 'text-to-image' | 'image-to-image' | 'video' | 'upscale' | 'audio' | 'other'

export const WORKFLOW_TAG_LABELS: Record<WorkflowTag, string> = {
  'text-to-image': '文生图',
  'image-to-image': '图生图',
  video: '视频生成',
  upscale: 'Upscale',
  audio: '音频',
  other: '其他',
}

export const WORKFLOW_TAG_TONES: Record<
  WorkflowTag,
  'accent' | 'violet' | 'info' | 'warning' | 'success' | 'neutral'
> = {
  'text-to-image': 'accent',
  'image-to-image': 'violet',
  video: 'info',
  upscale: 'warning',
  audio: 'success',
  other: 'neutral',
}

function mentionsUpscale(text: string): boolean {
  return /upscale|放大|超分|super.?res|esrgan|hires/i.test(text)
}

/**
 * Tags a workflow by what its manifest declares.
 *
 * Order matters: upscale is checked before image-to-image because an upscaler is
 * also an image→image graph, and the more specific label is the useful one.
 */
export function workflowTags(workflow: Workflow): WorkflowTag[] {
  const manifest: WorkflowManifest | null = workflow.manifest
  const outputs = workflow.output_types.join(' ').toLowerCase()
  const artifactInputs = manifest?.inputs.filter((input) => input.kind === 'artifact') ?? []
  const haystack = [
    workflow.id,
    manifest?.display_name ?? '',
    manifest?.description ?? '',
  ].join(' ')

  const inputContentTypes = artifactInputs
    .map((input) => input.content_type ?? '')
    .join(' ')
    .toLowerCase()

  const producesVideo =
    outputs.includes('video') ||
    (manifest?.outputs ?? []).some((output) => output.content_type.startsWith('video/'))
  const producesAudio =
    outputs.includes('audio') ||
    (manifest?.outputs ?? []).some((output) => output.content_type.startsWith('audio/'))
  const producesImage =
    outputs.includes('image') ||
    (manifest?.outputs ?? []).some((output) => output.content_type.startsWith('image/'))

  const takesImage = inputContentTypes.includes('image') || artifactInputs.length > 0

  const tags: WorkflowTag[] = []

  if (producesVideo) tags.push('video')
  if (producesAudio) tags.push('audio')

  if (producesImage || tags.length === 0) {
    if (mentionsUpscale(haystack)) tags.push('upscale')
    else if (takesImage) tags.push('image-to-image')
    else if (producesImage) tags.push('text-to-image')
  }

  return tags.length > 0 ? tags : ['other']
}

/** Every tag present in a catalog, in the canonical display order. */
export function collectTags(workflows: Workflow[]): WorkflowTag[] {
  const order: WorkflowTag[] = [
    'text-to-image',
    'image-to-image',
    'video',
    'upscale',
    'audio',
    'other',
  ]
  const present = new Set(workflows.flatMap(workflowTags))
  return order.filter((tag) => present.has(tag))
}
