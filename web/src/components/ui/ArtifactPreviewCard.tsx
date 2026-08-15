import { useEffect, useRef, useState } from 'react'
import type { ArtifactView } from '@/api/types'
import { downloadArtifact } from '@/api/upload'
import { copyText, formatBytes } from '@/lib/format'
import { useToast } from '@/state/toast'
import { useArtifactDownload } from '@/api/queries'
import { Button, cx } from './primitives'
import { Copyable } from './display'
import {
  IconAudio,
  IconCheck,
  IconClose,
  IconCopy,
  IconDownload,
  IconExpand,
  IconExternalLink,
  IconFile,
  IconImage,
  IconRefresh,
  IconShare,
  IconVideo,
  IconZoomIn,
  IconZoomOut,
} from '@/components/layout/icons'

export type MediaType = 'image' | 'video' | 'audio' | 'text' | 'other'

export interface ResolvedArtifact {
  artifactId: string
  downloadUrl: string
  mediaType: MediaType
  artifact: ArtifactView
}

export function detectMediaType(contentType?: string, fileName?: string): MediaType {
  const ct = (contentType || '').toLowerCase()
  const fn = (fileName || '').toLowerCase()

  if (
    ct.startsWith('image/') ||
    /\.(png|jpe?g|webp|gif|svg|bmp|avif|tiff)$/i.test(fn)
  ) {
    return 'image'
  }

  if (
    ct.startsWith('video/') ||
    /\.(mp4|webm|mov|m4v|mkv|avi|ogv)$/i.test(fn)
  ) {
    return 'video'
  }

  if (
    ct.startsWith('audio/') ||
    /\.(mp3|wav|ogg|flac|m4a|aac)$/i.test(fn)
  ) {
    return 'audio'
  }

  if (
    ct.includes('json') ||
    ct.includes('text/') ||
    /\.(json|txt|md|log|csv|yaml|yml)$/i.test(fn)
  ) {
    return 'text'
  }

  return 'other'
}

function getExtensionBadge(fileName?: string, contentType?: string): string {
  if (fileName && fileName.includes('.')) {
    const ext = fileName.split('.').pop()?.toUpperCase()
    if (ext && ext.length <= 5) return ext
  }
  if (contentType) {
    const sub = contentType.split('/')[1]?.toUpperCase()
    if (sub) return sub.replace('X-', '')
  }
  return 'FILE'
}

interface ArtifactPreviewCardProps {
  artifactId: string
  index?: number
  onOpenLightbox?: (artifactId: string, mediaUrl: string, mediaType: MediaType, artifact: ArtifactView) => void
  /** Lets a parent build comparison/export tools from the same resolved query. */
  onResolved?: (item: ResolvedArtifact) => void
  onShowcase?: (item: ResolvedArtifact) => void
  layout?: 'grid' | 'list'
}

const MAX_TEXT_PREVIEW_BYTES = 64 * 1024

function isPdfContentType(contentType?: string, fileName?: string): boolean {
  const ct = (contentType || '').toLowerCase()
  if (ct === 'application/pdf') return true
  if (ct === 'application/x-pdf') return true
  if ((fileName || '').toLowerCase().endsWith('.pdf')) return true
  return false
}

function isTextContentType(contentType?: string): boolean {
  const ct = (contentType || '').toLowerCase()
  if (ct.startsWith('text/')) return true
  if (ct.includes('json')) return true
  if (ct === 'application/xml') return true
  if (ct === 'application/javascript') return true
  if (ct === 'application/x-yaml') return true
  return false
}

function TextPreviewContent({ downloadUrl }: { downloadUrl: string }) {
  const [text, setText] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [loading, setLoading] = useState(true)

  useEffect(() => {
    let aborted = false
    const controller = new AbortController()
    setLoading(true)
    setError(null)
    fetch(downloadUrl, { signal: controller.signal })
      .then(async (res) => {
        if (!res.ok) throw new Error(`HTTP ${res.status}`)
        const buf = await res.arrayBuffer()
        if (aborted) return
        const slice = buf.slice(0, MAX_TEXT_PREVIEW_BYTES)
        const decoded = new TextDecoder('utf-8', { fatal: false }).decode(slice)
        setText(decoded)
        setLoading(false)
      })
      .catch((err: unknown) => {
        if (aborted || controller.signal.aborted) return
        setError(err instanceof Error ? err.message : String(err))
        setLoading(false)
      })
    return () => {
      aborted = true
      controller.abort()
    }
  }, [downloadUrl])

  if (loading) {
    return (
      <div className="flex size-full items-center justify-center p-8 text-center">
        <span className="text-[11px] text-subtle">加载预览…</span>
      </div>
    )
  }
  if (error) {
    return (
      <div className="flex size-full flex-col items-center justify-center gap-3 p-8 text-center">
        <IconFile className="size-6 text-muted" />
        <p className="text-[11px] text-muted">预览加载失败：{error}</p>
      </div>
    )
  }
  return (
    <pre className="size-full overflow-auto whitespace-pre-wrap break-all bg-surface-2/30 p-3 font-mono text-[11px] leading-relaxed text-text">
      {text}
      {text && text.length >= MAX_TEXT_PREVIEW_BYTES && (
        <span className="mt-2 block text-[10px] text-subtle">
          仅显示前 {MAX_TEXT_PREVIEW_BYTES / 1024} KB，下载完整内容请点击下方按钮。
        </span>
      )}
    </pre>
  )
}

export function ArtifactPreviewCard({
  artifactId,
  index,
  onOpenLightbox,
  onResolved,
  onShowcase,
  layout = 'grid',
}: ArtifactPreviewCardProps) {
  const toast = useToast()
  const { data, isLoading, isError, refetch } = useArtifactDownload(artifactId)
  const [isDownloading, setIsDownloading] = useState(false)
  const [copiedLink, setCopiedLink] = useState(false)

  const artifact = data?.artifact
  const downloadUrl = data?.download?.url

  const mediaType = detectMediaType(artifact?.content_type, artifact?.name)
  const extension = getExtensionBadge(artifact?.name, artifact?.content_type)

  useEffect(() => {
    if (!artifact || !downloadUrl) return
    onResolved?.({ artifactId, downloadUrl, mediaType, artifact })
  }, [artifact, artifactId, downloadUrl, mediaType, onResolved])

  const handleDownload = async () => {
    setIsDownloading(true)
    try {
      await downloadArtifact(artifactId)
    } catch (err) {
      toast.fromError(err, '获取下载链接失败')
    } finally {
      setIsDownloading(false)
    }
  }

  const handleCopyLink = async () => {
    if (!downloadUrl) return
    if (await copyText(downloadUrl)) {
      setCopiedLink(true)
      toast.success('已复制下载 URL 到剪贴板')
      setTimeout(() => setCopiedLink(false), 2000)
    }
  }

  if (isLoading) {
    return (
      <div className="group relative overflow-hidden rounded-xl border border-border/80 bg-surface/90 p-4 shadow-sm backdrop-blur-sm">
        <div className="flex items-center justify-between pb-3">
          <div className="skeleton h-4 w-28 rounded" />
          <div className="skeleton h-6 w-14 rounded-md" />
        </div>
        <div className="skeleton aspect-video w-full rounded-lg" />
        <div className="mt-3 flex items-center justify-between">
          <div className="skeleton h-3 w-36 rounded" />
          <div className="skeleton h-8 w-20 rounded-lg" />
        </div>
      </div>
    )
  }

  if (isError || !data || !artifact) {
    return (
      <div className="rounded-xl border border-danger/30 bg-danger/5 p-4 text-center">
        <p className="text-xs font-medium text-danger">加载输出失败</p>
        <Copyable value={artifactId} className="mt-1 text-subtle" />
        <div className="mt-3 flex justify-center gap-2">
          <Button size="sm" variant="ghost" onClick={() => void refetch()}>
            <IconRefresh className="size-3.5" />
            重试
          </Button>
          <Button size="sm" loading={isDownloading} onClick={handleDownload}>
            强制下载
          </Button>
        </div>
      </div>
    )
  }

  // Compact List view
  if (layout === 'list') {
    return (
      <div className="group flex flex-wrap items-center justify-between gap-3 rounded-xl border border-border/70 bg-surface/90 px-4 py-3 shadow-xs transition duration-200 hover:border-accent/40 hover:bg-surface-2/70 hover:shadow-md backdrop-blur-sm">
        <div className="flex items-center gap-3.5 min-w-0 flex-1">
          {/* Icon Badge */}
          <div
            className={cx(
              'flex size-10 shrink-0 items-center justify-center rounded-lg border text-sm font-semibold transition-transform duration-200 group-hover:scale-105',
              mediaType === 'image' && 'border-accent/30 bg-accent/10 text-accent shadow-xs',
              mediaType === 'video' && 'border-violet/30 bg-violet/10 text-violet shadow-xs',
              mediaType === 'audio' && 'border-info/30 bg-info/10 text-info shadow-xs',
              mediaType === 'text' && 'border-warning/30 bg-warning/10 text-warning shadow-xs',
              mediaType === 'other' && 'border-border-strong/60 bg-surface-2 text-muted',
            )}
          >
            {mediaType === 'image' && <IconImage className="size-5" />}
            {mediaType === 'video' && <IconVideo className="size-5" />}
            {mediaType === 'audio' && <IconAudio className="size-5" />}
            {mediaType === 'text' && <IconFile className="size-5" />}
            {mediaType === 'other' && <IconFile className="size-5" />}
          </div>

          <div className="min-w-0 flex-1">
            <div className="flex items-center gap-2">
              <p className="truncate text-xs font-semibold tracking-tight text-text">
                {artifact.name || `输出 ${index !== undefined ? index + 1 : ''}`}
              </p>
              <span className="rounded border border-border-strong/40 bg-surface-2 px-1.5 py-0.5 text-[10px] font-mono font-medium text-subtle">
                {extension}
              </span>
            </div>
            <div className="mt-0.5 flex items-center gap-2 text-[11px] text-muted">
              <span>{formatBytes(artifact.size_bytes)}</span>
              <span>·</span>
              <Copyable value={artifact.id} display={artifact.id.slice(0, 12) + '…'} />
            </div>
          </div>
        </div>

        <div className="flex items-center gap-1.5 shrink-0">
          {downloadUrl && (mediaType === 'image' || mediaType === 'video') && onShowcase && (
            <Button
              size="sm"
              variant="ghost"
              onClick={() =>
                onShowcase({ artifactId, downloadUrl, mediaType, artifact })
              }
              title="导出带生成参数的分享卡片" aria-label="导出带生成参数的分享卡片"
            >
              <IconShare className="size-3.5" />
              参数卡
            </Button>
          )}

          {downloadUrl && (mediaType === 'image' || mediaType === 'video') && (
            <Button
              size="sm"
              variant="ghost"
              onClick={() => onOpenLightbox?.(artifactId, downloadUrl, mediaType, artifact)}
              title="全屏大图 / 视频预览" aria-label="全屏预览"
            >
              <IconExpand className="size-3.5" />
              预览
            </Button>
          )}

          {downloadUrl && (
            <a
              href={downloadUrl}
              target="_blank"
              rel="noopener noreferrer"
              className="inline-flex h-8 items-center gap-1 rounded-lg px-2.5 text-xs text-muted transition hover:bg-surface-2 hover:text-text"
              title="在新标签页打开" aria-label="在新标签页打开"
            >
              <IconExternalLink className="size-3.5" />
            </a>
          )}

          <Button size="sm" variant="secondary" loading={isDownloading} onClick={handleDownload}>
            <IconDownload className="size-3.5" />
            下载
          </Button>
        </div>
      </div>
    )
  }

  // Grid view (Rich Media Card)
  return (
    <div className="group relative flex flex-col overflow-hidden rounded-xl border border-border/80 bg-surface/90 shadow-[var(--shadow-card)] backdrop-blur-md transition-all duration-300 hover:border-accent/50 hover:shadow-lg hover:-translate-y-0.5">
      {/* Top Bar */}
      <div className="flex items-center justify-between border-b border-border/60 px-3.5 py-2.5 bg-surface-2/40">
        <div className="flex items-center gap-2 min-w-0">
          <span
            className={cx(
              'inline-flex items-center gap-1 rounded-md px-2 py-0.5 text-[10px] font-mono font-semibold uppercase tracking-wider',
              mediaType === 'image' && 'bg-accent/15 text-accent border border-accent/30',
              mediaType === 'video' && 'bg-violet/15 text-violet border border-violet/30',
              mediaType === 'audio' && 'bg-info/15 text-info border border-info/30',
              mediaType === 'text' && 'bg-warning/15 text-warning border border-warning/30',
              mediaType === 'other' && 'bg-surface-2 text-muted border border-border',
            )}
          >
            {mediaType === 'image' && <IconImage className="size-3" />}
            {mediaType === 'video' && <IconVideo className="size-3" />}
            {mediaType === 'audio' && <IconAudio className="size-3" />}
            {extension}
          </span>
          <span className="truncate text-xs font-medium text-text" title={artifact.name}>
            {artifact.name || `输出 ${index !== undefined ? index + 1 : ''}`}
          </span>
        </div>

        <span className="text-[11px] font-mono text-muted shrink-0">
          {formatBytes(artifact.size_bytes)}
        </span>
      </div>

      {/* Media Viewport */}
      <div className="relative flex min-h-[220px] max-h-[420px] w-full items-center justify-center overflow-hidden bg-black/50">
        {mediaType === 'image' && downloadUrl && (
          <div
            className="group/img relative flex size-full items-center justify-center cursor-pointer overflow-hidden bg-[radial-gradient(#ffffff0d_1px,transparent_1px)] [background-size:16px_16px]"
            onClick={() => onOpenLightbox?.(artifactId, downloadUrl, 'image', artifact)}
          >
            <img
              src={downloadUrl}
              alt={artifact.name}
              className="max-h-[380px] w-auto object-contain transition duration-500 group-hover/img:scale-105"
              loading="lazy"
              decoding="async"
              sizes="(max-width: 640px) 100vw, 480px"
            />
            <div className="absolute inset-0 flex items-center justify-center bg-black/45 opacity-0 backdrop-blur-[3px] transition duration-200 group-hover/img:opacity-100">
              <span className="inline-flex items-center gap-1.5 rounded-full bg-accent px-4 py-2 text-xs font-semibold text-accent-fg shadow-xl transition-transform duration-200 group-hover/img:scale-105">
                <IconExpand className="size-4" />
                全屏放大预览
              </span>
            </div>
          </div>
        )}

        {mediaType === 'video' && downloadUrl && (
          <div className="relative size-full bg-black flex items-center justify-center group/vid">
            <video
              src={downloadUrl}
              controls
              preload="metadata"
              playsInline
              className="max-h-[380px] w-full object-contain"
            />
            <button
              type="button"
              onClick={() => onOpenLightbox?.(artifactId, downloadUrl, 'video', artifact)}
              className="absolute top-3 right-3 opacity-0 group-hover/vid:opacity-100 inline-flex items-center gap-1.5 rounded-lg bg-black/70 px-2.5 py-1.5 text-xs text-white backdrop-blur border border-white/20 transition hover:bg-black/90"
              title="全屏播放" aria-label="全屏播放"
            >
              <IconExpand className="size-3.5" />
              全屏
            </button>
          </div>
        )}

        {mediaType === 'audio' && downloadUrl && (
          <div className="flex w-full flex-col items-center justify-center gap-4 p-6 bg-gradient-to-b from-surface-2/50 to-surface/90">
            <div className="relative flex items-center justify-center">
              <div className="grid size-14 place-items-center rounded-full border border-info/30 bg-info/10 text-info shadow-inner">
                <IconAudio className="size-7 animate-pulse" />
              </div>
              <div className="absolute -inset-1 rounded-full border border-info/20 animate-ping opacity-30" />
            </div>
            <audio controls src={downloadUrl} className="w-full max-w-sm" />
          </div>
        )}

        {(mediaType === 'text' || mediaType === 'other') && downloadUrl && isPdfContentType(artifact.content_type, artifact.name) && (
          <object
            data={downloadUrl}
            type="application/pdf"
            className="size-full min-h-[220px] bg-white"
            aria-label={artifact.name}
          >
            <div className="flex size-full flex-col items-center justify-center gap-3 p-8 text-center bg-surface-2/30">
              <div className="grid size-12 place-items-center rounded-xl border border-border-strong/40 bg-surface-2 text-muted shadow-xs">
                <IconFile className="size-6" />
              </div>
              <div>
                <p className="text-xs font-semibold text-text">{artifact.name}</p>
                <p className="mt-1 text-[11px] font-mono text-muted">{artifact.content_type || 'PDF'}</p>
                <a
                  href={downloadUrl}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="mt-2 inline-block text-xs text-accent hover:underline"
                >
                  在新标签页打开 PDF
                </a>
              </div>
            </div>
          </object>
        )}

        {(mediaType === 'text' || (mediaType === 'other' && !isPdfContentType(artifact.content_type, artifact.name))) && downloadUrl && isTextContentType(artifact.content_type) && (
          <div className="size-full min-h-[220px]">
            <TextPreviewContent downloadUrl={downloadUrl} />
          </div>
        )}

        {((mediaType === 'other' && !isPdfContentType(artifact.content_type, artifact.name) && !isTextContentType(artifact.content_type)) || !downloadUrl) && (
          <div className="flex size-full flex-col items-center justify-center gap-3 p-8 text-center bg-surface-2/30">
            <div className="grid size-12 place-items-center rounded-xl border border-border-strong/40 bg-surface-2 text-muted shadow-xs">
              <IconFile className="size-6" />
            </div>
            <div>
              <p className="text-xs font-semibold text-text">{artifact.name}</p>
              <p className="mt-1 text-[11px] font-mono text-muted">{artifact.content_type || '二进制文件'}</p>
            </div>
          </div>
        )}
      </div>

      {/* Footer / Actions Bar */}
      <div className="flex items-center justify-between border-t border-border/60 px-3.5 py-2.5 bg-surface/90">
        <div className="min-w-0 flex-1 pr-2">
          <Copyable value={artifactId} display={artifactId.slice(0, 14) + '…'} className="text-subtle" />
        </div>

        <div className="flex items-center gap-1 shrink-0">
          {downloadUrl && (
            <button
              type="button"
              onClick={() => void handleCopyLink()}
              title={copiedLink ? '已复制 URL' : '复制直链'}
              className="inline-flex size-8 items-center justify-center rounded-lg text-muted transition hover:bg-surface-2 hover:text-text"
            >
              {copiedLink ? <IconCheck className="size-4 text-success" /> : <IconCopy className="size-4" />}
            </button>
          )}

          {downloadUrl && (
            <a
              href={downloadUrl}
              target="_blank"
              rel="noopener noreferrer"
              title="在新标签页打开" aria-label="在新标签页打开"
              className="inline-flex size-8 items-center justify-center rounded-lg text-muted transition hover:bg-surface-2 hover:text-text"
            >
              <IconExternalLink className="size-4" />
            </a>
          )}

          {downloadUrl && (mediaType === 'image' || mediaType === 'video') && (
            <Button
              size="sm"
              variant="ghost"
              onClick={() => onOpenLightbox?.(artifactId, downloadUrl, mediaType, artifact)}
              title="全屏大图 / 视频" aria-label="全屏预览"
            >
              <IconExpand className="size-3.5" />
            </Button>
          )}

          {downloadUrl && (mediaType === 'image' || mediaType === 'video') && onShowcase && (
            <Button
              size="sm"
              variant="ghost"
              onClick={() =>
                onShowcase({ artifactId, downloadUrl, mediaType, artifact })
              }
              title="导出带生成参数的分享卡片" aria-label="导出带生成参数的分享卡片"
            >
              <IconShare className="size-3.5" />
            </Button>
          )}

          <Button size="sm" variant="primary" loading={isDownloading} onClick={handleDownload}>
            <IconDownload className="size-3.5" />
            下载
          </Button>
        </div>
      </div>
    </div>
  )
}

/* ----------------------------------------------------------- Media Lightbox Modal */

export type LightboxItem = ResolvedArtifact

interface MediaLightboxModalProps {
  items: LightboxItem[]
  currentIndex: number
  onClose: () => void
  onSelectIndex: (index: number) => void
}

export function MediaLightboxModal({
  items,
  currentIndex,
  onClose,
  onSelectIndex,
}: MediaLightboxModalProps) {
  const [zoom, setZoom] = useState(1)
  const [rotation, setRotation] = useState(0)
  const current = items[currentIndex]
  const toast = useToast()
  const videoRef = useRef<HTMLVideoElement | null>(null)

  useEffect(() => {
    setZoom(1)
    setRotation(0)
  }, [currentIndex])

  const containerRef = useRef<HTMLDivElement | null>(null)
  const previousFocus = useRef<HTMLElement | null>(null)

  useEffect(() => {
    previousFocus.current = document.activeElement as HTMLElement
    containerRef.current?.focus()
    return () => {
      if (previousFocus.current && document.body.contains(previousFocus.current)) {
        previousFocus.current.focus()
      }
    }
  }, [])

  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        onClose()
      } else if (e.key === 'ArrowLeft') {
        if (currentIndex > 0) onSelectIndex(currentIndex - 1)
      } else if (e.key === 'ArrowRight') {
        if (currentIndex < items.length - 1) onSelectIndex(currentIndex + 1)
      } else if (e.key === '+' || e.key === '=') {
        setZoom((z) => Math.min(4, z + 0.25))
      } else if (e.key === '-') {
        setZoom((z) => Math.max(0.4, z - 0.25))
      }
    }
    window.addEventListener('keydown', handleKeyDown)
    return () => window.removeEventListener('keydown', handleKeyDown)
  }, [currentIndex, items.length, onClose, onSelectIndex])

  if (!current) return null

  const { artifact, downloadUrl, mediaType } = current

  return (
    <div
      ref={containerRef}
      role="dialog"
      aria-modal="true"
      aria-label={artifact.name}
      tabIndex={-1}
      className="fixed inset-0 z-50 flex flex-col bg-black/95 backdrop-blur-2xl animate-fade-in-up outline-none"
    >
      {/* Header */}
      <div className="flex flex-wrap items-center justify-between border-b border-white/10 px-6 py-4 text-white bg-black/40">
        <div className="flex items-center gap-3 min-w-0">
          <span className="rounded bg-white/15 px-2.5 py-1 font-mono text-xs font-semibold uppercase tracking-wider">
            {mediaType}
          </span>
          <div className="min-w-0">
            <h3 className="truncate text-sm font-semibold tracking-tight">{artifact.name}</h3>
            <p className="text-[11px] font-mono text-white/60">
              {formatBytes(artifact.size_bytes)} · {artifact.content_type || '对象'}
            </p>
          </div>
        </div>

        {/* Toolbar & Close */}
        <div className="flex flex-wrap items-center gap-2 shrink-0">
          {mediaType === 'image' && (
            <>
              <button
                type="button"
                onClick={() => setZoom((z) => Math.max(0.4, z - 0.25))}
                className="inline-flex size-9 items-center justify-center rounded-lg bg-white/10 text-white transition hover:bg-white/20 active:scale-95"
                title="缩小 (-)" aria-label="缩小"
              >
                <IconZoomOut className="size-4" />
              </button>
              <span className="font-mono text-xs text-white/80 w-14 text-center">
                {Math.round(zoom * 100)}%
              </span>
              <button
                type="button"
                onClick={() => setZoom((z) => Math.min(4, z + 0.25))}
                className="inline-flex size-9 items-center justify-center rounded-lg bg-white/10 text-white transition hover:bg-white/20 active:scale-95"
                title="放大 (+)" aria-label="放大"
              >
                <IconZoomIn className="size-4" />
              </button>

              <button
                type="button"
                onClick={() => setRotation((r) => (r + 90) % 360)}
                className="inline-flex h-9 px-3 items-center justify-center rounded-lg bg-white/10 text-xs font-mono text-white transition hover:bg-white/20 active:scale-95"
                title="旋转 90°" aria-label="旋转 90 度"
              >
                旋转
              </button>

              <button
                type="button"
                onClick={() => {
                  setZoom(1)
                  setRotation(0)
                }}
                className="inline-flex h-9 px-3 items-center justify-center rounded-lg bg-white/10 text-xs font-mono text-white transition hover:bg-white/20 active:scale-95"
              >
                重置
              </button>
            </>
          )}

          <a
            href={downloadUrl}
            target="_blank"
            rel="noopener noreferrer"
            className="inline-flex h-9 items-center gap-1.5 rounded-lg bg-white/10 px-3 text-xs font-medium text-white transition hover:bg-white/20"
          >
            <IconExternalLink className="size-4" />
            原文件
          </a>

          <button
            type="button"
            onClick={async () => {
              try {
                await downloadArtifact(artifact.id)
              } catch (err) {
                toast.fromError(err, '下载失败')
              }
            }}
            className="inline-flex h-9 items-center gap-1.5 rounded-lg bg-accent px-4 text-xs font-semibold text-accent-fg shadow-lg transition hover:brightness-110 active:scale-95"
          >
            <IconDownload className="size-4" />
            下载
          </button>

          <button
            type="button"
            onClick={onClose}
            className="ml-2 inline-flex size-9 items-center justify-center rounded-lg bg-white/10 text-white transition hover:bg-red-500/80 active:scale-95"
            title="关闭 (Esc)" aria-label="关闭"
          >
            <IconClose className="size-5" />
          </button>
        </div>
      </div>

      {/* Main Viewport */}
      <div className="relative flex flex-1 items-center justify-center overflow-hidden p-6 select-none">
        {mediaType === 'image' && (
          <div className="flex size-full items-center justify-center overflow-auto">
            <img
              src={downloadUrl}
              alt={artifact.name}
              style={{
                transform: `scale(${zoom}) rotate(${rotation}deg)`,
              }}
              className="max-h-[82vh] max-w-[90vw] object-contain transition-transform duration-200 ease-out shadow-2xl rounded-xl"
            />
          </div>
        )}

        {mediaType === 'video' && (
          <div className="flex size-full items-center justify-center">
            <video
              ref={videoRef}
              src={downloadUrl}
              controls
              autoPlay
              playsInline
              className="max-h-[85vh] max-w-[90vw] rounded-2xl bg-black object-contain shadow-2xl ring-1 ring-white/10"
            />
          </div>
        )}

        {mediaType === 'audio' && (
          <div className="flex max-w-lg w-full flex-col items-center justify-center gap-6 p-8 rounded-2xl bg-surface/90 border border-white/10 backdrop-blur-xl shadow-2xl">
            <div className="grid size-20 place-items-center rounded-full border border-info/40 bg-info/10 text-info">
              <IconAudio className="size-10 animate-pulse" />
            </div>
            <div className="text-center">
              <h4 className="text-base font-semibold text-white">{artifact.name}</h4>
              <p className="mt-1 text-xs text-white/60 font-mono">{formatBytes(artifact.size_bytes)}</p>
            </div>
            <audio controls autoPlay src={downloadUrl} className="w-full" />
          </div>
        )}

        {(mediaType === 'text' || mediaType === 'other') && (
          <div className="flex max-w-xl w-full flex-col items-center justify-center gap-4 p-8 rounded-2xl bg-surface/90 border border-white/10 backdrop-blur-xl shadow-2xl text-center">
            <IconFile className="size-12 text-white/60" />
            <div>
              <h4 className="text-base font-semibold text-white">{artifact.name}</h4>
              <p className="mt-1 text-xs text-white/60 font-mono">{artifact.content_type || '二进制文件'}</p>
            </div>
          </div>
        )}

        {/* Navigation Arrows */}
        {items.length > 1 && (
          <>
            {currentIndex > 0 && (
              <button
                type="button"
                onClick={() => onSelectIndex(currentIndex - 1)}
                className="absolute left-6 top-1/2 -translate-y-1/2 rounded-full border border-white/20 bg-black/70 p-3.5 text-white backdrop-blur transition hover:bg-white hover:text-black active:scale-95 shadow-xl"
                title="上一张 (←)" aria-label="上一张"
              >
                <svg className="size-6" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5">
                  <path d="M15 18l-6-6 6-6" />
                </svg>
              </button>
            )}

            {currentIndex < items.length - 1 && (
              <button
                type="button"
                onClick={() => onSelectIndex(currentIndex + 1)}
                className="absolute right-6 top-1/2 -translate-y-1/2 rounded-full border border-white/20 bg-black/70 p-3.5 text-white backdrop-blur transition hover:bg-white hover:text-black active:scale-95 shadow-xl"
                title="下一张 (→)" aria-label="下一张"
              >
                <svg className="size-6" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5">
                  <path d="M9 18l6-6-6-6" />
                </svg>
              </button>
            )}
          </>
        )}
      </div>

      {/* Footer Carousel / Thumbnails */}
      {items.length > 1 && (
        <div className="flex items-center justify-center gap-2 border-t border-white/10 bg-black/60 py-3.5 px-6 overflow-x-auto">
          {items.map((item, idx) => (
            <button
              key={item.artifactId}
              type="button"
              onClick={() => onSelectIndex(idx)}
              className={cx(
                'flex items-center gap-1.5 rounded-lg px-3 py-1.5 text-xs font-mono transition-all',
                idx === currentIndex
                  ? 'bg-accent text-accent-fg font-semibold shadow-md scale-105'
                  : 'bg-white/10 text-white/70 hover:bg-white/20 hover:text-white',
              )}
              title={`切换到 ${item.artifact.name}`}
            >
              {item.mediaType === 'image' && <IconImage className="size-3.5" />}
              {item.mediaType === 'video' && <IconVideo className="size-3.5" />}
              <span className="truncate max-w-[120px]">{item.artifact.name || `输出 ${idx + 1}`}</span>
            </button>
          ))}
        </div>
      )}
    </div>
  )
}
