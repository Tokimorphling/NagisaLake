import { useMemo, useState } from 'react'
import { Link, useParams } from 'react-router-dom'
import { useDevices, useJobs, useWorkflows } from '@/api/queries'
import { TERMINAL_JOB_STATES } from '@/api/types'
import { formatRelative } from '@/lib/format'
import {
  availabilityLabel,
  availabilityTone,
  submitBlockedReason,
  workflowCapacity,
} from '@/lib/workflow-status'
import { WORKFLOW_TAG_LABELS, WORKFLOW_TAG_TONES, workflowTags } from '@/lib/workflow-tags'
import { useAuth } from '@/state/auth'
import { Page, PageHeader } from '@/components/layout/AppLayout'
import { IconPlay, IconWorkflow } from '@/components/layout/icons'
import { Badge, Copyable, EmptyState, ErrorState, JobStateBadge, SkeletonRows } from '@/components/ui/display'
import { LoadBar } from '@/components/ui/charts'
import { WorkflowGraph } from '@/components/ui/WorkflowGraph'
import { Button, Card, CardHeader } from '@/components/ui/primitives'
import { JobForm } from './JobForm'

export function WorkflowDetailPage() {
  const { workflowId = '' } = useParams()
  const { organizationId, atLeast } = useAuth()
  const workflows = useWorkflows(organizationId)
  const devices = useDevices(organizationId, atLeast('member'))
  const jobs = useJobs(organizationId)
  const [formOpen, setFormOpen] = useState(false)

  const workflow = useMemo(
    () => workflows.data?.find((candidate) => candidate.id === workflowId) ?? null,
    [workflowId, workflows.data],
  )

  // Only the pages already loaded; the list is newest-first so this is the
  // recent history rather than the complete one.
  const relatedJobs = useMemo(() => {
    const items = jobs.data?.pages.flatMap((page) => page.items) ?? []
    return items.filter((job) => job.workflow_id === workflowId).slice(0, 8)
  }, [jobs.data, workflowId])

  if (workflows.isLoading) {
    return (
      <Page>
        <Card>
          <SkeletonRows rows={6} />
        </Card>
      </Page>
    )
  }

  if (workflows.isError) {
    return (
      <Page>
        <Card>
          <ErrorState
            message={(workflows.error as Error).message}
            onRetry={() => void workflows.refetch()}
          />
        </Card>
      </Page>
    )
  }

  if (!workflow) {
    return (
      <Page>
        <Card>
          <EmptyState
            icon={<IconWorkflow className="size-8" />}
            title="找不到该 workflow"
            description={`目录中没有 ${workflowId}。它可能未通过审核、已下线，或当前组织无权访问。`}
            action={
              <Link to="/workflows">
                <Button size="sm" variant="primary">
                  返回目录
                </Button>
              </Link>
            }
          />
        </Card>
      </Page>
    )
  }

  const manifest = workflow.manifest
  const capacity = workflowCapacity(workflow)
  const blockedReason = submitBlockedReason(workflow, capacity)
  const canSubmit = atLeast('member')
  const tags = workflowTags(workflow)
  const availableCount = workflow.workers.filter((w) => w.available).length

  return (
    <Page>
      <PageHeader
        title={manifest?.display_name || workflow.id}
        description={
          <span className="flex flex-col items-start gap-1.5">
            <span className="flex flex-wrap items-center gap-1.5">
              <Copyable value={workflow.id} className="text-accent" />
              <span className="font-mono text-subtle">· {workflow.version}</span>
            </span>
            {manifest?.description && <span>{manifest.description}</span>}
          </span>
        }
        actions={
          <div className="flex items-center gap-2">
            <Link to="/workflows">
              <Button size="sm" variant="ghost">
                ← 返回目录
              </Button>
            </Link>
            <Button
              size="sm"
              variant={blockedReason ? 'secondary' : 'primary'}
              disabled={blockedReason !== null || !canSubmit}
              title={!canSubmit ? '需要 member 或更高角色' : (blockedReason ?? undefined)}
              onClick={() => setFormOpen(true)}
            >
              <IconPlay className="size-3.5" />
              提交作业
            </Button>
          </div>
        }
      />

      <div className="mb-4 flex flex-wrap items-center gap-1.5">
        <Badge tone={availabilityTone(capacity.availability)}>
          <span
            className="size-1.5 rounded-full bg-current"
            aria-hidden="true"
          />
          {availabilityLabel(capacity.availability)}
        </Badge>
        {tags.map((tag) => (
          <Badge key={tag} tone={WORKFLOW_TAG_TONES[tag]}>
            {WORKFLOW_TAG_LABELS[tag]}
          </Badge>
        ))}
        {workflow.output_types.map((type) => (
          <Badge key={type}>{type}</Badge>
        ))}
      </div>

      <div className="grid items-start gap-6 lg:grid-cols-[1fr_20rem]">
        <div className="min-w-0 space-y-6">
          <Card>
            <CardHeader
              title="链路概览"
              description="按 manifest 声明推导的输入 → 参数 → 输出边界。"
            />
            <WorkflowGraph workflow={workflow} />
          </Card>

          {manifest && manifest.warnings.length > 0 && (
            <Card>
              <CardHeader
                title="Manifest 警告"
                description={`Worker 上报了 ${manifest.warnings.length} 条警告`}
              />
              <ul className="space-y-2 p-5">
                {manifest.warnings.map((warning, index) => (
                  <li
                    key={index}
                    className="rounded-lg border border-warning/30 bg-warning/10 px-3 py-2 text-[11px] leading-relaxed text-warning"
                  >
                    {warning}
                  </li>
                ))}
              </ul>
            </Card>
          )}

          <Card>
            <CardHeader
              title="最近作业"
              description="仅统计已加载的作业分页，不是完整历史。"
            />
            {relatedJobs.length === 0 ? (
              <EmptyState
                title="该 workflow 还没有作业记录"
                description="提交一个作业后，运行历史会出现在这里。"
              />
            ) : (
              <ul className="divide-y divide-border/60">
                {relatedJobs.map((job) => (
                  <li key={job.id}>
                    <Link
                      to={`/jobs/${job.id}`}
                      className="flex flex-wrap items-center justify-between gap-2 px-5 py-3 transition hover:bg-surface-2/50"
                    >
                      <span className="flex min-w-0 items-center gap-2.5">
                        <span className="font-mono text-xs text-accent">
                          {job.id.slice(0, 12)}…
                        </span>
                        <JobStateBadge state={job.state} />
                      </span>
                      <span className="flex shrink-0 items-center gap-3 text-[11px] text-muted">
                        {job.progress !== null && !TERMINAL_JOB_STATES.includes(job.state) && (
                          <span className="font-mono tabular-nums text-accent">
                            {Math.round(job.progress * 100)}%
                          </span>
                        )}
                        {formatRelative(job.created_at_unix_ms)}
                      </span>
                    </Link>
                  </li>
                ))}
              </ul>
            )}
          </Card>
        </div>

        <div className="space-y-4">
          <Card>
            <CardHeader title="执行容量" description="来自 Worker 上报的实时负载" />
            <div className="space-y-4 p-5">
              {capacity.devices === 0 ? (
                <p className="text-xs leading-relaxed text-subtle">
                  没有在线设备提供该 workflow。manifest 仍可浏览，但无法提交作业。
                </p>
              ) : (
                <>
                  <LoadBar
                    label="执行槽占用"
                    used={capacity.active}
                    total={capacity.parallelism}
                    hint={`${capacity.devices} 台设备在线`}
                  />
                  <LoadBar
                    label="Worker 队列"
                    used={capacity.queued}
                    total={capacity.queueDepth}
                    tone="violet"
                    hint={capacity.queueDepth === 0 ? '该设备未启用队列' : undefined}
                  />
                </>
              )}
            </div>
          </Card>

          <Card>
            <CardHeader title="执行设备" description={`${workflow.workers.length} 台 Worker（${availableCount} 可接单）`} />
            {workflow.workers.length === 0 ? (
              <EmptyState title="无在线设备" />
            ) : (
              <ul className="divide-y divide-border/60">
                {workflow.workers.map((worker) => {
                  const device = devices.data?.find(
                    (candidate) =>
                      candidate.device_organization_id === worker.organization_id &&
                      candidate.device_id === worker.worker_id,
                  )
                  return (
                    <li key={`${worker.organization_id}/${worker.worker_id}`} className="px-5 py-3">
                      <div className="flex items-center justify-between gap-2">
                        <span className="min-w-0 truncate text-xs font-medium">
                          {device?.node_name || worker.worker_id}
                        </span>
                        <Badge tone={worker.available ? 'success' : 'warning'}>
                          {worker.available ? '可接单' : '已满'}
                        </Badge>
                      </div>
                      <p className="mt-1 font-mono text-[10px] text-subtle">
                        执行 {worker.active_jobs}/{worker.parallelism}
                        {worker.queue_depth > 0 &&
                          ` · 排队 ${worker.queued_jobs}/${worker.queue_depth}`}
                      </p>
                    </li>
                  )
                })}
              </ul>
            )}
          </Card>

          {!workflow.manifest_consistent && (
            <Card className="border-danger/40">
              <div className="p-5">
                <p className="text-xs font-semibold text-danger">Manifest 不一致</p>
                <p className="mt-1.5 text-[11px] leading-relaxed text-muted">
                  多个 Worker 上报了不同的 manifest，Hub 拒绝提交以避免参数与实际图不符。
                </p>
              </div>
            </Card>
          )}
        </div>
      </div>

      {formOpen && (
        <JobForm
          workflow={workflow}
          devices={devices.data ?? []}
          open
          onClose={() => setFormOpen(false)}
        />
      )}
    </Page>
  )
}
