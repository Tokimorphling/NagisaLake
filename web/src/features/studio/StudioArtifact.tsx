import { useEffect, useRef, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { endpoints } from '@/api/endpoints'
import { Button } from '@/components/ui/primitives'
import { IconAudio, IconDownload, IconFile, IconRefresh } from '@/components/layout/icons'
import { formatBytes } from '@/lib/format'

/** Resolve only the selected output, not one presign request per history row. */
export function StudioArtifact({ artifactId, organizationId, visible }: { artifactId: string; organizationId: string; visible: boolean }) {
  const ticket = useQuery({
    queryKey: ['studio-artifact', organizationId, artifactId],
    queryFn: ({ signal }) => endpoints.download(artifactId, { organizationId, signal }),
    enabled: !!artifactId && visible,
    staleTime: 60_000,
    retry: false,
  })
  const [mediaError, setMediaError] = useState(false)
  const [revision, setRevision] = useState(0)
  const media = useRef<HTMLMediaElement | null>(null)
  const data = ticket.data
  const url = data?.download.url
  useEffect(() => setMediaError(false), [url])
  useEffect(() => { if (!visible) media.current?.pause() }, [visible])
  const retry = () => { setMediaError(false); setRevision((value) => value + 1); void ticket.refetch() }
  if (ticket.isPending) return <div className="studio-media-stage skeleton" aria-label="正在读取输出" />
  if (ticket.isError || !data) return <div className="studio-media-stage"><div className="space-y-3 p-6 text-center text-sm text-muted"><IconFile className="mx-auto size-8" /><p>媒体暂时无法加载，链接可能已经过期。</p><Button size="sm" onClick={retry} disabled={ticket.isFetching}><IconRefresh className="size-3.5" />重新获取预览</Button></div></div>
  const type = data.artifact.content_type.toLowerCase().split(';')[0]
  const safeUrl = typeof url === 'string' && /^https?:\/\//i.test(url)
  const direct = safeUrl && data.download.method === 'GET' && Object.keys(data.download.headers).every((name) => name.toLowerCase() === 'host')
  const image = /^(image\/(png|jpeg|webp|gif|avif|bmp))$/.test(type)
  const video = type.startsWith('video/')
  const audio = type.startsWith('audio/')
  return <div>
    <div className="studio-media-stage">
      {mediaError ? <div className="space-y-3 p-6 text-center text-sm text-muted"><IconFile className="mx-auto size-8" /><p>链接可能过期，或浏览器不支持该格式。你仍可下载输出。</p><Button size="sm" onClick={retry} disabled={ticket.isFetching}>重新获取预览</Button></div>
        : direct && image ? <img key={`${url}-${revision}`} src={url} alt={data.artifact.name} decoding="async" onError={() => setMediaError(true)} />
        : direct && video ? <video key={`${url}-${revision}`} ref={(node) => { media.current = node }} src={url} controls playsInline preload="metadata" onError={() => setMediaError(true)} />
          : direct && audio ? <div className="w-full space-y-6 p-8 text-center"><IconAudio className="mx-auto size-12 text-accent" /><audio key={`${url}-${revision}`} ref={(node) => { media.current = node }} src={url} controls preload="metadata" className="w-full" onError={() => setMediaError(true)} /></div>
            : <div className="space-y-3 p-8 text-center text-sm text-muted"><IconFile className="mx-auto size-8" /><p>该格式或请求头不支持直接预览，请通过任务详情查看。</p></div>}
    </div>
    <div className="flex flex-wrap items-center justify-between gap-2 border-t border-border/60 px-4 py-3 text-xs text-muted">
      <span className="min-w-0 truncate" title={data.artifact.name}>{data.artifact.name} <span className="text-subtle">· {formatBytes(data.artifact.size_bytes)}</span></span>
      {direct && <a className="studio-text-action" href={url} download={data.artifact.name} target="_blank" rel="noopener noreferrer"><IconDownload className="size-4" />下载输出</a>}
    </div>
  </div>
}
