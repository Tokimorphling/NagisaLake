import { useDeferredValue, useEffect, useMemo, useState } from 'react'
import { Link, useLocation, useSearchParams } from 'react-router-dom'
import { useArtifactDownload, useDevices, useJob, useJobs, useWorkflows } from '@/api/queries'
import type { Workflow } from '@/api/types'
import {
  availabilityLabel,
  availabilityTone,
  capacitySummary,
  submitBlockedReason,
  workflowCapacity,
} from '@/lib/workflow-status'
import {
  collectTags,
  WORKFLOW_TAG_LABELS,
  WORKFLOW_TAG_TONES,
  workflowTags,
  type WorkflowTag,
} from '@/lib/workflow-tags'
import { useAuth } from '@/state/auth'
import { Page, PageHeader } from '@/components/layout/AppLayout'
import { IconPlay, IconWorkflow } from '@/components/layout/icons'
import { Badge, EmptyState, ErrorState, SkeletonRows } from '@/components/ui/display'
import { Button, Card, Checkbox, Input, Select, cx } from '@/components/ui/primitives'
import { detectMediaType } from '@/components/ui/ArtifactPreviewCard'
import { JobForm, type GalleryRemixSeed } from './JobForm'

interface GalleryRemixLocationState {
  galleryRemix?: GalleryRemixSeed
}

function galleryRemixSeed(value: unknown, expectedItemId: string | null): GalleryRemixSeed | null {
  if (!expectedItemId || !value || typeof value !== 'object') return null
  const candidate = (value as GalleryRemixLocationState).galleryRemix
  if (
    !candidate ||
    candidate.itemId !== expectedItemId ||
    typeof candidate.workflowVersion !== 'string' ||
    !candidate.parameters ||
    typeof candidate.parameters !== 'object' ||
    Array.isArray(candidate.parameters)
  ) {
    return null
  }
  return candidate
}

export function WorkflowsPage() {
  const { organizationId, atLeast } = useAuth()
  const location = useLocation()
  const [searchParams, setSearchParams] = useSearchParams()
  const remixJobId = searchParams.get('remix_job_id')
  const launchWorkflowId = searchParams.get('launch')
  const galleryRemix = galleryRemixSeed(location.state, searchParams.get('gallery_remix'))
  const remixJob = useJob(organizationId, remixJobId || '')

  const workflows = useWorkflows(organizationId)
  const devices = useDevices(organizationId, atLeast('member'))
  const jobs = useJobs(organizationId)
  const [query, setQuery] = useState('')
  const [onlyAvailable, setOnlyAvailable] = useState(false)
  const [activeTag, setActiveTag] = useState<'all' | WorkflowTag>('all')
  const [sort, setSort] = useState<'availability' | 'name'>('availability')
  const [selected, setSelected] = useState<Workflow | null>(null)
  // Debounce search input so typing doesn't refilter on every keystroke.
  const deferredQuery = useDeferredValue(query)

  // Deep links from Remix and Dashboard open the same catalog form used by a
  // manual card click. Closing clears the deep link so the effect cannot reopen
  // the modal immediately.
  useEffect(() => {
    if (!workflows.data || selected) return
    const targetId = remixJob.data?.workflow_id ?? launchWorkflowId
    if (!targetId) return
    const match = workflows.data.find((workflow) => workflow.id === targetId)
    if (match) setSelected(match)
  }, [launchWorkflowId, remixJob.data, selected, workflows.data])

  const availableTags = useMemo(() => collectTags(workflows.data ?? []), [workflows.data])

  useEffect(() => {
    if (activeTag !== 'all' && !availableTags.includes(activeTag)) setActiveTag('all')
  }, [activeTag, availableTags])

  const previewArtifactIds = useMemo(() => {
    const result = new Map<string, string>()
    const recentJobs = jobs.data?.pages.flatMap((page) => page.items) ?? []
    for (const job of recentJobs) {
      if (job.state !== 'completed' || job.output_artifact_ids.length === 0) continue
      if (!result.has(job.workflow_id)) result.set(job.workflow_id, job.output_artifact_ids[0])
    }
    return result
  }, [jobs.data])

  const filtered = useMemo(() => {
    const needle = deferredQuery.trim().toLowerCase()
    const matched = (workflows.data ?? [])
      .filter((workflow) =>
        onlyAvailable ? workflowCapacity(workflow).availability !== 'offline' : true,
      )
      .filter((workflow) => activeTag === 'all' || workflowTags(workflow).includes(activeTag))
      .filter((workflow) => {
        if (!needle) return true
        return (
          workflow.id.toLowerCase().includes(needle) ||
          workflow.manifest?.display_name?.toLowerCase().includes(needle) ||
          workflow.manifest?.description?.toLowerCase().includes(needle)
        )
      })
    if (sort === 'name') {
      return [...matched].sort((left, right) => {
        const leftName = left.manifest?.display_name ?? left.id
        const rightName = right.manifest?.display_name ?? right.id
        return leftName.localeCompare(rightName)
      })
    }
    return matched.sort((left, right) => {
      const rank = { available: 0, queueing: 1, busy: 2, offline: 3 } as const
      const byAvailability =
        rank[workflowCapacity(left).availability] - rank[workflowCapacity(right).availability]
      return (
        byAvailability ||
        left.id.localeCompare(right.id) ||
        right.version.localeCompare(left.version)
      )
    })
  }, [activeTag, deferredQuery, onlyAvailable, sort, workflows.data])

  const canSubmit = atLeast('member')

  return (
    <Page>
      <PageHeader
        title="Workflow 目录"
        description="只显示已审核且当前账户可访问的 workflow。表单由 Worker 上报的公开 manifest 自动生成。"
        actions={
          <Button size="sm" onClick={() => void workflows.refetch()} loading={workflows.isFetching}>
            刷新
          </Button>
        }
      />

      <div className="mb-4 flex flex-wrap items-center gap-3">
        <Input
          value={query}
          placeholder="搜索 workflow 名称或描述"
          className="max-w-xs"
          onChange={(event) => setQuery(event.target.value)}
        />
        <Checkbox
          label="仅显示可用"
          checked={onlyAvailable}
          onChange={setOnlyAvailable}
        />
        <Select
          value={sort}
          className="h-8 w-auto text-xs"
          onChange={(event) => setSort(event.target.value as 'availability' | 'name')}
          aria-label="排序方式"
        >
          <option value="availability">按可用性</option>
          <option value="name">按名称</option>
        </Select>
        {workflows.data && (
          <span className="text-xs font-mono text-subtle">
            {filtered.length} / {workflows.data.length}
          </span>
        )}
      </div>

      {availableTags.length > 0 && (
        <div className="mb-5 flex flex-wrap items-center gap-2" aria-label="Workflow 分类筛选">
          <button
            type="button"
            onClick={() => setActiveTag('all')}
            className={cx(
              'rounded-full border px-3 py-1.5 text-xs transition',
              activeTag === 'all'
                ? 'border-accent/40 bg-accent/15 font-semibold text-accent'
                : 'border-border bg-surface text-muted hover:border-border-strong hover:text-text',
            )}
          >
            全部
          </button>
          {availableTags.map((tag) => (
            <button
              key={tag}
              type="button"
              onClick={() => setActiveTag(tag)}
              className={cx(
                'rounded-full border px-3 py-1.5 text-xs transition',
                activeTag === tag
                  ? 'border-accent/40 bg-accent/15 font-semibold text-accent'
                  : 'border-border bg-surface text-muted hover:border-border-strong hover:text-text',
              )}
            >
              {WORKFLOW_TAG_LABELS[tag]}
            </button>
          ))}
        </div>
      )}

      {remixJobId && remixJob.isError && (
        <div className="mb-4 rounded-xl border border-danger/30 bg-danger/10 px-4 py-3 text-xs text-danger">
          无法读取要 Remix 的作业：{(remixJob.error as Error).message}
        </div>
      )}

      {searchParams.has('gallery_remix') && !galleryRemix && (
        <div className="mb-4 rounded-xl border border-warning/30 bg-warning/10 px-4 py-3 text-xs text-warning">
          Gallery Remix 参数已失效，请返回 Gallery 重新选择。
        </div>
      )}

      {workflows.isLoading ? (
        <Card>
          <SkeletonRows rows={5} />
        </Card>
      ) : workflows.isError ? (
        <Card>
          <ErrorState
            message={(workflows.error as Error).message}
            onRetry={() => void workflows.refetch()}
          />
        </Card>
      ) : filtered.length === 0 ? (
        <Card>
          <EmptyState
            title={workflows.data?.length ? '没有匹配的 workflow' : '目录为空'}
            description={
              workflows.data?.length
                ? '调整搜索条件或取消“仅显示可用”。'
                : '注册一台 ComfyUI 设备并让 Worker 上报 manifest 后，workflow 会出现在这里。'
            }
          />
        </Card>
      ) : (
        <div className="grid gap-4 sm:grid-cols-2 md:grid-cols-2 xl:grid-cols-3">
          {filtered.map((workflow) => (
            <WorkflowCard
              key={`${workflow.id}@${workflow.version}`}
              workflow={workflow}
              canSubmit={canSubmit}
              previewArtifactId={previewArtifactIds.get(workflow.id)}
              onSubmit={() => setSelected(workflow)}
            />
          ))}
        </div>
      )}

      {workflows.hasNextPage && (
        <div className="mt-5 flex justify-center border-t border-border px-4 py-3">
          <Button
            size="sm"
            loading={workflows.isFetchingNextPage}
            onClick={() => void workflows.fetchNextPage()}
          >
            加载更多
          </Button>
        </div>
      )}

      {selected && (
        <JobForm
          workflow={selected}
          devices={devices.data ?? []}
          remixJob={remixJob.data}
          galleryRemix={galleryRemix}
          open
          onClose={() => {
            setSelected(null)
            const next = new URLSearchParams(searchParams)
            next.delete('remix_job_id')
            next.delete('gallery_remix')
            next.delete('launch')
            setSearchParams(next, { replace: true })
          }}
        />
      )}
    </Page>
  )
}

function WorkflowCard({
  workflow,
  canSubmit,
  previewArtifactId,
  onSubmit,
}: {
  workflow: Workflow
  canSubmit: boolean
  previewArtifactId?: string
  onSubmit: () => void
}) {
  const [hovered, setHovered] = useState(false)
  const preview = useArtifactDownload(previewArtifactId ?? '', hovered && Boolean(previewArtifactId))
  const manifest = workflow.manifest
  const parameters = manifest?.inputs.filter((input) => input.kind === 'parameter').length ?? 0
  const artifacts = manifest?.inputs.filter((input) => input.kind === 'artifact').length ?? 0
  const capacity = workflowCapacity(workflow)
  const blockedReason = submitBlockedReason(workflow, capacity)
  const tags = workflowTags(workflow)
  const previewType = detectMediaType(
    preview.data?.artifact.content_type,
    preview.data?.artifact.name,
  )
  const previewHint = preview.isFetching
    ? '正在加载最近输出…'
    : hovered && preview.data && previewType !== 'image' && previewType !== 'video'
      ? '最近输出不可视化'
      : previewArtifactId
        ? '悬停预览最近输出'
        : '暂无历史预览'

  return (
    <div
      className="h-full"
      onMouseEnter={() => setHovered(true)}
      onMouseLeave={() => setHovered(false)}
    >
      <Card
        className={cx(
          'flex h-full flex-col overflow-hidden transition hover:-translate-y-0.5 hover:border-accent/40 hover:shadow-lg',
          capacity.availability === 'offline' && 'opacity-80',
        )}
      >
        <Link
          to={`/workflows/${encodeURIComponent(workflow.id)}`}
          className="relative block aspect-[16/7] overflow-hidden border-b border-border/60 bg-surface-2"
          aria-label={`查看 ${manifest?.display_name || workflow.id} 详情`}
        >
          {hovered && preview.data?.download.url && previewType === 'image' ? (
            <img
              src={preview.data.download.url}
              alt={`${manifest?.display_name || workflow.id} 最近输出`}
              className="size-full object-cover animate-fade-in-up"
            />
          ) : hovered && preview.data?.download.url && previewType === 'video' ? (
            <video
              src={preview.data.download.url}
              autoPlay
              muted
              loop
              playsInline
              preload="metadata"
              className="size-full object-cover animate-fade-in-up"
            />
          ) : (
            <div className="absolute inset-0 grid place-items-center bg-[radial-gradient(circle_at_25%_20%,color-mix(in_oklab,var(--app-accent)_22%,transparent),transparent_55%),radial-gradient(circle_at_85%_70%,color-mix(in_oklab,var(--app-violet)_22%,transparent),transparent_55%)]">
              <div className="grid size-12 place-items-center rounded-2xl border border-white/10 bg-surface/70 text-accent shadow-lg backdrop-blur">
                <IconWorkflow className="size-6" />
              </div>
            </div>
          )}
          <span className="absolute bottom-2.5 left-3 rounded-full border border-white/10 bg-black/55 px-2.5 py-1 text-[10px] font-medium text-white backdrop-blur">
            {previewHint}
          </span>
        </Link>

        <div className="flex-1 space-y-3 p-5">
        <div className="flex items-start justify-between gap-2">
          <div className="min-w-0">
            <Link
              to={`/workflows/${encodeURIComponent(workflow.id)}`}
              className="block truncate text-sm font-semibold tracking-tight hover:text-accent"
            >
              {manifest?.display_name || workflow.id}
            </Link>
            <p className="mt-0.5 truncate font-mono text-[10px] text-subtle">
              {workflow.id} · {workflow.version}
            </p>
          </div>
          <Badge tone={availabilityTone(capacity.availability)}>
            <span
              className={cx(
                'size-1.5 rounded-full bg-current',
                capacity.availability !== 'offline' && 'animate-pulse',
              )}
              aria-hidden="true"
            />
            {availabilityLabel(capacity.availability)}
          </Badge>
        </div>

        {manifest?.description && (
          <p className="line-clamp-2 text-xs leading-relaxed text-muted">{manifest.description}</p>
        )}

        <div className="flex flex-wrap gap-1.5">
          {tags.map((tag) => (
            <Badge key={tag} tone={WORKFLOW_TAG_TONES[tag]}>
              {WORKFLOW_TAG_LABELS[tag]}
            </Badge>
          ))}
          {artifacts > 0 && <Badge tone="violet">{artifacts} 个输入文件</Badge>}
          {parameters > 0 && <Badge tone="info">{parameters} 个参数</Badge>}
          {workflow.output_types.map((type) => (
            <Badge key={type}>{type}</Badge>
          ))}
        </div>

        {!workflow.manifest_consistent && (
          <p className="rounded-md border border-danger/30 bg-danger/10 px-2.5 py-1.5 text-[11px] leading-relaxed text-danger">
            多个 Worker 的 manifest 不一致，暂不可提交。
          </p>
        )}
        {capacity.availability === 'busy' && workflow.manifest_consistent && (
          <p className="rounded-md border border-warning/30 bg-warning/10 px-2.5 py-1.5 text-[11px] leading-relaxed text-warning">
            设备的执行槽与队列均已满，现在无法提交。等正在执行或排队的作业结束后再试。
          </p>
        )}
        {capacity.availability === 'offline' && workflow.workers.length === 0 && (
          <p className="text-[11px] leading-relaxed text-subtle">
            manifest 可浏览，但没有在线设备，无法提交。
          </p>
        )}
        {manifest && manifest.warnings.length > 0 && workflow.manifest_consistent && (
          <p className="text-[11px] leading-relaxed text-warning">
            {manifest.warnings.length} 条 manifest 警告
          </p>
        )}
        </div>

        <div className="flex items-center justify-between gap-2 border-t border-border px-5 py-3">
          <span className="text-[11px] text-subtle">{capacitySummary(capacity)}</span>
          <Button
            size="sm"
            variant={blockedReason ? 'secondary' : 'primary'}
            disabled={blockedReason !== null || !canSubmit}
            title={!canSubmit ? '需要 member 或更高角色' : (blockedReason ?? undefined)}
            onClick={onSubmit}
          >
            <IconPlay className="size-3.5" />
            快速启动
          </Button>
        </div>
      </Card>
    </div>
  )
}
