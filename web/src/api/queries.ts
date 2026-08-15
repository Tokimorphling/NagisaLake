import { useEffect, useState } from 'react'
import {
  useInfiniteQuery,
  useMutation,
  useQuery,
  useQueryClient,
} from '@tanstack/react-query'
import type { InfiniteData } from '@tanstack/react-query'
import { endpoints } from './endpoints'
import { consumeSse } from './sse'
import { TERMINAL_JOB_STATES } from './types'
import type {
  GalleryItem,
  GalleryItemsPage,
  Job,
  JobBatch,
  JobEventStreamPayload,
  JobSummary,
  JobsPage,
  Role,
} from './types'

/** Query keys are org-scoped so switching organizations cannot leak cache. */
export const keys = {
  publicSettings: ['public-settings'] as const,
  workflows: (org: string | null) => ['workflows', org] as const,
  devices: (org: string | null) => ['devices', org] as const,
  jobs: (org: string | null) => ['jobs', org] as const,
  job: (org: string | null, id: string) => ['job', org, id] as const,
  batches: (org: string | null) => ['batches', org] as const,
  batch: (org: string | null, id: string) => ['batch', org, id] as const,
  batchJobs: (org: string | null, id: string) => ['batch-jobs', org, id] as const,
  apiKeys: (org: string | null) => ['api-keys', org] as const,
  workerCredentials: (org: string | null) => ['worker-credentials', org] as const,
  members: (org: string | null) => ['members', org] as const,
  organizationInvites: (org: string | null) => ['organization-invites', org] as const,
  quota: (org: string | null) => ['quota', org] as const,
  auditLogs: (org: string | null) => ['audit-logs', org] as const,
  artifactDownload: (artifactId: string) => ['artifact-download', artifactId] as const,
  gallery: ['gallery'] as const,
  galleryDownload: (itemId: string) => ['gallery-download', itemId] as const,
}

export function galleryPagesToItems(data: InfiniteData<GalleryItemsPage>): GalleryItem[] {
  return data.pages.flatMap((page) => page.items)
}

export function useGalleryItems() {
  return useInfiniteQuery({
    queryKey: keys.gallery,
    queryFn: ({ pageParam }) =>
      endpoints.galleryItems({ limit: 24, cursor: pageParam ?? undefined }),
    initialPageParam: null as string | null,
    getNextPageParam: (lastPage) => lastPage.next_cursor,
    select: galleryPagesToItems,
    staleTime: 30_000,
  })
}

export function useGalleryDownload(itemId: string, enabled = true) {
  return useQuery({
    queryKey: keys.galleryDownload(itemId),
    queryFn: () => endpoints.galleryItemDownload(itemId),
    enabled: enabled && itemId !== '',
    staleTime: 60_000,
  })
}

export function useUnpublishGalleryItem() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: endpoints.unpublishGalleryItem,
    onSuccess: async (_value, itemId) => {
      queryClient.removeQueries({ queryKey: keys.galleryDownload(itemId) })
      await queryClient.invalidateQueries({ queryKey: keys.gallery })
    },
  })
}

export function usePublicSettings() {
  return useQuery({
    queryKey: keys.publicSettings,
    queryFn: endpoints.publicSettings,
    staleTime: 5 * 60_000,
    retry: false,
  })
}

export function useWorkflows(org: string | null) {
  return useInfiniteQuery({
    queryKey: keys.workflows(org),
    queryFn: ({ pageParam }) =>
      endpoints.workflows({ limit: 50, cursor: pageParam ?? undefined }),
    initialPageParam: null as string | null,
    getNextPageParam: (lastPage) => lastPage.next_cursor,
    select: (data) => data.pages.flatMap((page) => page.items),
    enabled: org !== null,
    staleTime: 30_000,
    refetchInterval: 5_000,
  })
}

export function useDevices(org: string | null, enabled = true) {
  return useInfiniteQuery({
    queryKey: keys.devices(org),
    queryFn: ({ pageParam }) =>
      endpoints.devices({ limit: 50, cursor: pageParam ?? undefined }),
    initialPageParam: null as string | null,
    getNextPageParam: (lastPage) => lastPage.next_cursor,
    select: (data) => data.pages.flatMap((page) => page.items),
    enabled: org !== null && enabled,
    staleTime: 15_000,
    refetchInterval: 20_000,
  })
}

const JOBS_PAGE_SIZE = 50

/**
 * Paginated job list. The Hub returns one bounded page plus a cursor; the
 * unbounded version reached 120 MiB at 100k jobs.
 *
 * A poll refetches every page currently loaded, not just the newest one, so the
 * cost scales with how far the user has scrolled. `maxPages` caps that: older
 * pages are dropped as new ones load, which keeps a long scroll from turning
 * into a dozen requests every 3 seconds. Activity of interest lands on the first
 * page anyway, since the list is newest-first.
 */
export function useJobs(org: string | null) {
  const hasActive = (jobs: JobSummary[] | undefined) =>
    jobs?.some((job) => !TERMINAL_JOB_STATES.includes(job.state)) ?? false

  return useInfiniteQuery({
    queryKey: keys.jobs(org),
    queryFn: ({ pageParam }) =>
      endpoints.jobs({ limit: JOBS_PAGE_SIZE, cursor: pageParam ?? undefined }),
    initialPageParam: null as string | null,
    getNextPageParam: (lastPage) => lastPage.next_cursor,
    enabled: org !== null,
    maxPages: 5,
    // No SSE yet: poll while anything is still in flight, then back off.
    refetchInterval: (query) =>
      hasActive(query.state.data?.pages[0]?.items) ? 3_000 : 20_000,
  })
}

export function useJob(org: string | null, id: string) {
  return useQuery({
    queryKey: keys.job(org, id),
    queryFn: () => endpoints.job(id),
    enabled: org !== null && id !== '',
  })
}

export type JobEventStreamStatus = 'connecting' | 'live' | 'reconnecting' | 'closed'

/** Keeps a job detail and any already-loaded list rows current over SSE. */
export function useJobEvents(
  org: string | null,
  id: string,
  enabled = true,
): JobEventStreamStatus {
  const queryClient = useQueryClient()
  const [status, setStatus] = useState<JobEventStreamStatus>('closed')

  useEffect(() => {
    if (!enabled || org === null || id === '') {
      setStatus('closed')
      return
    }

    let stopped = false
    let retry = 0
    let reconnectTimer: ReturnType<typeof setTimeout> | null = null
    let controller: AbortController | null = null
    let lastSequence =
      queryClient.getQueryData<Job>(keys.job(org, id))?.events.at(-1)?.sequence ?? 0

    const applyPayload = (payload: JobEventStreamPayload) => {
      if (payload.job_id !== id) return
      if (payload.event && payload.event.sequence <= lastSequence) return
      if (payload.event) lastSequence = payload.event.sequence

      queryClient.setQueryData<Job>(keys.job(org, id), (previous) => {
        if (!previous) return previous
        const events = payload.event
          ? [
              ...previous.events.filter((event) => event.sequence !== payload.event?.sequence),
              payload.event,
            ].sort((left, right) => left.sequence - right.sequence)
          : previous.events
        return {
          ...previous,
          state: payload.state,
          progress: payload.progress,
          error: payload.error,
          events,
          updated_at_unix_ms: payload.event?.unix_ms ?? previous.updated_at_unix_ms,
        }
      })

      queryClient.setQueriesData<InfiniteData<JobsPage>>(
        { queryKey: keys.jobs(org) },
        (previous) => {
          if (!previous) return previous
          let changed = false
          const pages = previous.pages.map((page) => ({
            ...page,
            items: page.items.map((item) => {
              if (item.id !== id) return item
              changed = true
              return {
                ...item,
                state: payload.state,
                progress: payload.progress,
                error: payload.error,
                updated_at_unix_ms: payload.event?.unix_ms ?? item.updated_at_unix_ms,
              }
            }),
          }))
          return changed ? { ...previous, pages } : previous
        },
      )
    }

    const scheduleReconnect = () => {
      if (stopped) return
      retry = Math.min(retry + 1, 6)
      setStatus('reconnecting')
      const delay = Math.min(1_000 * 2 ** (retry - 1), 10_000)
      reconnectTimer = setTimeout(() => void connect(), delay)
    }

    const connect = async () => {
      if (stopped) return
      controller = new AbortController()
      setStatus(retry === 0 ? 'connecting' : 'reconnecting')
      try {
        const response = await endpoints.jobEvents(id, lastSequence, controller.signal)
        retry = 0
        setStatus('live')
        await consumeSse(response, (eventName, data) => {
          if (eventName === 'error') throw new Error(data || 'job event stream failed')
          if (eventName !== 'job') return
          applyPayload(JSON.parse(data) as JobEventStreamPayload)
        })
        if (stopped) return
        const current = queryClient.getQueryData<Job>(keys.job(org, id))
        if (current && TERMINAL_JOB_STATES.includes(current.state)) setStatus('closed')
        else scheduleReconnect()
      } catch (error) {
        if (stopped || (error as Error)?.name === 'AbortError') return
        scheduleReconnect()
      }
    }

    void connect()
    return () => {
      stopped = true
      controller?.abort()
      if (reconnectTimer) clearTimeout(reconnectTimer)
    }
  }, [enabled, id, org, queryClient])

  return status
}

export function useArtifactDownload(artifactId: string, enabled = true) {
  return useQuery({
    queryKey: keys.artifactDownload(artifactId),
    queryFn: () => endpoints.download(artifactId),
    enabled: enabled && artifactId !== '',
    staleTime: 10 * 60_000,
  })
}

export function useApiKeys(org: string | null) {
  return useInfiniteQuery({
    queryKey: keys.apiKeys(org),
    queryFn: ({ pageParam }) =>
      endpoints.apiKeys(org as string, { limit: 50, cursor: pageParam ?? undefined }),
    initialPageParam: null as string | null,
    getNextPageParam: (lastPage) => lastPage.next_cursor,
    select: (data) => data.pages.flatMap((page) => page.items),
    enabled: org !== null,
  })
}

export function useWorkerCredentials(org: string | null) {
  return useInfiniteQuery({
    queryKey: keys.workerCredentials(org),
    queryFn: ({ pageParam }) =>
      endpoints.workerCredentials(org as string, {
        limit: 50,
        cursor: pageParam ?? undefined,
      }),
    initialPageParam: null as string | null,
    getNextPageParam: (lastPage) => lastPage.next_cursor,
    select: (data) => data.pages.flatMap((page) => page.items),
    enabled: org !== null,
  })
}

export function useMembers(org: string | null, enabled = true) {
  return useQuery({
    queryKey: keys.members(org),
    queryFn: () => endpoints.members(org as string),
    enabled: org !== null && enabled,
  })
}

export function useOrganizationInvites(org: string | null, enabled = true) {
  return useQuery({
    queryKey: keys.organizationInvites(org),
    queryFn: () => endpoints.organizationInvites(org as string),
    enabled: org !== null && enabled,
  })
}

export function useQuota(org: string | null) {
  return useQuery({
    queryKey: keys.quota(org),
    queryFn: () => endpoints.quota(org as string),
    enabled: org !== null,
    refetchInterval: 30_000,
  })
}

export function useUpdateQuota(org: string | null) {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (body: {
      max_concurrent_jobs: number
      max_storage_bytes: number
      max_jobs_per_period: number
      period_seconds: number
    }) => endpoints.updateQuota(org as string, body),
    onSuccess: (quota) => {
      queryClient.setQueryData(keys.quota(org), quota)
    },
  })
}

export function useAuditLogs(org: string | null, enabled = true) {
  return useInfiniteQuery({
    queryKey: keys.auditLogs(org),
    queryFn: ({ pageParam }) =>
      endpoints.auditLogs(org as string, { limit: 50, cursor: pageParam ?? undefined }),
    initialPageParam: null as string | null,
    getNextPageParam: (lastPage) => lastPage.next_cursor,
    select: (data) => data.pages.flatMap((page) => page.items),
    enabled: org !== null && enabled,
  })
}

export function useCancelJob(org: string | null) {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (id: string) => endpoints.cancelJob(id),
    onSuccess: (job) => {
      queryClient.setQueryData(keys.job(org, job.id), job)
      void queryClient.invalidateQueries({ queryKey: keys.jobs(org) })
      void queryClient.invalidateQueries({ queryKey: keys.workflows(org) })
      void queryClient.invalidateQueries({ queryKey: keys.quota(org) })
    },
  })
}

const BATCHES_PAGE_SIZE = 50

/** Paginated job batch list. Polls while any batch is still in flight. */
export function useBatches(org: string | null) {
  const hasActive = (batches: JobBatch[] | undefined) =>
    batches?.some((batch) => !isBatchTerminal(batch)) ?? false

  return useInfiniteQuery({
    queryKey: keys.batches(org),
    queryFn: ({ pageParam }) =>
      endpoints.batches({ limit: BATCHES_PAGE_SIZE, cursor: pageParam ?? undefined }),
    initialPageParam: null as string | null,
    getNextPageParam: (lastPage) => lastPage.next_cursor,
    enabled: org !== null,
    maxPages: 5,
    refetchInterval: (query) =>
      hasActive(query.state.data?.pages[0]?.items) ? 3_000 : 20_000,
  })
}

export function useBatch(org: string | null, id: string) {
  return useQuery({
    queryKey: keys.batch(org, id),
    queryFn: () => endpoints.batch(id),
    enabled: org !== null && id !== '',
    refetchInterval: (query) =>
      query.state.data && !isBatchTerminal(query.state.data) ? 3_000 : false,
  })
}

export function useBatchJobs(org: string | null, id: string) {
  return useInfiniteQuery({
    queryKey: keys.batchJobs(org, id),
    queryFn: ({ pageParam }) =>
      endpoints.batchJobs(id, { limit: BATCHES_PAGE_SIZE, cursor: pageParam ?? undefined }),
    initialPageParam: null as string | null,
    getNextPageParam: (lastPage) => lastPage.next_cursor,
    enabled: org !== null && id !== '',
    maxPages: 5,
    refetchInterval: 5_000,
  })
}

export function useCancelBatch(org: string | null) {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (id: string) => endpoints.cancelBatch(id),
    onSuccess: (batch) => {
      queryClient.setQueryData(keys.batch(org, batch.id), batch)
      void queryClient.invalidateQueries({ queryKey: keys.batches(org) })
      void queryClient.invalidateQueries({ queryKey: keys.batchJobs(org, batch.id) })
      void queryClient.invalidateQueries({ queryKey: keys.jobs(org) })
      void queryClient.invalidateQueries({ queryKey: keys.quota(org) })
    },
  })
}

/** A batch is terminal once every job has reached a terminal state. */
export function isBatchTerminal(batch: JobBatch): boolean {
  const { counts, total } = batch
  const settled = counts.completed + counts.failed + counts.cancelled
  return settled >= total
}

export function useChangeMemberRole(org: string | null) {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({ userId, role }: { userId: string; role: Role }) =>
      endpoints.changeMemberRole(org as string, userId, role),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: keys.members(org) })
    },
  })
}

export function useRemoveMember(org: string | null) {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (userId: string) => endpoints.removeMember(org as string, userId),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: keys.members(org) })
    },
  })
}

export function useCreateOrganizationInvite(org: string | null) {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (body: { role: Role; expires_in_seconds?: number }) =>
      endpoints.createOrganizationInvite(org as string, body),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: keys.organizationInvites(org) })
    },
  })
}

export function useRevokeOrganizationInvite(org: string | null) {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (inviteId: string) => endpoints.revokeOrganizationInvite(org as string, inviteId),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: keys.organizationInvites(org) })
    },
  })
}

export function useAcceptOrganizationInvite() {
  return useMutation({ mutationFn: (code: string) => endpoints.acceptOrganizationInvite(code) })
}

export function useTransferOrganizationOwner(org: string | null) {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (userId: string) => endpoints.transferOrganizationOwner(org as string, userId),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: keys.members(org) })
    },
  })
}
