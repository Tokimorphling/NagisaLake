import { useMemo, useState } from 'react'
import { useAuditLogs } from '@/api/queries'
import type { AuditLog } from '@/api/types'
import { formatDateTime, formatRelative } from '@/lib/format'
import { useAuth } from '@/state/auth'
import { useToast } from '@/state/toast'
import { Page, PageHeader } from '@/components/layout/AppLayout'
import { IconDownload } from '@/components/layout/icons'
import {
  Badge,
  Copyable,
  EmptyState,
  ErrorState,
  SkeletonRows,
  Table,
  Td,
  Th,
} from '@/components/ui/display'
import { Button, Card, CardHeader, Input, Select, cx } from '@/components/ui/primitives'

type DateRange = 'today' | '7d' | '30d' | 'all'

const DATE_RANGES: Array<{ value: DateRange; label: string }> = [
  { value: 'today', label: '今天' },
  { value: '7d', label: '7 天' },
  { value: '30d', label: '30 天' },
  { value: 'all', label: '全部' },
]

function rangeStartMs(range: DateRange): number | null {
  if (range === 'all') return null
  const now = Date.now()
  const day = 86_400_000
  if (range === 'today') {
    const d = new Date()
    d.setHours(0, 0, 0, 0)
    return d.getTime()
  }
  if (range === '7d') return now - 7 * day
  if (range === '30d') return now - 30 * day
  return null
}

function csvEscape(value: string | null | undefined): string {
  if (value === null || value === undefined) return ''
  const s = String(value)
  if (/[",\n\r]/.test(s)) return `"${s.replace(/"/g, '""')}"`
  return s
}

function exportAuditCsv(rows: AuditLog[]) {
  const header = [
    'created_at',
    'action',
    'resource_type',
    'resource_id',
    'actor_kind',
    'actor_id',
    'request_id',
    'outcome',
    'metadata',
  ]
  const lines = [header.join(',')]
  for (const log of rows) {
    lines.push(
      [
        csvEscape(formatDateTime(log.created_at)),
        csvEscape(log.action),
        csvEscape(log.resource_type),
        csvEscape(log.resource_id),
        csvEscape(log.actor_kind),
        csvEscape(log.actor_id),
        csvEscape(log.request_id),
        csvEscape(log.outcome),
        csvEscape(log.metadata_json),
      ].join(','),
    )
  }
  const blob = new Blob([lines.join('\n')], { type: 'text/csv;charset=utf-8;' })
  const url = URL.createObjectURL(blob)
  const a = document.createElement('a')
  a.href = url
  a.download = `audit-${new Date().toISOString().slice(0, 10)}.csv`
  document.body.appendChild(a)
  a.click()
  document.body.removeChild(a)
  URL.revokeObjectURL(url)
}

function outcomeTone(outcome: string): 'success' | 'danger' | 'warning' | 'neutral' {
  if (outcome === 'success') return 'success'
  if (outcome === 'denied') return 'warning'
  if (outcome === 'failure' || outcome === 'error') return 'danger'
  return 'neutral'
}

export function AuditPage() {
  const { organizationId, atLeast } = useAuth()
  const canRead = atLeast('admin')
  const logs = useAuditLogs(organizationId, canRead)
  const [query, setQuery] = useState('')
  const [action, setAction] = useState('')
  const [range, setRange] = useState<DateRange>('all')
  const [expanded, setExpanded] = useState<string | null>(null)
  const toast = useToast()

  const actionOptions = useMemo(
    () => [...new Set((logs.data ?? []).map((log) => log.action))].sort(),
    [logs.data],
  )

  const filtered = useMemo(() => {
    const needle = query.trim().toLowerCase()
    const startMs = rangeStartMs(range)
    return (logs.data ?? [])
      .filter((log) => (action ? log.action === action : true))
      .filter((log) => (startMs !== null ? log.created_at >= startMs : true))
      .filter((log) => {
        if (!needle) return true
        return (
          log.action.toLowerCase().includes(needle) ||
          log.resource_type.toLowerCase().includes(needle) ||
          (log.resource_id ?? '').toLowerCase().includes(needle) ||
          (log.request_id ?? '').toLowerCase().includes(needle) ||
          log.metadata_json.toLowerCase().includes(needle)
        )
      })
  }, [action, logs.data, query, range])

  const handleExport = () => {
    if (filtered.length === 0) {
      toast.error('没有可导出的记录')
      return
    }
    try {
      exportAuditCsv(filtered)
      toast.success(`已导出 ${filtered.length} 条审计记录`)
    } catch (error) {
      toast.fromError(error, '导出失败')
    }
  }

  if (!canRead) {
    return (
      <Page>
        <PageHeader title="审计" />
        <Card>
          <EmptyState
            title="需要 admin 或 owner 角色"
            description="审计日志只对 admin 和 owner 开放。"
          />
        </Card>
      </Page>
    )
  }

  return (
    <Page>
      <PageHeader
        title="审计"
        description="按创建时间倒序分页加载。敏感动作会记录 actor、request id 和结果。"
        actions={
          <div className="flex items-center gap-2">
            <Button size="sm" variant="ghost" onClick={handleExport} disabled={filtered.length === 0}>
              <IconDownload className="size-3.5" />
              导出 CSV
            </Button>
            <Button size="sm" onClick={() => void logs.refetch()} loading={logs.isFetching}>
              刷新
            </Button>
          </div>
        }
      />

      <div className="mb-4 flex flex-wrap items-center gap-3">
        <div className="flex flex-wrap gap-1 rounded-lg border border-border bg-surface-2 p-1">
          {DATE_RANGES.map((option) => (
            <button
              key={option.value}
              type="button"
              onClick={() => setRange(option.value)}
              className={cx(
                'rounded-md px-2.5 py-1 text-xs transition',
                range === option.value
                  ? 'bg-accent text-accent-fg font-medium'
                  : 'text-muted hover:text-text',
              )}
            >
              {option.label}
            </button>
          ))}
        </div>
        <Input
          value={query}
          placeholder="搜索动作、资源或 request id"
          className="max-w-xs"
          onChange={(event) => setQuery(event.target.value)}
        />
        {actionOptions.length > 1 && (
          <Select
            value={action}
            className="max-w-52"
            onChange={(event) => setAction(event.target.value)}
          >
            <option value="">所有动作</option>
            {actionOptions.map((option) => (
              <option key={option} value={option}>
                {option}
              </option>
            ))}
          </Select>
        )}
        <span className="text-xs text-subtle">
          {filtered.length} / {logs.data?.length ?? 0}
        </span>
      </div>

      <Card>
        <CardHeader title="最近事件" />
        {logs.isLoading ? (
          <SkeletonRows rows={6} />
        ) : logs.isError ? (
          <ErrorState message={(logs.error as Error).message} onRetry={() => void logs.refetch()} />
        ) : filtered.length === 0 ? (
          <EmptyState
            title={logs.data?.length ? '没有匹配的记录' : '暂无审计记录'}
            description={logs.data?.length ? '调整搜索或动作过滤条件。' : undefined}
          />
        ) : (
          <Table>
            <thead>
              <tr>
                <Th>时间</Th>
                <Th>动作</Th>
                <Th>资源</Th>
                <Th>调用方</Th>
                <Th>结果</Th>
                <Th className="text-right">详情</Th>
              </tr>
            </thead>
            <tbody>
              {filtered.map((log) => (
                <AuditRow
                  key={log.id}
                  log={log}
                  expanded={expanded === log.id}
                  onToggle={() => setExpanded(expanded === log.id ? null : log.id)}
                />
              ))}
            </tbody>
          </Table>
        )}
        {logs.hasNextPage && (
          <div className="flex justify-center border-t border-border px-4 py-3">
            <Button
              size="sm"
              loading={logs.isFetchingNextPage}
              onClick={() => void logs.fetchNextPage()}
            >
              加载更多
            </Button>
          </div>
        )}
      </Card>
    </Page>
  )
}

function AuditRow({
  log,
  expanded,
  onToggle,
}: {
  log: AuditLog
  expanded: boolean
  onToggle: () => void
}) {
  let metadata = log.metadata_json
  try {
    metadata = JSON.stringify(JSON.parse(log.metadata_json), null, 2)
  } catch {
    // Keep the raw string when it is not valid JSON.
  }
  const hasMetadata = metadata !== '{}' && metadata.trim() !== ''

  return (
    <>
      <tr className="transition hover:bg-surface-2/50">
        <Td className="whitespace-nowrap">
          <span className="text-xs">{formatRelative(log.created_at)}</span>
          <p className="font-mono text-[10px] text-subtle">{formatDateTime(log.created_at)}</p>
        </Td>
        <Td>
          <code className="font-mono text-[11px]">{log.action}</code>
        </Td>
        <Td>
          <div className="flex flex-col items-start gap-0.5">
            <span className="text-xs text-muted">{log.resource_type}</span>
            {log.resource_id && (
              <Copyable
                value={log.resource_id}
                display={`${log.resource_id.slice(0, 10)}${log.resource_id.length > 10 ? '…' : ''}`}
                className="text-subtle"
              />
            )}
          </div>
        </Td>
        <Td>
          <div className="flex flex-col items-start gap-1">
            {log.actor_kind && <Badge tone="info">{log.actor_kind}</Badge>}
            {log.actor_id && (
              <Copyable
                value={log.actor_id}
                display={`${log.actor_id.slice(0, 8)}…`}
                className="text-subtle"
              />
            )}
          </div>
        </Td>
        <Td>
          <Badge tone={outcomeTone(log.outcome)}>{log.outcome}</Badge>
        </Td>
        <Td className="text-right">
          <Button size="sm" variant="ghost" disabled={!hasMetadata} onClick={onToggle}>
            {expanded ? '收起' : '展开'}
          </Button>
        </Td>
      </tr>
      {expanded && hasMetadata && (
        <tr>
          <Td className="bg-surface-2/40" />
          <td colSpan={5} className="border-b border-border/60 bg-surface-2/40 px-4 py-3">
            {log.request_id && (
              <p className="mb-2 text-[11px] text-muted">
                request id <Copyable value={log.request_id} />
              </p>
            )}
            <pre className="overflow-x-auto rounded-lg border border-border bg-surface px-3 py-2 font-mono text-[11px] leading-relaxed">
              {metadata}
            </pre>
          </td>
        </tr>
      )}
    </>
  )
}
