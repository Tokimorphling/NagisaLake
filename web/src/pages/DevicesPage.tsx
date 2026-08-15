import { useMemo, useState } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { endpoints } from '@/api/endpoints'
import { keys, useDevices, useJobs, useWorkflows } from '@/api/queries'
import type {
  CreatedDeviceInvite,
  Device,
  DeviceWorkflowRule,
  JobSummary,
  Workflow,
} from '@/api/types'
import {
  averageQueueAgeMs,
  deviceWorkerMetrics,
  formatCompactDuration,
} from '@/lib/insights'
import { formatDateTime } from '@/lib/format'
import { useAuth } from '@/state/auth'
import { useToast } from '@/state/toast'
import { Page, PageHeader } from '@/components/layout/AppLayout'
import { IconDevice, IconPlus } from '@/components/layout/icons'
import { Modal } from '@/components/ui/Modal'
import { SecretModal } from '@/components/ui/SecretModal'
import { Badge, Copyable, EmptyState, ErrorState, SkeletonRows } from '@/components/ui/display'
import { Button, Card, Checkbox, Field, Input, Select, cx } from '@/components/ui/primitives'
import { LoadBar, Sparkline, useRollingSeries } from '@/components/ui/charts'

const EXPIRY_OPTIONS = [
  { value: '3600', label: '1 小时' },
  { value: '86400', label: '1 天' },
  { value: '604800', label: '7 天' },
  { value: '2592000', label: '30 天' },
  { value: '', label: '不过期' },
]

const GRANT_DURATION_OPTIONS = [
  { value: '', label: '不限时' },
  { value: '3600', label: '1 小时' },
  { value: '86400', label: '1 天' },
  { value: '604800', label: '7 天' },
  { value: '2592000', label: '30 天' },
]

export function DevicesPage() {
  const { organizationId, atLeast, user } = useAuth()
  const devices = useDevices(organizationId)
  const workflows = useWorkflows(organizationId)
  const jobs = useJobs(organizationId)
  const queryClient = useQueryClient()
  const toast = useToast()

  const [inviteTarget, setInviteTarget] = useState<Device | null>(null)
  const [redeeming, setRedeeming] = useState(false)
  const [createdInvite, setCreatedInvite] = useState<CreatedDeviceInvite | null>(null)
  const [query, setQuery] = useState('')
  const [onlyOnline, setOnlyOnline] = useState(false)

  const canShare = atLeast('member')
  const needle = query.trim().toLowerCase()
  const filteredDevices = useMemo(() => {
    return (devices.data ?? []).filter((device) => {
      if (onlyOnline && !device.connected) return false
      if (!needle) return true
      return (
        device.node_name.toLowerCase().includes(needle) ||
        device.device_id.toLowerCase().includes(needle)
      )
    })
  }, [devices.data, needle, onlyOnline])
  const organizationDevices = filteredDevices.filter(
    (device) => device.access_kind === 'organization_device',
  )
  const sharedPoolDevices = filteredDevices.filter(
    (device) => device.access_kind === 'shared_pool_device',
  )

  return (
    <Page>
      <PageHeader
        title="设备"
        description="包含当前组织可用的 organization device，以及通过邀请码获得使用权的 shared pool device。离线设备的 workflow 仍可浏览，但不能提交作业。"
        actions={
          <>
            <Button size="sm" onClick={() => setRedeeming(true)} disabled={!canShare}>
              <IconPlus className="size-3.5" />
              兑换邀请码
            </Button>
            <Button size="sm" onClick={() => void devices.refetch()} loading={devices.isFetching}>
              刷新
            </Button>
          </>
        }
      />

      {(devices.data?.length ?? 0) > 0 && (
        <div className="mb-4 flex flex-wrap items-center gap-3">
          <Input
            value={query}
            placeholder="搜索设备名称或 ID"
            className="max-w-xs"
            onChange={(event) => setQuery(event.target.value)}
          />
          <Checkbox label="仅在线" checked={onlyOnline} onChange={setOnlyOnline} />
          {devices.data && (
            <span className="text-xs font-mono text-subtle">
              {filteredDevices.length} / {devices.data.length}
            </span>
          )}
        </div>
      )}

      {devices.isLoading ? (
        <Card>
          <SkeletonRows rows={4} />
        </Card>
      ) : devices.isError ? (
        <Card>
          <ErrorState
            message={(devices.error as Error).message}
            onRetry={() => void devices.refetch()}
          />
        </Card>
      ) : (devices.data?.length ?? 0) === 0 ? (
        <Card>
          <EmptyState
            icon={<IconDevice className="size-8" />}
            title="还没有设备"
            description="先在“凭据”页创建 nwk_ Worker 凭据，配置到 CLI 或 ComfyUI 节点；Worker 反向连接 Hub 后设备会出现在这里。"
          />
        </Card>
      ) : (
        <div className="space-y-6">
          <DeviceGroup
            title="组织设备 / organization device"
            count={organizationDevices.length}
            devices={organizationDevices}
            workflows={workflows.data ?? []}
            metricsTick={workflows.dataUpdatedAt}
            jobs={jobs.data?.pages[0]?.items ?? []}
            jobsTick={jobs.dataUpdatedAt}
            currentUserId={user?.id}
            onShare={canShare ? setInviteTarget : undefined}
          />
          {sharedPoolDevices.length > 0 && (
            <DeviceGroup
              title="共享池设备 / shared pool device"
              count={sharedPoolDevices.length}
              devices={sharedPoolDevices}
              workflows={workflows.data ?? []}
              metricsTick={workflows.dataUpdatedAt}
              jobs={jobs.data?.pages[0]?.items ?? []}
              jobsTick={jobs.dataUpdatedAt}
              currentUserId={user?.id}
            />
          )}
        </div>
      )}

      {devices.hasNextPage && (
        <div className="flex justify-center border-t border-border px-4 py-3">
          <Button
            size="sm"
            loading={devices.isFetchingNextPage}
            onClick={() => void devices.fetchNextPage()}
          >
            加载更多
          </Button>
        </div>
      )}

      {inviteTarget && (
        <InviteModal
          device={inviteTarget}
          onClose={() => setInviteTarget(null)}
          onCreated={(invite) => {
            setInviteTarget(null)
            setCreatedInvite(invite)
          }}
        />
      )}

      <RedeemModal
        open={redeeming}
        onClose={() => setRedeeming(false)}
        onRedeemed={() => {
          setRedeeming(false)
          void queryClient.invalidateQueries({ queryKey: keys.devices(organizationId) })
          void queryClient.invalidateQueries({ queryKey: keys.workflows(organizationId) })
          toast.success('邀请码已兑换', '设备和其已审核 workflow 现在可用')
        }}
      />

      <SecretModal
        open={createdInvite !== null}
        title="设备邀请码已创建"
        description={
          createdInvite
            ? `最多可使用 ${createdInvite.max_uses} 次；授权并发 ${createdInvite.max_concurrent_jobs ?? '不限'}；workflow ${
                createdInvite.allowed_workflows.length === 0
                  ? '全部'
                  : `${createdInvite.allowed_workflows.length} 个版本`
              }。通过受信渠道交给被邀请用户。`
            : ''
        }
        secret={createdInvite?.code ?? ''}
        onClose={() => setCreatedInvite(null)}
      />
    </Page>
  )
}

function DeviceGroup({
  title,
  count,
  devices,
  workflows,
  metricsTick,
  jobs,
  jobsTick,
  currentUserId,
  onShare,
}: {
  title: string
  count: number
  devices: Device[]
  workflows: Workflow[]
  metricsTick: number
  jobs: JobSummary[]
  jobsTick: number
  currentUserId?: string
  onShare?: (device: Device) => void
}) {
  if (devices.length === 0) {
    return (
      <section className="space-y-3">
        <h2 className="text-xs font-semibold tracking-wide text-muted uppercase">
          {title} <span className="text-subtle">({count})</span>
        </h2>
        <Card>
          <EmptyState title="这一组还没有设备" />
        </Card>
      </section>
    )
  }

  return (
    <section className="space-y-3">
      <h2 className="text-xs font-semibold tracking-wide text-muted uppercase">
        {title} <span className="text-subtle">({count})</span>
      </h2>
      <div className="grid gap-4 md:grid-cols-2">
        {devices.map((device) => (
          <DeviceCard
            key={`${device.device_organization_id}/${device.device_id}`}
            device={device}
            workflows={workflows}
            metricsTick={metricsTick}
            jobs={jobs}
            jobsTick={jobsTick}
            currentUserId={currentUserId}
            onShare={onShare}
          />
        ))}
      </div>
    </section>
  )
}

function DeviceCard({
  device,
  workflows,
  metricsTick,
  jobs,
  jobsTick,
  currentUserId,
  onShare,
}: {
  device: Device
  workflows: Workflow[]
  metricsTick: number
  jobs: JobSummary[]
  jobsTick: number
  currentUserId?: string
  onShare?: (device: Device) => void
}) {
  const worker = deviceWorkerMetrics(
    workflows,
    device.device_organization_id,
    device.device_id,
  )
  const utilization = worker?.parallelism
    ? Math.round((worker.active_jobs / worker.parallelism) * 100)
    : 0
  const utilizationTrend = useRollingSeries(utilization, metricsTick)
  const queueTrend = useRollingSeries(worker?.queued_jobs ?? 0, metricsTick)
  const deviceQueueAge = averageQueueAgeMs(jobs.filter((job) => job.worker_id === device.device_id))
  const queueAgeTrend = useRollingSeries(Math.round(deviceQueueAge / 1000), jobsTick)
  const isOrganizationDevice = device.access_kind === 'organization_device'
  const isSharedPoolDevice = device.access_kind === 'shared_pool_device'
  const canShareDevice =
    Boolean(onShare) &&
    isOrganizationDevice &&
    (device.owner_user_id === currentUserId || device.owner_user_id === null)
  const accessLabel = isOrganizationDevice
    ? device.owner_user_id === currentUserId
      ? '你注册的 organization device'
      : device.owner_user_id === null
        ? 'organization device'
        : '组织内成员的 organization device'
    : 'shared pool device'

  return (
    <Card className="overflow-hidden">
      <div className="space-y-4 p-5">
        <div className="flex items-start justify-between gap-2">
          <div className="min-w-0">
            <h3 className="truncate text-sm font-semibold tracking-tight">
              {device.node_name || device.device_id}
            </h3>
            <Copyable value={device.device_id} className="mt-0.5 text-subtle" />
          </div>
          <Badge tone={device.connected ? 'success' : 'neutral'}>
            <span
              className={cx(
                'size-1.5 rounded-full bg-current',
                device.connected && 'animate-pulse',
              )}
              aria-hidden="true"
            />
            {device.connected ? '在线' : '离线'}
          </Badge>
        </div>

        <dl className="grid grid-cols-2 gap-2 text-xs">
          <div>
            <dt className="text-muted">namespace</dt>
            <dd className="truncate font-mono text-[11px]">{device.namespace || '—'}</dd>
          </div>
          <div>
            <dt className="text-muted">Worker 版本</dt>
            <dd className="truncate font-mono text-[11px]">{device.worker_version || '—'}</dd>
          </div>
        </dl>

        <div className="rounded-xl border border-border/70 bg-surface-2/40 p-3.5">
          <div className="mb-3 flex items-start justify-between gap-3">
            <div>
              <p className="text-xs font-semibold">算力与队列</p>
              <p className="mt-0.5 text-[10px] text-subtle">随 Workflow 目录轮询采样</p>
            </div>
            <span className="font-mono text-sm font-semibold text-accent">
              {worker ? `${utilization}%` : '—'}
            </span>
          </div>

          {worker ? (
            <div className="space-y-3">
              <Sparkline values={utilizationTrend} className="h-8" />
              <LoadBar
                label="执行槽"
                used={worker.active_jobs}
                total={worker.parallelism}
              />
              <div className="grid grid-cols-[1fr_5rem] items-end gap-3">
                <LoadBar
                  label="任务队列"
                  used={worker.queued_jobs}
                  total={worker.queue_depth}
                  tone="violet"
                />
                <Sparkline values={queueTrend} tone="violet" className="h-7" showArea={false} />
              </div>
              <div className="grid grid-cols-[1fr_5rem] items-end gap-3 border-t border-border/50 pt-2.5">
                <div>
                  <p className="text-[10px] text-muted">平均排队等待</p>
                  <p className="mt-0.5 font-mono text-xs tabular-nums">
                    {deviceQueueAge > 0 ? formatCompactDuration(deviceQueueAge) : '无排队'}
                  </p>
                </div>
                <Sparkline values={queueAgeTrend} tone="warning" className="h-7" showArea={false} />
              </div>
            </div>
          ) : (
            <p className="py-3 text-center text-[11px] leading-relaxed text-subtle">
              {device.connected ? '目录中暂时没有该设备的负载快照' : '设备离线，无法读取运行指标'}
            </p>
          )}

          <div className="mt-3 flex items-center justify-between border-t border-border/60 pt-2.5 text-[10px]">
            <span className="text-muted">GPU 显存占用</span>
            <span className="font-mono text-subtle" title="当前 Worker heartbeat 未包含显存 telemetry">
              未上报
            </span>
          </div>
        </div>

        <div className="space-y-1.5">
          <p className="text-xs text-muted">
            已审核 workflow <span className="text-subtle">({device.workflows.length})</span>
          </p>
          {device.workflows.length === 0 ? (
            <p className="text-[11px] text-subtle">该设备尚未上报 workflow</p>
          ) : (
            <div className="flex flex-wrap gap-1.5">
              {device.workflows.slice(0, 6).map((workflow) => (
                <Badge key={`${workflow.id}@${workflow.version}`} tone="info">
                  {workflow.id}
                  <span className="text-subtle">{workflow.version}</span>
                </Badge>
              ))}
              {device.workflows.length > 6 && <Badge>+{device.workflows.length - 6}</Badge>}
            </div>
          )}
        </div>

        {isSharedPoolDevice && (
          <div className="rounded-lg border border-border bg-surface-2 px-3 py-2 text-[11px]">
            <div className="mb-2 font-medium text-muted">shared pool device 策略</div>
            <div className="grid gap-1 text-subtle sm:grid-cols-3">
              <span>
                workflow:{' '}
                <span className="text-text">
                  {device.allowed_workflows.length === 0
                    ? '全部已审核'
                    : `${device.allowed_workflows.length} 个版本`}
                </span>
              </span>
              <span>
                并发:{' '}
                <span className="font-mono text-text">
                  {device.max_concurrent_jobs ?? '不限'}
                </span>
              </span>
              <span>
                授权:{' '}
                <span className="text-text">
                  {device.grant_expires_at ? formatDateTime(device.grant_expires_at) : '不限时'}
                </span>
              </span>
            </div>
          </div>
        )}
      </div>

      <div className="flex items-center justify-between gap-2 border-t border-border px-5 py-3">
        <span className="text-[11px] text-subtle">{accessLabel}</span>
        {canShareDevice && onShare && (
          <Button size="sm" onClick={() => onShare(device)}>
            创建邀请码
          </Button>
        )}
      </div>
    </Card>
  )
}

const WORKFLOW_KEY_SEPARATOR = '\u0000'

function workflowRuleKey(rule: DeviceWorkflowRule): string {
  return `${rule.id}${WORKFLOW_KEY_SEPARATOR}${rule.version}`
}

function parseWorkflowRuleKey(key: string): DeviceWorkflowRule {
  const splitAt = key.indexOf(WORKFLOW_KEY_SEPARATOR)
  if (splitAt < 0) return { id: key, version: '' }
  return {
    id: key.slice(0, splitAt),
    version: key.slice(splitAt + WORKFLOW_KEY_SEPARATOR.length),
  }
}

function InviteModal({
  device,
  onClose,
  onCreated,
}: {
  device: Device
  onClose: () => void
  onCreated: (invite: CreatedDeviceInvite) => void
}) {
  const toast = useToast()
  const [maxUses, setMaxUses] = useState('1')
  const [expiry, setExpiry] = useState('86400')
  const [workflowScope, setWorkflowScope] = useState<'all' | 'subset'>('all')
  const [selectedWorkflowKeys, setSelectedWorkflowKeys] = useState<Set<string>>(() => new Set())
  const [maxConcurrentJobs, setMaxConcurrentJobs] = useState('1')
  const [grantDuration, setGrantDuration] = useState('604800')
  const [busy, setBusy] = useState(false)
  const missingWorkflowSelection = workflowScope === 'subset' && selectedWorkflowKeys.size === 0

  const allowedWorkflows = useMemo(
    () =>
      workflowScope === 'all'
        ? []
        : Array.from(selectedWorkflowKeys).map(parseWorkflowRuleKey),
    [selectedWorkflowKeys, workflowScope],
  )

  const toggleWorkflow = (key: string, checked: boolean) => {
    setSelectedWorkflowKeys((previous) => {
      const next = new Set(previous)
      if (checked) {
        next.add(key)
      } else {
        next.delete(key)
      }
      return next
    })
  }

  const create = async () => {
    if (missingWorkflowSelection) return
    setBusy(true)
    try {
      const invite = await endpoints.createDeviceInvite({
        device_organization_id: device.device_organization_id,
        device_id: device.device_id,
        max_uses: Number.parseInt(maxUses, 10) || 1,
        expires_in_seconds: expiry ? Number.parseInt(expiry, 10) : undefined,
        allowed_workflows: allowedWorkflows,
        max_concurrent_jobs: maxConcurrentJobs
          ? Number.parseInt(maxConcurrentJobs, 10)
          : undefined,
        grant_duration_seconds: grantDuration
          ? Number.parseInt(grantDuration, 10)
          : undefined,
      })
      onCreated(invite)
    } catch (error) {
      toast.fromError(error, '创建邀请码失败')
    } finally {
      setBusy(false)
    }
  }

  return (
    <Modal
      open
      title="创建设备邀请码"
      description={`被邀请用户可以向 ${device.node_name || device.device_id} 提交作业，但不会加入该设备所属组织。`}
      onClose={onClose}
      footer={
        <>
          <Button size="sm" onClick={onClose} disabled={busy}>
            取消
          </Button>
          <Button
            size="sm"
            variant="primary"
            loading={busy}
            disabled={missingWorkflowSelection}
            onClick={create}
          >
            创建
          </Button>
        </>
      }
    >
      <div className="space-y-4">
        <Field label="最大使用次数" hint="1 到 100 次。同一用户重复兑换同一邀请码不会重复计数。">
          {(id) => (
            <Input
              id={id}
              type="number"
              min={1}
              max={100}
              value={maxUses}
              onChange={(event) => setMaxUses(event.target.value)}
            />
          )}
        </Field>
        <Field label="有效期" hint="服务端会把有效期限制在 1 分钟到 30 天之间。">
          {(id) => (
            <Select id={id} value={expiry} onChange={(event) => setExpiry(event.target.value)}>
              {EXPIRY_OPTIONS.filter((option) => option.value !== '' && Number(option.value) <= 2592000).map(
                (option) => (
                  <option key={option.value} value={option.value}>
                    {option.label}
                  </option>
                ),
              )}
            </Select>
          )}
        </Field>
        <Field
          label="允许的 workflow"
          hint="选择全部会允许这个设备当前和后续已审核的 workflow；选择子集会固定到具体 id/version。"
          error={missingWorkflowSelection ? '至少选择一个 workflow 版本' : null}
        >
          {(id) => (
            <Select
              id={id}
              value={workflowScope}
              onChange={(event) => {
                const next = event.target.value as 'all' | 'subset'
                setWorkflowScope(next)
                if (next === 'all') setSelectedWorkflowKeys(new Set())
              }}
            >
              <option value="all">全部已审核 workflow</option>
              <option value="subset">只允许选中的版本</option>
            </Select>
          )}
        </Field>
        {workflowScope === 'subset' && (
          <div className="max-h-40 space-y-2 overflow-y-auto rounded-lg border border-border bg-surface-2 p-3">
            {device.workflows.length === 0 ? (
              <p className="text-xs text-subtle">这个设备还没有已审核 workflow。</p>
            ) : (
              device.workflows.map((workflow) => {
                const key = workflowRuleKey(workflow)
                return (
                  <Checkbox
                    key={key}
                    label={
                      <span className="font-mono text-xs">
                        {workflow.id}
                        <span className="text-subtle"> {workflow.version}</span>
                      </span>
                    }
                    checked={selectedWorkflowKeys.has(key)}
                    onChange={(checked) => toggleWorkflow(key, checked)}
                  />
                )
              })
            )}
          </div>
        )}
        <Field label="共享并发上限" hint="限制每个被授权用户在这个 shared pool device 上的同时运行数量。">
          {(id) => (
            <Input
              id={id}
              type="number"
              min={1}
              max={1000}
              value={maxConcurrentJobs}
              onChange={(event) => setMaxConcurrentJobs(event.target.value)}
            />
          )}
        </Field>
        <Field label="授权使用时长" hint="兑换成功后开始计时；留空表示授权不自动过期。">
          {(id) => (
            <Select
              id={id}
              value={grantDuration}
              onChange={(event) => setGrantDuration(event.target.value)}
            >
              {GRANT_DURATION_OPTIONS.map((option) => (
                <option key={option.value} value={option.value}>
                  {option.label}
                </option>
              ))}
            </Select>
          )}
        </Field>
        <p className="rounded-lg border border-border bg-surface-2 px-3 py-2 text-xs leading-relaxed text-muted">
          shared pool device 执行的作业，其输入输出对象、幂等记录和配额都记在提交方的组织，不记在设备
          owner 的组织。
        </p>
      </div>
    </Modal>
  )
}

function RedeemModal({
  open,
  onClose,
  onRedeemed,
}: {
  open: boolean
  onClose: () => void
  onRedeemed: () => void
}) {
  const toast = useToast()
  const [code, setCode] = useState('')
  const [busy, setBusy] = useState(false)

  const redeem = async () => {
    setBusy(true)
    try {
      await endpoints.acceptDeviceInvite(code.trim())
      setCode('')
      onRedeemed()
    } catch (error) {
      toast.fromError(error, '兑换邀请码失败')
    } finally {
      setBusy(false)
    }
  }

  return (
    <Modal
      open={open}
      title="兑换设备邀请码"
      description="兑换后你会获得该设备的使用权，可以把作业定向到它。"
      onClose={onClose}
      footer={
        <>
          <Button size="sm" onClick={onClose} disabled={busy}>
            取消
          </Button>
          <Button
            size="sm"
            variant="primary"
            loading={busy}
            disabled={!code.trim().startsWith('ndi_')}
            onClick={redeem}
          >
            兑换
          </Button>
        </>
      }
    >
      <Field
        label="邀请码"
        required
        hint="以 ndi_ 开头。仅授予特定设备的使用权，不会把你加入设备所属组织。"
      >
        {(id) => (
          <Input
            id={id}
            autoFocus
            value={code}
            placeholder="ndi_..."
            className="font-mono"
            onChange={(event) => setCode(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === 'Enter' && code.trim() && !busy) void redeem()
            }}
          />
        )}
      </Field>
    </Modal>
  )
}
