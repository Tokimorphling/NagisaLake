import { useCallback, useEffect, useMemo, useState } from 'react'
import type { WorkflowInput } from '@/api/types'
import { useToast } from '@/state/toast'

export interface GenerationDraftOptions {
  parameters: WorkflowInput[]
  artifacts: WorkflowInput[]
  seedParameters?: Record<string, unknown>
  seededArtifactIds?: Record<string, string>
}

const EMPTY_SEEDED_ARTIFACTS: Record<string, string> = {}

function defaultParameterValue(input: WorkflowInput): string {
  if (input.default === null || input.default === undefined) return ''
  if (typeof input.default === 'object') return JSON.stringify(input.default)
  return String(input.default)
}

function initialParameterValues(
  parameters: WorkflowInput[],
  seedParameters?: Record<string, unknown>,
): Record<string, string> {
  return Object.fromEntries(
    parameters.map((input) => {
      if (seedParameters && input.name in seedParameters) {
        const value = seedParameters[input.name]
        return [input.name, typeof value === 'object' ? (JSON.stringify(value) ?? '') : String(value)]
      }
      return [input.name, defaultParameterValue(input)]
    }),
  )
}

/** Mirrors the useful subset of native file input `accept` matching rules. */
export function acceptsFile(file: File, accept?: string | null): boolean {
  if (!accept?.trim()) return true
  const fileName = file.name.toLowerCase()
  return accept.split(',').some((raw) => {
    const rule = raw.trim().toLowerCase()
    if (!rule) return false
    if (rule.startsWith('.')) return fileName.endsWith(rule)
    if (rule.endsWith('/*')) return file.type.startsWith(rule.slice(0, -1))
    return file.type.toLowerCase() === rule
  })
}

export function isNumeric(type: string): boolean {
  return type === 'integer' || type === 'number'
}

/** UI-only draft state. It knows nothing about jobs, navigation, or HTTP. */
export function useGenerationDraft({
  parameters,
  artifacts,
  seedParameters,
  seededArtifactIds = EMPTY_SEEDED_ARTIFACTS,
}: GenerationDraftOptions) {
  const toast = useToast()
  const seededValues = useMemo(
    () => initialParameterValues(parameters, seedParameters),
    [parameters, seedParameters],
  )
  const [values, setValues] = useState<Record<string, string>>(() => seededValues)
  const [files, setFiles] = useState<Record<string, File | null>>({})
  const [existingArtifactIds, setExistingArtifactIds] = useState<Record<string, string>>(
    () => seededArtifactIds,
  )
  const [target, setTarget] = useState('')

  // Remix data can arrive after the form component has mounted. Sync only when
  // the seed identity changes so normal typing is never overwritten.
  useEffect(() => {
    setValues(seededValues)
    setExistingArtifactIds(seededArtifactIds)
  }, [seededArtifactIds, seededValues])

  const missingRequiredCount = useMemo(
    () => artifacts.filter((input) => input.required && !files[input.name] && !existingArtifactIds[input.name]).length,
    [artifacts, existingArtifactIds, files],
  )
  const missingParamsCount = useMemo(
    () => parameters.filter((input) => input.required && (values[input.name] ?? '').trim() === '').length,
    [parameters, values],
  )

  const reset = useCallback(() => {
    setValues(seededValues)
    setFiles({})
    setExistingArtifactIds(seededArtifactIds)
    setTarget('')
  }, [seededArtifactIds, seededValues])

  const assignDroppedFiles = useCallback(
    (incoming: File[]) => {
      const remaining = [...incoming]
      const assigned: Record<string, File> = {}
      for (const input of artifacts) {
        const matchIndex = remaining.findIndex((file) => acceptsFile(file, input.content_type))
        if (matchIndex === -1) continue
        assigned[input.name] = remaining.splice(matchIndex, 1)[0]
      }
      const count = Object.keys(assigned).length
      if (count === 0) {
        toast.error('没有匹配的文件', '请检查 manifest 声明的输入媒体类型')
        return
      }
      setFiles((current) => ({ ...current, ...assigned }))
      toast.info(`已放入 ${count} 个输入文件`)
    },
    [artifacts, toast],
  )

  return {
    values,
    setValues,
    files,
    setFiles,
    existingArtifactIds,
    setExistingArtifactIds,
    target,
    setTarget,
    reset,
    assignDroppedFiles,
    missingCount: missingRequiredCount + missingParamsCount,
  }
}
