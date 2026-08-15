import { useToast } from '@/state/toast'
import { cx } from './primitives'

const TONE_STYLES = {
  success: 'border-success/30 bg-success/10 text-success',
  error: 'border-danger/30 bg-danger/10 text-danger',
  info: 'border-info/30 bg-info/10 text-info',
  warning: 'border-warning/30 bg-warning/10 text-warning',
} as const

const TONE_ICONS = {
  success: 'M3 8.5 6.5 12 13 4.5',
  error: 'M8 4.5v5m0 2.5v.5',
  info: 'M8 7v5M8 4.5v.5',
  warning: 'M8 4.5 2 14h12L8 4.5ZM8 7v3M8 12v.5',
} as const

export function Toaster() {
  const { toasts, dismiss, pause, resume } = useToast()

  return (
    <div
      className="pointer-events-none fixed inset-x-0 bottom-0 z-[60] flex flex-col items-center gap-2 p-4 sm:inset-x-auto sm:right-0 sm:items-end"
      role="region"
      aria-label="通知"
      aria-live="polite"
    >
      {toasts.map((toast) => (
        <div
          key={toast.id}
          role={toast.tone === 'error' ? 'alert' : 'status'}
          onMouseEnter={() => pause(toast.id)}
          onMouseLeave={() => resume(toast.id)}
          className={cx(
            'pointer-events-auto flex w-full max-w-sm items-start gap-2.5 rounded-lg border px-3.5 py-3',
            'bg-surface shadow-[var(--shadow-card)] backdrop-blur',
            toast.leaving ? 'animate-fade-out-down' : 'animate-fade-in-up',
            TONE_STYLES[toast.tone],
          )}
        >
          <svg viewBox="0 0 16 16" className="mt-0.5 size-4 shrink-0" aria-hidden="true">
            <path
              d={TONE_ICONS[toast.tone]}
              fill="none"
              stroke="currentColor"
              strokeWidth="1.5"
              strokeLinecap="round"
              strokeLinejoin="round"
            />
          </svg>
          <div className="min-w-0 flex-1">
            <p className="text-xs leading-relaxed font-medium text-text">{toast.message}</p>
            {toast.detail && (
              <p className="mt-0.5 font-mono text-[10px] break-all text-muted">{toast.detail}</p>
            )}
          </div>
          <button
            type="button"
            onClick={() => dismiss(toast.id)}
            aria-label="关闭通知"
            className="-mr-1 -mt-0.5 rounded p-1 text-subtle transition hover:bg-surface-2 hover:text-text"
          >
            <svg viewBox="0 0 16 16" className="size-3" aria-hidden="true">
              <path
                d="m4 4 8 8m0-8-8 8"
                fill="none"
                stroke="currentColor"
                strokeWidth="1.75"
                strokeLinecap="round"
              />
            </svg>
          </button>
        </div>
      ))}
    </div>
  )
}
