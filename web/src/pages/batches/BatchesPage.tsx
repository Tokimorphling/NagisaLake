import { useMemo, useState } from 'react'
import { Link, useNavigate } from 'react-router-dom'
import { useBatches, useCancelBatch, isBatchTerminal } from '@/api/queries'
import type { BatchJobCounts } from '@/api/types'
import { formatRelative } from '@/lib/format'
import { useAuth } from '@/state/auth'
import { useToast } from '@/state/toast'
import { Page, PageHeader } from '@/components/layout/AppLayout'
import { ConfirmModal } from '@/components/ui/Modal'
import {
  EmptyState,
  ErrorState,
  SkeletonRows,
  Table,
  Td,
  Th,
} from '@/components/ui/display'
import { Button, Card, cx } from '@/components/ui/primitives'

export function BatchesPage() {
  const { organizationId } = useAuth()
  const batches = useBatches(organizationId)
  const cancel = useCancelBatch(organizationId)
  const toast = useToast()
  const navigate = useNavigate()
  const [pendingCancel, setPendingCancel] = useState<string | null>(null)

  const items = useMemo(
    () => batches.data?.pages.flatMap((page) => page.items) ?? [],
    [batches.data],
  )

  const confirmCancel = async () => {
    if (!pendingCancel) return
    try {
      await cancel.mutateAsync(pendingCancel)
      toast.success('已请求取消批次未完成项')
    } catch (error) {
      toast.fromError(error, '取消批次失败')
    } finally {
      setPendingCancel(null)
    }
  }

  return (
    <Page>
      <PageHeader
        title="批次"
        description="按创建时间倒序分页返回，滚到底可加载更早的批次。进行中的批次会自动轮询。"
        actions={
          <>
            {batches.isFetching ? (
              <span className="inline-flex items-center gap-1.5 rounded-full bg-accent/10 px-2.5 py-1 text-[11px] font-medium text-accent">
                <span className="size-1.5 animate-pulse rounded-full bg-accent" aria-hidden="true" />
                自动刷新中
              </span>
            ) : null}
            <Button size="sm" onClick={() => void batches.refetch()} loading={batches.isFetching}>
              刷新
            </Button>
          </>
        }
      />

      <Card>
        {batches.isLoading ? (
          <SkeletonRows rows={6} />
        ) : batches.isError ? (
          <ErrorState
            message={(batches.error as Error).message}
            onRetry={() => void batches.refetch()}
          />
        ) : items.length === 0 ? (
          <EmptyState
            title="还没有批次"
            description="批次用于一次性提交大量同类型作业。可通过 API 创建批次。"
          />
        ) : (
          <Table>
            <thead>
              <tr>
                <Th>批次 ID</Th>
                <Th>Workflow</Th>
                <Th>总数</Th>
                <Th className="min-w-[14rem]">状态分布</Th>
                <Th>批次状态</Th>
                <Th>创建</Th>
                <Th className="text-right">操作</Th>
              </tr>
            </thead>
            <tbody>
              {items.map((batch) => {
                const terminal = isBatchTerminal(batch)
                return (
                  <tr
                    key={batch.id}
                    className="cursor-pointer transition hover:bg-surface-2/50"
                    onClick={() => navigate(`/batches/${batch.id}`)}
                  >
                    <Td>
                      <Link
                        to={`/batches/${batch.id}`}
                        className="font-mono text-xs text-accent hover:underline"
                        onClick={(event) => event.stopPropagation()}
                      >
                        {batch.id.slice(0, 12)}…
                      </Link>
                    </Td>
                    <Td className="whitespace-nowrap">
                      <span className="text-xs">{batch.workflow_id}</span>
                      <span className="ml-1.5 text-[10px] text-subtle">{batch.workflow_version}</span>
                    </Td>
                    <Td>
                      <span className="font-mono text-xs tabular-nums">{batch.total}</span>
                    </Td>
                    <Td>
                      <BatchCountsBar counts={batch.counts} total={batch.total} />
                    </Td>
                    <Td>
                      <BatchStatusBadge status={batch.status} terminal={terminal} />
                    </Td>
                    <Td className="whitespace-nowrap text-xs text-muted">
                      {formatRelative(batch.created_at)}
                    </Td>
                    <Td className="text-right">
                      <Button
                        size="sm"
                        variant="ghost"
                        disabled={terminal}
                        onClick={(event) => {
                          event.stopPropagation()
                          setPendingCancel(batch.id)
                        }}
                      >
                        取消未完成
                      </Button>
                    </Td>
                  </tr>
                )
              })}
            </tbody>
          </Table>
        )}
        {batches.hasNextPage && (
          <div className="flex justify-center border-t border-border px-4 py-3">
            <Button
              size="sm"
              loading={batches.isFetchingNextPage}
              onClick={() => void batches.fetchNextPage()}
            >
              加载更多
            </Button>
          </div>
        )}
      </Card>

      <ConfirmModal
        open={pendingCancel !== null}
        title="取消批次未完成项"
        destructive
        confirmLabel="取消未完成项"
        loading={cancel.isPending}
        description="Hub 会向执行设备发送取消指令，仅取消批次中尚未完成的作业。已完成或已失败的作业不受影响。"
        onConfirm={confirmCancel}
        onClose={() => setPendingCancel(null)}
      />
    </Page>
  )
}

const COUNT_SEGMENTS: Array<{ key: keyof BatchJobCounts; tone: string; label: string }> = [
  { key: 'queued', tone: 'bg-subtle', label: '排队' },
  { key: 'received', tone: 'bg-info', label: '已接收' },
  { key: 'accepted', tone: 'bg-info', label: '已接受' },
  { key: 'running', tone: 'bg-accent', label: '执行中' },
  { key: 'uploading', tone: 'bg-violet', label: '上传中' },
  { key: 'completed', tone: 'bg-success', label: '完成' },
  { key: 'failed', tone: 'bg-danger', label: '失败' },
  { key: 'cancelled', tone: 'bg-subtle', label: '取消' },
]

function BatchCountsBar({ counts, total }: { counts: BatchJobCounts; total: number }) {
  if (total <= 0) {
    return <span className="text-xs text-subtle">—</span>
  }
  return (
    <div className="flex items-center gap-2">
      <div className="flex h-1.5 w-32 overflow-hidden rounded-full bg-surface-2 ring-1 ring-border/50">
        {COUNT_SEGMENTS.map((segment) => {
          const value = counts[segment.key]
          if (value <= 0) return null
          const width = (value / total) * 100
          return (
            <div
              key={segment.key}
              className={cx('h-full transition-[width]', segment.tone)}
              style={{ width: `${width}%` }}
              title={`${segment.label}: ${value}`}
            />
          )
        })}
      </div>
      <span className="font-mono text-[10px] tabular-nums text-muted">
        {counts.completed + counts.failed + counts.cancelled}/{total}
      </span>
    </div>
  )
}

function BatchStatusBadge({ status, terminal }: { status: string; terminal: boolean }) {
  const tone = terminal
    ? 'border-success/30 bg-success/10 text-success'
    : 'border-accent/30 bg-accent/10 text-accent'
  const dot = terminal ? 'bg-success' : 'bg-accent animate-pulse'
  return (
    <span
      className={cx(
        'inline-flex items-center gap-1 rounded-md border px-2 py-0.5 text-xs font-medium',
        tone,
      )}
    >
      <span className={cx('size-1.5 rounded-full', dot)} aria-hidden="true" />
      {status}
    </span>
  )
}
