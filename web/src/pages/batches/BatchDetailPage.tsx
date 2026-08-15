import { useMemo, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { useBatch, useBatchJobs, useCancelBatch, isBatchTerminal } from '@/api/queries'
import type { JobState } from '@/api/types'
import { TERMINAL_JOB_STATES } from '@/api/types'
import { formatDateTime, formatRelative } from '@/lib/format'
import { useAuth } from '@/state/auth'
import { useToast } from '@/state/toast'
import { Page, PageHeader } from '@/components/layout/AppLayout'
import { ConfirmModal } from '@/components/ui/Modal'
import {
  Copyable,
  EmptyState,
  ErrorState,
  JobStateBadge,
  SkeletonRows,
  Table,
  Td,
  Th,
} from '@/components/ui/display'
import { Button, Card, CardHeader, cx } from '@/components/ui/primitives'

const JOB_FILTERS: Array<{ value: 'all' | 'active' | JobState; label: string }> = [
  { value: 'all', label: '全部' },
  { value: 'active', label: '进行中' },
  { value: 'completed', label: '已完成' },
  { value: 'failed', label: '失败' },
  { value: 'cancelled', label: '已取消' },
]

export function BatchDetailPage() {
  const { batchId = '' } = useParams()
  const navigate = useNavigate()
  const { organizationId } = useAuth()
  const batch = useBatch(organizationId, batchId)
  const batchJobs = useBatchJobs(organizationId, batchId)
  const cancel = useCancelBatch(organizationId)
  const toast = useToast()
  const [confirming, setConfirming] = useState(false)
  const [filter, setFilter] = useState<'all' | 'active' | JobState>('all')

  const jobs = useMemo(
    () => batchJobs.data?.pages.flatMap((page) => page.items) ?? [],
    [batchJobs.data],
  )

  const filtered = useMemo(
    () =>
      jobs.filter((job) => {
        if (filter === 'all') return true
        if (filter === 'active') return !TERMINAL_JOB_STATES.includes(job.state)
        return job.state === filter
      }),
    [filter, jobs],
  )

  if (batch.isLoading) {
    return (
      <Page>
        <Card>
          <SkeletonRows rows={6} />
        </Card>
      </Page>
    )
  }

  if (batch.isError || !batch.data) {
    return (
      <Page>
        <Card>
          <ErrorState
            message={(batch.error as Error)?.message ?? '批次不存在或不属于当前组织'}
            onRetry={() => void batch.refetch()}
          />
        </Card>
      </Page>
    )
  }

  const data = batch.data
  const terminal = isBatchTerminal(data)
  const settled = data.counts.completed + data.counts.failed + data.counts.cancelled
  const progressRatio = data.total > 0 ? Math.round((settled / data.total) * 100) : 0

  const confirmCancel = async () => {
    try {
      await cancel.mutateAsync(data.id)
      toast.success('已请求取消批次未完成项')
    } catch (error) {
      toast.fromError(error, '取消批次失败')
    } finally {
      setConfirming(false)
    }
  }

  return (
    <Page>
      <PageHeader
        title={`批次 ${data.id.slice(0, 12)}…`}
        description={
          <span className="flex flex-col items-start gap-1 sm:flex-row sm:items-center sm:gap-2">
            <span className="font-medium text-text">
              {data.workflow_id} · <span className="font-mono text-subtle">{data.workflow_version}</span>
            </span>
            <Copyable value={data.id} display={data.id} className="max-w-full text-accent" />
          </span>
        }
        actions={
          <div className="flex items-center gap-2">
            <Link to="/batches">
              <Button size="sm" variant="ghost">
                ← 返回列表
              </Button>
            </Link>
            <Button
              size="sm"
              variant="danger"
              disabled={terminal}
              onClick={() => setConfirming(true)}
            >
              取消未完成项
            </Button>
          </div>
        }
      />

      <div className="grid items-start gap-6 lg:grid-cols-[1fr_18rem]">
        <div className="min-w-0 space-y-6">
          {/* Status & Progress */}
          <Card className="overflow-hidden">
            <CardHeader
              title="批次状态"
              actions={
                <span
                  className={cx(
                    'inline-flex items-center gap-1 rounded-md border px-2 py-0.5 text-xs font-medium',
                    terminal
                      ? 'border-success/30 bg-success/10 text-success'
                      : 'border-accent/30 bg-accent/10 text-accent',
                  )}
                >
                  <span
                    className={cx(
                      'size-1.5 rounded-full',
                      terminal ? 'bg-success' : 'bg-accent animate-pulse',
                    )}
                    aria-hidden="true"
                  />
                  {data.status}
                </span>
              }
            />
            <div className="space-y-5 p-6">
              <div className="space-y-2">
                <div className="flex justify-between text-xs">
                  <span className="font-medium text-muted">完成进度</span>
                  <span className="font-mono font-semibold tabular-nums text-accent">
                    {settled} / {data.total} ({progressRatio}%)
                  </span>
                </div>
                <div className="h-2.5 overflow-hidden rounded-full bg-surface-2 ring-1 ring-border/50">
                  <div
                    className="h-full rounded-full bg-gradient-to-r from-accent via-cyan-400 to-violet transition-[width] duration-500"
                    style={{ width: `${progressRatio}%` }}
                  />
                </div>
              </div>

              <dl className="grid gap-4 text-xs sm:grid-cols-2 border-t border-border/60 pt-4">
                <Detail label="作业总数">
                  <span className="font-mono">{data.total}</span>
                </Detail>
                <Detail label="创建时间">
                  {formatDateTime(data.created_at)}
                  <span className="ml-1.5 text-subtle">({formatRelative(data.created_at)})</span>
                </Detail>
              </dl>
            </div>
          </Card>

          {/* Jobs Table */}
          <Card>
            <CardHeader
              title="批次作业"
              description="按状态筛选、服务器分页返回的子任务列表。"
              actions={
                <span className="text-xs text-subtle">
                  {filtered.length} / {jobs.length}
                  {batchJobs.hasNextPage && ' 已加载'}
                </span>
              }
            />

            <div className="mb-4 flex flex-wrap gap-1 border-b border-border/60 p-3">
              {JOB_FILTERS.map((option) => (
                <button
                  key={option.value}
                  type="button"
                  onClick={() => setFilter(option.value)}
                  className={cx(
                    'rounded-md px-2.5 py-1 text-xs transition',
                    filter === option.value
                      ? 'bg-accent text-accent-fg font-medium'
                      : 'text-muted hover:text-text',
                  )}
                >
                  {option.label}
                </button>
              ))}
            </div>

            {batchJobs.isLoading ? (
              <SkeletonRows rows={6} />
            ) : batchJobs.isError ? (
              <ErrorState
                message={(batchJobs.error as Error).message}
                onRetry={() => void batchJobs.refetch()}
              />
            ) : filtered.length === 0 ? (
              <EmptyState
                title={jobs.length === 0 ? '批次内暂无作业' : '没有匹配的作业'}
                description={
                  jobs.length === 0
                    ? '批次正在调度中，作业会陆续出现。'
                    : '换一个状态过滤条件。'
                }
              />
            ) : (
              <Table>
                <thead>
                  <tr>
                    <Th>作业 ID</Th>
                    <Th>状态</Th>
                    <Th className="hidden sm:table-cell">进度</Th>
                    <Th className="hidden sm:table-cell">设备</Th>
                    <Th>更新</Th>
                  </tr>
                </thead>
                <tbody>
                  {filtered.map((job) => (
                    <tr
                      key={job.id}
                      className="cursor-pointer transition hover:bg-surface-2/50"
                      onClick={() => navigate(`/jobs/${job.id}`)}
                    >
                      <Td>
                        <Link
                          to={`/jobs/${job.id}`}
                          className="font-mono text-xs text-accent hover:underline"
                          onClick={(event) => event.stopPropagation()}
                        >
                          {job.id.slice(0, 12)}…
                        </Link>
                      </Td>
                      <Td>
                        <JobStateBadge state={job.state} />
                      </Td>
                      <Td className="hidden w-28 sm:table-cell">
                        {job.progress === null ? (
                          <span className="text-xs text-subtle">—</span>
                        ) : (
                          <div className="flex items-center gap-2">
                            <div className="h-1 w-14 overflow-hidden rounded-full bg-surface-2">
                              <div
                                className="h-full rounded-full bg-accent transition-[width]"
                                style={{ width: `${Math.round(job.progress * 100)}%` }}
                              />
                            </div>
                            <span className="font-mono text-[10px] tabular-nums text-muted">
                              {Math.round(job.progress * 100)}%
                            </span>
                          </div>
                        )}
                      </Td>
                      <Td className="hidden sm:table-cell">
                        <Copyable
                          value={job.worker_id}
                          display={job.worker_id.slice(0, 10) + (job.worker_id.length > 10 ? '…' : '')}
                          className="text-muted"
                        />
                      </Td>
                      <Td className="whitespace-nowrap text-xs text-muted">
                        {formatRelative(job.updated_at_unix_ms)}
                      </Td>
                    </tr>
                  ))}
                </tbody>
              </Table>
            )}
            {batchJobs.hasNextPage && (
              <div className="flex justify-center border-t border-border px-4 py-3">
                <Button
                  size="sm"
                  loading={batchJobs.isFetchingNextPage}
                  onClick={() => void batchJobs.fetchNextPage()}
                >
                  加载更多
                </Button>
              </div>
            )}
          </Card>
        </div>

        {/* Counts Sidebar */}
        <Card className="lg:sticky lg:top-6 lg:self-start overflow-hidden">
          <CardHeader title="状态分布" description={`总计 ${data.total} 个作业`} />
          <div className="space-y-3 p-5">
            <CountRow label="排队 (queued)" value={data.counts.queued} tone="bg-subtle" />
            <CountRow label="已接收 (received)" value={data.counts.received} tone="bg-info" />
            <CountRow label="已接受 (accepted)" value={data.counts.accepted} tone="bg-info" />
            <CountRow label="执行中 (running)" value={data.counts.running} tone="bg-accent" />
            <CountRow label="上传中 (uploading)" value={data.counts.uploading} tone="bg-violet" />
            <CountRow label="已完成 (completed)" value={data.counts.completed} tone="bg-success" />
            <CountRow label="失败 (failed)" value={data.counts.failed} tone="bg-danger" />
            <CountRow label="已取消 (cancelled)" value={data.counts.cancelled} tone="bg-subtle" />
          </div>
        </Card>
      </div>

      <ConfirmModal
        open={confirming}
        title="取消批次未完成项"
        destructive
        confirmLabel="确认取消"
        loading={cancel.isPending}
        description="Hub 会向执行设备发送取消指令，仅取消批次中尚未完成的作业。已完成或已失败的作业不受影响。"
        onConfirm={confirmCancel}
        onClose={() => setConfirming(false)}
      />
    </Page>
  )
}

function Detail({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="space-y-1">
      <dt className="text-muted">{label}</dt>
      <dd className="min-w-0">{children}</dd>
    </div>
  )
}

function CountRow({
  label,
  value,
  tone,
}: {
  label: string
  value: number
  tone: string
}) {
  return (
    <div className="flex items-center justify-between gap-3">
      <span className="flex items-center gap-2 text-xs text-muted">
        <span className={cx('size-2 rounded-full', tone)} aria-hidden="true" />
        {label}
      </span>
      <span className="font-mono text-xs tabular-nums text-text">{value}</span>
    </div>
  )
}
