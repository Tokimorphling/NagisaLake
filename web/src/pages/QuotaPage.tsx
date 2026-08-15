import { useEffect, useState } from 'react'
import { useQuota, useUpdateQuota } from '@/api/queries'
import { formatBytes, formatDateTime, formatDuration } from '@/lib/format'
import { useAuth } from '@/state/auth'
import { useToast } from '@/state/toast'
import { Page, PageHeader } from '@/components/layout/AppLayout'
import { ErrorState, Meter, SkeletonRows } from '@/components/ui/display'
import { Button, Card, CardHeader, Field, Input } from '@/components/ui/primitives'

interface QuotaDraft {
  maxConcurrentJobs: string
  maxStorageGiB: string
  maxJobsPerPeriod: string
  periodDays: string
}

const BYTES_PER_GIB = 1024 ** 3

export function QuotaPage() {
  const { organizationId, atLeast } = useAuth()
  const quota = useQuota(organizationId)
  const updateQuota = useUpdateQuota(organizationId)
  const toast = useToast()
  const canManage = atLeast('admin')
  const [draft, setDraft] = useState<QuotaDraft | null>(null)
  const [initialDraft, setInitialDraft] = useState<QuotaDraft | null>(null)

  useEffect(() => {
    if (!quota.data) return
    const next: QuotaDraft = {
      maxConcurrentJobs: String(quota.data.max_concurrent_jobs),
      maxStorageGiB: String(Math.round(quota.data.max_storage_bytes / BYTES_PER_GIB)),
      maxJobsPerPeriod: String(quota.data.max_jobs_per_period),
      periodDays: String(Math.max(1, Math.round(quota.data.period_seconds / 86_400))),
    }
    setDraft(next)
    setInitialDraft(next)
  }, [quota.data])

  const isDirty =
    draft !== null &&
    initialDraft !== null &&
    (draft.maxConcurrentJobs !== initialDraft.maxConcurrentJobs ||
      draft.maxStorageGiB !== initialDraft.maxStorageGiB ||
      draft.maxJobsPerPeriod !== initialDraft.maxJobsPerPeriod ||
      draft.periodDays !== initialDraft.periodDays)

  const discardChanges = () => {
    if (initialDraft) setDraft(initialDraft)
  }

  const save = async (event: React.FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    if (!draft) return
    const maxConcurrentJobs = Number(draft.maxConcurrentJobs)
    const maxStorageGiB = Number(draft.maxStorageGiB)
    const maxJobsPerPeriod = Number(draft.maxJobsPerPeriod)
    const periodDays = Number(draft.periodDays)
    if (![maxConcurrentJobs, maxStorageGiB, maxJobsPerPeriod, periodDays].every(Number.isFinite)) {
      toast.error('配额策略无效', '所有字段都必须是数字')
      return
    }
    try {
      await updateQuota.mutateAsync({
        max_concurrent_jobs: Math.round(maxConcurrentJobs),
        max_storage_bytes: Math.round(maxStorageGiB * BYTES_PER_GIB),
        max_jobs_per_period: Math.round(maxJobsPerPeriod),
        period_seconds: Math.round(periodDays * 86_400),
      })
      toast.success('配额策略已更新')
      setInitialDraft(draft)
    } catch (error) {
      toast.fromError(error, '更新配额策略失败')
    }
  }

  return (
    <Page>
      <PageHeader
        title="配额"
        description="配额按组织统计。shared pool device 执行的作业记在提交方组织，不记在设备 owner 组织。"
        actions={<Button size="sm" onClick={() => void quota.refetch()} loading={quota.isFetching}>刷新</Button>}
      />

      {quota.isLoading ? (
        <Card><SkeletonRows rows={4} /></Card>
      ) : quota.isError ? (
        <Card><ErrorState message={(quota.error as Error).message} onRetry={() => void quota.refetch()} /></Card>
      ) : quota.data ? (
        <div className="space-y-4">
          <div className="grid items-start gap-4 lg:grid-cols-[1fr_18rem]">
            <Card>
              <CardHeader title="用量" description="active jobs 由 Hub 定时按作业事实对账。" />
              <div className="space-y-6 p-5">
                <Meter label="并发作业" value={quota.data.active_jobs} total={quota.data.max_concurrent_jobs} />
                <Meter label="周期作业数" value={quota.data.period_jobs} total={quota.data.max_jobs_per_period} tone="violet" />
                <Meter label="存储用量" value={quota.data.storage_bytes} total={quota.data.max_storage_bytes} formatValue={formatBytes} />
              </div>
            </Card>

            <Card>
              <CardHeader title="周期" />
              <dl className="space-y-3 p-5 text-xs">
                <div className="space-y-1"><dt className="text-muted">周期长度</dt><dd className="font-mono">{formatDuration(quota.data.period_seconds)}</dd></div>
                <div className="space-y-1"><dt className="text-muted">周期开始</dt><dd className="font-mono">{formatDateTime(quota.data.period_started_at)}</dd></div>
                <div className="space-y-1"><dt className="text-muted">周期结束</dt><dd className="font-mono">{formatDateTime(quota.data.period_started_at + quota.data.period_seconds * 1000)}</dd></div>
              </dl>
            </Card>
          </div>

          {canManage && draft && (
            <Card>
              <CardHeader title="配额策略" description="降低上限不会取消已有作业；并发占用会在作业终态或对账时释放。" />
              <form className="grid gap-4 p-5 sm:grid-cols-2 lg:grid-cols-4" onSubmit={(event) => void save(event)}>
                <Field label="最大并发作业" hint="1-10,000">
                  {(id) => <Input id={id} type="number" min={1} max={10_000} value={draft.maxConcurrentJobs} onChange={(event) => setDraft({ ...draft, maxConcurrentJobs: event.target.value })} />}
                </Field>
                <Field label="最大存储（GiB）" hint="1-5,120">
                  {(id) => <Input id={id} type="number" min={1} max={5120} value={draft.maxStorageGiB} onChange={(event) => setDraft({ ...draft, maxStorageGiB: event.target.value })} />}
                </Field>
                <Field label="周期最大作业数" hint="1-1,000,000,000">
                  {(id) => <Input id={id} type="number" min={1} max={1_000_000_000} value={draft.maxJobsPerPeriod} onChange={(event) => setDraft({ ...draft, maxJobsPerPeriod: event.target.value })} />}
                </Field>
                <Field label="周期长度（天）" hint="最多 365 天">
                  {(id) => <Input id={id} type="number" min={1} max={365} value={draft.periodDays} onChange={(event) => setDraft({ ...draft, periodDays: event.target.value })} />}
                </Field>
                <div className="flex flex-wrap items-center justify-end gap-2 sm:col-span-2 lg:col-span-4">
                  <Button
                    type="button"
                    variant="ghost"
                    size="sm"
                    disabled={!isDirty || updateQuota.isPending}
                    onClick={discardChanges}
                  >
                    放弃修改
                  </Button>
                  <Button type="submit" loading={updateQuota.isPending} disabled={!isDirty}>
                    保存策略
                  </Button>
                </div>
              </form>
            </Card>
          )}
        </div>
      ) : null}
    </Page>
  )
}
