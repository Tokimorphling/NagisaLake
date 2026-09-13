import { useEffect, useRef, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { agentSkills, cancelAgentRun, streamAgentRun } from '@/api/agent'
import { Button } from '@/components/ui/primitives'

/** Text-only helper: users explicitly apply the final result to their draft. */
export function AgentPanel({ input, onApply, organizationId }: {
  input: string; onApply: (text: string) => void; organizationId: string
}) {
  const catalog = useQuery({
    queryKey: ['agent-skills', organizationId],
    queryFn: ({ signal }) => agentSkills(organizationId, signal),
    staleTime: 60_000,
    retry: false,
  })
  const [skill, setSkill] = useState('')
  const [output, setOutput] = useState('')
  const [status, setStatus] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const [complete, setComplete] = useState(false)
  const operation = useRef<{ controller: AbortController; id: string | null; frame: number | null } | null>(null)

  useEffect(() => () => {
    const active = operation.current
    operation.current = null
    if (active?.frame != null) cancelAnimationFrame(active.frame)
    active?.controller.abort()
  }, [])

  const selectedSkill = catalog.data?.items.find((item) => item.id === skill)?.id ?? catalog.data?.items[0]?.id ?? ''
  const start = async () => {
    if (!selectedSkill || busy || operation.current || !input.trim()) return
    const active = { controller: new AbortController(), id: null as string | null, frame: null as number | null }
    operation.current = active
    setBusy(true); setComplete(false); setOutput(''); setError(null); setStatus('正在连接…')
    let preview = ''
    try {
      await streamAgentRun({ skill: selectedSkill, input }, organizationId, active.controller.signal, (event) => {
        if (operation.current !== active) return
        switch (event.type) {
          case 'started': active.id = event.execution_id; setStatus('正在执行…'); break
          case 'stage_started': setStatus(`正在执行 ${event.name}…`); break
          case 'stage_finished': setStatus(`${event.name} 完成，正在生成文本…`); break
          case 'stage_failed': setStatus(`${event.name} 调用失败`); break
          case 'warning': setStatus('连接恢复中，正在核对执行结果…'); break
          case 'text_delta':
            preview += event.text
            // Batch token updates once per frame, not one React render per token.
            if (active.frame === null) active.frame = requestAnimationFrame(() => {
              active.frame = null
              if (operation.current === active) setOutput(preview)
            })
            break
          case 'completed':
            if (active.frame !== null) cancelAnimationFrame(active.frame)
            active.frame = null
            setOutput(event.text); setComplete(true); setStatus('已完成')
            break
          case 'error': setError(event.message); setStatus('执行失败'); break
          case 'cancelled': setStatus('已取消'); break
        }
      })
    } catch (cause) {
      if (operation.current === active && !active.controller.signal.aborted) {
        setError(cause instanceof Error ? cause.message : 'Agent 请求失败')
        setStatus('未取得最终结果')
      }
    } finally {
      if (active.frame !== null) cancelAnimationFrame(active.frame)
      if (operation.current === active) { operation.current = null; setBusy(false) }
    }
  }

  const cancel = () => {
    const active = operation.current
    if (!active) return
    active.controller.abort() // disconnect also cancels server-side RunHandle
    if (active.id) void cancelAgentRun(active.id, organizationId).catch(() => undefined)
    setStatus('取消请求已发送')
  }

  if (!catalog.isPending && !catalog.isError && (!catalog.data?.enabled || !catalog.data.items.length)) return null
  return <section className="space-y-2 rounded-lg border border-border bg-surface-2/50 p-3" aria-label="提示词助手">
    <div className="flex flex-wrap items-center gap-2">
      <label htmlFor="agent-skill" className="text-xs font-medium text-muted">提示词助手</label>
      <select id="agent-skill" aria-label="选择 Skill" value={selectedSkill} disabled={busy || catalog.isPending} onChange={(event) => setSkill(event.target.value)} className="min-w-0 flex-1 rounded border border-border bg-surface px-2 py-1 text-xs">
        {catalog.data?.items.map((item) => <option key={item.id} value={item.id}>{item.id}</option>)}
      </select>
      {busy ? <Button size="sm" variant="ghost" onClick={cancel}>取消</Button> : <Button size="sm" variant="ghost" disabled={!selectedSkill || !input.trim()} onClick={() => void start()}>优化提示词</Button>}
    </div>
    {catalog.isError && <p role="alert" className="text-xs text-danger">无法加载助手配置。<button type="button" onClick={() => void catalog.refetch()}>重试</button></p>}
    {status && <p className="text-xs text-muted" aria-live="polite">{status}</p>}
    {error && <p role="alert" className="text-xs text-danger">{error}</p>}
    {output && <pre className="max-h-60 overflow-auto whitespace-pre-wrap break-words text-xs">{output}</pre>}
    {complete && <Button size="sm" variant="ghost" onClick={() => onApply(output)}>将结果应用到提示词</Button>}
  </section>
}
