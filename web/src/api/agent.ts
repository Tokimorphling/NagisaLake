import { api, openAuthenticatedStream } from './client'
import { consumeSse } from './sse'

export interface AgentSkill { id: string; version: string }
export interface AgentRequest { skill: string; input: string; options?: Record<string, unknown> }
export type AgentEvent =
  | { type: 'started'; execution_id: string; skill: string }
  | { type: 'stage_started' | 'stage_finished' | 'stage_failed'; call_id: string; name: string }
  | { type: 'text_delta'; text: string }
  | { type: 'warning'; code: string }
  | { type: 'completed'; execution_id: string; text: string }
  | { type: 'error'; execution_id: string; code: string; message: string }
  | { type: 'cancelled'; execution_id: string }

export function agentSkills(organizationId: string, signal?: AbortSignal) {
  return api.get<{ enabled: boolean; items: AgentSkill[] }>('/agent/skills', { organizationId, signal })
}

export function cancelAgentRun(id: string, organizationId: string) {
  return api.post<void>(`/agent/runs/${encodeURIComponent(id)}/cancel`, undefined, { organizationId })
}

function parseEvent(data: string): AgentEvent {
  const value: unknown = JSON.parse(data)
  if (!value || typeof value !== 'object') throw new Error('无效的 Agent 事件')
  const event = value as Record<string, unknown>
  const strings = (...fields: string[]) => fields.every((key) => typeof event[key] === 'string')
  const valid = event.type === 'started' ? strings('execution_id', 'skill')
    : event.type === 'stage_started' || event.type === 'stage_finished' || event.type === 'stage_failed' ? strings('call_id', 'name')
      : event.type === 'text_delta' ? strings('text')
        : event.type === 'completed' ? strings('execution_id', 'text')
          : event.type === 'error' ? strings('execution_id', 'code', 'message')
            : event.type === 'cancelled' ? strings('execution_id')
              : event.type === 'warning' && strings('code')
  if (!valid) throw new Error('无效的 Agent 事件')
  return value as AgentEvent
}

/** One POST creates one execution. Never replay it on a network/stream error. */
export async function streamAgentRun(
  request: AgentRequest,
  organizationId: string,
  signal: AbortSignal,
  onEvent: (event: AgentEvent) => void,
): Promise<void> {
  const response = await openAuthenticatedStream('/agent/runs/stream', {
    method: 'POST', body: request, organizationId, signal,
  })
  if (!response.headers.get('content-type')?.startsWith('text/event-stream')) {
    await response.body?.cancel()
    throw new Error('Hub 没有返回 Agent 事件流')
  }
  let terminal = false
  await consumeSse(response, (_name, data) => {
    const event = parseEvent(data)
    if (terminal) return
    terminal = event.type === 'completed' || event.type === 'error' || event.type === 'cancelled'
    onEvent(event)
  })
  if (!terminal && !signal.aborted) throw new Error('Agent 连接中断，未收到最终结果；请勿自动重试')
}
