import { useEffect, useId, useRef, useState } from 'react'
import { cx } from './primitives'

/**
 * Small inline SVG charts.
 *
 * Everything here is dependency-free and driven by plain arrays. The viewBox is
 * normalised so a chart scales with its container instead of assuming a pixel
 * size, and `preserveAspectRatio="none"` on the sparkline lets it stretch to any
 * card width without distorting stroke width (vector-effect keeps that fixed).
 */

export type ChartTone = 'accent' | 'violet' | 'success' | 'warning' | 'danger' | 'info'

const TONE_VARS: Record<ChartTone, string> = {
  accent: 'var(--app-accent)',
  violet: 'var(--app-violet)',
  success: 'var(--app-success)',
  warning: 'var(--app-warning)',
  danger: 'var(--app-danger)',
  info: 'var(--app-info)',
}

/** Collects one point per query refresh for session-local mini trend charts. */
export function useRollingSeries(
  value: number,
  tick: number,
  limit = 18,
  resetKey?: string | null,
): number[] {
  const [values, setValues] = useState<number[]>([value])
  const previousTick = useRef(tick)
  const previousResetKey = useRef(resetKey)

  useEffect(() => {
    if (previousResetKey.current !== resetKey) {
      previousResetKey.current = resetKey
      previousTick.current = tick
      setValues([value])
      return
    }
    if (previousTick.current === tick) return
    previousTick.current = tick
    setValues((current) => [...current, value].slice(-limit))
  }, [limit, resetKey, tick, value])

  return values
}

/* ------------------------------------------------------------- Sparkline */

export function Sparkline({
  values,
  tone = 'accent',
  className,
  showArea = true,
}: {
  values: number[]
  tone?: ChartTone
  className?: string
  showArea?: boolean
}) {
  const gradientId = useId()
  const color = TONE_VARS[tone]

  if (values.length === 0) {
    return <div className={cx('h-10 rounded bg-surface-2/60', className)} aria-hidden="true" />
  }

  const width = 100
  const height = 32
  const max = Math.max(...values)
  const min = Math.min(...values)
  // A flat series would divide by zero; render it as a mid-height line instead.
  const span = max - min || 1
  const step = values.length > 1 ? width / (values.length - 1) : width

  const points = values.map((value, index) => {
    const x = values.length > 1 ? index * step : width / 2
    const y = height - ((value - min) / span) * (height - 4) - 2
    return { x, y }
  })

  const line = points.map((point, index) => `${index === 0 ? 'M' : 'L'}${point.x.toFixed(2)} ${point.y.toFixed(2)}`).join(' ')
  const area = `${line} L${width} ${height} L0 ${height} Z`

  return (
    <svg
      viewBox={`0 0 ${width} ${height}`}
      preserveAspectRatio="none"
      className={cx('h-10 w-full overflow-visible', className)}
      role="img"
      aria-label={`趋势：最新值 ${values[values.length - 1] ?? '—'}`}
    >
      <defs>
        <linearGradient id={gradientId} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor={color} stopOpacity="0.35" />
          <stop offset="100%" stopColor={color} stopOpacity="0" />
        </linearGradient>
      </defs>
      {showArea && <path d={area} fill={`url(#${gradientId})`} />}
      <path
        d={line}
        fill="none"
        stroke={color}
        strokeWidth="1.5"
        strokeLinecap="round"
        strokeLinejoin="round"
        vectorEffect="non-scaling-stroke"
      />
      {points.length > 0 && (
        <rect
          x={points[points.length - 1].x - 1}
          y={points[points.length - 1].y - 1}
          width="2"
          height="2"
          fill={color}
          vectorEffect="non-scaling-stroke"
        />
      )}
    </svg>
  )
}

/* ----------------------------------------------------------------- Bars */

export function MiniBars({
  values,
  labels,
  tone = 'accent',
  className,
}: {
  values: number[]
  labels?: string[]
  tone?: ChartTone
  className?: string
}) {
  const color = TONE_VARS[tone]
  const max = Math.max(...values, 1)

  return (
    <div className={cx('flex items-end gap-[3px]', className)}>
      {values.map((value, index) => {
        const ratio = value / max
        return (
          <div
            key={index}
            className="group/bar relative flex-1"
            title={labels?.[index] ? `${labels[index]}: ${value}` : String(value)}
          >
            <div
              className="w-full rounded-sm transition-all duration-300"
              style={{
                // Keep a hairline for zero buckets so the axis stays readable.
                height: `${Math.max(ratio * 100, value > 0 ? 8 : 3)}%`,
                minHeight: 2,
                backgroundColor: color,
                opacity: value > 0 ? 0.35 + ratio * 0.65 : 0.18,
              }}
            />
          </div>
        )
      })}
    </div>
  )
}

/* ---------------------------------------------------------------- Donut */

export interface DonutSlice {
  label: string
  value: number
  tone: ChartTone
}

/**
 * Ring chart for a small set of categories.
 *
 * Slices are drawn as stroked circle arcs via stroke-dasharray, which avoids
 * hand-rolling arc path maths and stays crisp at any size.
 */
export function Donut({
  slices,
  total,
  centerLabel,
  centerValue,
  size = 132,
  thickness = 13,
}: {
  slices: DonutSlice[]
  /** Denominator. Defaults to the slice sum; pass a cap to show headroom. */
  total?: number
  centerLabel?: string
  centerValue?: string
  size?: number
  thickness?: number
}) {
  const sum = slices.reduce((accumulator, slice) => accumulator + slice.value, 0)
  const denominator = total !== undefined && total > 0 ? Math.max(total, sum) : sum
  const radius = (size - thickness) / 2
  const circumference = 2 * Math.PI * radius

  let offset = 0

  return (
    <div className="flex items-center gap-4">
      <div className="relative shrink-0" style={{ width: size, height: size }}>
        <svg width={size} height={size} className="-rotate-90" role="img" aria-label={`共 ${denominator} 项：${slices.map((s) => `${s.label} ${s.value}`).join('、')}`}>
          <circle
            cx={size / 2}
            cy={size / 2}
            r={radius}
            fill="none"
            stroke="var(--app-surface-2)"
            strokeWidth={thickness}
          />
          {denominator > 0 &&
            slices.map((slice) => {
              if (slice.value <= 0) return null
              const fraction = slice.value / denominator
              const dash = fraction * circumference
              const element = (
                <circle
                  key={slice.label}
                  cx={size / 2}
                  cy={size / 2}
                  r={radius}
                  fill="none"
                  stroke={TONE_VARS[slice.tone]}
                  strokeWidth={thickness}
                  strokeLinecap="butt"
                  strokeDasharray={`${dash} ${circumference - dash}`}
                  strokeDashoffset={-offset}
                  className="transition-all duration-500"
                />
              )
              offset += dash
              return element
            })}
        </svg>
        <div className="absolute inset-0 flex flex-col items-center justify-center">
          <span className="text-xl font-semibold tabular-nums tracking-tight">
            {centerValue ?? sum}
          </span>
          {centerLabel && (
            <span className="mt-0.5 text-[10px] text-subtle">{centerLabel}</span>
          )}
        </div>
      </div>

      <ul className="min-w-0 flex-1 space-y-1.5">
        {slices.map((slice) => (
          <li key={slice.label} className="flex items-center justify-between gap-2 text-xs">
            <span className="flex min-w-0 items-center gap-1.5">
              <span
                className="size-2 shrink-0 rounded-full"
                style={{ backgroundColor: TONE_VARS[slice.tone] }}
                aria-hidden="true"
              />
              <span className="truncate text-muted">{slice.label}</span>
            </span>
            <span className="shrink-0 font-mono tabular-nums">{slice.value}</span>
          </li>
        ))}
      </ul>
    </div>
  )
}

/* ------------------------------------------------------------ Load gauge */

/** Horizontal capacity bar: used vs total, with the remainder visible. */
export function LoadBar({
  label,
  used,
  total,
  tone = 'accent',
  hint,
}: {
  label: string
  used: number
  total: number
  tone?: ChartTone
  hint?: string
}) {
  const ratio = total > 0 ? Math.min(1, used / total) : 0
  const percentage = Math.round(ratio * 100)
  const overloaded = total > 0 && used >= total

  return (
    <div className="space-y-1.5">
      <div className="flex items-baseline justify-between gap-2 text-xs">
        <span className="truncate text-muted">{label}</span>
        <span className="shrink-0 font-mono tabular-nums">
          {used}
          <span className="text-subtle"> / {total > 0 ? total : '—'}</span>
        </span>
      </div>
      <div className="h-1.5 overflow-hidden rounded-full bg-surface-2 ring-1 ring-border/70 ring-inset">
        <div
          className="h-full rounded-full transition-[width] duration-500"
          style={{
            width: `${Math.max(percentage, used > 0 ? 3 : 0)}%`,
            backgroundColor: overloaded ? TONE_VARS.danger : TONE_VARS[tone],
          }}
        />
      </div>
      {hint && <p className="text-[10px] text-subtle">{hint}</p>}
    </div>
  )
}
