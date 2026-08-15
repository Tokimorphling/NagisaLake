import { useId } from 'react'
import { cx } from './primitives'

/* ----------------------------------------------------------------- Tabs */

export interface TabItem {
  id: string
  label: string
}

/**
 * Accessible tablist. Uses roving tabindex with `aria-selected` and
 * `aria-controls`/`aria-labelledby` pairing so the tabs and panels are
 * announced correctly by assistive technology. The caller renders the
 * active panel content as children.
 */
export function Tabs({
  items,
  value,
  onChange,
  children,
  className,
}: {
  items: TabItem[]
  value: string
  onChange: (id: string) => void
  children?: React.ReactNode
  className?: string
}) {
  const baseId = useId()
  const activeIndex = Math.max(
    0,
    items.findIndex((item) => item.id === value),
  )

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'ArrowRight') {
      e.preventDefault()
      onChange(items[(activeIndex + 1) % items.length].id)
    } else if (e.key === 'ArrowLeft') {
      e.preventDefault()
      onChange(items[(activeIndex - 1 + items.length) % items.length].id)
    } else if (e.key === 'Home') {
      e.preventDefault()
      onChange(items[0].id)
    } else if (e.key === 'End') {
      e.preventDefault()
      onChange(items[items.length - 1].id)
    }
  }

  return (
    <div className={cx(className)}>
      <div role="tablist" className="flex gap-1 border-b border-border">
        {items.map((item) => {
          const isActive = item.id === value
          return (
            <button
              key={item.id}
              type="button"
              role="tab"
              id={`${baseId}-tab-${item.id}`}
              aria-selected={isActive}
              aria-controls={`${baseId}-panel-${item.id}`}
              tabIndex={isActive ? 0 : -1}
              onClick={() => onChange(item.id)}
              onKeyDown={onKeyDown}
              className={cx(
                'border-b-2 px-4 py-2 text-xs font-medium transition-colors',
                isActive
                  ? 'border-accent text-accent'
                  : 'border-transparent text-muted hover:border-border-strong hover:text-text',
              )}
            >
              {item.label}
            </button>
          )
        })}
      </div>
      {children && (
        <div
          role="tabpanel"
          id={`${baseId}-panel-${value}`}
          aria-labelledby={`${baseId}-tab-${value}`}
          tabIndex={0}
          className="pt-4 focus:outline-none"
        >
          {children}
        </div>
      )}
    </div>
  )
}

/* --------------------------------------------------------------- Tooltip */

/**
 * Lightweight tooltip that shows on hover and focus, using ARIA labelling.
 * No portal or positioning library — it renders inline as a sibling.
 */
export function Tooltip({
  content,
  children,
  className,
}: {
  content: string
  children: React.ReactNode
  className?: string
}) {
  const id = useId()
  return (
    <span className={cx('group relative inline-flex', className)}>
      <span
        aria-describedby={id}
        tabIndex={0}
        className="inline-flex focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/40 rounded"
      >
        {children}
      </span>
      <span
        id={id}
        role="tooltip"
        className={cx(
          'pointer-events-none absolute bottom-full left-1/2 z-50 mb-2 -translate-x-1/2 whitespace-nowrap rounded-lg border border-border bg-elevated px-2.5 py-1.5 text-xs text-text shadow-lg',
          'opacity-0 transition-opacity duration-150 group-hover:opacity-100 group-focus-within:opacity-100',
        )}
      >
        {content}
      </span>
    </span>
  )
}
