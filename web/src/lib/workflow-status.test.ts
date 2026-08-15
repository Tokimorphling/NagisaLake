import { describe, expect, it } from 'vitest'
import type { Workflow, WorkflowWorker } from '@/api/types'
import {
  availabilityLabel,
  capacitySummary,
  submitBlockedReason,
  workflowCapacity,
} from './workflow-status'

function worker(overrides: Partial<WorkflowWorker> = {}): WorkflowWorker {
  return {
    organization_id: 'org',
    worker_id: 'lan/device',
    parallelism: 1,
    queue_depth: 0,
    active_jobs: 0,
    queued_jobs: 0,
    available: true,
    ...overrides,
  }
}

function workflow(overrides: Partial<Workflow> = {}): Workflow {
  return {
    id: 'seedvr2-3b-int8-upscale-image',
    version: 'v2',
    output_types: ['image/png'],
    manifest: {
      schema_version: 1,
      display_name: 'upscale',
      description: null,
      inputs: [],
      outputs: [],
      warnings: [],
    },
    manifest_consistent: true,
    workers: [],
    available: false,
    ...overrides,
  }
}

describe('workflowCapacity', () => {
  it('keeps queue_depth zero compatible with immediate-only admission', () => {
    const capacity = workflowCapacity(
      workflow({ available: true, workers: [worker({ parallelism: 2, active_jobs: 1 })] }),
    )
    expect(capacity.availability).toBe('available')
    expect(capacity.parallelism).toBe(2)
    expect(capacity.queueDepth).toBe(0)
  })

  // The bug this module exists for: a device running a job used to render as
  // 离线, indistinguishable from a machine that was switched off.
  it('separates a busy device from an absent one', () => {
    const busy = workflowCapacity(
      workflow({ available: false, workers: [worker({ active_jobs: 1, available: false })] }),
    )
    expect(busy.availability).toBe('busy')
    expect(busy.devices).toBe(1)
    expect(busy.active).toBe(1)

    const offline = workflowCapacity(workflow({ available: false, workers: [] }))
    expect(offline.availability).toBe('offline')
    expect(offline.devices).toBe(0)
    expect(availabilityLabel(offline.availability)).toBe('离线')
    expect(availabilityLabel(busy.availability)).toBe('忙碌')
  })

  it('reports queueing when execution is full but the worker queue has room', () => {
    const capacity = workflowCapacity(
      workflow({
        available: true,
        workers: [worker({ queue_depth: 2, active_jobs: 1, queued_jobs: 1 })],
      }),
    )
    expect(capacity.queued).toBe(1)
    expect(capacity.queueDepth).toBe(2)
    expect(capacity.availability).toBe('queueing')
    expect(availabilityLabel(capacity.availability)).toBe('可排队')
  })

  it('reports busy when execution and queue capacity are both full', () => {
    const capacity = workflowCapacity(
      workflow({
        available: false,
        workers: [
          worker({ queue_depth: 2, active_jobs: 1, queued_jobs: 2, available: false }),
        ],
      }),
    )
    expect(capacity.availability).toBe('busy')
  })

  it('sums across several devices', () => {
    const capacity = workflowCapacity(
      workflow({
        available: true,
        workers: [
          worker({ worker_id: 'a', parallelism: 2, active_jobs: 2, available: false }),
          worker({
            worker_id: 'b',
            parallelism: 4,
            queue_depth: 3,
            active_jobs: 1,
            queued_jobs: 1,
          }),
        ],
      }),
    )
    expect(capacity.devices).toBe(2)
    expect(capacity.active).toBe(3)
    expect(capacity.queued).toBe(1)
    expect(capacity.parallelism).toBe(6)
    expect(capacity.queueDepth).toBe(3)
    expect(capacity.availability).toBe('available')
  })
})

describe('capacitySummary', () => {
  it('says nothing is online when no device offers the workflow', () => {
    expect(capacitySummary(workflowCapacity(workflow({ workers: [] })))).toBe('无在线设备')
  })

  it('shows execution and queue occupancy separately', () => {
    const summary = capacitySummary(
      workflowCapacity(
        workflow({
          available: false,
          workers: [
            worker({
              parallelism: 2,
              queue_depth: 3,
              active_jobs: 2,
              queued_jobs: 3,
              available: false,
            }),
          ],
        }),
      ),
    )
    expect(summary).toContain('1 台设备在线')
    expect(summary).toContain('执行中 2/2')
    expect(summary).toContain('排队 3/3')
  })

  it('omits queue occupancy when queue_depth is zero', () => {
    const summary = capacitySummary(
      workflowCapacity(
        workflow({ available: true, workers: [worker({ parallelism: 4, active_jobs: 1 })] }),
      ),
    )
    expect(summary).not.toContain('排队')
    expect(summary).toContain('执行中 1/4')
  })
})

describe('submitBlockedReason', () => {
  it('allows submission when a permit is free', () => {
    const target = workflow({ available: true, workers: [worker()] })
    expect(submitBlockedReason(target, workflowCapacity(target))).toBeNull()
  })

  it('allows submission into a worker-side queue', () => {
    const target = workflow({
      available: true,
      workers: [worker({ queue_depth: 2, active_jobs: 1 })],
    })
    expect(workflowCapacity(target).availability).toBe('queueing')
    expect(submitBlockedReason(target, workflowCapacity(target))).toBeNull()
  })

  it('blocks only when execution and queue capacity are both full', () => {
    const target = workflow({
      available: false,
      workers: [
        worker({ queue_depth: 2, active_jobs: 1, queued_jobs: 2, available: false }),
      ],
    })
    const reason = submitBlockedReason(target, workflowCapacity(target))
    expect(reason).toContain('执行槽与队列均已满')
    expect(reason).not.toContain('离线')
  })

  it('explains an offline workflow', () => {
    const target = workflow({ available: false, workers: [] })
    expect(submitBlockedReason(target, workflowCapacity(target))).toContain('没有在线设备')
  })

  // Contract problems outrank capacity: a drifted manifest is not submittable
  // even when a device is idle.
  it('reports a missing or inconsistent manifest first', () => {
    const noManifest = workflow({ available: true, workers: [worker()], manifest: null })
    expect(submitBlockedReason(noManifest, workflowCapacity(noManifest))).toContain('manifest')

    const drifted = workflow({
      available: true,
      workers: [worker()],
      manifest_consistent: false,
    })
    expect(submitBlockedReason(drifted, workflowCapacity(drifted))).toContain('不同 manifest')
  })
})
