import { useEffect, useMemo, useState } from 'react'
import { IconCheck, IconCopy, IconSparkles, IconClose } from '@/components/layout/icons'
import { Button, cx } from '@/components/ui/primitives'
import { copyText } from '@/lib/format'
import type { AgentRunState } from './useAgentRun'
import { agentSections } from './output'

export function AgentResult({ state, onCancel, onApply, applyLabel = '将结果应用到提示词', applied = false, compact = false, inputChanged = false }: {
  state: AgentRunState; onCancel: () => void; onApply?: (text: string) => void; applyLabel?: string; applied?: boolean; compact?: boolean; inputChanged?: boolean
}) {
  const [copied, setCopied] = useState(false)
  const [copyError, setCopyError] = useState(false)
  const sections = useMemo(() => agentSections(state.output), [state.output])
  useEffect(() => { setCopied(false); setCopyError(false) }, [state.output])
  const busy = state.phase === 'running'
  const completed = state.phase === 'completed'
  const finished = state.stages.filter((stage) => stage.state === 'completed').length

  if (state.phase === 'idle') return <div className={cx('studio-empty', compact && 'py-6')}>
    <div className="studio-empty-mark"><IconSparkles className="size-8" /></div>
    <h2 className="text-base font-semibold">让想法更具体一点</h2>
    <p className="max-w-sm text-sm leading-6 text-muted">选择 Skill，描述你的创意。这里会实时显示工具调用过程与生成结果。</p>
    <div className="mt-2 flex flex-wrap justify-center gap-2 text-[11px] text-subtle"><span className="studio-pill">独立会话</span><span className="studio-pill">实时输出</span><span className="studio-pill">由你确认应用</span></div>
  </div>

  return <article className={cx('studio-agent-result', compact && 'studio-agent-result--compact')} aria-label="Agent 执行结果" aria-busy={busy}>
    <header className="flex flex-wrap items-center gap-3 border-b border-border/60 p-4 sm:p-5">
      <span className={cx('grid size-10 place-items-center rounded-xl', completed ? 'bg-success/10 text-success' : 'bg-accent/10 text-accent')}>
        {completed ? <IconCheck className="size-5" /> : <IconSparkles className={cx('size-5', busy && 'animate-pulse')} />}
      </span>
      <div className="min-w-0 flex-1"><h2 className="truncate text-sm font-semibold">{state.skill}</h2><p className="mt-1 text-xs text-muted" role="status">{state.status}</p></div>
      {busy && !compact && <Button size="sm" variant="ghost" onClick={onCancel}><IconClose className="size-3.5" />取消</Button>}
      {completed && <span className="studio-pill text-success">执行完成</span>}
    </header>
    <div className="space-y-5 p-4 sm:p-5">
      {state.stages.length > 0 && <details className="studio-stage-log" open={busy || state.phase === 'failed'}>
        <summary className="cursor-pointer text-xs font-medium text-muted">调用记录 · {finished}/{state.stages.length} 已完成</summary>
        <ol className="mt-3 space-y-2">
          {state.stages.map((stage, index) => <li key={stage.id} className="flex items-center gap-2.5 rounded-lg bg-surface-2/70 px-3 py-2 text-xs">
            <span className={cx('grid size-5 place-items-center rounded-full', stage.state === 'completed' ? 'bg-success/10 text-success' : stage.state === 'failed' ? 'bg-danger/10 text-danger' : 'bg-accent/10 text-accent')}>
              {stage.state === 'completed' ? <IconCheck className="size-3" /> : index + 1}
            </span>
            <span className="min-w-0 flex-1 truncate">{stage.name === 'skill' ? state.skill : stage.name}</span>
            <span className="text-subtle">{stage.state === 'running' ? '调用中' : stage.state === 'completed' ? '调用成功' : '调用失败'}</span>
          </li>)}
        </ol>
      </details>}
      {inputChanged && !applied && <p className="rounded-lg bg-warning/10 p-3 text-xs leading-5 text-warning">当前草稿已更改。下方结果对应本次执行时的输入，应用前请先确认。</p>}
      {state.error && <p role="alert" className="rounded-xl border border-danger/25 bg-danger/5 p-3 text-sm text-danger">{state.error}</p>}
      {state.output ? <div className="space-y-4" aria-label="生成文本">
        {sections.map((section, index) => <section key={index} className="studio-output-section">
          {section.title && <h3 className="mb-2 break-words text-xs font-semibold text-accent">{section.title}</h3>}
          <p className="whitespace-pre-wrap break-words text-sm leading-7 [overflow-wrap:anywhere]">{section.text}</p>
        </section>)}
      </div> : busy ? <div className="space-y-3 py-4" aria-label="等待 Agent 输出"><div className="skeleton h-3 w-4/5 rounded" /><div className="skeleton h-3 w-full rounded" /><div className="skeleton h-3 w-3/5 rounded" /><p className="pt-2 text-xs text-subtle">正在加载 Skill 与整理思路…</p></div> : <p className="py-4 text-sm text-muted">本次执行未返回可用文本。</p>}
      {state.input && <details className="border-t border-border/50 pt-3 text-xs text-muted"><summary className="cursor-pointer">查看本次输入</summary><p className="mt-2 max-h-40 overflow-auto whitespace-pre-wrap break-words leading-6">{state.input}</p></details>}
      {state.executionId && <p className="truncate font-mono text-[10px] text-subtle" title={state.executionId}>执行 ID · {state.executionId}</p>}
    </div>
    {state.output && <footer className="flex flex-wrap items-center justify-between gap-2 border-t border-border/60 p-3 sm:px-5">
      <Button size="sm" variant="ghost" onClick={async () => { const ok = await copyText(state.output); setCopied(ok); setCopyError(!ok) }}><IconCopy className="size-3.5" />{copied ? '已复制' : '复制文本'}</Button>
      {completed && onApply && <Button size="sm" variant="primary" disabled={applied} onClick={() => onApply(state.output)}><IconCheck className="size-3.5" />{applied ? '已应用到提示词' : applyLabel}</Button>}
      {copyError && <p role="alert" className="w-full text-xs text-danger">无法访问剪贴板，请手动选择文本复制。</p>}
      {!completed && <span className="text-[11px] text-subtle">未完成的预览不会自动应用</span>}
    </footer>}
  </article>
}
