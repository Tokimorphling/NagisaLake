import { useEffect, useMemo, useRef, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { useDevices, useGalleryItems, useJobs, useWorkflows } from '@/api/queries'
import type { GalleryItem, Workflow } from '@/api/types'
import { formatRelative } from '@/lib/format'
import { useAuth } from '@/state/auth'
import { IconAudio, IconExternalLink, IconImage, IconPlay, IconSparkles, IconVideo } from '@/components/layout/icons'
import { Badge, EmptyState, JobStateBadge } from '@/components/ui/display'
import { Button, Card, Textarea, cx } from '@/components/ui/primitives'
import { GalleryGrid, GalleryLightbox } from '@/components/gallery/GalleryPrimitives'
import { JobForm } from '@/pages/workflows/JobForm'
import { AgentPanel } from '@/features/agent/AgentPanel'
import { modelsForMedia, resolveStudioWorkflow, STUDIO_MODELS, type StudioMedia, type StudioModel } from '@/features/studio/catalog/models'

function mediaFromRoute(value: string | undefined): StudioMedia {
  if (value === 'image' || value === 'audio' || value === 'avatar') return value
  return 'video'
}

function workflowTitle(workflow: Workflow): string {
  return workflow.manifest?.display_name || workflow.id
}

function promptField(workflow: Workflow | null): string | null {
  return workflow?.manifest?.inputs.find((input) => input.kind === 'parameter' && /prompt|text|description/i.test(input.name))?.name ?? null
}

export function StudioPage() {
  const { media: mediaParam } = useParams<{ media: string }>()
  const media = mediaFromRoute(mediaParam)
  const { organizationId, atLeast } = useAuth()
  const navigate = useNavigate()
  const workflows = useWorkflows(organizationId)
  const devices = useDevices(organizationId, atLeast('member'))
  const jobs = useJobs(organizationId)
  const gallery = useGalleryItems()
  const models = useMemo(() => modelsForMedia(media), [media])
  const [modelId, setModelId] = useState(models[0]?.id ?? STUDIO_MODELS[0]?.id ?? '')
  const [modeId, setModeId] = useState('i2v')
  const [prompt, setPrompt] = useState('')
  const [tab, setTab] = useState<'inspiration' | 'history'>('inspiration')
  const [formOpen, setFormOpen] = useState(false)
  const [seedParameters, setSeedParameters] = useState<Record<string, unknown> | undefined>()
  const [previewItem, setPreviewItem] = useState<GalleryItem | null>(null)
  const promptRef = useRef<HTMLTextAreaElement>(null)

  useEffect(() => {
    if (!models.some((model) => model.id === modelId)) setModelId(models[0]?.id ?? '')
  }, [modelId, models])

  const selectedModel: StudioModel | null = models.find((model) => model.id === modelId) ?? models[0] ?? null
  const selectedMode = selectedModel?.modes.find((mode) => mode.id === modeId) ?? selectedModel?.modes[0] ?? null
  const MediaIcon = media === 'image' ? IconImage : media === 'audio' ? IconAudio : media === 'avatar' ? IconSparkles : IconVideo
  const selectedWorkflow = useMemo(
    () => (selectedModel && selectedMode ? resolveStudioWorkflow(workflows.data ?? [], selectedModel, selectedMode) : null),
    [selectedMode, selectedModel, workflows.data],
  )
  const promptName = promptField(selectedWorkflow)
  const recentJobs = jobs.data?.pages.flatMap((page) => page.items).slice(0, 12) ?? []
  const inspiration = gallery.data?.slice(0, 12) ?? []

  useEffect(() => {
    if (!selectedModel?.modes.some((mode) => mode.id === modeId)) setModeId(selectedModel?.modes[0]?.id ?? '')
  }, [modeId, selectedModel])

  useEffect(() => {
    const handleShortcut = (event: KeyboardEvent) => {
      const modifier = event.metaKey || event.ctrlKey
      if (modifier && event.key === 'Enter') {
        event.preventDefault()
        if (selectedWorkflow?.available) openGenerator(selectedWorkflow, promptName ? { [promptName]: prompt } : undefined)
      } else if (event.key === '/' && document.activeElement?.tagName !== 'TEXTAREA' && document.activeElement?.tagName !== 'INPUT') {
        event.preventDefault()
        promptRef.current?.focus()
      }
    }
    window.addEventListener('keydown', handleShortcut)
    return () => window.removeEventListener('keydown', handleShortcut)
  }, [prompt, promptName, selectedWorkflow])

  const openGenerator = (workflow: Workflow | null = selectedWorkflow, params?: Record<string, unknown>) => {
    if (!workflow) return
    setSeedParameters(params)
    setFormOpen(true)
  }

  const handleRemix = (item: GalleryItem) => {
    const workflow = workflows.data?.find((candidate) => candidate.id === item.workflow_id)
    if (!workflow) {
      navigate(`/workflows?launch=${encodeURIComponent(item.workflow_id)}&gallery_remix=${encodeURIComponent(item.id)}`, {
        state: { galleryRemix: { itemId: item.id, workflowVersion: item.workflow_version, parameters: item.parameters } },
      })
      return
    }
    openGenerator(workflow, item.parameters)
  }

  const primaryArtifacts = selectedWorkflow?.manifest?.inputs.filter((input) => input.kind === 'artifact') ?? []

  return (
    <div className="grid h-full min-h-0 grid-rows-[minmax(0,1fr)_minmax(0,1fr)] grid-cols-1 lg:grid-rows-1 lg:grid-cols-[minmax(420px,42vw)_minmax(0,1fr)]">
      <section className="min-h-0 overflow-y-auto border-r border-border/80 bg-bg px-5 py-6 sm:px-8">
        <div className="mx-auto max-w-xl space-y-6">
          <div>
            <div className="flex items-center gap-2 text-xs font-medium text-accent"><IconSparkles className="size-3.5" /> 创作模式</div>
            <h1 className="mt-2 text-2xl font-semibold tracking-tight">把想法变成作品</h1>
            <p className="mt-1.5 text-sm text-muted">选择模型和创作方式，剩余参数会根据 Worker manifest 自动准备。</p>
          </div>

          <div className="relative rounded-2xl border border-border bg-surface p-4 shadow-sm">
            <div className="flex items-center justify-between gap-3">
              <div className="flex items-center gap-3">
                <span className="grid size-10 place-items-center rounded-xl bg-accent/12 text-accent"><MediaIcon className="size-5" /></span>
                <div><p className="text-sm font-semibold">模型</p><p className="text-xs text-muted">{selectedModel?.description ?? '从已发布的工作流中选择'}</p></div>
              </div>
              <select value={modelId} onChange={(event) => setModelId(event.target.value)} className="h-9 max-w-40 rounded-lg border border-border bg-surface-2 px-2 text-xs text-text focus:border-accent focus:outline-none">
                {models.map((model) => <option key={model.id} value={model.id}>{model.name}</option>)}
              </select>
            </div>
            {selectedModel && <div className="mt-4 flex flex-wrap gap-2">
              {selectedModel.modes.map((mode) => <button key={mode.id} type="button" onClick={() => setModeId(mode.id)} className={cx('rounded-lg border px-3 py-2 text-xs transition', selectedMode?.id === mode.id ? 'border-accent/50 bg-accent/12 font-semibold text-accent' : 'border-border bg-surface-2/50 text-muted hover:border-border-strong hover:text-text')}>{mode.label}</button>)}
            </div>}
          </div>

          <Card className="overflow-hidden">
            <div className="flex items-center justify-between border-b border-border px-4 py-3">
              <div><p className="text-sm font-semibold">{selectedMode?.label ?? '生成'}</p><p className="mt-0.5 text-xs text-muted">{selectedWorkflow ? workflowTitle(selectedWorkflow) : workflows.isLoading ? '正在查找可用 Workflow…' : '暂无匹配的可用 Workflow'}</p></div>
              {selectedWorkflow && <Badge tone={selectedWorkflow.available ? 'success' : 'warning'}>{selectedWorkflow.available ? '可用' : '排队中'}</Badge>}
            </div>
            <div className="space-y-4 p-4">
              {workflows.isError && <div className="flex items-center justify-between gap-3 rounded-lg border border-danger/30 bg-danger/5 px-3 py-2 text-xs text-danger"><span>Workflow 目录加载失败</span><Button size="sm" variant="ghost" className="h-7 text-danger" onClick={() => void workflows.refetch()}>重试</Button></div>}
              {primaryArtifacts.length > 0 && <div className="space-y-2">
                <p className="text-xs font-medium text-muted">参考素材</p>
                <div className="grid gap-2 sm:grid-cols-2">
                  {primaryArtifacts.slice(0, 2).map((input) => <button key={input.name} type="button" onClick={() => openGenerator()} className="flex min-h-20 items-center justify-center rounded-xl border border-dashed border-border-strong/70 bg-surface-2/40 px-3 text-center text-xs text-muted transition hover:border-accent/60 hover:bg-accent/5 hover:text-accent">+ 上传{input.name}</button>)}
                </div>
              </div>}
              <div className="space-y-2">
                <label htmlFor="studio-prompt" className="text-xs font-medium text-muted">提示词 {promptName && <span className="text-subtle">· {promptName}</span>}</label>
                <Textarea ref={promptRef} id="studio-prompt" value={prompt} onChange={(event) => setPrompt(event.target.value)} placeholder="描述主体、动作、镜头和氛围…" className="min-h-28 resize-y bg-surface-2/60" />
                {organizationId && atLeast('member') && <AgentPanel key={organizationId} organizationId={organizationId} input={prompt} onApply={setPrompt} />}
              </div>
              <div className="flex flex-wrap items-center gap-2 border-t border-border/70 pt-3 text-[11px] text-subtle">
                <span>高级参数在提交面板中按 manifest 展开</span>
                {selectedWorkflow?.manifest?.outputs.map((output) => <Badge key={output.name} tone="neutral">{output.content_type}</Badge>)}
              </div>
              <Button size="md" variant="primary" className="h-11 w-full rounded-xl text-sm" disabled={!selectedWorkflow || !selectedWorkflow.available || workflows.isLoading} onClick={() => openGenerator(selectedWorkflow, promptName ? { [promptName]: prompt } : undefined)}>
                <IconSparkles className="size-4" /> 立即生成
              </Button>
              {!selectedWorkflow && !workflows.isLoading && <p className="text-center text-xs text-warning">请先让 Worker 发布与该创作模式匹配的 manifest。</p>}
            </div>
          </Card>
        </div>
      </section>

      <section className="min-h-0 overflow-y-auto bg-surface/35 px-5 py-6 sm:px-7">
        <div className="mx-auto max-w-4xl">
          <div className="flex items-center justify-between gap-3">
            <div><h2 className="text-lg font-semibold tracking-tight">发现</h2><p className="mt-1 text-xs text-muted">从灵感开始，或继续你的历史任务</p></div>
            <div className="flex rounded-lg border border-border bg-surface p-1"><button type="button" onClick={() => setTab('inspiration')} className={cx('rounded-md px-3 py-1.5 text-xs', tab === 'inspiration' ? 'bg-accent/12 font-semibold text-accent' : 'text-muted')}>灵感模板</button><button type="button" onClick={() => setTab('history')} className={cx('rounded-md px-3 py-1.5 text-xs', tab === 'history' ? 'bg-accent/12 font-semibold text-accent' : 'text-muted')}>历史任务</button></div>
          </div>

          {tab === 'inspiration' ? <InspirationGrid items={inspiration} loading={gallery.isLoading} error={gallery.isError} onRetry={() => void gallery.refetch()} onView={setPreviewItem} onRemix={handleRemix} /> : <HistoryGrid jobs={recentJobs} loading={jobs.isLoading} error={jobs.isError} onRetry={() => void jobs.refetch()} />}
        </div>
      </section>

      {selectedWorkflow && <JobForm key={`${selectedWorkflow.id}@${selectedWorkflow.version}:${formOpen ? JSON.stringify(seedParameters ?? {}) : 'closed'}`} workflow={selectedWorkflow} devices={devices.data ?? []} open={formOpen} onClose={() => { setFormOpen(false); setSeedParameters(undefined) }} initialParameters={seedParameters} />}
      {previewItem && <GalleryLightbox item={previewItem} onClose={() => setPreviewItem(null)} onRemix={() => { setPreviewItem(null); handleRemix(previewItem) }} />}
    </div>
  )
}

function InspirationGrid({ items, loading, error, onRetry, onView, onRemix }: { items: GalleryItem[]; loading: boolean; error: boolean; onRetry: () => void; onView: (item: GalleryItem) => void; onRemix: (item: GalleryItem) => void }) {
  if (loading) return <div className="mt-5 grid grid-cols-2 gap-3 sm:grid-cols-3">{Array.from({ length: 6 }, (_, index) => <div key={index} className="skeleton aspect-[4/5] rounded-xl" />)}</div>
  if (error) return <div className="mt-8 rounded-xl border border-danger/30 bg-danger/5 p-6 text-center"><p className="text-xs text-danger">灵感加载失败</p><Button size="sm" variant="ghost" className="mt-3" onClick={onRetry}>重试</Button></div>
  if (items.length === 0) return <div className="mt-8"><EmptyState title="还没有公开灵感" description="完成一项任务并发布到 Gallery 后，它会出现在这里。" /></div>
  return <GalleryGrid items={items} className="mt-5" compact onView={onView} onRemix={onRemix} />
}

function HistoryGrid({ jobs, loading, error, onRetry }: { jobs: Array<{ id: string; workflow_id: string; state: Parameters<typeof JobStateBadge>[0]['state']; progress: number | null; created_at_unix_ms: number }>; loading: boolean; error: boolean; onRetry: () => void }) {
  if (loading) return <div className="mt-5 space-y-2">{Array.from({ length: 5 }, (_, index) => <div key={index} className="skeleton h-16 rounded-xl" />)}</div>
  if (error) return <div className="mt-8 rounded-xl border border-danger/30 bg-danger/5 p-6 text-center"><p className="text-xs text-danger">历史任务加载失败</p><Button size="sm" variant="ghost" className="mt-3 text-danger" onClick={onRetry}>重试</Button></div>
  if (jobs.length === 0) return <div className="mt-8"><EmptyState title="还没有历史任务" description="提交第一项生成任务后，它会显示在这里。" /></div>
  return <div className="mt-5 space-y-2">{jobs.map((job) => <Link key={job.id} to={`/jobs/${job.id}`} className="block"><Card className="flex items-center gap-3 px-3 py-3 transition hover:border-accent/40 hover:bg-surface-2/50"><span className="grid size-9 shrink-0 place-items-center rounded-lg bg-accent/10 text-accent"><IconPlay className="size-4" /></span><div className="min-w-0 flex-1"><p className="truncate text-xs font-medium">{job.workflow_id}</p><p className="mt-1 text-[10px] text-subtle">{formatRelative(job.created_at_unix_ms)} · {job.id.slice(0, 8)}</p></div><div className="flex shrink-0 items-center gap-2"><JobStateBadge state={job.state} />{job.progress !== null && <span className="font-mono text-[10px] text-subtle">{job.progress}%</span>}<IconExternalLink className="size-3.5 text-subtle" /></div></Card></Link>)}</div>
}
