import { useCallback, useEffect, useRef, useState } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { endpoints } from '@/api/endpoints'
import { keys } from '@/api/queries'
import { fetchArtifactContent } from '@/api/upload'
import type { ArtifactView, Job } from '@/api/types'
import { formatDateTime } from '@/lib/format'
import { PosterTaintError, posterToBlob, renderPosterWithFallback } from '@/lib/poster'
import { formatDurationMs, jobDurationMs, showcaseFields } from '@/lib/showcase'
import { useTheme } from '@/state/theme'
import { useToast } from '@/state/toast'
import { Modal } from './Modal'
import { Button, Spinner, cx } from './primitives'
import { IconCopy, IconDownload, IconRefresh, IconShare } from '@/components/layout/icons'

/**
 * Composes a shareable parameter card from a job output.
 *
 * Rendering happens on a detached canvas via renderPoster; this component owns
 * the preview, the theme choice, and the export paths (download / clipboard).
 */
export function ShowcaseModal({
  open,
  job,
  artifact,
  mediaKind,
  onClose,
}: {
  open: boolean
  job: Job
  artifact: ArtifactView
  mediaKind: 'image' | 'video'
  onClose: () => void
}) {
  const toast = useToast()
  const queryClient = useQueryClient()
  const { resolved } = useTheme()
  const [posterTheme, setPosterTheme] = useState<'dark' | 'light'>(resolved)
  const [previewUrl, setPreviewUrl] = useState<string | null>(null)
  const [rendering, setRendering] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [shared, setShared] = useState(false)
  const [sharing, setSharing] = useState(false)
  const canvasRef = useRef<HTMLCanvasElement | null>(null)
  const previewUrlRef = useRef<string | null>(null)
  // Guards against an out-of-order render (theme toggled twice quickly)
  // overwriting the preview with a stale canvas.
  const renderToken = useRef(0)

  const build = useCallback(async () => {
    const token = ++renderToken.current
    setRendering(true)
    setError(null)
    try {
      const fields = showcaseFields(job)
      const duration = formatDurationMs(jobDurationMs(job))
      const metrics = [...fields.metrics]
      metrics.push({ label: '耗时', value: duration })
      if (metrics.length % 3 !== 0) {
        metrics.push({ label: 'Workflow', value: job.workflow_version })
      }

      const { download: mediaTicket } = await endpoints.download(artifact.id)
      const { canvas, blob } = await renderPosterWithFallback(
        {
          mediaKind,
          title: job.workflow_id,
          subtitle: `${artifact.name} · ${job.id.slice(0, 12)}…`,
          metrics: metrics.slice(0, 6),
          footer: formatDateTime(job.created_at_unix_ms),
          theme: posterTheme,
        },
        mediaTicket,
        () => fetchArtifactContent(artifact.id),
      )
      if (token !== renderToken.current) return

      canvasRef.current = canvas
      const nextPreviewUrl = URL.createObjectURL(blob)
      if (previewUrlRef.current) URL.revokeObjectURL(previewUrlRef.current)
      previewUrlRef.current = nextPreviewUrl
      setPreviewUrl(nextPreviewUrl)
    } catch (caught) {
      if (token !== renderToken.current) return
      const message =
        caught instanceof PosterTaintError
          ? caught.message
          : caught instanceof Error
            ? caught.message
            : '合成分享卡片失败'
      setError(message)
    } finally {
      if (token === renderToken.current) setRendering(false)
    }
  }, [artifact.id, artifact.name, job, mediaKind, posterTheme])

  useEffect(() => {
    if (!open) return
    void build()
  }, [build, open])

  useEffect(() => {
    setShared(false)
  }, [artifact.id])

  // Invalidate an in-flight build and release the last preview when unmounting.
  useEffect(
    () => () => {
      renderToken.current += 1
      if (previewUrlRef.current) URL.revokeObjectURL(previewUrlRef.current)
      previewUrlRef.current = null
    },
    [],
  )

  const download = () => {
    const canvas = canvasRef.current
    if (!canvas) return
    void (async () => {
      try {
        const blob = await posterToBlob(canvas)
        const url = URL.createObjectURL(blob)
        const anchor = document.createElement('a')
        anchor.href = url
        anchor.download = `nagisalake-${job.workflow_id}-${job.id.slice(0, 8)}.png`
        document.body.appendChild(anchor)
        anchor.click()
        anchor.remove()
        // Revoke on the next tick; revoking synchronously can cancel the download.
        window.setTimeout(() => URL.revokeObjectURL(url), 10_000)
        toast.success('分享卡片已导出为 PNG')
      } catch (caught) {
        toast.fromError(caught, '导出失败')
      }
    })()
  }

  const copy = () => {
    const canvas = canvasRef.current
    if (!canvas) return
    void (async () => {
      try {
        if (!('clipboard' in navigator) || typeof ClipboardItem === 'undefined') {
          throw new Error('当前浏览器或非 HTTPS origin 不支持图片写入剪贴板，请改用导出 PNG')
        }
        const blob = await posterToBlob(canvas)
        await navigator.clipboard.write([new ClipboardItem({ 'image/png': blob })])
        toast.success('分享卡片已复制到剪贴板')
      } catch (caught) {
        toast.fromError(caught, '复制到剪贴板失败')
      }
    })()
  }

  const shareToGallery = async () => {
    if (sharing || shared) return
    setSharing(true)
    try {
      await endpoints.publishGalleryItem(artifact.id)
      await queryClient.invalidateQueries({ queryKey: keys.gallery })
      setShared(true)
      toast.success('已成功分享到公共 Gallery')
    } catch (caught) {
      toast.fromError(caught, '分享到 Gallery 失败')
    } finally {
      setSharing(false)
    }
  }

  return (
    <Modal
      open={open}
      width="lg"
      title="导出与分享参数卡"
      description="把输出与生成参数合成为一张可分享的图片，或直接一键共享至公共 Gallery。"
      onClose={onClose}
      footer={
        <div className="flex w-full flex-wrap items-center justify-between gap-2">
          <div className="flex rounded-lg border border-border/80 bg-surface-2 p-0.5">
            {(['dark', 'light'] as const).map((option) => (
              <button
                key={option}
                type="button"
                onClick={() => setPosterTheme(option)}
                className={cx(
                  'rounded-md px-3 py-1 text-xs transition',
                  posterTheme === option
                    ? 'bg-surface text-accent font-semibold shadow-xs'
                    : 'text-muted hover:text-text',
                )}
              >
                {option === 'dark' ? '深色卡片' : '浅色卡片'}
              </button>
            ))}
          </div>
          <div className="flex items-center gap-2">
            <Button size="sm" variant="ghost" onClick={() => void build()} disabled={rendering}>
              <IconRefresh className="size-3.5" />
              重新合成
            </Button>
            <Button size="sm" variant="secondary" onClick={copy} disabled={rendering || !previewUrl}>
              <IconCopy className="size-3.5" />
              复制图片
            </Button>
            <Button size="sm" variant="secondary" onClick={download} disabled={rendering || !previewUrl}>
              <IconDownload className="size-3.5" />
              导出 PNG
            </Button>
            <Button
              size="sm"
              variant="primary"
              onClick={() => void shareToGallery()}
              loading={sharing}
              disabled={sharing || shared}
            >
              <IconShare className="size-3.5" />
              {shared ? '已分享至 Gallery' : sharing ? '正在分享…' : '分享到 Gallery'}
            </Button>
          </div>
        </div>
      }
    >
      <div className="min-h-[18rem]">
        {rendering && (
          <div className="flex flex-col items-center justify-center gap-3 py-16 text-xs text-muted">
            <Spinner className="size-6 text-accent" />
            正在合成分享卡片…
          </div>
        )}

        {!rendering && error && (
          <div className="rounded-xl border border-danger/30 bg-danger/10 p-4 text-xs leading-relaxed text-danger">
            <p className="font-semibold">无法合成分享卡片</p>
            <p className="mt-1.5">{error}</p>
          </div>
        )}

        {!rendering && !error && previewUrl && (
          <div className="flex justify-center">
            <img
              src={previewUrl}
              alt="分享卡片预览"
              className="max-h-[60vh] w-auto rounded-xl border border-border shadow-lg"
            />
          </div>
        )}
      </div>
    </Modal>
  )
}
