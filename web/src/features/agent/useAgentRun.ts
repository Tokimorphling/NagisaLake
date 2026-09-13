import { useCallback, useEffect, useRef, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { agentSkills, cancelAgentRun, streamAgentRun, type AgentRequest } from '@/api/agent'

export interface AgentStage { id: string; name: string; state: 'running' | 'completed' | 'failed' }
export interface AgentRunState {
  phase: 'idle' | 'running' | 'completed' | 'failed' | 'cancelled'
  executionId: string | null
  skill: string
  input: string
  output: string
  status: string
  error: string | null
  stages: AgentStage[]
}
const empty = (): AgentRunState => ({ phase: 'idle', executionId: null, skill: '', input: '', output: '', status: '', error: null, stages: [] })

export function useAgentSkills(organizationId: string, enabled = true) {
  return useQuery({
    queryKey: ['agent-skills', organizationId],
    queryFn: ({ signal }) => agentSkills(organizationId, signal),
    enabled: enabled && organizationId !== '',
    staleTime: 60_000,
    retry: false,
  })
}

/** Shared single-execution controller for Studio enhancement and Agent mode. */
export function useAgentRun(organizationId: string) {
  const [state, setState] = useState<AgentRunState>(empty)
  const [scope, setScope] = useState(organizationId)
  const active = useRef<{ controller: AbortController; id: string | null; frame: number | null } | null>(null)

  useEffect(() => {
    setScope(organizationId)
    setState(empty())
    return () => {
      const current = active.current
      active.current = null
      if (current?.frame != null) cancelAnimationFrame(current.frame)
      current?.controller.abort()
    }
  }, [organizationId])

  const start = useCallback(async (request: AgentRequest) => {
    if (active.current || !organizationId || !request.skill || !request.input.trim()) return
    const current = { controller: new AbortController(), id: null as string | null, frame: null as number | null }
    active.current = current
    setState({ ...empty(), phase: 'running', skill: request.skill, input: request.input, status: '正在连接执行引擎…' })
    let preview = ''
    let terminal = false
    try {
      await streamAgentRun(request, organizationId, current.controller.signal, (event) => {
        if (active.current !== current || current.controller.signal.aborted) return
        switch (event.type) {
          case 'started':
            current.id = event.execution_id
            setState((value) => ({ ...value, executionId: event.execution_id, status: '正在执行 Skill…' }))
            break
          case 'stage_started':
          case 'stage_finished':
          case 'stage_failed': {
            const next = event.type === 'stage_started' ? 'running' : event.type === 'stage_finished' ? 'completed' : 'failed'
            setState((value) => {
              const stage: AgentStage = { id: event.call_id, name: event.name, state: next }
              const stages = value.stages.some((item) => item.id === stage.id)
                ? value.stages.map((item) => item.id === stage.id ? stage : item)
                : [...value.stages, stage].slice(-64)
              return { ...value, stages, status: next === 'running' ? `正在调用 ${event.name}…` : next === 'completed' ? 'Skill 已返回，正在生成文本…' : `${event.name} 调用失败` }
            })
            break
          }
          case 'warning': setState((value) => ({ ...value, status: '正在恢复连接并核对执行结果…' })); break
          case 'text_delta':
            preview += event.text
            if (current.frame === null) current.frame = requestAnimationFrame(() => {
              current.frame = null
              if (active.current === current && !terminal && !current.controller.signal.aborted) setState((value) => ({ ...value, output: preview, status: '正在生成文本…' }))
            })
            break
          case 'completed':
          case 'error':
          case 'cancelled':
            terminal = true
            if (current.frame !== null) cancelAnimationFrame(current.frame)
            current.frame = null
            setState((value) => ({ ...value,
              phase: event.type === 'completed' ? 'completed' : event.type === 'error' ? 'failed' : 'cancelled',
              output: event.type === 'completed' ? event.text : preview,
              status: event.type === 'completed' ? '执行完成' : event.type === 'error' ? '执行失败' : '已取消',
              error: event.type === 'error' ? event.message : null,
            }))
            break
        }
      })
    } catch (error) {
      if (active.current === current && !current.controller.signal.aborted) {
        setState((value) => ({ ...value, phase: 'failed', output: preview, status: '未取得最终结果', error: error instanceof Error ? error.message : 'Agent 请求失败' }))
      }
    } finally {
      if (current.frame !== null) cancelAnimationFrame(current.frame)
      if (active.current === current) active.current = null
    }
  }, [organizationId])

  const cancel = useCallback(() => {
    const current = active.current
    if (!current) return
    current.controller.abort()
    if (current.frame !== null) cancelAnimationFrame(current.frame)
    current.frame = null
    if (current.id) void cancelAgentRun(current.id, organizationId).catch(() => undefined)
    setState((value) => ({ ...value, phase: 'cancelled', status: '取消请求已发送' }))
  }, [organizationId])

  const reset = useCallback(() => {
    if (!active.current) setState(empty())
  }, [])

  const visibleState = scope === organizationId ? state : empty()
  return { state: visibleState, start, cancel, reset, busy: visibleState.phase === 'running' }
}
