import { Link } from 'react-router-dom'
import { useDevices, useJobs, useQuota, useWorkflows } from '@/api/queries'
import { TERMINAL_JOB_STATES } from '@/api/types'
import { formatBytes, formatRelative } from '@/lib/format'
import {
  averageQueueAgeMs,
  fleetMetrics,
  formatCompactDuration,
  todayUsage,
} from '@/lib/insights'
import { useAuth } from '@/state/auth'
import { Page, PageHeader } from '@/components/layout/AppLayout'
import { IconDevice, IconJobs, IconPlay, IconWorkflow } from '@/components/layout/icons'
import {
  Copyable,
  EmptyState,
  JobStateBadge,
  Meter,
  SkeletonRows,
  Table,
  Td,
  Th,
} from '@/components/ui/display'
import { Button, Card, CardHeader } from '@/components/ui/primitives'
import { Donut, Sparkline, useRollingSeries, type ChartTone } from '@/components/ui/charts'

export function DashboardPage() {
  const { organizationId, currentMembership, atLeast, user } = useAuth()
  const workflows = useWorkflows(organizationId)
  const devices = useDevices(organizationId, atLeast('member'))
  const jobs = useJobs(organizationId)
  const quota = useQuota(organizationId)

  const onlineDevices = devices.data?.filter((device) => device.connected).length ?? 0
  const availableWorkflows = workflows.data?.filter((workflow) => workflow.available).length ?? 0
  // The list is paginated; the dashboard only summarises the newest page.
  const firstPage = jobs.data?.pages[0]?.items ?? []
  const activeJobs = firstPage.filter((job) => !TERMINAL_JOB_STATES.includes(job.state)).length
  const recentJobs = firstPage.slice(0, 6)
  const fleet = fleetMetrics(workflows.data ?? [])
  const queuedJobs = firstPage.filter(
    (job) => job.state === 'queued' || job.state === 'received' || job.state === 'accepted',
  ).length
  const queueAgeMs = averageQueueAgeMs(firstPage)
  const usage = todayUsage(firstPage)

  const onlineTrend = useRollingSeries(onlineDevices, devices.dataUpdatedAt, 18, organizationId)
  const workflowTrend = useRollingSeries(
    availableWorkflows,
    workflows.dataUpdatedAt,
    18,
    organizationId,
  )
  const activeTrend = useRollingSeries(activeJobs, jobs.dataUpdatedAt, 18, organizationId)
  const slotTrend = useRollingSeries(
    fleet.parallelism > 0 ? Math.round((fleet.active / fleet.parallelism) * 100) : 0,
    workflows.dataUpdatedAt,
    18,
    organizationId,
  )
  const queueTrend = useRollingSeries(
    Math.round(queueAgeMs / 1000),
    jobs.dataUpdatedAt,
    18,
    organizationId,
  )
  const queuedTrend = useRollingSeries(queuedJobs, jobs.dataUpdatedAt, 18, organizationId)

  const workflowUse = new Map<string, number>()
  firstPage.forEach((job) => workflowUse.set(job.workflow_id, (workflowUse.get(job.workflow_id) ?? 0) + 1))
  const quickWorkflows = (workflows.data ?? [])
    .filter((workflow) => workflow.available && workflow.manifest_consistent)
    .sort(
      (left, right) =>
        (workflowUse.get(right.id) ?? 0) - (workflowUse.get(left.id) ?? 0) ||
        left.id.localeCompare(right.id),
    )
    .slice(0, 4)

  const toMinutes = (milliseconds: number) =>
    milliseconds > 0 ? Math.max(1, Math.round(milliseconds / 60_000)) : 0

  return (
    <Page>
      <PageHeader
        title={`欢迎回来${user?.email ? `，${user.email.split('@')[0]}` : ''}`}
        description={
          currentMembership
            ? `当前组织 ${currentMembership.organization_name} · 角色 ${currentMembership.role}`
            : '正在加载组织信息'
        }
        actions={
          <Button
            variant="primary"
            size="sm"
            loading={jobs.isFetching || workflows.isFetching || devices.isFetching || quota.isFetching}
            onClick={() => void Promise.all([jobs.refetch(), workflows.refetch(), devices.refetch(), quota.refetch()])}
          >
            刷新
          </Button>
        }
      />

      <div className="grid gap-4 sm:grid-cols-3">
        <StatCard
          icon={<IconDevice className="size-4" />}
          label="在线设备"
          value={onlineDevices}
          total={devices.data?.length}
          to="/devices"
          loading={devices.isLoading}
          hint={atLeast('member') ? undefined : '需要 member 角色'}
          trend={onlineTrend}
        />
        <StatCard
          icon={<IconWorkflow className="size-4" />}
          label="可用 Workflow"
          value={availableWorkflows}
          total={workflows.data?.length}
          to="/workflows"
          loading={workflows.isLoading}
          trend={workflowTrend}
          trendTone="violet"
        />
        <StatCard
          icon={<IconJobs className="size-4" />}
          label="进行中作业"
          value={activeJobs}
          total={jobs.data ? firstPage.length : undefined}
          to="/jobs"
          loading={jobs.isLoading}
          trend={activeTrend}
          trendTone="info"
        />
      </div>

      <div className="mt-4 grid gap-4 md:grid-cols-3">
        <MiniInsight
          label="执行槽占用"
          value={fleet.parallelism > 0 ? `${fleet.active} / ${fleet.parallelism}` : '—'}
          hint={fleet.devices > 0 ? `${fleet.devices} 台设备提供实时容量` : '暂无在线容量'}
          values={slotTrend}
          tone="accent"
        />
        <MiniInsight
          label="平均排队等待"
          value={queuedJobs > 0 ? formatCompactDuration(queueAgeMs) : '无排队'}
          hint="received / accepted 作业的当前等待年龄"
          values={queueTrend}
          tone="warning"
        />
        <MiniInsight
          label="队列中的作业"
          value={queuedJobs}
          hint={`${fleet.queued} 个已进入 Worker 队列`}
          values={queuedTrend}
          tone="violet"
        />
      </div>

      <p className="mt-2 text-[10px] leading-relaxed text-subtle">
        趋势从本次打开控制台后开始采样。当前 Worker 协议尚未上报 GPU 显存 telemetry，因此界面只展示可验证的执行槽与队列数据。
      </p>

      <div className="mt-4 grid items-stretch gap-4 lg:grid-cols-[1fr_20rem]">
        <Card>
          <CardHeader
            title="常用 Workflow 快捷启动"
            description="按最近已加载作业的使用频次排序，点击后直接打开参数表单。"
            actions={
              <Link to="/workflows">
                <Button size="sm">全部 Workflow</Button>
              </Link>
            }
          />
          {quickWorkflows.length === 0 ? (
            <EmptyState
              title="暂无可快速启动的 Workflow"
              description="设备上线并上报 manifest 后会自动出现在这里。"
            />
          ) : (
            <div className="grid gap-3 p-5 sm:grid-cols-2">
              {quickWorkflows.map((workflow) => (
                <Link
                  key={`${workflow.id}@${workflow.version}`}
                  to={`/workflows?launch=${encodeURIComponent(workflow.id)}`}
                  className="group flex items-center gap-3 rounded-xl border border-border bg-surface-2/45 p-3 transition hover:border-accent/40 hover:bg-accent/5"
                >
                  <span className="grid size-9 shrink-0 place-items-center rounded-lg bg-accent/10 text-accent transition group-hover:scale-105">
                    <IconPlay className="size-4" />
                  </span>
                  <span className="min-w-0 flex-1">
                    <span className="block truncate text-xs font-semibold">
                      {workflow.manifest?.display_name || workflow.id}
                    </span>
                    <span className="mt-0.5 block truncate font-mono text-[10px] text-subtle">
                      {workflow.id} · {workflow.version}
                    </span>
                  </span>
                  <span className="text-xs text-accent opacity-0 transition group-hover:opacity-100">启动 →</span>
                </Link>
              ))}
            </div>
          )}
        </Card>

        <Card>
          <CardHeader
            title="今日算力消耗"
            description={`基于最近 ${firstPage.length} 个作业的墙钟时长实时估算`}
          />
          <div className="p-5">
            <Donut
              slices={[
                { label: '完成（分钟）', value: toMinutes(usage.completedMs), tone: 'success' },
                { label: '运行中（分钟）', value: toMinutes(usage.activeMs), tone: 'accent' },
                { label: '失败/取消（分钟）', value: toMinutes(usage.failedMs), tone: 'danger' },
              ]}
              centerValue={formatCompactDuration(usage.totalMs)}
              centerLabel={`${usage.jobs} 个今日作业`}
              size={116}
              thickness={11}
            />
            <p className="mt-4 text-[10px] leading-relaxed text-subtle">
              该值是作业时间区间估算，并非 GPU-seconds；并行任务可能重叠。
            </p>
          </div>
        </Card>
      </div>

      <div className="mt-4 grid items-start gap-4 lg:grid-cols-[1fr_20rem]">
        <Card>
          <CardHeader
            title="最近作业"
            description="轮询刷新；协议终态为 completed、failed、cancelled。"
            actions={
              <Link to="/jobs">
                <Button size="sm">查看全部</Button>
              </Link>
            }
          />
          {jobs.isLoading ? (
            <SkeletonRows rows={4} />
          ) : recentJobs.length === 0 ? (
            <EmptyState
              title="还没有作业"
              description="从 Workflow 目录选择一个已审核的 workflow 并提交第一个作业。"
              action={
                <Link to="/workflows">
                  <Button size="sm" variant="primary">
                    浏览 Workflow
                  </Button>
                </Link>
              }
            />
          ) : (
            <Table>
              <thead>
                <tr>
                  <Th>作业</Th>
                  <Th>Workflow</Th>
                  <Th>状态</Th>
                  <Th className="text-right">创建</Th>
                </tr>
              </thead>
              <tbody>
                {recentJobs.map((job) => (
                  <tr key={job.id} className="transition hover:bg-surface-2/50">
                    <Td>
                      <Link
                        to={`/jobs/${job.id}`}
                        className="font-mono text-xs text-accent hover:underline"
                      >
                        {job.id.slice(0, 8)}…
                      </Link>
                    </Td>
                    <Td>
                      <span className="text-xs">{job.workflow_id}</span>
                      <span className="ml-1.5 text-[10px] text-subtle">{job.workflow_version}</span>
                    </Td>
                    <Td>
                      <JobStateBadge state={job.state} />
                    </Td>
                    <Td className="text-right text-xs whitespace-nowrap text-muted">
                      {formatRelative(job.created_at_unix_ms)}
                    </Td>
                  </tr>
                ))}
              </tbody>
            </Table>
          )}
        </Card>

        <div className="space-y-4">
          <Card>
            <CardHeader title="配额" description="当前计费周期用量" />
            <div className="space-y-4 p-5">
              {quota.isLoading ? (
                <div className="space-y-3">
                  <div className="skeleton h-8 rounded-lg" />
                  <div className="skeleton h-8 rounded-lg" />
                </div>
              ) : quota.isError ? (
                <p className="text-xs leading-relaxed text-muted">
                  无法读取配额，可能缺少 <code className="font-mono">quota:read</code> 权限。
                </p>
              ) : quota.data ? (
                <>
                  <Meter
                    label="并发作业"
                    value={quota.data.active_jobs}
                    total={quota.data.max_concurrent_jobs}
                  />
                  <Meter
                    label="周期作业数"
                    value={quota.data.period_jobs}
                    total={quota.data.max_jobs_per_period}
                    tone="violet"
                  />
                  <Meter
                    label="存储用量"
                    value={quota.data.storage_bytes}
                    total={quota.data.max_storage_bytes}
                    formatValue={formatBytes}
                  />
                  <Link to="/quota" className="block text-xs text-accent hover:underline">
                    查看详情
                  </Link>
                </>
              ) : null}
            </div>
          </Card>

          <Card>
            <CardHeader title="组织" />
            <dl className="space-y-3 p-5 text-xs">
              <div className="space-y-1">
                <dt className="text-muted">Organization ID</dt>
                <dd>
                  {organizationId ? (
                    <Copyable value={organizationId} display={`${organizationId.slice(0, 18)}…`} />
                  ) : (
                    '—'
                  )}
                </dd>
              </div>
              <div className="space-y-1">
                <dt className="text-muted">账户状态</dt>
                <dd className="flex items-center gap-1.5">
                  <span
                    className={
                      user?.email_verified
                        ? 'size-1.5 rounded-full bg-success'
                        : 'size-1.5 rounded-full bg-warning'
                    }
                    aria-hidden="true"
                  />
                  {user?.status ?? '—'}
                  {user && !user.email_verified && (
                    <span className="text-subtle">· 邮箱未验证</span>
                  )}
                </dd>
              </div>
            </dl>
          </Card>
        </div>
      </div>
    </Page>
  )
}

function StatCard({
  icon,
  label,
  value,
  total,
  to,
  loading,
  hint,
  trend,
  trendTone = 'accent',
}: {
  icon: React.ReactNode
  label: string
  value: number
  total?: number
  to: string
  loading: boolean
  hint?: string
  trend?: number[]
  trendTone?: ChartTone
}) {
  return (
    <Link
      to={to}
      className="group rounded-xl border border-border bg-surface p-4 shadow-[var(--shadow-card)] transition hover:border-border-strong hover:bg-surface-2/40"
    >
      <div className="flex items-center gap-2 text-muted">
        <span className="grid size-7 place-items-center rounded-md bg-accent/10 text-accent">
          {icon}
        </span>
        <span className="text-xs">{label}</span>
      </div>
      <div className="mt-3 flex items-baseline gap-1.5">
        {loading ? (
          <span className="skeleton inline-block h-7 w-12 rounded" />
        ) : (
          <>
            <span className="text-2xl font-semibold tracking-tight tabular-nums">{value}</span>
            {total !== undefined && <span className="text-xs text-subtle">/ {total}</span>}
          </>
        )}
      </div>
      {hint && <p className="mt-1 text-[10px] text-subtle">{hint}</p>}
      {trend && trend.length > 0 && (
        <Sparkline values={trend} tone={trendTone} className="mt-2 h-7 opacity-80" />
      )}
    </Link>
  )
}

function MiniInsight({
  label,
  value,
  hint,
  values,
  tone,
}: {
  label: string
  value: React.ReactNode
  hint: string
  values: number[]
  tone: ChartTone
}) {
  return (
    <Card className="overflow-hidden">
      <div className="flex items-start justify-between gap-3 p-4 pb-1">
        <div className="min-w-0">
          <p className="text-xs text-muted">{label}</p>
          <p className="mt-1 text-lg font-semibold tabular-nums tracking-tight">{value}</p>
          <p className="mt-0.5 truncate text-[10px] text-subtle" title={hint}>
            {hint}
          </p>
        </div>
      </div>
      <Sparkline values={values} tone={tone} className="h-9" />
    </Card>
  )
}
