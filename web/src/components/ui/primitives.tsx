import type { ButtonHTMLAttributes, InputHTMLAttributes, ReactNode, SelectHTMLAttributes, TextareaHTMLAttributes } from 'react'
import { forwardRef, useId } from 'react'

export function cx(...values: Array<string | false | null | undefined>): string {
  return values.filter(Boolean).join(' ')
}

/* ---------------------------------------------------------------- Button */

type ButtonVariant = 'primary' | 'secondary' | 'ghost' | 'danger'
type ButtonSize = 'sm' | 'md'

const BUTTON_BASE =
  'inline-flex items-center justify-center gap-2 rounded-lg font-medium whitespace-nowrap transition-all duration-150 ' +
  'active:scale-[0.98] disabled:cursor-not-allowed disabled:opacity-50 disabled:active:scale-100 focus-visible:outline-2 focus-visible:outline-offset-2'

const BUTTON_VARIANTS: Record<ButtonVariant, string> = {
  primary:
    'bg-accent text-accent-fg shadow-sm hover:brightness-110 hover:shadow-md hover:shadow-accent/20 ' +
    'disabled:hover:brightness-100 disabled:hover:shadow-none',
  secondary:
    'border border-border/80 bg-surface-2/80 text-text hover:border-border-strong hover:bg-elevated hover:shadow-xs backdrop-blur-sm',
  ghost: 'text-muted hover:bg-surface-2/80 hover:text-text',
  danger: 'border border-danger/40 bg-danger/10 text-danger hover:bg-danger/20 hover:border-danger/60',
}

const BUTTON_SIZES: Record<ButtonSize, string> = {
  sm: 'h-8 px-3 text-xs',
  md: 'h-10 px-4 text-sm',
}

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant
  size?: ButtonSize
  loading?: boolean
}

export const Button = forwardRef<HTMLButtonElement, ButtonProps>(function Button(
  { variant = 'secondary', size = 'md', loading = false, disabled, type = 'button', children, className, ...rest },
  ref,
) {
  return (
    <button
      ref={ref}
      type={type}
      disabled={disabled || loading}
      aria-busy={loading || undefined}
      className={cx(BUTTON_BASE, BUTTON_VARIANTS[variant], BUTTON_SIZES[size], className)}
      {...rest}
    >
      {loading && <Spinner className="size-3.5" />}
      {children}
    </button>
  )
})

export function Spinner({ className }: { className?: string }) {
  return (
    <svg className={cx('animate-spin', className)} viewBox="0 0 24 24" aria-hidden="true">
      <circle cx="12" cy="12" r="9" fill="none" stroke="currentColor" strokeOpacity="0.25" strokeWidth="3" />
      <path d="M21 12a9 9 0 0 0-9-9" fill="none" stroke="currentColor" strokeWidth="3" strokeLinecap="round" />
    </svg>
  )
}

/* ------------------------------------------------------------------ Card */

export function Card({ children, className }: { children: ReactNode; className?: string }) {
  return (
    // min-w-0 lets a card shrink inside a grid/flex parent so inner
    // overflow-x-auto regions scroll instead of widening the page.
    <div
      className={cx(
        'min-w-0 rounded-xl border border-border/80 bg-surface/90 shadow-[var(--shadow-card)] backdrop-blur-md transition-all duration-200',
        className,
      )}
    >
      {children}
    </div>
  )
}

export function CardHeader({
  title,
  description,
  actions,
}: {
  title: ReactNode
  description?: ReactNode
  actions?: ReactNode
}) {
  return (
    <div className="flex flex-wrap items-start justify-between gap-3 border-b border-border px-5 py-4">
      <div className="min-w-0">
        <h2 className="text-sm font-semibold tracking-tight">{title}</h2>
        {description && <p className="mt-1 text-xs leading-relaxed text-muted">{description}</p>}
      </div>
      {actions && <div className="flex shrink-0 items-center gap-2">{actions}</div>}
    </div>
  )
}

/* ----------------------------------------------------------------- Field */

interface FieldProps {
  label: string
  hint?: ReactNode
  error?: string | null
  required?: boolean
  children: (id: string) => ReactNode
}

export function Field({ label, hint, error, required, children }: FieldProps) {
  const id = useId()
  return (
    <div className="space-y-1.5">
      <label htmlFor={id} className="flex items-center gap-1.5 text-xs font-medium text-muted">
        {label}
        {required && (
          <span className="text-danger" aria-hidden="true">
            *
          </span>
        )}
      </label>
      {children(id)}
      {error ? (
        <p className="text-xs text-danger">{error}</p>
      ) : (
        hint && <p className="text-xs leading-relaxed text-subtle">{hint}</p>
      )}
    </div>
  )
}

const CONTROL =
  'w-full rounded-lg border border-border bg-surface-2 px-3 text-sm text-text transition ' +
  'placeholder:text-subtle hover:border-border-strong focus:border-accent focus-visible:outline-2 focus-visible:outline-offset-2 ' +
  'disabled:cursor-not-allowed disabled:opacity-60'

export const Input = forwardRef<HTMLInputElement, InputHTMLAttributes<HTMLInputElement>>(
  function Input({ className, ...rest }, ref) {
    return <input ref={ref} className={cx(CONTROL, 'h-10', className)} {...rest} />
  },
)

export const Textarea = forwardRef<HTMLTextAreaElement, TextareaHTMLAttributes<HTMLTextAreaElement>>(
  function Textarea({ className, ...rest }, ref) {
    return (
      <textarea
        ref={ref}
        rows={3}
        className={cx(CONTROL, 'min-h-16 py-2 leading-relaxed', className)}
        {...rest}
      />
    )
  },
)

export const Select = forwardRef<HTMLSelectElement, SelectHTMLAttributes<HTMLSelectElement>>(
  function Select({ className, ...rest }, ref) {
    return (
      <select
        ref={ref}
        className={cx(CONTROL, 'h-10 cursor-pointer appearance-none bg-no-repeat pr-9', className)}
        style={{
          backgroundImage:
            "url(\"data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 12 12'%3E%3Cpath d='M2.5 4.5 6 8l3.5-3.5' fill='none' stroke='%23888' stroke-width='1.5' stroke-linecap='round'/%3E%3C/svg%3E\")",
          backgroundPosition: 'right 0.6rem center',
          backgroundSize: '0.8rem',
        }}
        {...rest}
      />
    )
  },
)

export function Checkbox({
  label,
  checked,
  onChange,
  disabled,
}: {
  label: ReactNode
  checked: boolean
  onChange: (checked: boolean) => void
  disabled?: boolean
}) {
  return (
    <label
      className={cx(
        'flex cursor-pointer items-center gap-2 text-sm',
        disabled && 'cursor-not-allowed opacity-60',
      )}
    >
      <input
        type="checkbox"
        checked={checked}
        disabled={disabled}
        onChange={(event) => onChange(event.target.checked)}
        className="size-4 rounded border-border-strong bg-surface-2 accent-[var(--app-accent)]"
      />
      {label}
    </label>
  )
}
