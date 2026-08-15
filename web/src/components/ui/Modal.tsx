import { useEffect, useRef } from 'react'
import type { ReactNode } from 'react'
import { Button, cx } from './primitives'

function useFocusTrap(enabled: boolean, containerRef: React.RefObject<HTMLElement | null>) {
  const previousActiveElement = useRef<HTMLElement | null>(null)

  useEffect(() => {
    if (!enabled) return

    // Store the previously focused element
    previousActiveElement.current = document.activeElement as HTMLElement

    // Find all focusable elements in the container
    const container = containerRef.current
    if (!container) return

    const focusableSelector =
      'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])'
    
    const getFocusableElements = () => 
      Array.from(container.querySelectorAll<HTMLElement>(focusableSelector))

    // Focus the first focusable element
    const focusableElements = getFocusableElements()
    if (focusableElements.length > 0) {
      focusableElements[0].focus()
    } else {
      container.focus()
    }

    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key !== 'Tab') return

      const focusableElements = getFocusableElements()
      if (focusableElements.length === 0) return

      const firstElement = focusableElements[0]
      const lastElement = focusableElements[focusableElements.length - 1]

      if (e.shiftKey) {
        // Shift+Tab: if we're on first element, wrap to last
        if (document.activeElement === firstElement) {
          e.preventDefault()
          lastElement.focus()
        }
      } else {
        // Tab: if we're on last element, wrap to first
        if (document.activeElement === lastElement) {
          e.preventDefault()
          firstElement.focus()
        }
      }
    }

    container.addEventListener('keydown', handleKeyDown)

    return () => {
      container.removeEventListener('keydown', handleKeyDown)
      // Restore focus to the previously focused element
      if (previousActiveElement.current && document.body.contains(previousActiveElement.current)) {
        previousActiveElement.current.focus()
      }
    }
  }, [enabled, containerRef])
}

export function Modal({
  open,
  title,
  description,
  onClose,
  children,
  footer,
  width = 'md',
}: {
  open: boolean
  title: ReactNode
  description?: ReactNode
  onClose: () => void
  children: ReactNode
  footer?: ReactNode
  width?: 'sm' | 'md' | 'lg' | 'xl' | 'full'
}) {
  const dialogRef = useRef<HTMLDivElement>(null)
  const titleId = useRef(`modal-title-${Math.random().toString(36).slice(2, 9)}`)
  const descId = useRef(`modal-desc-${Math.random().toString(36).slice(2, 9)}`)

  useFocusTrap(open, dialogRef)

  useEffect(() => {
    if (!open) return
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        event.stopPropagation()
        onClose()
      }
    }
    document.addEventListener('keydown', onKeyDown)
    const previousOverflow = document.body.style.overflow
    const previousDocumentOverflow = document.documentElement.style.overflow
    document.body.style.overflow = 'hidden'
    document.documentElement.style.overflow = 'hidden' // iOS Safari fix
    return () => {
      document.removeEventListener('keydown', onKeyDown)
      document.body.style.overflow = previousOverflow
      document.documentElement.style.overflow = previousDocumentOverflow
    }
  }, [onClose, open])

  if (!open) return null

  const widthClasses = {
    sm: 'sm:max-w-md',
    md: 'sm:max-w-lg',
    lg: 'sm:max-w-2xl',
    xl: 'sm:max-w-4xl',
    full: 'sm:max-w-[calc(100vw-2rem)]',
  }

  return (
    <div className="fixed inset-0 z-50 flex items-end justify-center p-0 sm:items-center sm:p-4">
      <div
        className="absolute inset-0 bg-black/55 backdrop-blur-sm animate-fade-in"
        onClick={onClose}
        aria-hidden="true"
      />
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby={typeof title === 'string' ? titleId.current : undefined}
        aria-describedby={description ? descId.current : undefined}
        tabIndex={-1}
        className={cx(
          'relative flex max-h-[92dvh] w-full flex-col overflow-hidden',
          'rounded-t-2xl sm:rounded-2xl border border-border',
          'bg-surface shadow-2xl outline-none',
          'animate-slide-up sm:animate-scale-in',
          widthClasses[width],
        )}
      >
        <div className="flex items-start justify-between gap-4 border-b border-border px-5 py-4">
          <div className="min-w-0">
            <h2 id={titleId.current} className="text-sm font-semibold tracking-tight">
              {title}
            </h2>
            {description && (
              <p id={descId.current} className="mt-1 text-xs leading-relaxed text-muted">
                {description}
              </p>
            )}
          </div>
          <Button variant="ghost" size="sm" onClick={onClose} aria-label="关闭" className="-mr-1 -mt-1">
            <svg viewBox="0 0 16 16" className="size-4" aria-hidden="true">
              <path
                d="m4 4 8 8m0-8-8 8"
                fill="none"
                stroke="currentColor"
                strokeWidth="1.5"
                strokeLinecap="round"
              />
            </svg>
          </Button>
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto px-5 py-4">{children}</div>
        {footer && (
          <div className="flex flex-wrap justify-end gap-2 border-t border-border bg-surface-2/50 px-5 py-3">
            {footer}
          </div>
        )}
      </div>
    </div>
  )
}

export function ConfirmModal({
  open,
  title,
  description,
  confirmLabel = '确认',
  destructive = false,
  loading = false,
  onConfirm,
  onClose,
}: {
  open: boolean
  title: string
  description: ReactNode
  confirmLabel?: string
  destructive?: boolean
  loading?: boolean
  onConfirm: () => void
  onClose: () => void
}) {
  return (
    <Modal
      open={open}
      title={title}
      description={typeof description === 'string' ? description : undefined}
      onClose={onClose}
      width="sm"
      footer={
        <>
          <Button size="sm" onClick={onClose} disabled={loading}>
            取消
          </Button>
          <Button
            size="sm"
            variant={destructive ? 'danger' : 'primary'}
            loading={loading}
            onClick={onConfirm}
          >
            {confirmLabel}
          </Button>
        </>
      }
    >
      {typeof description === 'string' ? (
        <p className="text-sm leading-relaxed text-muted">{description}</p>
      ) : (
        description
      )}
    </Modal>
  )
}
