// Resolve the UI locale from the browser rather than hard-coding zh-CN.
// All Intl.* formatters derive from this so a non-Chinese user gets a
// locale-appropriate rendering. The relative-time helper below also
// branches on the locale for its display strings.
const LOCALE: string =
  (typeof navigator !== 'undefined' && navigator.language) || 'zh-CN'

const DATE_TIME = new Intl.DateTimeFormat(LOCALE, {
  year: 'numeric',
  month: '2-digit',
  day: '2-digit',
  hour: '2-digit',
  minute: '2-digit',
  second: '2-digit',
  hour12: false,
})

// Locale-aware relative-time formatter. Falls back to the manual zh-CN
// composition only when the runtime lacks RelativeTimeFormat.
const RTF: Intl.RelativeTimeFormat | null =
  typeof Intl !== 'undefined' && 'RelativeTimeFormat' in Intl
    ? new Intl.RelativeTimeFormat(LOCALE, { numeric: 'auto' })
    : null

export function formatDateTime(unixMs: number | null | undefined): string {
  if (unixMs === null || unixMs === undefined) return '—'
  return DATE_TIME.format(new Date(unixMs))
}

export function formatRelative(unixMs: number | null | undefined): string {
  if (unixMs === null || unixMs === undefined) return '—'
  const delta = unixMs - Date.now()
  const abs = Math.abs(delta)
  if (RTF) {
    if (abs < 45_000) return RTF.format(0, 'second')
    if (abs < 3_600_000) return RTF.format(Math.round(delta / 60_000), 'minute')
    if (abs < 86_400_000) return RTF.format(Math.round(delta / 3_600_000), 'hour')
    if (abs < 2_592_000_000) return RTF.format(Math.round(delta / 86_400_000), 'day')
    if (abs < 31_536_000_000) return RTF.format(Math.round(delta / 2_592_000_000), 'month')
    return RTF.format(Math.round(delta / 31_536_000_000), 'year')
  }
  // Fallback for very old runtimes without Intl.RelativeTimeFormat.
  const suffix = delta >= 0 ? '后' : '前'
  if (abs < 45_000) return '刚刚'
  if (abs < 3_600_000) return `${Math.round(abs / 60_000)} 分钟${suffix}`
  if (abs < 86_400_000) return `${Math.round(abs / 3_600_000)} 小时${suffix}`
  if (abs < 2_592_000_000) return `${Math.round(abs / 86_400_000)} 天${suffix}`
  return formatDateTime(unixMs)
}

const BYTE_UNITS = ['B', 'KiB', 'MiB', 'GiB', 'TiB']

export function formatBytes(bytes: number | null | undefined): string {
  if (bytes === null || bytes === undefined) return '—'
  if (bytes === 0) return '0 B'
  const exponent = Math.min(Math.floor(Math.log(Math.abs(bytes)) / Math.log(1024)), BYTE_UNITS.length - 1)
  const value = bytes / 1024 ** exponent
  return `${value.toFixed(value >= 100 || exponent === 0 ? 0 : 1)} ${BYTE_UNITS[exponent]}`
}

const DURATION_UNITS: Array<{ divisor: number; label: string }> = [
  { divisor: 86_400, label: '天' },
  { divisor: 3_600, label: '小时' },
  { divisor: 60, label: '分钟' },
]

export function formatDuration(seconds: number): string {
  for (const { divisor, label } of DURATION_UNITS) {
    if (seconds % divisor === 0) return `${seconds / divisor} ${label}`
  }
  return `${seconds} 秒`
}

export function shortId(id: string, head = 8): string {
  return id.length <= head + 4 ? id : `${id.slice(0, head)}…`
}

export function percent(value: number, total: number): number {
  if (total <= 0) return 0
  return Math.min(100, Math.max(0, Math.round((value / total) * 100)))
}

// Re-exported so existing call sites keep importing it from here. The
// implementation lives in platform.ts alongside the other secure-context
// fallbacks; navigator.clipboard is unavailable on a plain-HTTP LAN origin.
export { copyText } from './platform'
