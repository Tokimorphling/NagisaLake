import { useEffect, useRef, useState } from 'react'
import type { ReactNode } from 'react'
import type { JobState, Role } from '@/api/types'
import { copyText, formatRelative } from '@/lib/format'
import { Button, cx } from './primitives'

/* ----------------------------------------------------------------- Badge */

export type Tone = 'neutral' | 'accent' | 'success' | 'warning' | 'danger' | 'info' | 'violet'

const BADGE_TONES: Record<Tone, string> = {
  neutral: 'border-border-strong/60 bg-surface-2 text-muted',
  accent: 'border-accent/30 bg-accent/10 text-accent',
  success: 'border-success/30 bg-success/10 text-success',
  warning: 'border-warning/30 bg-warning/10 text-warning',
  danger: 'border-danger/30 bg-danger/10 text-danger',
  info: 'border-info/30 bg-info/10 text-info',
  violet: 'border-violet/30 bg-violet/10 text-violet',
}

export function Badge({
  children,
  tone = 'neutral',
  className,
}: {
  children: ReactNode
  tone?: Tone
  className?: string
}) {
  return (
    <span
      className={cx(
        'inline-flex items-center gap-1 rounded-md border px-2 py-0.5 text-xs font-medium',
        BADGE_TONES[tone],
        className,
      )}
    >
      {children}
    </span>
  )
}

const JOB_STATE_META: Record<JobState, { tone: Tone; label: string; strike?: boolean }> = {
  queued: { tone: 'neutral', label: '等待设备' },
  received: { tone: 'neutral', label: '已接收' },
  accepted: { tone: 'info', label: '排队中' },
  running: { tone: 'accent', label: '执行中' },
  uploading: { tone: 'violet', label: '上传输出' },
  completed: { tone: 'success', label: '已完成' },
  failed: { tone: 'danger', label: '失败' },
  cancelled: { tone: 'neutral', label: '已取消', strike: true },
}

export function JobStateBadge({ state }: { state: JobState }) {
  const meta = JOB_STATE_META[state] ?? { tone: 'neutral' as Tone, label: state }
  const active = state === 'running' || state === 'uploading'
  return (
    <Badge tone={meta.tone}>
      <span
        className={cx('size-1.5 rounded-full bg-current', active && 'animate-pulse')}
        aria-hidden="true"
      />
      <span className={meta.strike ? 'line-through opacity-80' : undefined}>{meta.label}</span>
    </Badge>
  )
}

const ROLE_META: Record<Role, { tone: Tone; label: string }> = {
  viewer: { tone: 'neutral', label: 'viewer' },
  member: { tone: 'info', label: 'member' },
  operator: { tone: 'accent', label: 'operator' },
  admin: { tone: 'violet', label: 'admin' },
  owner: { tone: 'violet', label: 'owner' },
}

export function RoleBadge({ role }: { role: Role }) {
  const meta = ROLE_META[role] ?? { tone: 'neutral' as Tone, label: role }
  return <Badge tone={meta.tone}>{meta.label}</Badge>
}

/* ------------------------------------------------------------ Copyable */

export function Copyable({
  value,
  display,
  className,
  mono = true,
}: {
  value: string
  display?: string
  className?: string
  mono?: boolean
}) {
  const [copied, setCopied] = useState(false)
  const timer = useRef<number | undefined>(undefined)

  useEffect(() => () => window.clearTimeout(timer.current), [])

  return (
    <button
      type="button"
      title={`点击复制：${value}`}
      onClick={async () => {
        if (await copyText(value)) {
          setCopied(true)
          window.clearTimeout(timer.current)
          timer.current = window.setTimeout(() => setCopied(false), 1500)
        }
      }}
      // min-w-0 is required for the inner truncate to engage when this sits in
      // a flex row, where items otherwise refuse to shrink below their content.
      className={cx(
        'group inline-flex max-w-full min-w-0 items-center gap-1.5 rounded px-1 -mx-1 text-left transition hover:bg-surface-2',
        mono && 'font-mono text-xs',
        className,
      )}
    >
      <span className="truncate">{display ?? value}</span>
      <span
        className={cx(
          'shrink-0 text-[10px] transition',
          copied ? 'text-success' : 'text-subtle opacity-0 group-hover:opacity-100',
        )}
      >
        {copied ? '已复制' : '复制'}
      </span>
    </button>
  )
}

/* ------------------------------------------------------ RelativeTime */

// Renders a relative timestamp and re-ticks every 30s so the label stays
// fresh without the parent needing to re-render or poll.
export function RelativeTime({
  value,
  className,
}: {
  value: number | null | undefined
  className?: string
}) {
  const [, setTick] = useState(0)
  useEffect(() => {
    if (value === null || value === undefined) return
    const id = window.setInterval(() => setTick((t) => t + 1), 30_000)
    return () => window.clearInterval(id)
  }, [value])
  return <span className={className}>{formatRelative(value)}</span>
}

/* ---------------------------------------------------------------- States */

export function EmptyState({
  title,
  description,
  action,
  icon,
}: {
  title: string
  description?: ReactNode
  action?: ReactNode
  icon?: ReactNode
}) {
  return (
    <div className="flex flex-col items-center justify-center gap-3 px-6 py-14 text-center">
      {icon && <div className="text-subtle">{icon}</div>}
      <div className="space-y-1">
        <p className="text-sm font-medium">{title}</p>
        {description && (
          <p className="mx-auto max-w-md text-xs leading-relaxed text-muted">{description}</p>
        )}
      </div>
      {action}
    </div>
  )
}

export function ErrorState({ message, onRetry }: { message: string; onRetry?: () => void }) {
  return (
    <div className="flex flex-col items-center justify-center gap-3 px-6 py-12 text-center">
      <p className="text-sm font-medium text-danger">加载失败</p>
      <p className="max-w-md text-xs leading-relaxed text-muted">{message}</p>
      {onRetry && (
        <Button size="sm" onClick={onRetry}>
          重试
        </Button>
      )}
    </div>
  )
}

export function SkeletonRows({ rows = 4, className }: { rows?: number; className?: string }) {
  return (
    <div className={cx('space-y-2 p-4', className)}>
      {Array.from({ length: rows }, (_, index) => (
        <div key={index} className="skeleton h-9 rounded-lg" />
      ))}
    </div>
  )
}

/* ----------------------------------------------------------------- Meter */

export function Meter({
  label,
  value,
  total,
  formatValue,
  tone = 'accent',
}: {
  label: string
  value: number
  total: number
  formatValue?: (value: number) => string
  tone?: Tone
}) {
  const unlimited = total <= 0
  const ratio = unlimited ? 0 : Math.min(100, Math.round((value / total) * 100))
  const format = formatValue ?? ((input: number) => input.toLocaleString('zh-CN'))
  const barTone =
    ratio >= 90 ? 'bg-danger' : ratio >= 70 ? 'bg-warning' : tone === 'violet' ? 'bg-violet' : 'bg-accent'

  return (
    <div className="space-y-2">
      <div className="flex items-baseline justify-between gap-2">
        <span className="text-xs text-muted">{label}</span>
        <span className="font-mono text-xs">
          {format(value)}
          <span className="text-subtle"> / {unlimited ? '不限' : format(total)}</span>
        </span>
      </div>
      {/* An inset ring keeps the track readable when usage is zero. */}
      <div className="h-1.5 overflow-hidden rounded-full bg-surface-2 ring-1 ring-border/70 ring-inset">
        <div
          className={cx('h-full rounded-full transition-[width] duration-500', barTone)}
          style={{ width: `${unlimited ? 0 : Math.max(ratio, value > 0 ? 2 : 0)}%` }}
        />
      </div>
    </div>
  )
}

/* ----------------------------------------------------------------- Table */

export function Table({ children }: { children: ReactNode }) {
  return (
    <div className="overflow-x-auto">
      <table className="w-full min-w-[36rem] border-collapse text-sm">{children}</table>
    </div>
  )
}

export function Th({ children, className }: { children?: ReactNode; className?: string }) {
  return (
    <th
      scope="col"
      className={cx(
        'border-b border-border px-4 py-2.5 text-left text-xs font-medium whitespace-nowrap text-muted',
        className,
      )}
    >
      {children}
    </th>
  )
}

export function Td({ children, className }: { children?: ReactNode; className?: string }) {
  return (
    <td className={cx('border-b border-border/60 px-4 py-3 align-middle', className)}>{children}</td>
  )
}
