import { createContext, useCallback, useContext, useMemo, useRef, useState } from 'react'
import type { ReactNode } from 'react'
import { ApiError } from '@/api/client'

type ToastTone = 'success' | 'error' | 'info' | 'warning'

interface Toast {
  id: number
  tone: ToastTone
  message: string
  detail?: string
  /** True while the exit animation is running, before removal. */
  leaving?: boolean
}

interface ToastContextValue {
  toasts: Toast[]
  dismiss: (id: number) => void
  pause: (id: number) => void
  resume: (id: number) => void
  success: (message: string, detail?: string) => void
  info: (message: string, detail?: string) => void
  error: (message: string, detail?: string) => void
  warning: (message: string, detail?: string) => void
  /** Renders an ApiError with its code and request id for support handoff. */
  fromError: (error: unknown, fallback?: string) => void
}

const ToastContext = createContext<ToastContextValue | null>(null)

const TIMEOUT_FOR_TONE: Record<ToastTone, number> = {
  success: 4000,
  info: 4000,
  warning: 6000,
  error: 8000,
}

const EXIT_ANIMATION_MS = 200

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<Toast[]>([])
  const nextId = useRef(1)
  const timers = useRef(new Map<number, number>())

  const clearTimer = useCallback((id: number) => {
    const timer = timers.current.get(id)
    if (timer !== undefined) {
      window.clearTimeout(timer)
      timers.current.delete(id)
    }
  }, [])

  const remove = useCallback((id: number) => {
    setToasts((current) => current.filter((toast) => toast.id !== id))
  }, [])

  const dismiss = useCallback(
    (id: number) => {
      clearTimer(id)
      // Start the exit animation, then actually drop the toast once it finishes.
      setToasts((current) =>
        current.map((toast) => (toast.id === id ? { ...toast, leaving: true } : toast)),
      )
      const timer = window.setTimeout(() => remove(id), EXIT_ANIMATION_MS)
      timers.current.set(id, timer)
    },
    [clearTimer, remove],
  )

  const schedule = useCallback(
    (id: number, tone: ToastTone) => {
      const timer = window.setTimeout(() => dismiss(id), TIMEOUT_FOR_TONE[tone])
      timers.current.set(id, timer)
    },
    [dismiss],
  )

  // Hover-to-pause: clear the active timer. resume() restarts it.
  const pause = useCallback((id: number) => clearTimer(id), [clearTimer])

  const resume = useCallback(
    (id: number) => {
      const toast = toasts.find((candidate) => candidate.id === id)
      if (!toast || toast.leaving) return
      schedule(id, toast.tone)
    },
    [schedule, toasts],
  )

  const push = useCallback(
    (tone: ToastTone, message: string, detail?: string) => {
      const id = nextId.current++
      setToasts((current) => [...current.slice(-3), { id, tone, message, detail }])
      schedule(id, tone)
    },
    [schedule],
  )

  const value = useMemo<ToastContextValue>(
    () => ({
      toasts,
      dismiss,
      pause,
      resume,
      success: (message, detail) => push('success', message, detail),
      info: (message, detail) => push('info', message, detail),
      error: (message, detail) => push('error', message, detail),
      warning: (message, detail) => push('warning', message, detail),
      fromError: (error, fallback = '操作失败') => {
        if (error instanceof ApiError) {
          const detail = [error.code, error.requestId && `request ${error.requestId}`]
            .filter(Boolean)
            .join(' · ')
          push('error', error.message || fallback, detail)
        } else {
          push('error', fallback, error instanceof Error ? error.message : undefined)
        }
      },
    }),
    [dismiss, pause, push, resume, toasts],
  )

  return <ToastContext.Provider value={value}>{children}</ToastContext.Provider>
}

export function useToast(): ToastContextValue {
  const value = useContext(ToastContext)
  if (!value) throw new Error('useToast must be used inside ToastProvider')
  return value
}
