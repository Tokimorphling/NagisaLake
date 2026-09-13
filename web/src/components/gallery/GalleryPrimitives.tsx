import { useEffect, useMemo, useState } from 'react'
import type { GalleryItem } from '@/api/types'
import { useGalleryDownload } from '@/api/queries'
import { formatDateTime, formatRelative } from '@/lib/format'
import { parseGalleryPrompt } from '@/lib/gallery-prompt'
import { showcaseFieldsFromParameters } from '@/lib/showcase'
import { useToast } from '@/state/toast'
import { IconAudio, IconClose, IconCopy, IconDownload, IconImage, IconSparkles, IconVideo } from '@/components/layout/icons'
import { Badge } from '@/components/ui/display'
import { Button, Card, cx } from '@/components/ui/primitives'

export function galleryFields(item: GalleryItem) {
  return showcaseFieldsFromParameters(item.parameters)
}

export function MediaKindBadge({ kind }: { kind: GalleryItem['media_kind'] }) {
  const icon = kind === 'image' ? <IconImage className="size-3" /> : kind === 'video' ? <IconVideo className="size-3" /> : <IconAudio className="size-3" />
  return <Badge tone={kind === 'image' ? 'accent' : 'violet'}>{icon}{kind === 'image' ? '图片' : kind === 'video' ? '视频' : '音频'}</Badge>
}

export function GalleryMedia({ item, expanded = false, previewOnHover = false }: { item: GalleryItem; expanded?: boolean; previewOnHover?: boolean }) {
  const media = useGalleryDownload(item.id)
  const [loaded, setLoaded] = useState(false)
  const url = media.data?.download.url ?? null

  useEffect(() => setLoaded(false), [url])

  if (media.isError) return <div className="flex size-full flex-col items-center justify-center gap-2 p-5 text-center text-xs text-muted"><span>媒体票据已失效或加载失败</span><Button size="sm" variant="ghost" onClick={() => void media.refetch()}>重试</Button></div>
  if (!url) return <div className="size-full skeleton" />
  if (item.media_kind === 'video') return <video key={url} src={url} controls={expanded} muted={!expanded || previewOnHover} playsInline preload="metadata" className={cx('size-full object-contain transition-opacity duration-300', loaded ? 'opacity-100' : 'opacity-0')} onLoadedData={() => setLoaded(true)} onMouseEnter={(event) => { if (previewOnHover) void event.currentTarget.play().catch(() => undefined) }} onMouseLeave={(event) => { if (previewOnHover) { event.currentTarget.pause(); event.currentTarget.currentTime = 0 } }} onError={() => void media.refetch()} />
  if (item.media_kind === 'audio') return <div className="flex size-full flex-col items-center justify-center gap-4 bg-gradient-to-br from-violet/20 to-accent/10 p-6"><IconAudio className="size-12 text-accent" />{expanded && <audio key={url} src={url} controls preload="metadata" className="w-full max-w-xl" onError={() => void media.refetch()} />}</div>
  return <img key={url} src={url} alt={item.display_name} loading={expanded ? 'eager' : 'lazy'} decoding="async" sizes="(max-width: 640px) 100vw, (max-width: 1280px) 50vw, 33vw" className={cx('size-full object-contain transition-opacity duration-300', loaded ? 'opacity-100' : 'opacity-0')} onLoad={() => setLoaded(true)} onError={() => void media.refetch()} />
}

export function GalleryCard({ item, onView, onDownload, onRemix, onDelete, downloading = false, deleting = false, canRemix = true }: { item: GalleryItem; onView: () => void; onDownload?: () => void; onRemix: () => void; onDelete?: () => void; downloading?: boolean; deleting?: boolean; canRemix?: boolean }) {
  const fields = galleryFields(item)
  return <Card className="group flex flex-col overflow-hidden transition-all duration-300 hover:-translate-y-0.5 hover:border-accent/40 hover:shadow-lg"><div className="relative aspect-[4/3] w-full overflow-hidden border-b border-border/60 bg-black/30"><GalleryMedia item={item} previewOnHover /><button type="button" onClick={onView} className="absolute inset-0 flex items-center justify-center bg-black/40 opacity-0 backdrop-blur-[2px] transition-opacity duration-200 group-hover:opacity-100 focus-visible:opacity-100" aria-label={`查看 ${item.display_name}`}><span className="rounded-full bg-accent px-4 py-2 text-xs font-semibold text-accent-fg shadow-xl">查看媒体与参数</span></button></div><div className="flex-1 space-y-2.5 p-4"><div className="flex items-start justify-between gap-2"><div className="min-w-0"><p className="truncate text-sm font-semibold tracking-tight">{item.display_name}</p><p className="mt-0.5 truncate font-mono text-[10px] text-subtle">{item.workflow_id} · {item.workflow_version}</p></div><MediaKindBadge kind={item.media_kind} /></div>{fields.prompt && <p className="line-clamp-2 text-xs leading-relaxed text-muted">{fields.prompt}</p>}{fields.metrics.length > 0 && <div className="flex flex-wrap gap-1.5">{fields.metrics.slice(0, 4).map((metric) => <span key={metric.label} className="rounded-md border border-border/60 bg-surface-2/60 px-2 py-0.5 text-[10px] font-mono text-muted">{metric.label}: {metric.value.length > 16 ? `${metric.value.slice(0, 14)}…` : metric.value}</span>)}</div>}</div><div className="flex items-center justify-between border-t border-border/60 px-4 py-2.5"><span className="text-[11px] text-subtle">{formatRelative(item.published_at_unix_ms)}</span><div className="flex items-center gap-1"><Button size="sm" variant="ghost" onClick={onRemix} disabled={!canRemix} title={canRemix ? '用公开参数运行此 Workflow' : '需要 member 或更高角色'}><IconSparkles className="size-3.5 text-accent" /></Button>{item.media_kind !== 'audio' && onDownload && <Button size="sm" variant="ghost" onClick={onDownload} loading={downloading} title="下载参数卡 PNG"><IconDownload className="size-3.5" /></Button>}{item.can_unpublish && onDelete && <Button size="sm" variant="ghost" onClick={onDelete} loading={deleting} title="取消发布"><IconClose className="size-3.5" /></Button>}</div></div></Card>
}

export function GalleryGrid({
  items,
  onView,
  onRemix,
  onDownload,
  onDelete,
  downloadingId,
  deletingId,
  className,
  compact = false,
  canRemix = true,
}: {
  items: GalleryItem[]
  onView: (item: GalleryItem) => void
  onRemix: (item: GalleryItem) => void
  onDownload?: (item: GalleryItem) => void
  onDelete?: (item: GalleryItem) => void
  downloadingId?: string | null
  deletingId?: string | null
  className?: string
  compact?: boolean
  canRemix?: boolean
}) {
  return <div className={cx(compact ? 'grid grid-cols-2 gap-3 sm:grid-cols-3 xl:grid-cols-4' : 'grid gap-5 sm:grid-cols-2 xl:grid-cols-3', className)}>{items.map((item) => <GalleryCard key={item.id} item={item} onView={() => onView(item)} onRemix={() => onRemix(item)} onDownload={onDownload ? () => onDownload(item) : undefined} onDelete={onDelete ? () => onDelete(item) : undefined} downloading={downloadingId === item.id} deleting={deletingId === item.id} canRemix={canRemix} />)}</div>
}

export function GalleryLightbox({ item, onClose, onDownload, onRemix, downloading = false, canRemix = true }: { item: GalleryItem; onClose: () => void; onDownload?: () => void; onRemix: () => void; downloading?: boolean; canRemix?: boolean }) {
  const [zoom, setZoom] = useState(1)
  const fields = galleryFields(item)
  const promptSections = useMemo(() => (fields.prompt ? parseGalleryPrompt(fields.prompt) : []), [fields.prompt])
  const toast = useToast()

  const copyPrompt = async () => {
    if (!fields.prompt) return
    try { await navigator.clipboard.writeText(fields.prompt); toast.success('Prompt 已复制到剪贴板') } catch (error) { toast.fromError(error, '复制 Prompt 失败') }
  }

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') onClose()
      else if (item.media_kind === 'image' && (event.key === '+' || event.key === '=')) setZoom((value) => Math.min(4, value + 0.25))
      else if (item.media_kind === 'image' && event.key === '-') setZoom((value) => Math.max(0.3, value - 0.25))
    }
    window.addEventListener('keydown', handleKeyDown)
    return () => window.removeEventListener('keydown', handleKeyDown)
  }, [item.media_kind, onClose])

  return <div className="fixed inset-0 z-50 flex flex-col overflow-hidden bg-black/95 backdrop-blur-2xl animate-fade-in-up" role="dialog" aria-modal="true" aria-label={`${item.display_name} 的媒体与生成参数`}><div className="flex shrink-0 flex-wrap items-center justify-between gap-3 border-b border-white/10 bg-black/40 px-3 py-3 text-white sm:px-6 sm:py-4"><div className="flex min-w-0 items-center gap-3"><MediaKindBadge kind={item.media_kind} /><div className="min-w-0"><h3 className="truncate text-sm font-semibold tracking-tight">{item.display_name}</h3><p className="text-[11px] font-mono text-white/60">{item.workflow_id}</p></div></div><div className="flex shrink-0 flex-wrap items-center gap-2"><button type="button" disabled={!canRemix} onClick={onRemix} className="inline-flex h-9 items-center gap-1.5 rounded-lg bg-white/10 px-3 text-xs font-semibold text-white transition hover:bg-white/20 disabled:cursor-not-allowed disabled:opacity-40"><IconSparkles className="size-3.5 text-accent" /><span className="hidden sm:inline">运行 Workflow</span><span className="sm:hidden">Remix</span></button>{item.media_kind !== 'audio' && onDownload && <Button size="sm" variant="primary" onClick={onDownload} loading={downloading}><IconDownload className="size-4" /><span className="hidden sm:inline">下载 PNG</span><span className="sm:hidden">下载</span></Button>}<button type="button" onClick={onClose} className="inline-flex size-9 items-center justify-center rounded-lg bg-white/10 text-white transition hover:bg-red-500/80 sm:ml-2" title="关闭 (Esc)" aria-label="关闭"><IconClose className="size-5" /></button></div></div><div className="min-h-0 flex-1 overflow-y-auto lg:overflow-hidden"><div className="grid min-h-full lg:h-full lg:grid-cols-[minmax(0,1fr)_minmax(360px,440px)]"><div className="relative flex h-[52dvh] min-h-80 items-center justify-center overflow-auto p-3 select-none sm:p-6 lg:h-full lg:min-h-0"><div className="h-full w-full max-w-6xl overflow-hidden rounded-xl shadow-2xl" style={item.media_kind === 'image' ? { transform: `scale(${zoom})` } : undefined}><GalleryMedia item={item} expanded /></div>{item.media_kind === 'image' && <div className="absolute bottom-5 left-1/2 flex -translate-x-1/2 items-center rounded-xl border border-white/10 bg-black/70 p-1 shadow-lg backdrop-blur sm:bottom-8"><button type="button" onClick={() => setZoom((value) => Math.max(0.3, value - 0.25))} className="inline-flex size-8 items-center justify-center rounded-lg text-white transition hover:bg-white/15" aria-label="缩小图片">−</button><span className="w-14 text-center font-mono text-xs text-white/80">{Math.round(zoom * 100)}%</span><button type="button" onClick={() => setZoom((value) => Math.min(4, value + 0.25))} className="inline-flex size-8 items-center justify-center rounded-lg text-white transition hover:bg-white/15" aria-label="放大图片">+</button><button type="button" onClick={() => setZoom(1)} className="inline-flex h-8 items-center rounded-lg px-2 text-[11px] font-mono text-white/80 transition hover:bg-white/15 hover:text-white">重置</button></div>}</div><aside className="border-t border-white/10 bg-black/60 text-white lg:min-h-0 lg:overflow-y-auto lg:border-l lg:border-t-0"><div className="space-y-6 p-4 sm:p-6"><div className="flex items-start justify-between gap-4 border-b border-white/10 pb-5"><div className="min-w-0"><p className="font-mono text-[10px] font-semibold uppercase tracking-wider text-white/45">Workflow 版本</p><p className="mt-1 break-words text-sm font-semibold text-white/90">{item.workflow_version}</p></div><time dateTime={new Date(item.published_at_unix_ms).toISOString()} className="shrink-0 text-right text-[11px] text-white/50">{formatDateTime(item.published_at_unix_ms)}</time></div>{fields.metrics.length > 0 && <section aria-labelledby="gallery-metrics-heading"><h4 id="gallery-metrics-heading" className="font-mono text-[10px] font-semibold uppercase tracking-[0.16em] text-white/50">生成参数</h4><div className="mt-3 grid grid-cols-2 gap-2.5">{fields.metrics.map((metric) => <div key={metric.label} className={cx('min-w-0 rounded-lg border border-white/10 bg-white/5 px-3 py-2.5', metric.value.length > 28 && 'col-span-2')}><span className="block font-mono text-[10px] uppercase tracking-wider text-white/45">{metric.label}</span><span className="mt-1 block break-words text-sm font-semibold text-white/90 [overflow-wrap:anywhere]">{metric.value}</span></div>)}</div></section>}{fields.prompt && <section aria-labelledby="gallery-prompt-heading"><div className="flex items-center justify-between gap-3"><h4 id="gallery-prompt-heading" className="font-mono text-[10px] font-semibold uppercase tracking-[0.16em] text-accent">Prompt</h4><button type="button" onClick={() => void copyPrompt()} className="inline-flex h-8 shrink-0 items-center gap-1.5 rounded-lg bg-white/10 px-2.5 text-[11px] font-semibold text-white transition hover:bg-white/20"><IconCopy className="size-3.5" />复制</button></div><div className="mt-3 space-y-2.5">{promptSections.map((section, index) => <div key={`${section.label ?? 'prompt'}-${index}`} className="rounded-lg border border-white/10 bg-white/[0.04] px-3.5 py-3">{section.label && <h5 className="mb-1.5 text-xs font-semibold tracking-wide text-accent/90">{section.label}</h5>}<p className="whitespace-pre-wrap break-words text-xs leading-5 text-white/75 [overflow-wrap:anywhere]">{section.value}</p></div>)}</div></section>}{fields.negative && <section aria-labelledby="gallery-negative-heading"><h4 id="gallery-negative-heading" className="font-mono text-[10px] font-semibold uppercase tracking-[0.16em] text-white/50">Negative</h4><p className="mt-3 whitespace-pre-wrap break-words rounded-lg border border-white/10 bg-white/[0.04] px-3.5 py-3 text-xs leading-5 text-white/65 [overflow-wrap:anywhere]">{fields.negative}</p></section>}</div></aside></div></div></div>
}
