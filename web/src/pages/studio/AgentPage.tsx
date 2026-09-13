import { useState } from 'react'
import { Link, useNavigate } from 'react-router-dom'
import { useAuth } from '@/state/auth'
import { Button, cx } from '@/components/ui/primitives'
import { IconSparkles, IconWorkflow } from '@/components/layout/icons'
import { AgentResult } from '@/features/agent/AgentResult'
import { useAgentRun, useAgentSkills } from '@/features/agent/useAgentRun'
import { MobilePaneSwitch, PromptEditor } from '@/features/studio/StudioPrimitives'
import { MEDIA_LABELS, STUDIO_MEDIA, type StudioMedia } from '@/features/studio/catalog/models'

export function AgentPage() {
  const { organizationId, user, atLeast } = useAuth()
  if (!organizationId) return <div className="studio-empty"><h1>请先选择组织</h1><Link to="/settings">账户设置</Link></div>
  return <AgentWorkspace key={`${user?.id}/${organizationId}`} organizationId={organizationId} userId={user?.id ?? ''} canRun={atLeast('member')} />
}

function AgentWorkspace({ organizationId, userId, canRun }: { organizationId: string; userId: string; canRun: boolean }) {
  const catalog = useAgentSkills(organizationId, canRun)
  const run = useAgentRun(organizationId)
  const navigate = useNavigate()
  const [prompt, setPrompt] = useState('')
  const [skill, setSkill] = useState('')
  const [destination, setDestination] = useState<StudioMedia>('video')
  const [pane, setPane] = useState<'composer' | 'results'>('composer')
  const selected = catalog.data?.items.find((item) => item.id === skill) ?? catalog.data?.items[0]
  const enabled = canRun && !!catalog.data?.enabled && !!selected
  const start = () => {
    if (!enabled || run.busy || !prompt.trim()) return
    setPane('results')
    void run.start({ skill: selected.id, input: prompt })
  }
  const examples = ['把一句创意扩展成有镜头感的视频提示词', '为纸船漂过安静池塘编写一段画面与声音描述', '让这段描述更简洁、具体，并保留原意']
  return <div className="studio-page">
    <MobilePaneSwitch value={pane} onChange={setPane} resultLabel="执行结果" />
    <div className="studio-workspace">
      <section className={cx('studio-composer', pane !== 'composer' && 'studio-pane-mobile-hidden')} aria-label="Agent 任务设置">
        <header className="studio-pane-header"><IconWorkflow className="size-5 text-accent" /><h1 className="text-sm font-semibold">Agent 工作台</h1><span className="studio-pill ml-auto">文本创作</span></header>
        <div className="studio-composer-body">
          <div><h2 className="text-xl font-semibold tracking-tight">从想法，到更好的表达</h2><p className="mt-2 text-xs leading-6 text-muted">调用经过 Hub 批准的 Skill，实时查看调用过程。这里只生成文本，不会直接启动媒体任务。</p></div>
          <div className="studio-model-select"><span className="grid size-11 shrink-0 place-items-center rounded-xl bg-accent/10 text-accent"><IconSparkles className="size-6" /></span><div className="min-w-0 flex-1"><label htmlFor="agent-workspace-skill" className="mb-1 block text-[10px] text-subtle">Skill / 能力</label><select id="agent-workspace-skill" value={selected?.id ?? ''} disabled={!enabled || run.busy} onChange={(event) => setSkill(event.target.value)}>{!selected && <option value="">{catalog.isPending && canRun ? '正在读取配置…' : '暂无可用 Skill'}</option>}{catalog.data?.items.map((item) => <option key={item.id} value={item.id}>{item.id}</option>)}</select></div></div>
          {catalog.isError && <div role="alert" className="text-xs text-danger">无法读取助手配置。<button type="button" className="ml-2 underline" onClick={() => void catalog.refetch()}>重试</button></div>}
          <PromptEditor value={prompt} onChange={setPrompt} label="你的需求" placeholder="写下你的创意、原始提示词或希望改进的内容…" onSubmit={start} controls={<span className="text-[11px] text-subtle">独立会话 · 由 Hub 路由执行模型</span>} />
          {!prompt && <div className="flex flex-wrap gap-2">{examples.map((example) => <button key={example} type="button" className="rounded-full border border-border bg-surface-2/70 px-3 py-2 text-left text-[11px] text-muted transition hover:border-accent/50 hover:text-text" onClick={() => setPrompt(example)}>{example}</button>)}</div>}
        </div>
        <footer className="studio-composer-footer space-y-3">
          <p className="text-[11px] leading-5 text-subtle">{!canRun ? '当前角色只读，执行 Skill 需要 member 或更高权限。' : !catalog.isPending && !enabled ? '请先在 Hub 配置允许调用的 Skill。' : '只发送本次文本。不会公开工具参数、内部文件或 reasoning。'}</p>
          <Button className="studio-submit w-full" variant="primary" disabled={run.busy ? false : !enabled || !prompt.trim()} onClick={run.busy ? run.cancel : start}><IconSparkles className="size-4" />{run.busy ? '取消执行' : '运行 Skill'}</Button>
        </footer>
      </section>
      <section className={cx('studio-results', pane !== 'results' && 'studio-pane-mobile-hidden')} aria-label="Agent 输出工作区">
        <header className="studio-pane-header"><h2 className="text-sm font-semibold">执行结果</h2><span className="text-[11px] text-subtle">实时进度与最终文本</span><button type="button" className="studio-text-action ml-auto" disabled={run.busy || run.state.phase === 'idle'} onClick={run.reset}>清空结果</button></header>
        <div className="studio-results-scroll space-y-4">
          {run.state.phase === 'completed' && <label className="flex items-center justify-end gap-2 text-xs text-muted">应用到<select value={destination} onChange={(event) => setDestination(event.target.value as StudioMedia)} className="rounded-lg border border-border bg-surface px-3 py-2 text-xs">{STUDIO_MEDIA.map((media) => <option key={media} value={media}>{MEDIA_LABELS[media]}</option>)}</select></label>}
          <AgentResult state={run.state} onCancel={run.cancel} inputChanged={run.state.input !== prompt} applyLabel="用于创作" onApply={canRun ? (text) => navigate(`/studio/${destination}`, {state:{studioPrompt:{organizationId,userId,prompt:text}}}) : undefined} />
        </div>
      </section>
    </div>
  </div>
}
