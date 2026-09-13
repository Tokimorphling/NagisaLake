import { useMemo, useRef, useState } from 'react'
import { Link } from 'react-router-dom'
import type { JobSummary, Workflow } from '@/api/types'
import { Button, cx } from '@/components/ui/primitives'
import { JobStateBadge } from '@/components/ui/display'
import { IconExternalLink, IconGrid, IconJobs, IconList, IconRefresh, IconSparkles } from '@/components/layout/icons'
import { formatRelative } from '@/lib/format'
import { filterHistory, promptText, workflowTitle } from './catalog/models'
import { StudioArtifact } from './StudioArtifact'

export function StudioHistory({ jobs, workflows, organizationId, search, loading, error, fetching, hasMore, loadingMore, onMore, onRetry, onInspiration, onUsePrompt, selectedId, onSelect, visible, canCreate }: {
  jobs: JobSummary[]; workflows: Workflow[]; organizationId: string; search: string; loading: boolean; error: boolean; fetching: boolean; hasMore: boolean; loadingMore: boolean
  onMore: () => void; onRetry: () => void; onInspiration: () => void; onUsePrompt: (text: string) => void; selectedId: string; onSelect: (id: string) => void; visible: boolean; canCreate: boolean
}) {
  const [filter, setFilter] = useState<'all' | 'active' | 'completed' | 'failed'>('all')
  const [grid, setGrid] = useState(false)
  const hero = useRef<HTMLDivElement>(null)
  const filtered = useMemo(() => filterHistory(jobs, search, filter), [jobs, search, filter])
  const selected = filtered.find((job) => job.id === selectedId) ?? filtered.find((job) => job.output_artifact_ids.length > 0) ?? filtered[0]
  if (loading && jobs.length === 0) return <div className="space-y-4" aria-label="正在加载历史任务"><div className="skeleton aspect-video rounded-2xl" /><div className="skeleton h-16 rounded-xl" /><div className="skeleton h-16 rounded-xl" /></div>
  if (error && jobs.length === 0) return <div className="studio-empty"><IconJobs className="size-8 text-muted" /><h2 className="text-base font-semibold">历史任务加载失败</h2><Button size="sm" onClick={onRetry}>重试</Button></div>
  return <div className="space-y-5">
    {error && <p role="alert" className="rounded-lg bg-warning/10 p-3 text-xs text-warning">暂时无法同步，当前显示上次读取的任务。<button type="button" className="ml-2 underline" onClick={onRetry}>重试</button></p>}
    <div className="flex flex-wrap items-center justify-between gap-3">
      <div className="flex flex-wrap gap-1" role="group" aria-label="筛选任务状态">
        {([['all','全部'],['active','进行中'],['completed','已完成'],['failed','失败']] as const).map(([key, label]) => <button key={key} type="button" onClick={() => setFilter(key)} aria-pressed={filter === key} className={cx('rounded-lg px-3 py-1.5 text-xs transition', filter === key ? 'bg-surface font-medium text-text shadow-sm' : 'text-muted hover:text-text')}>{label}</button>)}
      </div>
      <div className="flex gap-1" role="group" aria-label="任务列表显示方式">
        <button type="button" className="studio-rail-tool !size-8" onClick={() => setGrid(false)} aria-pressed={!grid} aria-label="列表显示"><IconList className={cx('size-4', !grid && 'text-accent')} /></button>
        <button type="button" className="studio-rail-tool !size-8" onClick={() => setGrid(true)} aria-pressed={grid} aria-label="卡片显示"><IconGrid className={cx('size-4', grid && 'text-accent')} /></button>
        <button type="button" className="studio-rail-tool !size-8" disabled={fetching} onClick={onRetry} aria-label="刷新任务"><IconRefresh className={cx('size-3.5', fetching && 'animate-spin')} /></button>
      </div>
    </div>
    {!selected ? <div className="studio-empty"><div className="studio-empty-mark"><IconJobs className="size-8" /></div><h2 className="text-base font-semibold">{jobs.length ? '没有匹配的已加载任务' : '你的作品，从这里开始'}</h2><p className="max-w-sm text-sm leading-6 text-muted">{jobs.length ? '试试其他关键词或状态，也可以加载更早的任务。' : '从左侧选择工作流并提交任务。执行进度与输出会自动出现在这里。'}</p>{!jobs.length && <Button size="sm" variant="ghost" onClick={onInspiration}><IconSparkles className="size-3.5" />先看看灵感</Button>}</div>
      : <div ref={hero}><JobResult key={`${organizationId}/${selected.id}`} job={selected} workflow={workflows.find((item) => item.id === selected.workflow_id && item.version === selected.workflow_version)} organizationId={organizationId} onUsePrompt={onUsePrompt} visible={visible} canCreate={canCreate} /></div>}
    {filtered.length > 0 && <div className="space-y-3"><div className="flex items-center justify-between text-xs text-muted"><span>本组织任务 · 已加载 {jobs.length} 项</span><span className="text-[10px] text-subtle">选择任务查看输出</span></div>
      <ul className={cx('grid gap-2', grid && 'xl:grid-cols-2')}>
        {filtered.map((job) => <li key={job.id}><button type="button" className="studio-job-row" aria-pressed={selected?.id === job.id} onClick={() => { onSelect(job.id); hero.current?.scrollIntoView({ block: 'start', behavior: 'auto' }) }}>
          <span className="grid size-10 shrink-0 place-items-center rounded-xl bg-surface-2 text-muted"><IconJobs className="size-4" /></span>
          <span className="min-w-0 flex-1"><span className="block truncate text-xs font-medium">{job.workflow_id}</span><span className="mt-1 block truncate text-[11px] text-subtle">{promptText(job.parameters) || job.id}</span></span>
          <span className="shrink-0 space-y-1.5 text-right"><JobStateBadge state={job.state} /><span className="block text-[10px] text-subtle">{formatRelative(job.created_at_unix_ms)}</span></span>
        </button></li>)}
      </ul>
    </div>}
    {hasMore && <Button className="w-full" size="sm" variant="ghost" disabled={loadingMore} onClick={onMore}>{loadingMore ? '正在加载…' : '加载更早的任务'}</Button>}
    <p className="text-center text-[10px] text-subtle">搜索仅匹配已加载的任务；历史和输出按当前组织授权读取。</p>
  </div>
}

function JobResult({ job, workflow, organizationId, onUsePrompt, visible, canCreate }: {
  job: JobSummary; workflow?: Workflow; organizationId: string; onUsePrompt: (text: string) => void; visible: boolean; canCreate: boolean
}) {
  const [artifactIndex, setArtifactIndex] = useState(0)
  const artifactId = job.output_artifact_ids[artifactIndex] ?? job.output_artifact_ids[0]
  const prompt = promptText(job.parameters)
  const progress = typeof job.progress === 'number' && Number.isFinite(job.progress) ? Math.min(100, Math.max(0, Math.round(job.progress * 100))) : null
  const metrics = Object.entries(job.parameters ?? {}).filter(([name, value]) => /^(width|height|duration|duration_seconds|fps|seed|steps|resolution|aspect_ratio|cfg)$/i.test(name) && (typeof value === 'number' || typeof value === 'string')).slice(0, 5)
  return <article className="studio-result-card" aria-label="当前任务输出">
    <header className="space-y-3 p-4 sm:p-5">
      <div className="flex flex-wrap items-center justify-between gap-2"><h2 className="min-w-0 truncate text-sm font-semibold">{workflow ? workflowTitle(workflow) : job.workflow_id}</h2><JobStateBadge state={job.state} /></div>
      {prompt && <p className="line-clamp-2 break-words text-xs leading-6 text-muted">{prompt}</p>}
      <div className="flex flex-wrap items-center gap-2 text-[10px] text-subtle"><span>{job.workflow_version}</span><span>·</span><span>{formatRelative(job.created_at_unix_ms)}</span>{metrics.map(([name, value]) => <span className="studio-pill" key={name}>{name} {String(value).slice(0, 30)}</span>)}</div>
    </header>
    {artifactId ? <StudioArtifact key={artifactId} artifactId={artifactId} organizationId={organizationId} visible={visible} />
      : <div className="studio-media-stage"><div className="w-full max-w-sm space-y-4 px-6 py-10 text-center"><IconJobs className="mx-auto size-9 text-muted" /><p className="text-sm text-muted">{job.state === 'failed' ? '这次生成未能完成' : job.state === 'cancelled' ? '任务已取消' : job.state === 'completed' ? '任务已完成，暂无可预览输出' : 'Worker 正在处理你的任务'}</p>{progress !== null && !['failed','cancelled'].includes(job.state) && <div><div className="h-1.5 overflow-hidden rounded bg-border"><div className="h-full rounded bg-accent transition-[width]" style={{width:`${progress}%`}} /></div><p className="mt-2 text-xs tabular-nums text-subtle">{progress}%</p></div>}{job.error && <p className="break-words text-xs leading-5 text-danger">{job.error}</p>}</div></div>}
    <footer className="flex flex-wrap items-center justify-between gap-2 border-t border-border/60 px-4 py-3">
      <button type="button" className="studio-text-action" disabled={!prompt || !canCreate} onClick={() => onUsePrompt(prompt)}><IconSparkles className="size-3.5" />使用提示词</button>
      <div className="flex items-center gap-3">
        {job.output_artifact_ids.length > 1 && <select aria-label="选择任务输出" className="rounded-lg border border-border bg-surface-2 px-2 py-1.5 text-xs" value={Math.min(artifactIndex, job.output_artifact_ids.length - 1)} onChange={(event) => setArtifactIndex(Number(event.target.value))}>{job.output_artifact_ids.map((id, index) => <option key={id} value={index}>输出 {index + 1}</option>)}</select>}
        <Link className="studio-text-action" to={`/jobs/${encodeURIComponent(job.id)}`}><IconExternalLink className="size-3.5" />任务详情</Link>
      </div>
    </footer>
  </article>
}
