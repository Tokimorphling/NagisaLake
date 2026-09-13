import { useDeferredValue, useEffect, useMemo, useRef, useState } from 'react'
import { Link, Navigate, useLocation, useNavigate, useParams } from 'react-router-dom'
import { useDevices, useGalleryItems, useJobs, useWorkflows } from '@/api/queries'
import type { GalleryItem, JobSummary } from '@/api/types'
import { useAuth } from '@/state/auth'
import { useToast } from '@/state/toast'
import { IconChevron, IconGallery, IconJobs, IconSearch, IconSettings, IconSparkles, IconWorkflow } from '@/components/layout/icons'
import { Badge } from '@/components/ui/display'
import { Button, cx } from '@/components/ui/primitives'
import { JobForm } from '@/pages/workflows/JobForm'
import { AgentResult } from '@/features/agent/AgentResult'
import { useAgentRun, useAgentSkills } from '@/features/agent/useAgentRun'
import { MediaTabs, MobilePaneSwitch, PromptEditor, useDesktopStudio } from '@/features/studio/StudioPrimitives'
import { StudioHistory } from '@/features/studio/StudioHistory'
import { StudioGallery } from '@/features/studio/StudioGallery'
import { compatibleParameters, isStudioMedia, MEDIA_LABELS, promptField, promptText, transferredPrompt, workflowKey, workflowsForMedia, workflowTitle, type StudioInputMode, type StudioMedia } from '@/features/studio/catalog/models'
import { availabilityLabel, availabilityTone, capacitySummary, submitBlockedReason, workflowCapacity } from '@/lib/workflow-status'

type ResultTab = 'history' | 'inspiration' | 'assistant'

export function StudioPage() {
  const { media } = useParams<{ media: string }>()
  const { organizationId, user, atLeast } = useAuth()
  if (!isStudioMedia(media)) return <Navigate to="/studio/video" replace />
  if (!organizationId) return <div className="studio-empty"><h1>请先选择组织</h1><Link to="/settings">账户设置</Link></div>
  // A scope change destroys drafts, stale artifact previews and active runs.
  return <StudioWorkspace key={`${user?.id}/${organizationId}/${media}`} organizationId={organizationId} userId={user?.id ?? ''} media={media} canCreate={atLeast('member')} />
}

function StudioWorkspace({ organizationId, userId, media, canCreate }: { organizationId: string; userId: string; media: StudioMedia; canCreate: boolean }) {
  const location = useLocation()
  const navigate = useNavigate()
  const toast = useToast()
  const desktop = useDesktopStudio()
  const workflows = useWorkflows(organizationId)
  const devices = useDevices(organizationId, canCreate)
  const history = useJobs(organizationId)
  const gallery = useGalleryItems()
  const catalog = useAgentSkills(organizationId, canCreate)
  const agent = useAgentRun(organizationId)
  const [mobilePane, setMobilePane] = useState<'composer' | 'results'>('composer')
  const [tab, setTab] = useState<ResultTab>('history')
  const [search, setSearch] = useState('')
  const deferredSearch = useDeferredValue(search)
  const [mode, setMode] = useState<StudioInputMode>('all')
  const [selectedKey, setSelectedKey] = useState('')
  const [prompt, setPrompt] = useState(() => transferredPrompt(location.state, organizationId, userId))
  useEffect(() => {
    if (!location.state || typeof location.state !== 'object' || !('studioPrompt' in location.state)) return
    const remaining = { ...(location.state as Record<string, unknown>) }
    delete remaining.studioPrompt
    // A transfer is one-shot. Do not leave private text in the browser's
    // history entry after importing it into the current user's local draft.
    navigate(`${location.pathname}${location.search}${location.hash}`, { replace: true, state: Object.keys(remaining).length ? remaining : null })
  }, [location.state, location.pathname, location.search, location.hash, navigate])
  const [enhance, setEnhance] = useState(false)
  const [skill, setSkill] = useState('')
  const [preset, setPreset] = useState<{ workflowKey: string; parameters: Record<string, unknown> } | null>(null)
  const [formSeed, setFormSeed] = useState<Record<string, unknown>>()
  const [formOpen, setFormOpen] = useState(false)
  const [selectedJob, setSelectedJob] = useState('')
  const [lastSubmittedId, setLastSubmittedId] = useState('')
  const [receipt, setReceipt] = useState<JobSummary | null>(null)
  const textarea = useRef<HTMLTextAreaElement>(null)
  const choices = useMemo(() => workflowsForMedia(workflows.data ?? [], media, mode), [workflows.data, media, mode])
  useEffect(() => {
    if (!selectedKey && choices.length) setSelectedKey(workflowKey(choices.find((workflow) => workflow.available) ?? choices[0]))
  }, [choices, selectedKey])
  // Never silently substitute a different model when a chosen worker goes away.
  const selected = choices.find((workflow) => workflowKey(workflow) === selectedKey) ?? null
  const capacity = selected ? workflowCapacity(selected) : null
  const blocked = selected && capacity ? submitBlockedReason(selected, capacity) : '请选择已发布的工作流'
  const promptName = promptField(selected)
  const references = selected?.manifest?.inputs.filter((field) => field.kind === 'artifact') ?? []
  const selectedSkill = catalog.data?.items.find((item) => item.id === skill)?.id ?? catalog.data?.items[0]?.id ?? ''
  const agentAvailable = !!catalog.data?.enabled && !!selectedSkill
  const loadedJobs = useMemo(() => history.data?.pages.flatMap((page) => page.items) ?? [], [history.data])
  useEffect(() => { if (receipt && loadedJobs.some((job) => job.id === receipt.id)) setReceipt(null) }, [loadedJobs, receipt])
  const jobs = useMemo(() => receipt && !loadedJobs.some((job) => job.id === receipt.id) ? [receipt, ...loadedJobs] : loadedJobs, [loadedJobs, receipt])

  const openReview = () => {
    if (!canCreate || !selected?.manifest || !selected.manifest_consistent) return
    const base = preset?.workflowKey === workflowKey(selected) ? preset.parameters : {}
    setFormSeed({ ...compatibleParameters(selected, base), ...(promptName ? { [promptName]: prompt } : {}) })
    setFormOpen(true)
  }
  const usePrompt = (text: string) => {
    if (!canCreate) return
    setPrompt(text)
    setMobilePane('composer')
    requestAnimationFrame(() => textarea.current?.focus())
  }
  const useTemplate = (item: GalleryItem) => {
    if (!canCreate) return
    const match = workflows.data?.find((workflow) => workflow.id === item.workflow_id && workflow.version === item.workflow_version)
    if (!match || !workflowsForMedia([match], media).length) {
      navigate(`/workflows?launch=${encodeURIComponent(item.workflow_id)}&gallery_remix=${encodeURIComponent(item.id)}`, { state: { galleryRemix: { itemId: item.id, workflowVersion: item.workflow_version, parameters: item.parameters } } })
      return
    }
    setMode('all'); setSelectedKey(workflowKey(match))
    setPreset({ workflowKey: workflowKey(match), parameters: compatibleParameters(match, item.parameters) })
    usePrompt(promptText(item.parameters))
    toast.info('已应用公开参数', '参考素材不会复用，请在提交面板中重新选择。')
  }
  const enhancePrompt = () => {
    if (!canCreate || !agentAvailable || !prompt.trim() || agent.busy) return
    setTab('assistant'); setMobilePane('results')
    void agent.start({ skill: selectedSkill, input: prompt })
  }
  const defaults = selected?.manifest?.inputs.filter((field) => field.kind === 'parameter' && /^(width|height|fps|duration|duration_seconds|steps|seed|resolution|aspect_ratio)$/i.test(field.name) && field.default !== null && field.default !== undefined && String(field.default).trim() !== '' && typeof field.default !== 'object').slice(0, 2) ?? []
  const canConfigure = canCreate && !!selected?.manifest && selected.manifest_consistent

  return <div className="studio-page">
    <h1 className="sr-only">NagisaLake {MEDIA_LABELS[media]}工作台</h1>
    <MobilePaneSwitch value={mobilePane} onChange={setMobilePane} />
    <div className="studio-workspace">
      <section className={cx('studio-composer', mobilePane !== 'composer' && 'studio-pane-mobile-hidden')} aria-label="创作设置">
        <MediaTabs />
        <div className="studio-composer-body">
          <div className="flex items-center justify-center gap-3 text-xs text-muted"><label htmlFor="studio-input-mode">创作类型</label><select id="studio-input-mode" value={mode} onChange={(event) => { setMode(event.target.value as StudioInputMode); setSelectedKey('') }} className="max-w-full rounded-lg bg-transparent px-2 py-1.5 text-sm font-medium text-text"><option value="all">全部工作流</option><option value="text">文本输入</option><option value="reference">参考素材输入</option></select></div>
          <div>
            <div className="studio-model-select">
              <span className="grid size-11 shrink-0 place-items-center rounded-xl bg-accent/10 text-accent"><IconWorkflow className="size-6" /></span>
              <div className="min-w-0 flex-1"><label htmlFor="studio-workflow" className="mb-1 block text-[10px] text-subtle">模型 / 工作流</label><select id="studio-workflow" aria-label="选择已发布工作流" value={selected ? workflowKey(selected) : ''} disabled={workflows.isPending} onChange={(event) => setSelectedKey(event.target.value)}><option value="" disabled>{workflows.isPending ? '正在读取工作流…' : selectedKey ? '已选工作流暂不可用，请重新选择' : '请选择工作流'}</option>{choices.map((workflow) => <option key={workflowKey(workflow)} value={workflowKey(workflow)}>{workflowTitle(workflow)} · {workflow.version}</option>)}</select></div>
            </div>
            <div className="mt-2 flex flex-wrap items-center justify-between gap-2 text-[11px] text-subtle">
              <span>{capacity ? capacitySummary(capacity) : '来自 Worker 发布的真实能力目录'}</span>
              {capacity && <Badge tone={availabilityTone(capacity.availability)}>{availabilityLabel(capacity.availability)}</Badge>}
            </div>
            {workflows.isError && <p role="alert" className="mt-2 text-xs text-danger">工作流读取失败。<button type="button" className="underline" onClick={() => void workflows.refetch()}>重试</button></p>}
            {!workflows.isPending && !choices.length && <p className="mt-2 text-xs leading-5 text-muted">当前已加载目录中没有匹配的工作流。<Link className="text-accent hover:underline" to="/workflows">查看完整目录</Link></p>}
            {workflows.hasNextPage && <button type="button" className="studio-text-action mt-1" disabled={workflows.isFetchingNextPage} onClick={() => void workflows.fetchNextPage()}>{workflows.isFetchingNextPage ? '加载中…' : '加载更多工作流'}</button>}
          </div>
          {references.length > 0 && <div className="space-y-2"><span className="text-xs text-muted">参考素材 · {references.length} 项</span><div className="flex flex-wrap gap-2">{references.map((field) => <button key={field.name} type="button" className="max-w-full truncate rounded-xl border border-dashed border-border-strong bg-surface-2/50 px-3 py-2.5 text-xs text-muted hover:border-accent disabled:opacity-40" disabled={!canConfigure} onClick={openReview}>＋ {field.name}{field.required ? '（必填）' : ''}</button>)}</div><p className="text-[10px] text-subtle">在提交面板中选择并直传；文本助手不读取参考素材。</p></div>}
          <PromptEditor value={prompt} onChange={setPrompt} textareaRef={textarea} label={promptName ? `提示词 · ${promptName}` : '创作草稿'} placeholder="描述主体、动作、镜头和氛围，让你的想法更具体一点…" onSubmit={openReview} controls={<button type="button" role="switch" aria-checked={enhance} className="studio-switch" disabled={!canCreate || agent.busy} onClick={() => setEnhance((value) => !value)}><span>增强提示词</span><span className="studio-switch-track" aria-hidden="true" /></button>} />
          {enhance && <div className="space-y-2 rounded-xl border border-border bg-surface-2/60 p-3">
            <div className="flex flex-wrap items-center gap-2"><label htmlFor="studio-skill" className="text-xs text-muted">Skill</label><select id="studio-skill" value={selectedSkill} disabled={!agentAvailable || agent.busy} onChange={(event) => setSkill(event.target.value)} className="min-w-0 flex-1 rounded-lg border border-border bg-surface px-2 py-2 text-xs">{catalog.data?.items.map((item) => <option key={item.id} value={item.id}>{item.id}</option>)}</select>{agent.busy ? <Button size="sm" variant="ghost" onClick={agent.cancel}>取消</Button> : <Button size="sm" variant="ghost" disabled={!agentAvailable || !prompt.trim()} onClick={enhancePrompt}><IconSparkles className="size-3.5" />优化提示词</Button>}</div>
            <p className="text-[10px] leading-5 text-muted">{catalog.isError ? '无法读取助手配置。' : catalog.isPending ? '正在读取助手配置…' : !agentAvailable ? 'Hub 尚未启用可用的 Skill。' : '独立执行文本优化，完成后由你确认应用，不会自动生成媒体。'}</p>
            {catalog.isError && <button type="button" className="studio-text-action" onClick={() => void catalog.refetch()}>重试配置读取</button>}
          </div>}
          {selected && !promptName && <p className="text-[11px] text-warning">该 manifest 没有可识别的文本提示词字段。草稿仅用于助手；生成输入请在参数面板中配置。</p>}
        </div>
        <footer className="studio-composer-footer">
          <div className="mb-3 flex flex-wrap items-center justify-between gap-2 text-[11px] text-subtle"><span>{!canCreate ? '只读角色：生成需要 member 或更高权限' : blocked ?? '参数确认后提交，任务将在右侧自动同步'}</span><span className="font-mono">Ctrl / ⌘ ↵</span></div>
          <div className="grid grid-cols-[minmax(0,1fr)_minmax(0,1.15fr)] gap-3">
            <button type="button" className="flex min-h-12 min-w-0 items-center gap-2 rounded-xl border border-border bg-surface-2 px-3 text-left text-xs disabled:opacity-40" disabled={!canConfigure} onClick={openReview}><IconSettings className="size-4 shrink-0 text-muted" /><span className="min-w-0 flex-1 truncate">{defaults.length ? defaults.map((field) => `${field.name} ${String(field.default).slice(0, 18)}`).join(' · ') : '设备与高级参数'}</span><IconChevron className="size-3 shrink-0 -rotate-90" /></button>
            <Button variant="primary" className="studio-submit" disabled={!canCreate || !!blocked} onClick={openReview}><IconSparkles className="size-4" />配置并生成</Button>
          </div>
        </footer>
      </section>
      <section className={cx('studio-results', mobilePane !== 'results' && 'studio-pane-mobile-hidden')} aria-label="结果工作区">
        <header className="studio-pane-header flex-wrap xl:flex-nowrap">
          <div className="studio-result-tabs" role="group" aria-label="结果内容">
            <button type="button" className="studio-result-tab" aria-pressed={tab === 'inspiration'} onClick={() => setTab('inspiration')}><IconGallery className="size-3.5" />灵感模板</button>
            <button type="button" className="studio-result-tab" aria-pressed={tab === 'history'} onClick={() => setTab('history')}><IconJobs className="size-3.5" />历史任务</button>
            <button type="button" className="studio-result-tab" aria-pressed={tab === 'assistant'} onClick={() => setTab('assistant')}><IconSparkles className={cx('size-3.5', agent.busy && 'animate-pulse')} />助手输出</button>
          </div>
          {tab !== 'assistant' && <label className="studio-search ml-auto flex min-w-0 flex-1 items-center gap-2 rounded-xl border border-border bg-surface px-3 py-2.5 xl:max-w-xs"><IconSearch className="size-4 shrink-0 text-subtle" /><input aria-label={tab === 'history' ? '搜索已加载任务' : '搜索已加载灵感'} value={search} onChange={(event) => setSearch(event.target.value)} placeholder={tab === 'history' ? '搜索已加载任务' : '搜索已加载灵感'} className="min-w-0 w-full bg-transparent text-xs outline-none placeholder:text-subtle" /></label>}
        </header>
        <div className="studio-results-scroll">
          {tab === 'history' ? <StudioHistory key={lastSubmittedId} jobs={jobs} workflows={workflows.data ?? []} organizationId={organizationId} search={deferredSearch} loading={history.isPending} error={history.isError} fetching={history.isFetching} hasMore={history.hasNextPage} loadingMore={history.isFetchingNextPage} onMore={() => void history.fetchNextPage()} onRetry={() => void history.refetch()} onInspiration={() => setTab('inspiration')} onUsePrompt={usePrompt} selectedId={selectedJob} onSelect={setSelectedJob} visible={desktop || mobilePane === 'results'} canCreate={canCreate} />
            : tab === 'inspiration' ? <StudioGallery items={gallery.data ?? []} media={media} search={deferredSearch} loading={gallery.isPending} error={gallery.isError} onRetry={() => void gallery.refetch()} onUse={useTemplate} canCreate={canCreate} />
              : <AgentResult state={agent.state} onCancel={agent.cancel} onApply={canCreate ? usePrompt : undefined} applied={!!prompt && prompt === agent.state.output} inputChanged={agent.state.input !== prompt} />}
        </div>
      </section>
    </div>
    {formOpen && selected && <JobForm key={workflowKey(selected)} workflow={selected} devices={devices.data ?? []} open onClose={() => setFormOpen(false)} initialParameters={formSeed} navigateOnSubmit={false} onSubmitted={(job) => { setReceipt(job); setLastSubmittedId(job.id); setSelectedJob(job.id); setTab('history'); setSearch(''); setMobilePane('results') }} />}
  </div>
}
