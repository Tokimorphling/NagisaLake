import type { JobSummary, Workflow, WorkflowWorker } from '@/api/types'
import { TERMINAL_JOB_STATES } from '@/api/types'

export interface FleetMetrics {
  devices: number
  active: number
  parallelism: number
  queued: number
  queueDepth: number
}

/** Worker load is repeated on every workflow it serves; dedupe before summing. */
export function uniqueWorkflowWorkers(workflows: Workflow[]): WorkflowWorker[] {
  const workers = new Map<string, WorkflowWorker>()
  for (const workflow of workflows) {
    for (const worker of workflow.workers) {
      const key = `${worker.organization_id}/${worker.worker_id}`
      if (!workers.has(key)) workers.set(key, worker)
    }
  }
  return [...workers.values()]
}

export function fleetMetrics(workflows: Workflow[]): FleetMetrics {
  return uniqueWorkflowWorkers(workflows).reduce<FleetMetrics>(
    (total, worker) => ({
      devices: total.devices + 1,
      active: total.active + worker.active_jobs,
      parallelism: total.parallelism + worker.parallelism,
      queued: total.queued + worker.queued_jobs,
      queueDepth: total.queueDepth + worker.queue_depth,
    }),
    { devices: 0, active: 0, parallelism: 0, queued: 0, queueDepth: 0 },
  )
}

export function deviceWorkerMetrics(
  workflows: Workflow[],
  organizationId: string,
  workerId: string,
): WorkflowWorker | null {
  return (
    uniqueWorkflowWorkers(workflows).find(
      (worker) =>
        worker.organization_id === organizationId && worker.worker_id === workerId,
    ) ?? null
  )
}

export function averageQueueAgeMs(jobs: JobSummary[], now = Date.now()): number {
  const waiting = jobs.filter(
    (job) => job.state === 'queued' || job.state === 'received' || job.state === 'accepted',
  )
  if (waiting.length === 0) return 0
  return waiting.reduce((sum, job) => sum + Math.max(0, now - job.created_at_unix_ms), 0) /
    waiting.length
}

export interface TodayUsage {
  completedMs: number
  failedMs: number
  activeMs: number
  totalMs: number
  jobs: number
}

/**
 * Estimates today's compute occupancy from job wall-clock intervals available
 * in list rows. It intentionally does not call this GPU time: concurrent jobs
 * and late bookkeeping updates make that a backend telemetry concern.
 */
export function todayUsage(jobs: JobSummary[], now = Date.now()): TodayUsage {
  const start = new Date(now)
  start.setHours(0, 0, 0, 0)
  const startMs = start.getTime()
  const usage: TodayUsage = {
    completedMs: 0,
    failedMs: 0,
    activeMs: 0,
    totalMs: 0,
    jobs: 0,
  }

  for (const job of jobs) {
    const terminal = TERMINAL_JOB_STATES.includes(job.state)
    const end = terminal ? Math.min(now, job.updated_at_unix_ms) : now
    const begin = Math.max(startMs, job.created_at_unix_ms)
    if (end < startMs || begin > now || end <= begin) continue
    const duration = end - begin
    usage.jobs += 1
    usage.totalMs += duration
    if (!terminal) usage.activeMs += duration
    else if (job.state === 'completed') usage.completedMs += duration
    else usage.failedMs += duration
  }
  return usage
}

export function formatCompactDuration(ms: number): string {
  if (ms < 60_000) return `${Math.round(ms / 1000)} 秒`
  if (ms < 3_600_000) return `${Math.round(ms / 60_000)} 分`
  const hours = ms / 3_600_000
  return `${hours.toFixed(hours < 10 ? 1 : 0)} 小时`
}
