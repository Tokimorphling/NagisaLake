import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState } from 'react'
import type { ReactNode } from 'react'
import { useNavigate } from 'react-router-dom'
import { useJobs } from '@/api/queries'
import type { JobState, JobSummary } from '@/api/types'
import { TERMINAL_JOB_STATES } from '@/api/types'
import {
  notifyPermission,
  playChime,
  requestNotifyPermission,
  showNotification,
} from '@/lib/notify'
import type { NotifyPermission } from '@/lib/notify'
import { useAuth } from './auth'
import { useToast } from './toast'

const DESKTOP_KEY = 'nagisalake.notify.desktop'
const SOUND_KEY = 'nagisalake.notify.sound'

interface NotificationContextValue {
  /** User's opt-in for system notifications, independent of browser permission. */
  desktopEnabled: boolean
  soundEnabled: boolean
  permission: NotifyPermission
  setSoundEnabled: (enabled: boolean) => void
  /** Turning this on prompts for browser permission when needed. */
  enableDesktop: () => Promise<NotifyPermission>
  disableDesktop: () => void
}

const NotificationContext = createContext<NotificationContextValue | null>(null)

function readFlag(key: string): boolean {
  try {
    return localStorage.getItem(key) === '1'
  } catch {
    return false
  }
}

function writeFlag(key: string, value: boolean): void {
  try {
    if (value) localStorage.setItem(key, '1')
    else localStorage.removeItem(key)
  } catch {
    // A blocked localStorage only costs persistence, not function.
  }
}

const STATE_LABELS: Partial<Record<JobState, string>> = {
  completed: '已完成',
  failed: '执行失败',
  cancelled: '已取消',
}

export function NotificationProvider({ children }: { children: ReactNode }) {
  const { organizationId } = useAuth()
  const navigate = useNavigate()
  const toast = useToast()
  const jobs = useJobs(organizationId)

  const [desktopEnabled, setDesktopEnabled] = useState(() => readFlag(DESKTOP_KEY))
  const [soundEnabled, setSoundEnabledState] = useState(() => readFlag(SOUND_KEY))
  const [permission, setPermission] = useState<NotifyPermission>(notifyPermission)

  /**
   * Last known state per job id. Alerts fire on an active → terminal transition,
   * so a job already finished when the page loaded never notifies.
   */
  const knownStates = useRef(new Map<string, JobState>())
  const seeded = useRef(false)
  const notified = useRef(new Set<string>())

  // Switching organizations swaps the whole job list; reseed rather than
  // announcing every terminal job in the new org.
  useEffect(() => {
    knownStates.current.clear()
    notified.current.clear()
    seeded.current = false
  }, [organizationId])

  const setSoundEnabled = useCallback((enabled: boolean) => {
    setSoundEnabledState(enabled)
    writeFlag(SOUND_KEY, enabled)
    // Play immediately on enable: it doubles as a preview and satisfies the
    // autoplay gesture requirement while the click is still fresh.
    if (enabled) playChime('success')
  }, [])

  const enableDesktop = useCallback(async () => {
    const current = notifyPermission()
    if (current === 'unsupported') {
      setPermission('unsupported')
      return 'unsupported' as NotifyPermission
    }
    const result = current === 'granted' ? current : await requestNotifyPermission()
    setPermission(result)
    const granted = result === 'granted'
    setDesktopEnabled(granted)
    writeFlag(DESKTOP_KEY, granted)
    return result
  }, [])

  const disableDesktop = useCallback(() => {
    setDesktopEnabled(false)
    writeFlag(DESKTOP_KEY, false)
  }, [])

  const firstPage: JobSummary[] = useMemo(
    () => jobs.data?.pages[0]?.items ?? [],
    [jobs.data],
  )

  useEffect(() => {
    if (firstPage.length === 0 && !seeded.current) return

    if (!seeded.current) {
      for (const job of firstPage) knownStates.current.set(job.id, job.state)
      seeded.current = true
      return
    }

    for (const job of firstPage) {
      const previous = knownStates.current.get(job.id)
      knownStates.current.set(job.id, job.state)

      const becameTerminal =
        previous !== undefined &&
        previous !== job.state &&
        !TERMINAL_JOB_STATES.includes(previous) &&
        TERMINAL_JOB_STATES.includes(job.state)

      if (!becameTerminal || notified.current.has(job.id)) continue
      notified.current.add(job.id)

      const label = STATE_LABELS[job.state] ?? job.state
      const title = `作业${label}`
      const body = `${job.workflow_id} · ${job.id.slice(0, 12)}…`
      const failed = job.state === 'failed'

      // Only alert out-of-band when the tab is not being watched. A visible tab
      // already shows the state change in the list.
      const hidden = document.visibilityState === 'hidden'

      if (hidden && desktopEnabled) {
        showNotification(title, {
          body,
          tag: `job-${job.id}`,
          onClick: () => navigate(`/jobs/${job.id}`),
        })
      }
      if (hidden && soundEnabled) {
        playChime(failed ? 'failure' : 'success')
      }
      if (!hidden) {
        if (failed) toast.error(title, body)
        else toast.success(title, body)
        if (soundEnabled) playChime(failed ? 'failure' : 'success')
      }
    }
  }, [desktopEnabled, firstPage, navigate, soundEnabled, toast])

  // A permission revoked in browser settings should not leave the toggle on.
  useEffect(() => {
    const sync = () => {
      const current = notifyPermission()
      setPermission(current)
      if (current !== 'granted' && desktopEnabled) {
        setDesktopEnabled(false)
        writeFlag(DESKTOP_KEY, false)
      }
    }
    document.addEventListener('visibilitychange', sync)
    return () => document.removeEventListener('visibilitychange', sync)
  }, [desktopEnabled])

  const value = useMemo<NotificationContextValue>(
    () => ({
      desktopEnabled,
      soundEnabled,
      permission,
      setSoundEnabled,
      enableDesktop,
      disableDesktop,
    }),
    [desktopEnabled, disableDesktop, enableDesktop, permission, setSoundEnabled, soundEnabled],
  )

  return <NotificationContext.Provider value={value}>{children}</NotificationContext.Provider>
}

export function useNotifications(): NotificationContextValue {
  const value = useContext(NotificationContext)
  if (!value) throw new Error('useNotifications must be used inside NotificationProvider')
  return value
}
