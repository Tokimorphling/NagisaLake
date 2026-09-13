// @vitest-environment jsdom
import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { AgentPanel } from './AgentPanel'
import { agentSkills, cancelAgentRun, streamAgentRun } from '@/api/agent'

vi.mock('@/api/agent', () => ({ agentSkills: vi.fn(), cancelAgentRun: vi.fn(), streamAgentRun: vi.fn() }))
let container: HTMLDivElement
let root: Root
let query: QueryClient
beforeEach(() => {
  vi.stubGlobal('IS_REACT_ACT_ENVIRONMENT', true)
  vi.mocked(agentSkills).mockResolvedValue({enabled:true,items:[{id:'rewrite',version:'1'}]})
  vi.mocked(cancelAgentRun).mockResolvedValue(undefined)
  container = document.createElement('div')
  document.body.append(container)
  root = createRoot(container)
  query = new QueryClient({defaultOptions:{queries:{retry:false}}})
  query.setQueryData(['agent-skills','org'], {enabled:true,items:[{id:'rewrite',version:'1'}]})
})
afterEach(async () => {
  await act(async () => root.unmount())
  query.clear(); container.remove(); vi.clearAllMocks(); vi.unstubAllGlobals()
})
async function mount(onApply: (text: string) => void) {
  await act(async () => root.render(<QueryClientProvider client={query}><AgentPanel input="original" onApply={onApply} organizationId="org" /></QueryClientProvider>))
}
function button(text: string) {
  const button = [...container.querySelectorAll('button')].find((node) => node.textContent === text)
  if (!button) throw new Error(`missing button: ${text}`)
  return button
}

describe('AgentPanel', () => {
  it('shows final output but applies it only after an explicit click', async () => {
    vi.mocked(streamAgentRun).mockImplementation(async (_request, _org, _signal, emit) => {
      emit({type:'started',execution_id:'run',skill:'rewrite'})
      emit({type:'completed',execution_id:'run',text:'improved'})
    })
    const apply = vi.fn()
    await mount(apply)
    await act(async () => button('优化提示词').click())
    expect(container.textContent).toContain('improved')
    expect(apply).not.toHaveBeenCalled()
    await act(async () => button('将结果应用到提示词').click())
    expect(apply).toHaveBeenCalledWith('improved')
  })
  it('cancels through both the typed command API and the fetch signal', async () => {
    let signal: AbortSignal | undefined
    vi.mocked(streamAgentRun).mockImplementation(async (_request, _org, received, emit) => {
      signal = received
      emit({type:'started',execution_id:'run',skill:'rewrite'})
      await new Promise<void>((resolve) => received.addEventListener('abort', () => resolve(), {once:true}))
    })
    await mount(vi.fn())
    await act(async () => button('优化提示词').click())
    await act(async () => button('取消').click())
    expect(signal?.aborted).toBe(true)
    expect(cancelAgentRun).toHaveBeenCalledWith('run','org')
    expect(container.textContent).toContain('取消请求已发送')
  })
})
