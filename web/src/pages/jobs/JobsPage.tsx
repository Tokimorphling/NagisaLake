import { useMemo, useState } from 'react'
import { Link, useNavigate } from 'react-router-dom'
import { useCancelJob, useJobs } from '@/api/queries'
import type { JobState } from '@/api/types'
import { TERMINAL_JOB_STATES } from '@/api/types'
import { formatRelative } from '@/lib/format'
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
import { Button, Card, Select, cx } from '@/components/ui/primitives'

const FILTERS: Array<{ value: 'all' | 'active' | JobState; label: string }> = [
  { value: 'all', label: '全部' },
  { value: 'active', label: '进行中' },
  { value: 'completed', label: '已完成' },
  { value: 'failed', label: '失败' },
  { value: 'cancelled', label: '已取消' },
]

export function JobsPage() {
  const { organizationId } = useAuth()
  const jobs = useJobs(organizationId)
  const cancel = useCancelJob(organizationId)
  const toast = useToast()
  const navigate = useNavigate()
  const [filter, setFilter] = useState<'all' | 'active' | JobState>('all')
  const [workflowFilter, setWorkflowFilter] = useState('')
  const [pendingCancel, setPendingCancel] = useState<string | null>(null)

  const items = useMemo(
    () => jobs.data?.pages.flatMap((page) => page.items) ?? [],
    [jobs.data],
  )

  const workflowOptions = useMemo(
    () => [...new Set(items.map((job) => job.workflow_id))].sort(),
    [items],
  )

  // Filtering happens over the pages loaded so far, not the whole history, so a
  // filter can look empty until more pages are pulled in.
  const filtered = useMemo(
    () =>
      items
        .filter((job) => {
          if (filter === 'all') return true
          if (filter === 'active') return !TERMINAL_JOB_STATES.includes(job.state)
          return job.state === filter
        })
        .filter((job) => (workflowFilter ? job.workflow_id === workflowFilter : true)),
    [filter, items, workflowFilter],
  )

  const confirmCancel = async () => {
    if (!pendingCancel) return
    try {
      await cancel.mutateAsync(pendingCancel)
      toast.success('已请求取消作业')
    } catch (error) {
      toast.fromError(error, '取消作业失败')
    } finally {
      setPendingCancel(null)
    }
  }

  return (
    <Page>
      <PageHeader
        title="作业"
        description="按创建时间倒序分页返回，滚到底可加载更早的作业。进行中的作业会自动轮询；筛选只作用于已加载的页。"
        actions={
          <>
            {jobs.isFetching ? (
              <span className="inline-flex items-center gap-1.5 rounded-full bg-accent/10 px-2.5 py-1 text-[11px] font-medium text-accent">
                <span className="size-1.5 animate-pulse rounded-full bg-accent" aria-hidden="true" />
                自动刷新中
              </span>
            ) : null}
            <Button size="sm" onClick={() => void jobs.refetch()} loading={jobs.isFetching}>
              刷新
            </Button>
          </>
        }
      />

      <div className="mb-4 flex flex-wrap items-center gap-3">
        <div className="flex flex-wrap gap-1 rounded-lg border border-border bg-surface-2 p-1">
          {FILTERS.map((option) => (
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
        {workflowOptions.length > 1 && (
          <Select
            value={workflowFilter}
            className="max-w-48"
            onChange={(event) => setWorkflowFilter(event.target.value)}
          >
            <option value="">所有 workflow</option>
            {workflowOptions.map((id) => (
              <option key={id} value={id}>
                {id}
              </option>
            ))}
          </Select>
        )}
        <span className="text-xs text-subtle">
          {filtered.length} / {items.length}
          {jobs.hasNextPage && ' 已加载'}
        </span>
      </div>

      <Card>
        {jobs.isLoading ? (
          <SkeletonRows rows={6} />
        ) : jobs.isError ? (
          <ErrorState message={(jobs.error as Error).message} onRetry={() => void jobs.refetch()} />
        ) : filtered.length === 0 ? (
          <EmptyState
            title={items.length === 0 ? '还没有作业' : '没有匹配的作业'}
            description={
              items.length === 0
                ? '从 Workflow 目录提交第一个作业。'
                : '换一个状态或 workflow 过滤条件。'
            }
            action={
              items.length === 0 ? (
                <Link to="/workflows">
                  <Button size="sm" variant="primary">
                    浏览 Workflow
                  </Button>
                </Link>
              ) : undefined
            }
          />
        ) : (
          <Table>
            <thead>
              <tr>
                <Th>作业 ID</Th>
                <Th>Workflow</Th>
                <Th>状态</Th>
                <Th className="hidden sm:table-cell">进度</Th>
                <Th className="hidden sm:table-cell">设备</Th>
                <Th>创建</Th>
                <Th className="text-right">操作</Th>
              </tr>
            </thead>
            <tbody>
              {filtered.map((job) => {
                const terminal = TERMINAL_JOB_STATES.includes(job.state)
                return (
                  <tr
                    key={job.id}
                    className="transition hover:bg-surface-2/50 cursor-pointer"
                    onClick={() => navigate(`/jobs/${job.id}`)}
                  >
                    <Td>
                      <Link
                        to={`/jobs/${job.id}`}
                        className="font-mono text-xs text-accent hover:underline"
                      >
                        {job.id.slice(0, 12)}…
                      </Link>
                    </Td>
                    <Td className="whitespace-nowrap">
                      <span className="text-xs">{job.workflow_id}</span>
                      <span className="ml-1.5 text-[10px] text-subtle">{job.workflow_version}</span>
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
                    <Td className="text-xs whitespace-nowrap text-muted">
                      {formatRelative(job.created_at_unix_ms)}
                    </Td>
                    <Td className="text-right">
                      <Button
                        size="sm"
                        variant="ghost"
                        disabled={terminal}
                        onClick={(event) => {
                          event.stopPropagation()
                          setPendingCancel(job.id)
                        }}
                      >
                        取消
                      </Button>
                    </Td>
                  </tr>
                )
              })}
            </tbody>
          </Table>
        )}
        {jobs.hasNextPage && (
          <div className="flex justify-center border-t border-border px-4 py-3">
            <Button
              size="sm"
              loading={jobs.isFetchingNextPage}
              onClick={() => void jobs.fetchNextPage()}
            >
              加载更多
            </Button>
          </div>
        )}
      </Card>

      <ConfirmModal
        open={pendingCancel !== null}
        title="取消作业"
        destructive
        confirmLabel="取消作业"
        loading={cancel.isPending}
        description="Hub 会向执行设备发送取消指令。已产生的输出不会被删除。member 只能取消自己创建的作业。"
        onConfirm={confirmCancel}
        onClose={() => setPendingCancel(null)}
      />
    </Page>
  )
}
