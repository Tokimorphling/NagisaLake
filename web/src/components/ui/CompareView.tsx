import { useCallback, useEffect, useRef, useState } from 'react'
import type { ArtifactView } from '@/api/types'
import { cx } from './primitives'
import { IconCompare, IconGrid } from '@/components/layout/icons'

export interface CompareItem {
  artifactId: string
  downloadUrl: string
  /** Only 'image' and 'video' are comparable; callers filter before passing in. */
  mediaType: 'image' | 'video'
  artifact: ArtifactView
  /** Shown as the item's caption, e.g. "输出 1" or the file name. */
  label: string
}

export type CompareMode = 'slider' | 'side-by-side'

/**
 * Before/after comparison for two media artifacts.
 *
 * The slider mode clips the "after" layer with inset(), which keeps both
 * elements laid out at identical size so the reveal line lands on the same
 * pixel column in both. Sizes differing between the two artifacts is normal
 * (a 512px input against a 2048px upscale), so both are object-contain inside
 * one fixed-aspect stage rather than sized to their intrinsic dimensions.
 */
export function CompareView({
  left,
  right,
  mode,
  onModeChange,
}: {
  left: CompareItem
  right: CompareItem
  mode: CompareMode
  onModeChange: (mode: CompareMode) => void
}) {
  const sliderAvailable = left.mediaType === 'image' && right.mediaType === 'image'
  const effectiveMode = sliderAvailable ? mode : 'side-by-side'

  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex items-center gap-2 text-[11px] font-mono text-subtle">
          <span className="rounded border border-border-strong/40 bg-surface-2 px-1.5 py-0.5">A</span>
          <span className="truncate max-w-[10rem] text-muted" title={left.label}>
            {left.label}
          </span>
          <span className="text-subtle">vs</span>
          <span className="rounded border border-accent/40 bg-accent/10 px-1.5 py-0.5 text-accent">
            B
          </span>
          <span className="truncate max-w-[10rem] text-muted" title={right.label}>
            {right.label}
          </span>
        </div>

        <div className="flex rounded-lg border border-border/80 bg-surface-2 p-0.5">
          <button
            type="button"
            onClick={() => onModeChange('slider')}
            disabled={!sliderAvailable}
            title={sliderAvailable ? '拖动分割线查看差异' : '视频对比使用并排模式'}
            className={cx(
              'flex items-center gap-1 rounded-md px-2.5 py-1 text-xs transition-all duration-150',
              effectiveMode === 'slider'
                ? 'bg-surface text-accent shadow-xs font-semibold'
                : 'text-muted hover:text-text disabled:cursor-not-allowed disabled:opacity-45',
            )}
          >
            <IconCompare className="size-3.5" />
            滑动对比
          </button>
          <button
            type="button"
            onClick={() => onModeChange('side-by-side')}
            className={cx(
              'flex items-center gap-1 rounded-md px-2.5 py-1 text-xs transition-all duration-150',
              effectiveMode === 'side-by-side'
                ? 'bg-surface text-accent shadow-xs font-semibold'
                : 'text-muted hover:text-text',
            )}
          >
            <IconGrid className="size-3.5" />
            并排对比
          </button>
        </div>
      </div>

      {effectiveMode === 'slider' ? (
        <SliderStage left={left} right={right} />
      ) : (
        <div className="grid gap-3 sm:grid-cols-2">
          <SideStage item={left} badge="A" />
          <SideStage item={right} badge="B" accent />
        </div>
      )}
    </div>
  )
}

function Media({ item, className }: { item: CompareItem; className?: string }) {
  if (item.mediaType === 'video') {
    return (
      <video
        src={item.downloadUrl}
        controls
        preload="metadata"
        playsInline
        className={cx('size-full object-contain', className)}
      />
    )
  }
  return (
    <img
      src={item.downloadUrl}
      alt={item.label}
      draggable={false}
      className={cx('size-full object-contain select-none', className)}
    />
  )
}

function SideStage({
  item,
  badge,
  accent = false,
}: {
  item: CompareItem
  badge: string
  accent?: boolean
}) {
  return (
    <div className="space-y-1.5">
      <div className="relative aspect-square overflow-hidden rounded-xl border border-border/80 bg-black/50">
        <Media item={item} />
        <span
          className={cx(
            'absolute left-2.5 top-2.5 rounded-md px-2 py-0.5 font-mono text-[10px] font-bold backdrop-blur',
            accent ? 'bg-accent text-accent-fg' : 'bg-black/70 text-white',
          )}
        >
          {badge}
        </span>
      </div>
      <p className="truncate text-center text-[11px] text-muted" title={item.label}>
        {item.label}
      </p>
    </div>
  )
}

function SliderStage({ left, right }: { left: CompareItem; right: CompareItem }) {
  const stageRef = useRef<HTMLDivElement>(null)
  const [position, setPosition] = useState(50)
  const [dragging, setDragging] = useState(false)

  const updateFromClientX = useCallback((clientX: number) => {
    const stage = stageRef.current
    if (!stage) return
    const rect = stage.getBoundingClientRect()
    if (rect.width === 0) return
    const ratio = ((clientX - rect.left) / rect.width) * 100
    setPosition(Math.min(100, Math.max(0, ratio)))
  }, [])

  // Pointer events are tracked on the window so a fast drag that leaves the
  // stage keeps updating instead of sticking where the cursor exited.
  useEffect(() => {
    if (!dragging) return
    const onMove = (event: PointerEvent) => {
      event.preventDefault()
      updateFromClientX(event.clientX)
    }
    const onUp = () => setDragging(false)
    window.addEventListener('pointermove', onMove, { passive: false })
    window.addEventListener('pointerup', onUp)
    window.addEventListener('pointercancel', onUp)
    return () => {
      window.removeEventListener('pointermove', onMove)
      window.removeEventListener('pointerup', onUp)
      window.removeEventListener('pointercancel', onUp)
    }
  }, [dragging, updateFromClientX])

  return (
    <div className="space-y-2">
      <div
        ref={stageRef}
        className={cx(
          'relative aspect-square w-full overflow-hidden rounded-xl border border-border/80 bg-black/60',
          dragging ? 'cursor-ew-resize select-none' : 'cursor-ew-resize',
        )}
        onPointerDown={(event) => {
          setDragging(true)
          updateFromClientX(event.clientX)
        }}
      >
        {/* Base layer: A */}
        <div className="absolute inset-0">
          <Media item={left} />
        </div>

        {/* Reveal layer: B, clipped from the left edge to the handle. */}
        <div
          className="absolute inset-0"
          style={{ clipPath: `inset(0 0 0 ${position}%)` }}
        >
          <Media item={right} />
        </div>

        {/* Handle */}
        <div
          className="pointer-events-none absolute inset-y-0 w-0.5 bg-white/90 shadow-[0_0_12px_rgba(0,0,0,0.6)]"
          style={{ left: `${position}%` }}
        >
          <span className="absolute top-1/2 left-1/2 grid size-9 -translate-x-1/2 -translate-y-1/2 place-items-center rounded-full border-2 border-white bg-black/70 text-white backdrop-blur">
            <IconCompare className="size-4" />
          </span>
        </div>

        <span className="pointer-events-none absolute left-2.5 top-2.5 rounded-md bg-black/70 px-2 py-0.5 font-mono text-[10px] font-bold text-white backdrop-blur">
          A
        </span>
        <span className="pointer-events-none absolute right-2.5 top-2.5 rounded-md bg-accent px-2 py-0.5 font-mono text-[10px] font-bold text-accent-fg backdrop-blur">
          B
        </span>
      </div>

      {/* The range input is the keyboard and screen-reader path to the same
          state the pointer drag controls. */}
      <input
        type="range"
        min={0}
        max={100}
        step={0.5}
        value={position}
        aria-label="对比分割线位置"
        onChange={(event) => setPosition(Number(event.target.value))}
        className="w-full accent-[var(--app-accent)]"
      />
    </div>
  )
}
