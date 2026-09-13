import { useCallback, useEffect, useRef, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { useQueryClient } from '@tanstack/react-query'
import { endpoints } from '@/api/endpoints'
import { keys } from '@/api/queries'
import type { Job, Workflow, WorkflowInput } from '@/api/types'
import { uploadArtifact } from '@/api/upload'
import type { UploadProgress } from '@/api/upload'
import { randomUuid } from '@/lib/platform'
import { useAuth } from '@/state/auth'
import { useToast } from '@/state/toast'
import { isNumeric } from './useGenerationDraft'

export interface DeviceTarget {
  key: string
  organizationId: string
  deviceId: string
  label: string
  available: boolean
}

function coerceParameter(input: WorkflowInput, raw: string): unknown {
  const trimmed = raw.trim()
  switch (input.type) {
    case 'integer': {
      const value = Number.parseInt(trimmed, 10)
      return Number.isFinite(value) ? value : null
    }
    case 'number': {
      const value = Number.parseFloat(trimmed)
      return Number.isFinite(value) ? value : null
    }
    case 'boolean':
      return trimmed === 'true'
    default:
      return raw
  }
}

export interface GenerationSubmitOptions {
  workflow: Workflow
  parameters: WorkflowInput[]
  artifacts: WorkflowInput[]
  targets: DeviceTarget[]
  values: Record<string, string>
  files: Record<string, File | null>
  existingArtifactIds: Record<string, string>
  target: string
  onSubmitted?: (job: Job) => void
  /** Studio keeps the new task in its results pane; other callers keep the detail redirect. */
  navigateOnSubmit?: boolean
}

/** Network side of generation submission, independent from any particular UI. */
export function useGenerationSubmit({
  workflow,
  parameters,
  artifacts,
  targets,
  values,
  files,
  existingArtifactIds,
  target,
  onSubmitted,
  navigateOnSubmit = true,
}: GenerationSubmitOptions) {
  const { organizationId } = useAuth()
  const toast = useToast()
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const [progress, setProgress] = useState<UploadProgress | null>(null)
  const [submitting, setSubmitting] = useState(false)
  const active = useRef<AbortController | null>(null)
  useEffect(() => {
    setSubmitting(false)
    setProgress(null)
    return () => { const current = active.current; active.current = null; current?.abort() }
  }, [organizationId])

  const submit = useCallback(async () => {
    if (active.current || !organizationId) return
    const controller = new AbortController()
    active.current = controller
    const scope = { organizationId, signal: controller.signal }
    setSubmitting(true)
    try {
      const artifactIds: string[] = []
      const uploadCount = artifacts.length
      for (const input of artifacts) {
        const file = files[input.name]
        if (file) {
          artifactIds.push(
            await uploadArtifact(file, (value) => { if (!controller.signal.aborted) setProgress(value) }, {
              fileIndex: artifactIds.length + 1,
              fileTotal: uploadCount,
              ...scope,
            }),
          )
          continue
        }
        const existingArtifactId = existingArtifactIds[input.name]
        if (!existingArtifactId) throw new Error(`输入 ${input.name} 缺少文件`)
        artifactIds.push(existingArtifactId)
      }
      setProgress(null)

      const payload: Record<string, unknown> = {}
      for (const input of parameters) {
        const raw = values[input.name] ?? ''
        if (!input.required && raw.trim() === '') continue
        const coerced = coerceParameter(input, raw)
        if (coerced === null && isNumeric(input.type)) {
          throw new Error(`参数 ${input.name} 需要一个${input.type === 'integer' ? '整数' : '数字'}`)
        }
        payload[input.name] = coerced
      }

      const chosen = targets.find((candidate) => candidate.key === target)
      const job = await endpoints.submitJob(
        {
          workflow_id: workflow.id,
          workflow_version: workflow.version,
          parameters: payload,
          input_artifact_ids: artifactIds,
          device_organization_id: chosen?.organizationId,
          device_id: chosen?.deviceId,
        },
        randomUuid(),
        scope,
      )
      controller.signal.throwIfAborted()

      void queryClient.invalidateQueries({ queryKey: keys.jobs(organizationId) })
      void queryClient.invalidateQueries({ queryKey: keys.workflows(organizationId) })
      void queryClient.invalidateQueries({ queryKey: keys.quota(organizationId) })
      toast.success('作业已成功提交', job.id)
      onSubmitted?.(job)
      if (navigateOnSubmit) navigate(`/jobs/${job.id}`)
    } catch (error) {
      if (controller.signal.aborted || (error as Error)?.name === 'AbortError') return
      if (error instanceof Error && !('code' in error)) toast.error('提交失败', error.message)
      else toast.fromError(error, '提交作业失败')
    } finally {
      if (active.current === controller) {
        active.current = null
        setSubmitting(false)
        setProgress(null)
      }
    }
  }, [artifacts, existingArtifactIds, files, navigate, navigateOnSubmit, onSubmitted, organizationId, parameters, queryClient, target, targets, toast, values, workflow])

  return { progress, submitting, submit }
}
