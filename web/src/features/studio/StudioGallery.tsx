import { useMemo, useState } from 'react'
import { Link } from 'react-router-dom'
import type { GalleryItem } from '@/api/types'
import { GalleryGrid, GalleryLightbox } from '@/components/gallery/GalleryPrimitives'
import { Button } from '@/components/ui/primitives'
import { IconGallery } from '@/components/layout/icons'
import { galleryForMedia, type StudioMedia } from './catalog/models'

export function StudioGallery({ items, media, search, loading, error, onRetry, onUse, canCreate }: {
  items: GalleryItem[]; media: StudioMedia; search: string; loading: boolean; error: boolean; onRetry: () => void; onUse: (item: GalleryItem) => void; canCreate: boolean
}) {
  const [preview, setPreview] = useState<GalleryItem | null>(null)
  // Keep Studio a bounded preview. The full Gallery owns deeper browsing.
  const visible = useMemo(() => galleryForMedia(items, media, search).slice(0, 12), [items, media, search])
  if (loading) return <div className="grid grid-cols-2 gap-3" aria-label="正在加载灵感">{Array.from({length:6}, (_, index) => <div key={index} className="skeleton aspect-[4/3] rounded-2xl" />)}</div>
  if (error) return <div className="studio-empty"><IconGallery className="size-8 text-muted" /><h2 className="text-base font-semibold">灵感加载失败</h2><Button size="sm" onClick={onRetry}>重试</Button></div>
  return <div className="space-y-5">
    <div className="flex flex-wrap items-center justify-between gap-2 text-xs text-muted"><span>来自社区公开的媒体与参数快照</span><Link to="/gallery" className="text-accent hover:underline">浏览完整 Gallery →</Link></div>
    {visible.length ? <GalleryGrid items={visible} canRemix={canCreate} onView={setPreview} onRemix={onUse} />
      : <div className="studio-empty"><div className="studio-empty-mark"><IconGallery className="size-8" /></div><h2 className="text-base font-semibold">{search ? '没有匹配的已加载灵感' : '这里等待你的第一份灵感'}</h2><p className="max-w-sm text-sm leading-6 text-muted">生成作品并发布到 Gallery 后，即可在这里查看和复用公开参数。</p><Link to="/gallery" className="studio-text-action text-accent">前往 Gallery</Link></div>}
    {!canCreate && <p className="text-xs text-muted">当前角色为只读，可以浏览灵感；使用参数需要 member 或更高角色。</p>}
    {preview && <GalleryLightbox item={preview} canRemix={canCreate} onClose={() => setPreview(null)} onRemix={() => { setPreview(null); onUse(preview) }} />}
  </div>
}
