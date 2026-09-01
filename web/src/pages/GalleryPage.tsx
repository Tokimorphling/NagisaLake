import { useMemo, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { useGalleryItems, useUnpublishGalleryItem, type GalleryItem } from '@/state/gallery'
import { endpoints } from '@/api/endpoints'
import { fetchGalleryItemContent } from '@/api/gallery'
import { formatDateTime } from '@/lib/format'
import { renderPosterWithFallback } from '@/lib/poster'
import { useTheme } from '@/state/theme'
import { useToast } from '@/state/toast'
import { Page, PageHeader } from '@/components/layout/AppLayout'
import { IconImage, IconSearch } from '@/components/layout/icons'
import { EmptyState, ErrorState, SkeletonRows } from '@/components/ui/display'
import { Button, Card, Input, cx } from '@/components/ui/primitives'
import { ConfirmModal } from '@/components/ui/Modal'
import { GalleryGrid, GalleryLightbox, galleryFields } from '@/components/gallery/GalleryPrimitives'

type SortKey = 'newest' | 'oldest'
type FilterKind = 'all' | GalleryItem['media_kind']

function safeFilePart(value: string): string {
  return value.replace(/[^a-z0-9._-]+/gi, '-').replace(/^-+|-+$/g, '') || 'gallery'
}

function startBlobDownload(blob: Blob, fileName: string): void {
  const url = URL.createObjectURL(blob)
  const anchor = document.createElement('a')
  anchor.href = url
  anchor.download = fileName
  document.body.appendChild(anchor)
  anchor.click()
  anchor.remove()
  window.setTimeout(() => URL.revokeObjectURL(url), 10_000)
}

export function GalleryPage() {
  const gallery = useGalleryItems()
  const unpublish = useUnpublishGalleryItem()
  const { resolved: posterTheme } = useTheme()
  const toast = useToast()
  const navigate = useNavigate()
  const [query, setQuery] = useState('')
  const [sort, setSort] = useState<SortKey>('newest')
  const [filterKind, setFilterKind] = useState<FilterKind>('all')
  const [lightbox, setLightbox] = useState<GalleryItem | null>(null)
  const [downloadingId, setDownloadingId] = useState<string | null>(null)
  const [pendingDelete, setPendingDelete] = useState<GalleryItem | null>(null)

  const items = gallery.data ?? []
  const filtered = useMemo(() => {
    const needle = query.trim().toLowerCase()
    return items
      .filter((item) => filterKind === 'all' || item.media_kind === filterKind)
      .filter((item) => {
        if (!needle) return true
        const fields = galleryFields(item)
        return item.display_name.toLowerCase().includes(needle) || item.workflow_id.toLowerCase().includes(needle) || fields.prompt?.toLowerCase().includes(needle)
      })
      .sort((left, right) => sort === 'newest' ? right.published_at_unix_ms - left.published_at_unix_ms : left.published_at_unix_ms - right.published_at_unix_ms)
  }, [filterKind, items, query, sort])

  const handleDownloadPoster = async (item: GalleryItem) => {
    if (downloadingId || item.media_kind === 'audio') return
    setDownloadingId(item.id)
    try {
      const { download: mediaTicket } = await endpoints.galleryItemDownload(item.id)
      const fields = galleryFields(item)
      const metrics = [...fields.metrics]
      if (metrics.length % 3 !== 0) metrics.push({ label: 'Workflow', value: item.workflow_version })
      const { blob } = await renderPosterWithFallback({ mediaKind: item.media_kind, title: item.workflow_id, subtitle: `${item.artifact.name} · Gallery`, metrics: metrics.slice(0, 6), footer: formatDateTime(item.published_at_unix_ms), theme: posterTheme }, mediaTicket, () => fetchGalleryItemContent(item.id))
      startBlobDownload(blob, `nagisalake-${safeFilePart(item.workflow_id)}-${item.id.slice(0, 8)}.png`)
      toast.success('参数卡已下载')
    } catch (error) {
      toast.fromError(error, '下载参数卡失败')
    } finally {
      setDownloadingId(null)
    }
  }

  const handleRemix = (item: GalleryItem) => {
    navigate(`/workflows?launch=${encodeURIComponent(item.workflow_id)}&gallery_remix=${encodeURIComponent(item.id)}`, {
      state: { galleryRemix: { itemId: item.id, workflowVersion: item.workflow_version, parameters: item.parameters } },
    })
  }

  const confirmDelete = async () => {
    if (!pendingDelete) return
    try {
      await unpublish.mutateAsync(pendingDelete.id)
      if (lightbox?.id === pendingDelete.id) setLightbox(null)
      toast.success('已从公共 Gallery 移除')
    } catch (error) {
      toast.fromError(error, '移除 Gallery 项目失败')
    } finally {
      setPendingDelete(null)
    }
  }

  if (gallery.isLoading) return <Page><PageHeader title="公共 Gallery" description="正在加载共享媒体参数卡…" /><Card><SkeletonRows rows={6} /></Card></Page>
  if (gallery.isError) return <Page><PageHeader title="公共 Gallery" description="浏览已登录用户共享的多媒体参数卡。" /><Card><ErrorState message={(gallery.error as Error).message} onRetry={() => void gallery.refetch()} /></Card></Page>

  return <Page><PageHeader title="公共 Gallery" description="浏览全站共享的多媒体参数卡；发布内容仅对已登录用户可见。" actions={<div className="flex items-center gap-2"><span className="text-xs font-mono text-subtle">已加载 {items.length} 张卡片</span><Button size="sm" variant="ghost" loading={gallery.isFetching} onClick={() => void gallery.refetch()}>刷新</Button></div>} /><div className="mb-5 flex flex-wrap items-center gap-3"><div className="relative"><IconSearch className="absolute left-3 top-1/2 size-3.5 -translate-y-1/2 text-subtle" /><Input value={query} placeholder="搜索名称、Workflow 或 Prompt" className="max-w-xs pl-9" onChange={(event) => setQuery(event.target.value)} /></div><div className="flex rounded-lg border border-border/80 bg-surface-2 p-0.5">{([['all', '全部'], ['image', '图片'], ['video', '视频'], ['audio', '音频']] as const).map(([key, label]) => <button key={key} type="button" onClick={() => setFilterKind(key)} className={cx('rounded-md px-3 py-1.5 text-xs transition', filterKind === key ? 'bg-surface font-semibold text-accent shadow-xs' : 'text-muted hover:text-text')}>{label}</button>)}</div><div className="flex rounded-lg border border-border/80 bg-surface-2 p-0.5">{([['newest', '最新'], ['oldest', '最早']] as const).map(([key, label]) => <button key={key} type="button" onClick={() => setSort(key)} className={cx('rounded-md px-3 py-1.5 text-xs transition', sort === key ? 'bg-surface font-semibold text-accent shadow-xs' : 'text-muted hover:text-text')}>{label}</button>)}</div></div>{filtered.length === 0 ? <Card><EmptyState icon={<IconImage className="size-8" />} title={items.length === 0 ? 'Gallery 还没有分享内容' : '没有匹配结果'} description={items.length === 0 ? '在已完成作业的参数卡弹窗中点击「分享到 Gallery」即可发布。' : '调整搜索条件或切换筛选类型。'} /></Card> : <GalleryGrid items={filtered} onView={setLightbox} onRemix={handleRemix} onDownload={(item) => void handleDownloadPoster(item)} onDelete={setPendingDelete} downloadingId={downloadingId} deletingId={unpublish.isPending ? unpublish.variables : null} />}{gallery.hasNextPage && <div className="mt-6 flex justify-center border-t border-border pt-4"><Button size="sm" loading={gallery.isFetchingNextPage} onClick={() => void gallery.fetchNextPage()}>加载更多</Button></div>}{lightbox && <GalleryLightbox item={lightbox} onClose={() => setLightbox(null)} onDownload={() => void handleDownloadPoster(lightbox)} onRemix={() => handleRemix(lightbox)} downloading={downloadingId === lightbox.id} />}<ConfirmModal open={pendingDelete !== null} title="取消发布" description="确认从公共 Gallery 移除？此操作不可撤销。" destructive confirmLabel="取消发布" loading={unpublish.isPending} onConfirm={() => void confirmDelete()} onClose={() => setPendingDelete(null)} /></Page>
}
