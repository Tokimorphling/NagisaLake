import { useState } from 'react'
import { Button } from '@/components/ui/primitives'
import { AgentResult } from './AgentResult'
import { useAgentRun, useAgentSkills } from './useAgentRun'

/** Compact embedding of the same controller used by the full Studio workspace. */
export function AgentPanel({ input, onApply, organizationId }: {
  input: string; onApply: (text: string) => void; organizationId: string
}) {
  const catalog = useAgentSkills(organizationId)
  const [skill, setSkill] = useState('')
  const run = useAgentRun(organizationId)
  const selected = catalog.data?.items.find((item) => item.id === skill)?.id ?? catalog.data?.items[0]?.id ?? ''
  if (!catalog.isPending && !catalog.isError && (!catalog.data?.enabled || !catalog.data.items.length)) return null
  return <section className="space-y-3 rounded-xl border border-border bg-surface p-3" aria-label="提示词助手">
    <div className="flex flex-wrap items-center gap-2">
      <label htmlFor="agent-skill" className="text-xs font-medium text-muted">提示词助手</label>
      <select id="agent-skill" aria-label="选择 Skill" value={selected} disabled={run.busy || catalog.isPending} onChange={(event) => setSkill(event.target.value)} className="min-w-0 flex-1 rounded-lg border border-border bg-surface-2 px-2 py-1.5 text-xs">
        {catalog.data?.items.map((item) => <option key={item.id} value={item.id}>{item.id}</option>)}
      </select>
      {run.busy ? <Button size="sm" variant="ghost" onClick={run.cancel}>取消</Button> : <Button size="sm" variant="ghost" disabled={!selected || !input.trim()} onClick={() => void run.start({ skill: selected, input })}>优化提示词</Button>}
    </div>
    {catalog.isError && <p role="alert" className="text-xs text-danger">无法加载助手配置。<button type="button" onClick={() => void catalog.refetch()}>重试</button></p>}
    {run.state.phase !== 'idle' && <AgentResult compact state={run.state} onCancel={run.cancel} onApply={onApply} inputChanged={run.state.input !== input} />}
  </section>
}
