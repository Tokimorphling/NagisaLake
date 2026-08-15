import type { Workflow, WorkflowWorker } from '@/api/types'

/**
 * Whether a workflow can take work right now.
 *
 * The API's `available` flag means "some worker can accept another job", not
 * "some worker has a free execution slot". The latter is derived from each
 * worker's load so the UI can distinguish immediate execution from worker-side
 * queueing.
 *
 * `workers` only ever contains connected workers — the Hub builds it from live
 * sessions — so its length is what distinguishes the two.
 */
export type WorkflowAvailability = 'available' | 'queueing' | 'busy' | 'offline'

export interface WorkflowCapacity {
  availability: WorkflowAvailability
  /** Connected devices offering this workflow. */
  devices: number
  /** Jobs executing right now across those devices. */
  active: number
  /** Jobs accepted by a worker but waiting for an execution permit. */
  queued: number
  /** Total execution permits across connected devices. */
  parallelism: number
  /** Total worker-side queue capacity across connected devices. */
  queueDepth: number
}

function canStartImmediately(worker: WorkflowWorker): boolean {
  return worker.available && worker.active_jobs + worker.queued_jobs < worker.parallelism
}

export function workflowCapacity(workflow: Workflow): WorkflowCapacity {
  const workers = workflow.workers
  const totals = workers.reduce(
    (accumulator, worker) => ({
      active: accumulator.active + worker.active_jobs,
      queued: accumulator.queued + worker.queued_jobs,
      parallelism: accumulator.parallelism + worker.parallelism,
      queueDepth: accumulator.queueDepth + worker.queue_depth,
    }),
    { active: 0, queued: 0, parallelism: 0, queueDepth: 0 },
  )

  let availability: WorkflowAvailability
  if (workers.length === 0) availability = 'offline'
  else if (!workflow.available) availability = 'busy'
  else if (workers.some(canStartImmediately)) availability = 'available'
  else availability = 'queueing'

  return { availability, devices: workers.length, ...totals }
}

const LABELS: Record<WorkflowAvailability, string> = {
  available: '可用',
  queueing: '可排队',
  busy: '忙碌',
  offline: '离线',
}

const TONES: Record<WorkflowAvailability, 'success' | 'info' | 'warning' | 'neutral'> = {
  available: 'success',
  queueing: 'info',
  busy: 'warning',
  offline: 'neutral',
}

export function availabilityLabel(availability: WorkflowAvailability): string {
  return LABELS[availability]
}

export function availabilityTone(
  availability: WorkflowAvailability,
): 'success' | 'info' | 'warning' | 'neutral' {
  return TONES[availability]
}

/** One line describing where the capacity went, for the card footer. */
export function capacitySummary(capacity: WorkflowCapacity): string {
  if (capacity.devices === 0) return '无在线设备'
  const parts = [
    `${capacity.devices} 台设备在线`,
    `执行中 ${capacity.active}/${capacity.parallelism}`,
  ]
  if (capacity.queueDepth > 0) parts.push(`排队 ${capacity.queued}/${capacity.queueDepth}`)
  return parts.join(' · ')
}

/**
 * Why submission is blocked, or `null` when it is allowed.
 *
 * Queueing is submittable. Busy means all execution and queue capacity is
 * occupied, while offline means no connected worker offers the workflow.
 */
export function submitBlockedReason(
  workflow: Workflow,
  capacity: WorkflowCapacity,
): string | null {
  if (!workflow.manifest) return '该 workflow 未注册 manifest'
  if (!workflow.manifest_consistent) return '多个 Worker 上报了不同 manifest'
  if (capacity.availability === 'offline') return '没有在线设备提供该 workflow'
  if (capacity.availability === 'busy') {
    return '设备的执行槽与队列均已满，Hub 暂不接受新作业'
  }
  return null
}
