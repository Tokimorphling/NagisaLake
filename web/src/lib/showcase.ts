import type { Job } from '@/api/types'

/** A labelled parameter row on the showcase poster. */
export interface ShowcaseField {
  label: string
  value: string
}

function stringify(value: unknown): string {
  if (value === null || value === undefined) return ''
  if (typeof value === 'string') return value
  if (typeof value === 'object') return JSON.stringify(value)
  return String(value)
}

/** First parameter whose name matches, searched in the manifest's own order. */
function pick(
  parameters: Record<string, unknown>,
  pattern: RegExp,
): { name: string; value: string } | null {
  for (const [name, raw] of Object.entries(parameters)) {
    if (!pattern.test(name)) continue
    const value = stringify(raw).trim()
    if (value !== '') return { name, value }
  }
  return null
}

const PROMPT_PATTERN = /^prompt$|positive|^text$/i
const NEGATIVE_PATTERN = /negative/i

/**
 * Splits a job's parameters into the poster's prompt block and its metric rows.
 *
 * The manifest has no notion of "this is the prompt", so this is a name
 * heuristic. Anything not claimed by a known slot still shows up in `extra`, so
 * a workflow with unusual parameter names loses nothing.
 */
export function showcaseFieldsFromParameters(parameters: Record<string, unknown>): {
  prompt: string | null
  negative: string | null
  metrics: ShowcaseField[]
  extra: ShowcaseField[]
} {
  const claimed = new Set<string>()

  const prompt = pick(parameters, PROMPT_PATTERN)
  if (prompt) claimed.add(prompt.name)
  const negative = pick(parameters, NEGATIVE_PATTERN)
  if (negative) claimed.add(negative.name)

  const metrics: ShowcaseField[] = []
  const slots: Array<{ label: string; pattern: RegExp }> = [
    { label: 'Model', pattern: /model|ckpt|checkpoint/i },
    { label: 'Seed', pattern: /seed/i },
    { label: 'Steps', pattern: /^steps$|num_steps|sampling_steps/i },
    { label: 'CFG', pattern: /cfg|guidance/i },
    { label: 'Sampler', pattern: /sampler|scheduler/i },
    { label: 'Size', pattern: /resolution|^size$/i },
  ]

  for (const slot of slots) {
    const hit = pick(parameters, slot.pattern)
    if (!hit || claimed.has(hit.name)) continue
    claimed.add(hit.name)
    metrics.push({ label: slot.label, value: hit.value })
  }

  const extra: ShowcaseField[] = Object.entries(parameters)
    .filter(([name]) => !claimed.has(name))
    .map(([name, raw]) => ({ label: name, value: stringify(raw).trim() }))
    .filter((field) => field.value !== '')

  return {
    prompt: prompt?.value ?? null,
    negative: negative?.value ?? null,
    metrics,
    extra,
  }
}

export function showcaseFields(job: Job): ReturnType<typeof showcaseFieldsFromParameters> {
  return showcaseFieldsFromParameters(job.parameters ?? {})
}

/**
 * Wall-clock time from submission to the terminal event.
 *
 * Prefers the event timeline over `updated_at`, which also moves for late
 * bookkeeping writes that are not part of the run.
 */
export function jobDurationMs(job: Job): number | null {
  const terminal = job.events
    .filter((event) => event.kind === 'completed' || event.kind === 'failed' || event.kind === 'cancelled')
    .sort((left, right) => right.sequence - left.sequence)[0]
  const end = terminal?.unix_ms ?? (job.state === 'completed' ? job.updated_at_unix_ms : null)
  if (end === null) return null
  const delta = end - job.created_at_unix_ms
  return delta >= 0 ? delta : null
}

export function formatDurationMs(ms: number | null): string {
  if (ms === null) return '—'
  if (ms < 1000) return `${ms} ms`
  const seconds = ms / 1000
  if (seconds < 60) return `${seconds.toFixed(seconds < 10 ? 1 : 0)} 秒`
  const minutes = Math.floor(seconds / 60)
  const rest = Math.round(seconds % 60)
  if (minutes < 60) return `${minutes} 分 ${rest} 秒`
  const hours = Math.floor(minutes / 60)
  return `${hours} 小时 ${minutes % 60} 分`
}
