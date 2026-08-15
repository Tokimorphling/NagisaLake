import { describe, expect, it } from 'vitest'
import type { JobSummary, Workflow, WorkflowWorker } from '@/api/types'
import { averageQueueAgeMs, fleetMetrics, todayUsage } from './insights'

const worker: WorkflowWorker = {
  organization_id: 'org',
  worker_id: 'gpu-1',
  parallelism: 2,
  queue_depth: 3,
  active_jobs: 1,
  queued_jobs: 2,
  available: true,
}

const workflow = (id: string): Workflow => ({
  id,
  version: '1',
  output_types: ['image'],
  manifest: null,
  manifest_consistent: true,
  workers: [worker],
  available: true,
})

const job = (overrides: Partial<JobSummary>): JobSummary => ({
  id: 'job',
  workflow_id: 'wf',
  workflow_version: '1',
  parameters: {},
  input_artifact_ids: [],
  output_artifact_ids: [],
  worker_id: 'gpu-1',
  session_id: 'session',
  state: 'completed',
  progress: 1,
  prompt_id: null,
  error: null,
  created_at_unix_ms: 0,
  updated_at_unix_ms: 0,
  ...overrides,
})

describe('insight metrics', () => {
  it('deduplicates one worker repeated by multiple workflows', () => {
    expect(fleetMetrics([workflow('a'), workflow('b')])).toEqual({
      devices: 1,
      active: 1,
      parallelism: 2,
      queued: 2,
      queueDepth: 3,
    })
  })

  it('measures waiting age only for queued states', () => {
    const now = 20_000
    expect(
      averageQueueAgeMs(
        [
          job({ id: 'a', state: 'received', created_at_unix_ms: 10_000 }),
          job({ id: 'b', state: 'running', created_at_unix_ms: 0 }),
        ],
        now,
      ),
    ).toBe(10_000)
  })

  it('splits today wall-clock usage by outcome', () => {
    const now = new Date('2026-08-09T12:00:00').getTime()
    const result = todayUsage(
      [
        job({
          id: 'done',
          created_at_unix_ms: now - 10_000,
          updated_at_unix_ms: now - 5_000,
        }),
        job({ id: 'live', state: 'running', created_at_unix_ms: now - 2_000 }),
      ],
      now,
    )
    expect(result.completedMs).toBe(5_000)
    expect(result.activeMs).toBe(2_000)
    expect(result.totalMs).toBe(7_000)
  })
})
