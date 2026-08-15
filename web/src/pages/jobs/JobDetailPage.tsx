import { useCallback, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { useCancelJob, useJob, useJobEvents } from '@/api/queries'
import type { ArtifactView, JobEvent, JobEventKind } from '@/api/types'
import { TERMINAL_JOB_STATES } from '@/api/types'
import { downloadArtifact } from '@/api/upload'
import { formatDateTime, formatRelative } from '@/lib/format'
import { useAuth } from '@/state/auth'
import { useToast } from '@/state/toast'
import { Page, PageHeader } from '@/components/layout/AppLayout'
import {
  IconCompare,
  IconDownload,
  IconGrid,
  IconList,
  IconShare,
  IconSparkles,
} from '@/components/layout/icons'
import { ConfirmModal, Modal } from '@/components/ui/Modal'
import { Copyable, EmptyState, ErrorState, JobStateBadge, SkeletonRows } from '@/components/ui/display'
import { Button, Card, CardHeader, Select, cx } from '@/components/ui/primitives'
import {
  ArtifactPreviewCard,
  MediaLightboxModal,
  type MediaType,
  type ResolvedArtifact,
} from '@/components/ui/ArtifactPreviewCard'
import { CompareView, type CompareMode } from '@/components/ui/CompareView'
import { ShowcaseModal } from '@/components/ui/ShowcaseModal'

const EVENT_TONES: Record<JobEventKind, string> = {
  accepted: 'bg-info',
  running: 'bg-accent',
  progress: 'bg-accent/60',
  uploading: 'bg-violet',
  completed: 'bg-success',
  failed: 'bg-danger',
  cancelled: 'bg-subtle',
}

type ComparableArtifact = ResolvedArtifact & { mediaType: 'image' | 'video' }

export function JobDetailPage() {
  const { jobId = '' } = useParams()
  const navigate = useNavigate()
  const { organizationId } = useAuth()
  const job = useJob(organizationId, jobId)
  const streamStatus = useJobEvents(organizationId, jobId, Boolean(job.data))
  const cancel = useCancelJob(organizationId)
  const toast = useToast()
  const [confirming, setConfirming] = useState(false)
  const [downloadingAll, setDownloadingAll] = useState(false)
  const [outputLayout, setOutputLayout] = useState<'grid' | 'list'>('grid')
  const [lightboxItems, setLightboxItems] = useState<
    Array<{
      artifactId: string
      downloadUrl: string
      mediaType: MediaType
      artifact: ArtifactView
    }>
  >([])
  const [lightboxIndex, setLightboxIndex] = useState<number | null>(null)
  const [resolvedOutputs, setResolvedOutputs] = useState<Record<string, ResolvedArtifact>>({})
  const [compareOpen, setCompareOpen] = useState(false)
  const [compareMode, setCompareMode] = useState<CompareMode>('slider')
  const [compareLeftId, setCompareLeftId] = useState('')
  const [compareRightId, setCompareRightId] = useState('')
  const [showcaseItem, setShowcaseItem] = useState<ResolvedArtifact | null>(null)

  const handleOutputResolved = useCallback((item: ResolvedArtifact) => {
    setResolvedOutputs((current) =>
      current[item.artifactId]?.downloadUrl === item.downloadUrl
        ? current
        : { ...current, [item.artifactId]: item },
    )
  }, [])

  const handleShowcase = useCallback((item: ResolvedArtifact) => {
    if (item.mediaType === 'image' || item.mediaType === 'video') setShowcaseItem(item)
  }, [])

  const handleDownloadAll = async (outputIds: string[]) => {
    setDownloadingAll(true)
    try {
      for (const id of outputIds) {
        await downloadArtifact(id)
      }
      toast.success('已发起全部输出文件的下载')
    } catch (error) {
      toast.fromError(error, '部分文件下载失败')
    } finally {
      setDownloadingAll(false)
    }
  }

  const handleOpenLightbox = (
    artifactId: string,
    downloadUrl: string,
    mediaType: MediaType,
    artifact: ArtifactView,
  ) => {
    // Collect item or update current list
    setLightboxItems((prev) => {
      const exists = prev.some((item) => item.artifactId === artifactId)
      if (exists) return prev
      return [...prev, { artifactId, downloadUrl, mediaType, artifact }]
    })

    // Find index
    setTimeout(() => {
      setLightboxItems((current) => {
        const idx = current.findIndex((item) => item.artifactId === artifactId)
        if (idx !== -1) {
          setLightboxIndex(idx)
        } else {
          setLightboxItems([{ artifactId, downloadUrl, mediaType, artifact }])
          setLightboxIndex(0)
        }
        return current
      })
    }, 0)
  }

  if (job.isLoading) {
    return (
      <Page>
        <Card>
          <SkeletonRows rows={6} />
        </Card>
      </Page>
    )
  }

  if (job.isError || !job.data) {
    return (
      <Page>
        <Card>
          <ErrorState
            message={(job.error as Error)?.message ?? '作业不存在或不属于当前组织'}
            onRetry={() => void job.refetch()}
          />
        </Card>
      </Page>
    )
  }

  const data = job.data
  const terminal = TERMINAL_JOB_STATES.includes(data.state)
  const parameterEntries = Object.entries(data.parameters ?? {})
  const progressMessage = data.events.reduce<JobEvent | null>((latest, event) => {
    if (event.kind !== 'progress' || event.message.trim() === '') return latest
    return latest === null || event.sequence > latest.sequence ? event : latest
  }, null)?.message.trim()
  const comparableOutputs = data.output_artifact_ids
    .map((artifactId) => resolvedOutputs[artifactId])
    .filter(
      (item): item is ComparableArtifact =>
        Boolean(item && (item.mediaType === 'image' || item.mediaType === 'video')),
    )
  const compareLeft =
    comparableOutputs.find((item) => item.artifactId === compareLeftId) ?? comparableOutputs[0]
  const compareRight =
    comparableOutputs.find((item) => item.artifactId === compareRightId) ?? comparableOutputs[1]

  const openComparison = () => {
    if (comparableOutputs.length < 2) return
    setCompareLeftId(comparableOutputs[0].artifactId)
    setCompareRightId(comparableOutputs[1].artifactId)
    setCompareMode(
      comparableOutputs[0].mediaType === 'image' && comparableOutputs[1].mediaType === 'image'
        ? 'slider'
        : 'side-by-side',
    )
    setCompareOpen(true)
  }

  return (
    <Page>
      <PageHeader
        title={`作业 ${data.id.slice(0, 12)}…`}
        description={
          <span className="flex flex-col items-start gap-1 sm:flex-row sm:items-center sm:gap-2">
            <span className="font-medium text-text">
              {data.workflow_id} · <span className="font-mono text-subtle">{data.workflow_version}</span>
            </span>
            <Copyable value={data.id} display={data.id} className="max-w-full text-accent" />
          </span>
        }
        actions={
          <div className="flex items-center gap-2">
            <Link to="/jobs">
              <Button size="sm" variant="ghost">
                ← 返回列表
              </Button>
            </Link>
            <Button
              size="sm"
              variant="secondary"
              onClick={() => {
                navigate(`/workflows?remix_job_id=${data.id}`)
              }}
              title="以此作业参数填充重新发起"
            >
              <IconSparkles className="size-3.5 text-accent" />
              Remix 重跑
            </Button>
            <Button
              size="sm"
              variant="danger"
              disabled={terminal}
              onClick={() => setConfirming(true)}
            >
              取消作业
            </Button>
          </div>
        }
      />

      <div className="grid items-start gap-6 lg:grid-cols-[1fr_21rem]">
        <div className="min-w-0 space-y-6">
          {/* Status Card */}
          <Card className="overflow-hidden">
            <CardHeader
              title="作业状态"
              actions={
                <div className="flex items-center gap-2.5">
                  {!terminal && (
                    <span
                      role="status"
                      aria-live="polite"
                      className="flex items-center gap-1.5 rounded-full bg-accent/10 px-2.5 py-0.5 text-[10px] font-mono text-accent"
                    >
                      <span
                        className={cx(
                          'size-1.5 rounded-full',
                          streamStatus === 'live' ? 'bg-accent animate-ping' : 'bg-warning',
                        )}
                      />
                      {streamStatus === 'live' ? '实时连接' : '正在重连'}
                    </span>
                  )}
                  <JobStateBadge state={data.state} />
                </div>
              }
            />
            <div className="space-y-5 p-6">
              {data.progress !== null && (
                <div className="space-y-2">
                  <div className="flex justify-between text-xs">
                    <span className="font-medium text-muted">实时执行进度</span>
                    <span className="font-mono font-semibold tabular-nums text-accent">
                      {Math.round(data.progress * 100)}%
                    </span>
                  </div>
                  <div className="h-2.5 overflow-hidden rounded-full bg-surface-2 ring-1 ring-border/50">
                    <div
                      className="h-full rounded-full bg-gradient-to-r from-accent via-cyan-400 to-violet transition-[width] duration-500 shadow-md animate-glow"
                      style={{ width: `${Math.round(data.progress * 100)}%` }}
                    />
                  </div>
                </div>
              )}

              {!terminal && progressMessage && (
                <div className="rounded-xl border border-info/30 bg-info/10 p-3.5 text-xs leading-relaxed text-info backdrop-blur-sm">
                  <span className="font-semibold block mb-0.5">执行动态</span>
                  <p className="break-words whitespace-pre-wrap">{progressMessage}</p>
                </div>
              )}

              {data.error && (
                <div className="rounded-xl border border-danger/30 bg-danger/10 p-4">
                  <p className="text-xs font-semibold text-danger flex items-center gap-1.5">
                    <span>⚠️</span> 执行失败
                  </p>
                  <p className="mt-1.5 font-mono text-[11px] leading-relaxed break-all text-danger/90">
                    {data.error}
                  </p>
                </div>
              )}

              <dl className="grid gap-4 text-xs sm:grid-cols-2 pt-1 border-t border-border/60">
                <Detail label="执行设备 (Worker ID)">
                  <Copyable value={data.worker_id} />
                </Detail>
                <Detail label="ComfyUI Prompt ID">
                  {data.prompt_id ? <Copyable value={data.prompt_id} /> : <span className="text-subtle">—</span>}
                </Detail>
                <Detail label="创建时间">{formatDateTime(data.created_at_unix_ms)}</Detail>
                <Detail label="最后更新">
                  {formatDateTime(data.updated_at_unix_ms)}
                  <span className="ml-1.5 text-subtle">
                    ({formatRelative(data.updated_at_unix_ms)})
                  </span>
                </Detail>
              </dl>
            </div>
          </Card>

          {/* Media & Output Section */}
          <Card>
            <CardHeader
              title={
                <div className="flex items-center gap-2">
                  <span>输出预览 (Outputs)</span>
                  {data.output_artifact_ids.length > 0 && (
                    <span className="rounded-full bg-accent/15 px-2.5 py-0.5 text-[11px] font-mono font-semibold text-accent border border-accent/30">
                      {data.output_artifact_ids.length} 个文件
                    </span>
                  )}
                </div>
              }
              description={
                data.output_artifact_ids.length > 0
                  ? '点击图片或视频可进行全屏放大、旋转、缩放与画质全屏预览。'
                  : undefined
              }
              actions={
                data.output_artifact_ids.length > 0 ? (
                  <div className="flex items-center gap-2">
                    <Button
                      size="sm"
                      variant="secondary"
                      disabled={comparableOutputs.length < 2}
                      onClick={openComparison}
                      title={
                        comparableOutputs.length < 2
                          ? '至少需要两个已加载的图片或视频输出'
                          : '选择两个输出进行 AB 对比'
                      }
                    >
                      <IconCompare className="size-3.5" />
                      AB 对比
                    </Button>

                    <Button
                      size="sm"
                      variant="secondary"
                      disabled={comparableOutputs.length === 0}
                      onClick={() => setShowcaseItem(comparableOutputs[0] ?? null)}
                      title="把输出与生成参数合成为分享卡片"
                    >
                      <IconShare className="size-3.5" />
                      参数卡
                    </Button>

                    {/* View mode toggle */}
                    <div className="flex rounded-lg border border-border/80 bg-surface-2 p-0.5 backdrop-blur-sm">
                      <button
                        type="button"
                        onClick={() => setOutputLayout('grid')}
                        className={cx(
                          'flex items-center gap-1 rounded-md px-2.5 py-1 text-xs font-medium transition-all duration-150',
                          outputLayout === 'grid'
                            ? 'bg-surface text-accent shadow-xs font-semibold'
                            : 'text-muted hover:text-text',
                        )}
                        title="网格视图"
                      >
                        <IconGrid className="size-3.5" />
                        网格
                      </button>
                      <button
                        type="button"
                        onClick={() => setOutputLayout('list')}
                        className={cx(
                          'flex items-center gap-1 rounded-md px-2.5 py-1 text-xs font-medium transition-all duration-150',
                          outputLayout === 'list'
                            ? 'bg-surface text-accent shadow-xs font-semibold'
                            : 'text-muted hover:text-text',
                        )}
                        title="列表视图"
                      >
                        <IconList className="size-3.5" />
                        列表
                      </button>
                    </div>

                    {/* Batch download */}
                    {data.output_artifact_ids.length > 1 && (
                      <Button
                        size="sm"
                        variant="secondary"
                        loading={downloadingAll}
                        onClick={() => void handleDownloadAll(data.output_artifact_ids)}
                      >
                        <IconDownload className="size-3.5" />
                        全部下载
                      </Button>
                    )}
                  </div>
                ) : undefined
              }
            />

            {data.output_artifact_ids.length === 0 ? (
              <EmptyState
                title={terminal ? '没有输出对象' : '等待输出生成...'}
                description={
                  terminal
                    ? '该作业没有产生输出，或输出尚未回传。'
                    : '作业完成后输出的图片、视频或音效会自动展示在此处。'
                }
              />
            ) : (
              <div className="p-6">
                {outputLayout === 'grid' ? (
                  <div className="grid gap-5 sm:grid-cols-1 md:grid-cols-2">
                    {data.output_artifact_ids.map((artifactId, index) => (
                      <ArtifactPreviewCard
                        key={artifactId}
                        artifactId={artifactId}
                        index={index}
                        layout="grid"
                        onOpenLightbox={handleOpenLightbox}
                        onResolved={handleOutputResolved}
                        onShowcase={handleShowcase}
                      />
                    ))}
                  </div>
                ) : (
                  <div className="space-y-3">
                    {data.output_artifact_ids.map((artifactId, index) => (
                      <ArtifactPreviewCard
                        key={artifactId}
                        artifactId={artifactId}
                        index={index}
                        layout="list"
                        onOpenLightbox={handleOpenLightbox}
                        onResolved={handleOutputResolved}
                        onShowcase={handleShowcase}
                      />
                    ))}
                  </div>
                )}
              </div>
            )}
          </Card>

          {/* Parameters Section */}
          {parameterEntries.length > 0 && (
            <Card>
              <CardHeader title="提交参数" description={`共 ${parameterEntries.length} 项参数`} />
              <dl className="divide-y divide-border/60">
                {parameterEntries.map(([name, value]) => (
                  <div key={name} className="grid gap-1 px-6 py-3.5 sm:grid-cols-[11rem_1fr] hover:bg-surface-2/40 transition">
                    <dt className="font-mono text-xs font-medium text-muted">{name}</dt>
                    <dd className="font-mono text-xs break-all whitespace-pre-wrap text-text">
                      {typeof value === 'string' ? value : JSON.stringify(value, null, 2)}
                    </dd>
                  </div>
                ))}
              </dl>
            </Card>
          )}

          {/* Input Artifacts Section */}
          {data.input_artifact_ids.length > 0 && (
            <Card>
              <CardHeader title="输入对象 (Inputs)" description="按 manifest 顺序对应 Worker 的输入位，支持图片/视频预览。" />
              <div className="p-6 space-y-3">
                {data.input_artifact_ids.map((artifactId, index) => (
                  <ArtifactPreviewCard
                    key={artifactId}
                    artifactId={artifactId}
                    index={index}
                    layout="list"
                    onOpenLightbox={handleOpenLightbox}
                  />
                ))}
              </div>
            </Card>
          )}
        </div>

        {/* Timeline Events Sidebar */}
        <Card className="lg:sticky lg:top-6 lg:self-start overflow-hidden">
          <CardHeader title="事件日志 (Events)" description={`累计 ${data.events.length} 条记录`} />
          {data.events.length === 0 ? (
            <EmptyState title="暂无事件记录" />
          ) : (
            <ol className="space-y-0 p-5 max-h-[70vh] overflow-y-auto">
              {[...data.events]
                .sort((a, b) => b.sequence - a.sequence)
                .map((event, index, all) => (
                  <EventRow key={event.sequence} event={event} last={index === all.length - 1} />
                ))}
            </ol>
          )}
        </Card>
      </div>

      <ConfirmModal
        open={confirming}
        title="取消作业"
        destructive
        confirmLabel="确认取消"
        loading={cancel.isPending}
        description="Hub 会向执行设备发送取消指令。已产生的输出对象不会被删除。"
        onConfirm={async () => {
          try {
            await cancel.mutateAsync(data.id)
            toast.success('已请求取消作业')
          } catch (error) {
            toast.fromError(error, '取消作业失败')
          } finally {
            setConfirming(false)
          }
        }}
        onClose={() => setConfirming(false)}
      />

      <Modal
        open={compareOpen && Boolean(compareLeft && compareRight)}
        width="lg"
        title="AB 输出对比"
        description="图片支持拖动分割线或并排查看；视频自动使用并排模式，便于独立控制播放进度。"
        onClose={() => setCompareOpen(false)}
      >
        {compareLeft && compareRight && (
          <div className="space-y-4">
            <div className="grid gap-3 sm:grid-cols-2">
              <label className="space-y-1.5 text-xs font-medium text-muted">
                <span>A · 基准输出</span>
                <Select
                  value={compareLeft.artifactId}
                  onChange={(event) => setCompareLeftId(event.target.value)}
                >
                  {comparableOutputs.map((item, index) => (
                    <option key={item.artifactId} value={item.artifactId}>
                      输出 {index + 1} · {item.artifact.name || item.artifactId.slice(0, 8)}
                    </option>
                  ))}
                </Select>
              </label>
              <label className="space-y-1.5 text-xs font-medium text-muted">
                <span>B · 对比输出</span>
                <Select
                  value={compareRight.artifactId}
                  onChange={(event) => setCompareRightId(event.target.value)}
                >
                  {comparableOutputs.map((item, index) => (
                    <option key={item.artifactId} value={item.artifactId}>
                      输出 {index + 1} · {item.artifact.name || item.artifactId.slice(0, 8)}
                    </option>
                  ))}
                </Select>
              </label>
            </div>
            <CompareView
              left={{ ...compareLeft, label: compareLeft.artifact.name || '输出 A' }}
              right={{ ...compareRight, label: compareRight.artifact.name || '输出 B' }}
              mode={compareMode}
              onModeChange={setCompareMode}
            />
          </div>
        )}
      </Modal>

      {showcaseItem &&
        (showcaseItem.mediaType === 'image' || showcaseItem.mediaType === 'video') && (
          <ShowcaseModal
            open
            job={data}
            artifact={showcaseItem.artifact}
            mediaKind={showcaseItem.mediaType}
            onClose={() => setShowcaseItem(null)}
          />
        )}

      {/* Lightbox Preview Modal */}
      {lightboxIndex !== null && lightboxItems.length > 0 && (
        <MediaLightboxModal
          items={lightboxItems}
          currentIndex={lightboxIndex}
          onClose={() => setLightboxIndex(null)}
          onSelectIndex={(index) => setLightboxIndex(index)}
        />
      )}
    </Page>
  )
}

function Detail({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="space-y-1">
      <dt className="text-muted">{label}</dt>
      <dd className="min-w-0">{children}</dd>
    </div>
  )
}

function EventRow({ event, last }: { event: JobEvent; last: boolean }) {
  return (
    <li className="flex gap-3">
      <div className="flex flex-col items-center self-stretch">
        <span
          className={cx('mt-1 size-2 shrink-0 rounded-full', EVENT_TONES[event.kind] ?? 'bg-subtle')}
          aria-hidden="true"
        />
        {!last && <span className="mt-1 w-px flex-1 bg-border-strong" aria-hidden="true" />}
      </div>
      <div className={cx('min-w-0 flex-1', last ? 'pb-0' : 'pb-4')}>
        <div className="flex flex-wrap items-baseline justify-between gap-x-2">
          <span className="text-xs font-medium">{event.kind}</span>
          <span className="font-mono text-[10px] text-subtle">
            {formatDateTime(event.unix_ms)}
          </span>
        </div>
        {event.message && (
          <p className="mt-0.5 text-[11px] leading-relaxed break-words text-muted">
            {event.message}
          </p>
        )}
        {event.progress !== null && (
          <p className="mt-0.5 font-mono text-[10px] text-subtle">
            {Math.round(event.progress * 100)}%
          </p>
        )}
      </div>
    </li>
  )
}
