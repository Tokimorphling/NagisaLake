import type { PresignedRequest } from '@/api/types'
import type { ShowcaseField } from './showcase'

export interface PosterSpec {
  /** Media to feature. Must be same-origin or CORS-readable, or the draw taints the canvas. */
  mediaUrl: string
  mediaKind: 'image' | 'video'
  title: string
  subtitle: string
  metrics: ShowcaseField[]
  footer: string
  theme: 'dark' | 'light'
}

/** Raised when the browser refuses to export the canvas because of tainting. */
export class PosterTaintError extends Error {
  constructor() {
    super(
      '对象存储没有返回 CORS 响应头，浏览器禁止导出画布。请为 bucket 允许该前端 origin 的 GET 跨域读取后重试。',
    )
    this.name = 'PosterTaintError'
  }
}

/** Raised when an image/video URL cannot provide a decodable frame. */
export class PosterMediaLoadError extends Error {
  constructor(message: string) {
    super(message)
    this.name = 'PosterMediaLoadError'
  }
}

export interface RenderedPoster {
  canvas: HTMLCanvasElement
  blob: Blob
}

/**
 * A media element cannot attach custom headers, and only GET URLs are safe to
 * place in `src`. Any other signed request must use the authenticated proxy.
 *
 * The lifetime check is deliberately tight (2s): a ticket that is about to
 * expire would fail mid-render, so we prefer the same-origin fallback (which
 * re-fetches with auth) rather than starting a canvas draw that may race the
 * expiry. The previous 10s threshold caused the proxy path to absorb too much
 * traffic that could have used a still-valid direct URL.
 */
export function canUseDirectPosterTicket(ticket: PresignedRequest): boolean {
  const hasEnoughLifetime = ticket.expires_at_unix_ms - Date.now() > 2_000
  return (
    ticket.method.toUpperCase() === 'GET' &&
    Object.keys(ticket.headers).length === 0 &&
    hasEnoughLifetime
  )
}

function isDirectMediaFailure(error: unknown): boolean {
  return error instanceof PosterMediaLoadError || error instanceof PosterTaintError
}

/**
 * Uses an object-store ticket first, then retries once with a same-origin Blob
 * only when the direct media cannot be loaded or exported because of CORS.
 */
export async function withPosterMediaFallback<T>(
  ticket: PresignedRequest,
  useMediaUrl: (mediaUrl: string) => Promise<T>,
  fetchSameOrigin: () => Promise<Blob>,
): Promise<T> {
  if (canUseDirectPosterTicket(ticket)) {
    try {
      return await useMediaUrl(ticket.url)
    } catch (error) {
      if (!isDirectMediaFailure(error)) throw error
    }
  }

  const mediaBlob = await fetchSameOrigin()
  const localMediaUrl = URL.createObjectURL(mediaBlob)
  try {
    // This fallback is deliberately not caught: there is no third attempt, so
    // a broken/unsupported object cannot start an unbounded retry loop.
    return await useMediaUrl(localMediaUrl)
  } finally {
    URL.revokeObjectURL(localMediaUrl)
  }
}

const WIDTH = 1080
const PADDING = 56
const SCALE = 1
export const POSTER_OUTPUT_WIDTH = WIDTH * SCALE

interface Palette {
  bg: string
  bgAlt: string
  card: string
  border: string
  text: string
  muted: string
  subtle: string
  accent: string
  accentAlt: string
}

const PALETTES: Record<'dark' | 'light', Palette> = {
  dark: {
    bg: '#131722',
    bgAlt: '#1b2130',
    card: 'rgba(255,255,255,0.045)',
    border: 'rgba(255,255,255,0.11)',
    text: '#f4f6fa',
    muted: '#a9b2c4',
    subtle: '#79839a',
    accent: '#4fd7e6',
    accentAlt: '#a98cf7',
  },
  light: {
    bg: '#f7f8fb',
    bgAlt: '#eceff5',
    card: 'rgba(16,20,32,0.035)',
    border: 'rgba(16,20,32,0.10)',
    text: '#1b2030',
    muted: '#4d5668',
    subtle: '#727d94',
    accent: '#0e8fa5',
    accentAlt: '#6d46d6',
  },
}

/** Loads an <img> with CORS enabled so the canvas stays exportable. */
function loadImage(url: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const image = new Image()
    image.crossOrigin = 'anonymous'
    image.decoding = 'sync'
    image.onload = () => resolve(image)
    image.onerror = () => {
      image.onload = null
      image.onerror = null
      image.removeAttribute('src')
      reject(new PosterMediaLoadError('无法加载媒体，可能是跨域限制或链接已过期'))
    }
    image.src = url
  })
}

/** Grabs a representative frame from a video without attaching it to the DOM. */
function loadVideoFrame(url: string): Promise<HTMLVideoElement> {
  return new Promise((resolve, reject) => {
    const video = document.createElement('video')
    video.crossOrigin = 'anonymous'
    video.muted = true
    video.playsInline = true
    video.preload = 'auto'

    const fail = () => {
      disposePosterMedia(video)
      reject(new PosterMediaLoadError('无法解码视频帧，可能是跨域限制或编码不受支持'))
    }
    video.onerror = fail
    video.onloadeddata = () => {
      // Seek a little in: frame 0 of a generated video is often black.
      const target = Number.isFinite(video.duration) ? Math.min(0.1, video.duration / 10) : 0
      if (target > 0 && video.currentTime !== target) {
        video.onseeked = () => resolve(video)
        video.currentTime = target
      } else {
        resolve(video)
      }
    }
    video.src = url
  })
}

/** Stops detached media from continuing Range requests or retaining decoder buffers. */
function disposePosterMedia(media: HTMLImageElement | HTMLVideoElement): void {
  media.onerror = null
  if (media instanceof HTMLVideoElement) {
    media.onloadeddata = null
    media.onseeked = null
    media.pause()
    media.removeAttribute('src')
    media.load()
    return
  }
  media.onload = null
  media.removeAttribute('src')
}

function roundRect(
  ctx: CanvasRenderingContext2D,
  x: number,
  y: number,
  width: number,
  height: number,
  radius: number,
): void {
  ctx.beginPath()
  ctx.moveTo(x + radius, y)
  ctx.arcTo(x + width, y, x + width, y + height, radius)
  ctx.arcTo(x + width, y + height, x, y + height, radius)
  ctx.arcTo(x, y + height, x, y, radius)
  ctx.arcTo(x, y, x + width, y, radius)
  ctx.closePath()
}

/** Greedy wrap that also honours explicit newlines in the source text. */
function wrapText(
  ctx: CanvasRenderingContext2D,
  text: string,
  maxWidth: number,
  maxLines: number,
): string[] {
  const lines: string[] = []

  for (const paragraph of text.split('\n')) {
    if (lines.length >= maxLines) break
    let current = ''
    // CJK has no spaces, so wrap per character; the measurement is what matters.
    for (const char of paragraph) {
      const candidate = current + char
      if (ctx.measureText(candidate).width > maxWidth && current !== '') {
        lines.push(current)
        current = char
        if (lines.length >= maxLines) break
      } else {
        current = candidate
      }
    }
    if (lines.length < maxLines && current !== '') lines.push(current)
  }

  if (lines.length === maxLines) {
    const last = lines[maxLines - 1]
    if (ctx.measureText(last + '…').width > maxWidth) {
      lines[maxLines - 1] = last.slice(0, Math.max(0, last.length - 1)) + '…'
    }
  }
  return lines
}

const FONT_STACK =
  "'SF Pro Display', 'Inter', 'Segoe UI', 'PingFang SC', 'Hiragino Sans GB', 'Microsoft YaHei', sans-serif"
const MONO_STACK = "'SF Mono', 'JetBrains Mono', Menlo, Consolas, monospace"

function font(size: number, weight = 400, mono = false): string {
  return `${weight} ${size}px ${mono ? MONO_STACK : FONT_STACK}`
}

/**
 * Renders the share poster and returns the canvas.
 *
 * Layout is measured top-down so the canvas height fits the content: the media
 * keeps its aspect ratio (capped), then the metric blocks stack under it.
 */
export async function renderPoster(spec: PosterSpec): Promise<HTMLCanvasElement> {
  const media =
    spec.mediaKind === 'video' ? await loadVideoFrame(spec.mediaUrl) : await loadImage(spec.mediaUrl)
  try {
    return renderPosterFromMedia(spec, media)
  } finally {
    disposePosterMedia(media)
  }
}

function renderPosterFromMedia(
  spec: PosterSpec,
  media: HTMLImageElement | HTMLVideoElement,
): HTMLCanvasElement {
  const palette = PALETTES[spec.theme]

  const naturalWidth =
    media instanceof HTMLVideoElement ? media.videoWidth : media.naturalWidth
  const naturalHeight =
    media instanceof HTMLVideoElement ? media.videoHeight : media.naturalHeight
  if (!naturalWidth || !naturalHeight) {
    throw new PosterMediaLoadError('媒体尺寸无效，无法合成卡片')
  }

  const contentWidth = WIDTH - PADDING * 2
  const mediaWidth = contentWidth
  const mediaHeight = Math.min(
    Math.round((naturalHeight / naturalWidth) * mediaWidth),
    Math.round(WIDTH * 1.15),
  )

  const headerHeight = 132
  const metricRows = Math.ceil(spec.metrics.length / 3)
  const metricsHeight = metricRows > 0 ? metricRows * 96 + (metricRows - 1) * 12 : 0
  const footerHeight = 76

  const height =
    headerHeight +
    mediaHeight +
    (metricsHeight > 0 ? 24 + metricsHeight : 0) +
    footerHeight +
    PADDING

  const canvas = document.createElement('canvas')
  canvas.width = POSTER_OUTPUT_WIDTH
  canvas.height = height * SCALE
  const ctx = canvas.getContext('2d')
  if (!ctx) throw new Error('当前浏览器不支持 Canvas 2D')
  ctx.scale(SCALE, SCALE)
  ctx.textBaseline = 'top'

  // Background: base fill plus two soft accent washes, echoing the app's aurora.
  ctx.fillStyle = palette.bg
  ctx.fillRect(0, 0, WIDTH, height)
  const wash = ctx.createLinearGradient(0, 0, WIDTH, height)
  wash.addColorStop(0, palette.bgAlt)
  wash.addColorStop(1, palette.bg)
  ctx.fillStyle = wash
  ctx.fillRect(0, 0, WIDTH, height)

  const glowOne = ctx.createRadialGradient(WIDTH * 0.12, 0, 0, WIDTH * 0.12, 0, WIDTH * 0.7)
  glowOne.addColorStop(0, `${palette.accent}2e`)
  glowOne.addColorStop(1, 'transparent')
  ctx.fillStyle = glowOne
  ctx.fillRect(0, 0, WIDTH, height)

  const glowTwo = ctx.createRadialGradient(WIDTH * 0.92, height * 0.08, 0, WIDTH * 0.92, height * 0.08, WIDTH * 0.6)
  glowTwo.addColorStop(0, `${palette.accentAlt}26`)
  glowTwo.addColorStop(1, 'transparent')
  ctx.fillStyle = glowTwo
  ctx.fillRect(0, 0, WIDTH, height)

  let cursorY = PADDING - 8

  // Logo mark
  const markSize = 46
  const markGradient = ctx.createLinearGradient(PADDING, cursorY, PADDING + markSize, cursorY + markSize)
  markGradient.addColorStop(0, palette.accent)
  markGradient.addColorStop(1, palette.accentAlt)
  ctx.fillStyle = markGradient
  roundRect(ctx, PADDING, cursorY, markSize, markSize, 13)
  ctx.fill()
  ctx.strokeStyle = 'rgba(255,255,255,0.85)'
  ctx.lineWidth = 3
  ctx.lineCap = 'round'
  ctx.beginPath()
  ctx.moveTo(PADDING + 11, cursorY + 31)
  ctx.bezierCurveTo(
    PADDING + 18, cursorY + 20,
    PADDING + 24, cursorY + 25,
    PADDING + 35, cursorY + 15,
  )
  ctx.stroke()

  ctx.fillStyle = palette.text
  ctx.font = font(30, 700)
  ctx.fillText(spec.title, PADDING + markSize + 16, cursorY + 1, contentWidth - markSize - 16)
  ctx.fillStyle = palette.subtle
  ctx.font = font(20, 400, true)
  ctx.fillText(spec.subtitle, PADDING + markSize + 16, cursorY + 34, contentWidth - markSize - 16)

  cursorY += headerHeight - 8

  // Media, clipped to a rounded frame.
  ctx.save()
  roundRect(ctx, PADDING, cursorY, mediaWidth, mediaHeight, 22)
  ctx.clip()
  ctx.fillStyle = '#000'
  ctx.fillRect(PADDING, cursorY, mediaWidth, mediaHeight)
  // Cover-fit inside the frame so there are no letterbox bars.
  const scale = Math.max(mediaWidth / naturalWidth, mediaHeight / naturalHeight)
  const drawWidth = naturalWidth * scale
  const drawHeight = naturalHeight * scale
  ctx.drawImage(
    media,
    PADDING + (mediaWidth - drawWidth) / 2,
    cursorY + (mediaHeight - drawHeight) / 2,
    drawWidth,
    drawHeight,
  )
  ctx.restore()
  ctx.strokeStyle = palette.border
  ctx.lineWidth = 1.5
  roundRect(ctx, PADDING, cursorY, mediaWidth, mediaHeight, 22)
  ctx.stroke()

  cursorY += mediaHeight

  // Metric tiles, three per row.
  if (spec.metrics.length > 0) {
    cursorY += 24
    const gap = 12
    const tileWidth = (contentWidth - gap * 2) / 3
    spec.metrics.forEach((metric, index) => {
      const column = index % 3
      const row = Math.floor(index / 3)
      const x = PADDING + column * (tileWidth + gap)
      const y = cursorY + row * (96 + gap)

      ctx.fillStyle = palette.card
      roundRect(ctx, x, y, tileWidth, 96, 16)
      ctx.fill()
      ctx.strokeStyle = palette.border
      ctx.lineWidth = 1
      roundRect(ctx, x, y, tileWidth, 96, 16)
      ctx.stroke()

      ctx.fillStyle = palette.subtle
      ctx.font = font(16, 600, true)
      ctx.fillText(metric.label.toUpperCase(), x + 18, y + 20, tileWidth - 36)

      ctx.fillStyle = palette.text
      ctx.font = font(27, 600)
      const valueLines = wrapText(ctx, metric.value, tileWidth - 36, 1)
      ctx.fillText(valueLines[0] ?? '—', x + 18, y + 48, tileWidth - 36)
    })
    cursorY += metricsHeight
  }

  // Footer
  cursorY += 30
  ctx.strokeStyle = palette.border
  ctx.lineWidth = 1
  ctx.beginPath()
  ctx.moveTo(PADDING, cursorY)
  ctx.lineTo(WIDTH - PADDING, cursorY)
  ctx.stroke()

  ctx.fillStyle = palette.subtle
  ctx.font = font(19, 400, true)
  ctx.fillText(spec.footer, PADDING, cursorY + 18, contentWidth * 0.7)

  ctx.fillStyle = palette.accent
  ctx.font = font(19, 700)
  const brand = 'Nagisalake'
  const brandWidth = ctx.measureText(brand).width
  ctx.fillText(brand, WIDTH - PADDING - brandWidth, cursorY + 18)

  return canvas
}

/** Exports a rendered poster, translating canvas tainting into a clear error. */
export function posterToBlob(canvas: HTMLCanvasElement): Promise<Blob> {
  return new Promise((resolve, reject) => {
    try {
      canvas.toBlob((blob) => {
        if (blob) resolve(blob)
        else reject(new Error('导出 PNG 失败'))
      }, 'image/png')
    } catch {
      reject(new PosterTaintError())
    }
  })
}

/** Renders a poster and verifies it is exportable before accepting direct media. */
export function renderPosterWithFallback(
  spec: Omit<PosterSpec, 'mediaUrl'>,
  ticket: PresignedRequest,
  fetchSameOrigin: () => Promise<Blob>,
): Promise<RenderedPoster> {
  return withPosterMediaFallback(
    ticket,
    async (mediaUrl) => {
      const canvas = await renderPoster({ ...spec, mediaUrl })
      const blob = await posterToBlob(canvas)
      return { canvas, blob }
    },
    fetchSameOrigin,
  )
}
