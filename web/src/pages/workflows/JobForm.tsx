import { useEffect, useMemo, useRef, useState } from 'react'
import { useArtifactDownload } from '@/api/queries'
import type { Device, Job, Workflow, WorkflowInput } from '@/api/types'
import { formatBytes } from '@/lib/format'
import { submitBlockedReason, workflowCapacity } from '@/lib/workflow-status'
import { Badge } from '@/components/ui/display'
import { Modal } from '@/components/ui/Modal'
import { Button, Checkbox, Field, Input, Select, Textarea, cx } from '@/components/ui/primitives'
import { useGenerationDraft, acceptsFile, isNumeric } from '@/features/studio/hooks/useGenerationDraft'
import { useGenerationSubmit, type DeviceTarget } from '@/features/studio/hooks/useGenerationSubmit'

export interface GalleryRemixSeed {
  itemId: string
  workflowVersion: string
  parameters: Record<string, unknown>
}


export function JobForm({
  workflow,
  devices,
  open,
  onClose,
  remixJob,
  galleryRemix,
  initialParameters,
  onSubmitted,
  navigateOnSubmit = true,
}: {
  workflow: Workflow
  devices: Device[]
  open: boolean
  onClose: () => void
  remixJob?: Job | null
  galleryRemix?: GalleryRemixSeed | null
  /** Optional friendly Studio values, merged into the manifest-backed form. */
  initialParameters?: Record<string, unknown>
  onSubmitted?: (job: Job) => void
  navigateOnSubmit?: boolean
}) {
  const manifest = workflow.manifest
  const parameters = useMemo(
    () => manifest?.inputs.filter((input) => input.kind === 'parameter') ?? [],
    [manifest],
  )
  // Artifact order is the contract: input_artifact_ids is positional and its
  // length must exactly match the worker's bound inputs.
  const artifacts = useMemo(
    () => manifest?.inputs.filter((input) => input.kind === 'artifact') ?? [],
    [manifest],
  )

  const targets = useMemo<DeviceTarget[]>(() => {
    const byKey = new Map<string, DeviceTarget>()
    for (const worker of workflow.workers) {
      const key = `${worker.organization_id}/${worker.worker_id}`
      const device = devices.find(
        (candidate) =>
          candidate.device_organization_id === worker.organization_id &&
          candidate.device_id === worker.worker_id,
      )
      byKey.set(key, {
        key,
        organizationId: worker.organization_id,
        deviceId: worker.worker_id,
        label: device?.node_name ? `${device.node_name} (${worker.worker_id})` : worker.worker_id,
        available: worker.available,
      })
    }
    return [...byKey.values()]
  }, [devices, workflow.workers])

  const seedParameters = initialParameters ?? remixJob?.parameters ?? galleryRemix?.parameters
  const seededArtifactIds = useMemo(
    () =>
      Object.fromEntries(
        artifacts.flatMap((input, index) => {
          const artifactId = remixJob?.input_artifact_ids[index]
          return artifactId ? [[input.name, artifactId]] : []
        }),
      ) as Record<string, string>,
    [artifacts, remixJob],
  )

  const draft = useGenerationDraft({
    parameters,
    artifacts,
    seedParameters,
    seededArtifactIds,
  })
  const {
    values,
    setValues,
    files,
    setFiles,
    existingArtifactIds,
    setExistingArtifactIds,
    target,
    setTarget,
  } = draft
  const [globalDragging, setGlobalDragging] = useState(false)
  const dragDepth = useRef(0)

  const capacity = workflowCapacity(workflow)
  // Same three-state logic as the catalog card, so the form cannot say "offline"
  // about a device that is merely busy.
  const blockedReason = submitBlockedReason(workflow, capacity)

  const missingCount = draft.missingCount

  // A drag that enters anywhere over the page gets a full-screen target. This
  // is especially helpful for tall parameter forms where a field may be below
  // the current viewport.
  useEffect(() => {
    if (!open || artifacts.length === 0) return
    const containsFiles = (event: DragEvent) => event.dataTransfer?.types.includes('Files') ?? false
    const onDragEnter = (event: DragEvent) => {
      if (!containsFiles(event)) return
      event.preventDefault()
      dragDepth.current += 1
      setGlobalDragging(true)
    }
    const onDragOver = (event: DragEvent) => {
      if (containsFiles(event)) event.preventDefault()
    }
    const onDragLeave = (event: DragEvent) => {
      if (!containsFiles(event)) return
      dragDepth.current = Math.max(0, dragDepth.current - 1)
      if (dragDepth.current === 0) setGlobalDragging(false)
    }
    const onDrop = (event: DragEvent) => {
      if (!containsFiles(event)) return
      event.preventDefault()
      dragDepth.current = 0
      setGlobalDragging(false)
      draft.assignDroppedFiles(Array.from(event.dataTransfer?.files ?? []))
    }
    document.addEventListener('dragenter', onDragEnter)
    document.addEventListener('dragover', onDragOver)
    document.addEventListener('dragleave', onDragLeave)
    document.addEventListener('drop', onDrop)
    return () => {
      document.removeEventListener('dragenter', onDragEnter)
      document.removeEventListener('dragover', onDragOver)
      document.removeEventListener('dragleave', onDragLeave)
      document.removeEventListener('drop', onDrop)
    }
  }, [artifacts.length, draft.assignDroppedFiles, open])

  const { progress, submitting, submit } = useGenerationSubmit({
    workflow,
    parameters,
    artifacts,
    targets,
    values,
    files,
    existingArtifactIds,
    target,
    navigateOnSubmit,
    onSubmitted: (job) => {
      draft.reset()
      onClose()
      onSubmitted?.(job)
    },
  })

  const progressLabel = progress
    ? { hashing: '计算校验和', uploading: '上传中', completing: '校验中' }[progress.stage]
    : null

  const showFileCount = progress && (progress.fileTotal ?? 0) > 1

  return (
    <Modal
      open={open}
      width="lg"
      title={
        <div className="flex items-center gap-2">
          <span>{manifest?.display_name || workflow.id}</span>
          {(remixJob || galleryRemix) && (
            <span className="rounded-full bg-accent/20 px-2.5 py-0.5 text-[10px] font-mono font-semibold text-accent border border-accent/30">
              Remix 参数预填
            </span>
          )}
        </div>
      }
      description={
        manifest?.description ??
        `${workflow.id} · ${workflow.version} · 输出 ${workflow.output_types.join(', ') || '未声明'}`
      }
      onClose={onClose}
      footer={
        <div className="flex items-center justify-between w-full">
          <Button size="sm" variant="ghost" onClick={draft.reset} disabled={submitting}>
            重置表单
          </Button>
          <div className="flex items-center gap-2">
            <Button size="sm" variant="ghost" onClick={onClose} disabled={submitting}>
              取消
            </Button>
            <Button
              size="sm"
              variant="primary"
              loading={submitting}
              disabled={submitting || !!blockedReason || missingCount > 0}
              onClick={() => void submit()}
            >
              {missingCount > 0 ? `提交作业 · 还需 ${missingCount} 项` : '提交作业'}
            </Button>
          </div>
        </div>
      }
    >
      <div className="space-y-6 py-2">
        {remixJob && (
          <div className="rounded-xl border border-accent/30 bg-accent/10 p-3.5 text-xs leading-relaxed text-accent">
            <p className="font-semibold">正在 Remix 作业 {remixJob.id.slice(0, 12)}…</p>
            <p className="mt-1 text-muted">
              已复用匹配的参数与 {Object.keys(seededArtifactIds).length} 个原始输入对象。
              {remixJob.workflow_version !== workflow.version &&
                ` 原作业版本为 ${remixJob.workflow_version}，当前将提交到 ${workflow.version}，仅保留名称或位置仍匹配的输入。`}
            </p>
          </div>
        )}

        {galleryRemix && !remixJob && (
          <div className="rounded-xl border border-accent/30 bg-accent/10 p-3.5 text-xs leading-relaxed text-accent">
            <p className="font-semibold">正在 Remix Gallery 参数卡</p>
            <p className="mt-1 text-muted">
              已从公开快照预填名称仍匹配的生成参数。出于权限与隐私考虑，原作业的输入文件不会复用，请重新选择需要的文件。
              {galleryRemix.workflowVersion !== workflow.version &&
                ` 分享版本为 ${galleryRemix.workflowVersion}，当前将提交到 ${workflow.version}。`}
            </p>
          </div>
        )}

        {blockedReason && (
          <div className="rounded-xl border border-warning/30 bg-warning/10 p-3.5 text-xs text-warning">
            ⚠️ {blockedReason}
          </div>
        )}

        {artifacts.length > 0 && (
          <section className="space-y-3">
            <h3 className="text-xs font-mono font-semibold tracking-wider text-muted uppercase">
              输入文件 ({artifacts.length})
            </h3>
            {artifacts.map((input, index) => (
              <DropZoneField
                key={input.name}
                input={input}
                index={index}
                file={files[input.name] ?? null}
                existingArtifactId={existingArtifactIds[input.name]}
                disabled={submitting}
                onFileSelect={(file) =>
                  setFiles((current) => ({
                    ...current,
                    [input.name]: file,
                  }))
                }
                onClearExisting={() =>
                  setExistingArtifactIds((current) => {
                    const next = { ...current }
                    delete next[input.name]
                    return next
                  })
                }
              />
            ))}
          </section>
        )}

        {parameters.length > 0 && (
          <section className="space-y-3">
            <h3 className="text-xs font-mono font-semibold tracking-wider text-muted uppercase">
              参数配置 ({parameters.length})
            </h3>
            {parameters.map((input) => (
              <ParameterField
                key={input.name}
                input={input}
                value={values[input.name] ?? ''}
                disabled={submitting}
                onChange={(next) => setValues((current) => ({ ...current, [input.name]: next }))}
              />
            ))}
          </section>
        )}

        {targets.length > 0 && (
          <section className="space-y-3">
            <h3 className="text-xs font-semibold tracking-wide text-muted uppercase">执行设备</h3>
            <Field label="目标设备" hint="留空由 Hub 选择可用设备。shared pool device 的用量记在你当前组织。">
              {(id) => (
                <Select
                  id={id}
                  value={target}
                  disabled={submitting}
                  onChange={(event) => setTarget(event.target.value)}
                >
                  <option value="">自动选择</option>
                  {targets.map((candidate) => (
                    <option key={candidate.key} value={candidate.key}>
                      {candidate.label}
                      {candidate.available ? '' : '（队列已满）'}
                    </option>
                  ))}
                </Select>
              )}
            </Field>
          </section>
        )}

        {manifest && manifest.outputs.length > 0 && (
          <section className="space-y-2">
            <h3 className="text-xs font-semibold tracking-wide text-muted uppercase">预期输出</h3>
            <div className="flex flex-wrap gap-1.5">
              {manifest.outputs.map((output) => (
                <Badge key={output.name} tone="neutral">
                  {output.name}
                  <span className="text-subtle">{output.content_type}</span>
                </Badge>
              ))}
            </div>
          </section>
        )}

        {progress && (
          <div className="space-y-1.5 rounded-lg border border-accent/30 bg-accent/10 px-3 py-2.5 text-xs text-accent">
            <div className="flex items-center justify-between gap-2">
              <span className="font-medium">
                {progressLabel}
                {showFileCount && (
                  <span className="ml-1.5 font-mono text-[10px] text-accent/80">
                    文件 {progress.fileIndex}/{progress.fileTotal}
                  </span>
                )}
              </span>
              {progress.stage === 'uploading' && (
                <span className="font-mono text-[10px] text-accent/80">{progress.percent}%</span>
              )}
            </div>
            <div
              className="h-1.5 overflow-hidden rounded-full bg-accent/15 ring-1 ring-accent/20 ring-inset"
              role="progressbar"
              aria-valuenow={progress.percent}
              aria-valuemin={0}
              aria-valuemax={100}
              aria-label={progressLabel ?? '上传进度'}
            >
              <div
                className="h-full rounded-full bg-accent transition-[width] duration-200"
                style={{ width: `${Math.max(progress.stage === 'uploading' ? progress.percent : progress.stage === 'completing' ? 100 : 8, 2)}%` }}
              />
            </div>
            <p className="truncate font-mono text-[10px] text-accent/70">{progress.fileName}</p>
          </div>
        )}

        {globalDragging && (
          <div className="pointer-events-none fixed inset-0 z-[70] grid place-items-center bg-black/70 p-6 backdrop-blur-md">
            <div className="w-full max-w-lg rounded-3xl border-2 border-dashed border-accent bg-surface/95 p-10 text-center shadow-2xl shadow-accent/20">
              <div className="mx-auto grid size-16 place-items-center rounded-2xl bg-accent/15 text-accent">
                <svg className="size-8" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8">
                  <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4M17 8l-5-5-5 5M12 3v12" />
                </svg>
              </div>
              <p className="mt-5 text-lg font-semibold">释放文件，自动匹配输入位</p>
              <p className="mt-2 text-xs leading-relaxed text-muted">
                会按照 manifest 的媒体类型和输入顺序放入最多 {artifacts.length} 个文件
              </p>
            </div>
          </div>
        )}
      </div>
    </Modal>
  )
}

function ParameterField({
  input,
  value,
  disabled,
  onChange,
}: {
  input: WorkflowInput
  value: string
  disabled: boolean
  onChange: (value: string) => void
}) {
  const hint = `${input.type}${input.default !== null && input.default !== undefined ? ` · 默认 ${JSON.stringify(input.default)}` : ''}`

  if (input.type === 'boolean') {
    return (
      <div className="space-y-1.5">
        <Checkbox
          label={input.name}
          checked={value === 'true'}
          disabled={disabled}
          onChange={(checked) => onChange(checked ? 'true' : 'false')}
        />
        <p className="text-xs text-subtle">{hint}</p>
      </div>
    )
  }

  if (input.options.length > 0) {
    return (
      <Field label={input.name} required={input.required} hint={hint}>
        {(id) => (
          <Select
            id={id}
            value={value}
            disabled={disabled}
            required={input.required}
            onChange={(event) => onChange(event.target.value)}
          >
            {!input.required && <option value="">使用默认值</option>}
            {input.options.map((option) => (
              <option key={option} value={option}>
                {option}
              </option>
            ))}
          </Select>
        )}
      </Field>
    )
  }

  // Prompts benefit from a textarea; anything else stays a single line.
  const multiline = input.type === 'string' && /prompt|text|description/i.test(input.name)

  return (
    <Field label={input.name} required={input.required} hint={hint}>
      {(id) =>
        multiline ? (
          <Textarea
            id={id}
            value={value}
            disabled={disabled}
            required={input.required}
            onChange={(event) => onChange(event.target.value)}
          />
        ) : (
          <Input
            id={id}
            value={value}
            disabled={disabled}
            required={input.required}
            inputMode={isNumeric(input.type) ? 'numeric' : undefined}
            onChange={(event) => onChange(event.target.value)}
          />
        )
      }
    </Field>
  )
}

function DropZoneField({
  input,
  index,
  file,
  existingArtifactId,
  disabled,
  onFileSelect,
  onClearExisting,
}: {
  input: WorkflowInput
  index: number
  file: File | null
  existingArtifactId?: string
  disabled: boolean
  onFileSelect: (file: File | null) => void
  onClearExisting: () => void
}) {
  const [isDragging, setIsDragging] = useState(false)
  const [previewUrl, setPreviewUrl] = useState<string | null>(null)
  const existing = useArtifactDownload(existingArtifactId ?? '', Boolean(existingArtifactId))

  useEffect(() => {
    if (!file) {
      setPreviewUrl(null)
      return
    }
    if (file.type.startsWith('image/') || file.type.startsWith('video/')) {
      const url = URL.createObjectURL(file)
      setPreviewUrl(url)
      return () => URL.revokeObjectURL(url)
    }
  }, [file])

  return (
    <div className="space-y-1.5">
      <div className="flex items-center justify-between text-xs">
        <span className="font-medium text-muted flex items-center gap-1.5">
          {input.name}
          {input.required && <span className="text-danger">*</span>}
        </span>
        <span className="text-[11px] font-mono text-subtle">
          输入位 #{index + 1} ({input.type})
        </span>
      </div>

      <div
        onDragOver={(e) => {
          e.preventDefault()
          setIsDragging(true)
        }}
        onDragLeave={() => setIsDragging(false)}
        onDrop={(e) => {
          e.preventDefault()
          e.stopPropagation()
          setIsDragging(false)
          const dropped = e.dataTransfer.files[0]
          if (dropped && acceptsFile(dropped, input.content_type)) onFileSelect(dropped)
        }}
        className={cx(
          'group relative flex flex-col items-center justify-center rounded-xl border-2 border-dashed p-4 text-center transition-all duration-200 cursor-pointer overflow-hidden',
          isDragging
            ? 'border-accent bg-accent/10 shadow-lg'
            : 'border-border/80 bg-surface-2/40 hover:border-accent/50 hover:bg-surface-2/70',
          disabled && 'opacity-60 cursor-not-allowed',
        )}
      >
        <input
          type="file"
          accept={input.content_type ?? undefined}
          disabled={disabled}
          className="absolute inset-0 z-10 size-full cursor-pointer opacity-0"
          onChange={(e) => onFileSelect(e.target.files?.[0] ?? null)}
        />

        {previewUrl && file?.type.startsWith('image/') ? (
          <div className="relative flex flex-col items-center gap-2">
            <img
              src={previewUrl}
              alt={`${file.name} 预览`}
              className="aspect-video w-full max-w-sm rounded-lg border border-border object-cover shadow-sm"
            />
            <p className="text-xs font-mono font-medium text-accent">
              {file.name} ({formatBytes(file.size)})
            </p>
            <span className="text-[10px] text-subtle">点击或拖拽选择替换</span>
          </div>
        ) : previewUrl && file?.type.startsWith('video/') ? (
          <div className="relative flex w-full flex-col items-center gap-2">
            <video
              src={previewUrl}
              muted
              controls
              playsInline
              preload="metadata"
              className="aspect-video w-full max-w-sm rounded-lg border border-border bg-black object-cover shadow-sm"
            />
            <p className="text-xs font-mono font-medium text-accent">
              {file.name} ({formatBytes(file.size)})
            </p>
          </div>
        ) : file ? (
          <div className="flex flex-col items-center gap-1 py-2">
            <span className="text-xs font-mono font-semibold text-accent">{file.name}</span>
            <span className="text-[11px] font-mono text-muted">{formatBytes(file.size)}</span>
            <span className="text-[10px] text-subtle mt-1">点击或拖放更新</span>
          </div>
        ) : existingArtifactId ? (
          <div className="flex w-full flex-col items-center gap-2 py-1">
            {existing.data?.download.url &&
              existing.data.artifact.content_type.startsWith('image/') && (
                <img
                  src={existing.data.download.url}
                  alt={existing.data.artifact.name}
                  className="aspect-video w-full max-w-sm rounded-lg border border-border object-cover shadow-sm"
                />
              )}
            {existing.data?.download.url &&
              existing.data.artifact.content_type.startsWith('video/') && (
                <video
                  src={existing.data.download.url}
                  muted
                  playsInline
                  preload="metadata"
                  className="aspect-video w-full max-w-sm rounded-lg border border-border bg-black object-cover shadow-sm"
                />
              )}
            <span className="rounded-full border border-accent/30 bg-accent/10 px-2.5 py-1 text-[10px] font-semibold text-accent">
              复用原作业输入
            </span>
            <p className="max-w-full truncate font-mono text-[11px] text-muted">
              {existing.data?.artifact.name || `${existingArtifactId.slice(0, 18)}…`}
            </p>
            <span className="text-[10px] text-subtle">拖入或点击可替换此输入</span>
          </div>
        ) : (
          <div className="flex flex-col items-center gap-1.5 py-3">
            <div className="grid size-10 place-items-center rounded-full border border-accent/30 bg-accent/10 text-accent group-hover:scale-110 transition">
              <svg className="size-5" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4M17 8l-5-5-5 5M12 3v12" />
              </svg>
            </div>
            <p className="text-xs font-medium text-text">
              拖拽文件到此处，或<span className="text-accent underline ml-1">点击浏览</span>
            </p>
            <p className="text-[10px] text-subtle">
              {input.content_type ? `支持格式: ${input.content_type}` : '拖放任意匹配文件'}
            </p>
          </div>
        )}
      </div>
      {existingArtifactId && !file && (
        <button
          type="button"
          disabled={disabled}
          onClick={onClearExisting}
          className="text-[10px] text-subtle transition hover:text-danger disabled:opacity-50"
        >
          不复用此输入
        </button>
      )}
    </div>
  )
}
