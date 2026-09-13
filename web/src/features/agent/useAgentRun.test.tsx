// @vitest-environment jsdom
import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { cancelAgentRun, streamAgentRun, type AgentEvent } from '@/api/agent'
import { useAgentRun } from './useAgentRun'

vi.mock('@/api/agent', () => ({agentSkills:vi.fn(),streamAgentRun:vi.fn(),cancelAgentRun:vi.fn()}))
let root: Root
let container: HTMLDivElement
let current: ReturnType<typeof useAgentRun>
let emit: (event: AgentEvent) => void
let finish: () => void
let signal: AbortSignal
let frame: FrameRequestCallback | undefined
function Probe({org}: {org:string}) { current = useAgentRun(org); return <pre>{current.state.output}</pre> }
async function render(org = 'org-a') { await act(async () => root.render(<Probe org={org} />)) }
beforeEach(() => {
  vi.stubGlobal('IS_REACT_ACT_ENVIRONMENT',true)
  vi.stubGlobal('requestAnimationFrame', vi.fn((callback: FrameRequestCallback) => { frame = callback; return 1 }))
  vi.stubGlobal('cancelAnimationFrame',vi.fn())
  container=document.createElement('div');document.body.append(container);root=createRoot(container)
  vi.mocked(cancelAgentRun).mockResolvedValue(undefined)
  vi.mocked(streamAgentRun).mockImplementation(async (_request,_org,received,onEvent) => {
    emit=onEvent;signal=received
    await new Promise<void>((resolve) => { finish=resolve; received.addEventListener('abort',()=>resolve(),{once:true}) })
  })
})
afterEach(async () => { await act(async()=>root.unmount());container.remove();vi.clearAllMocks();vi.unstubAllGlobals() })

describe('shared Agent execution controller', () => {
  it('keeps one active POST, deduplicates call IDs and lets final text replace the preview', async () => {
    await render()
    await act(async()=>{void current.start({skill:'rewrite',input:'first'});void current.start({skill:'rewrite',input:'duplicate'})})
    expect(streamAgentRun).toHaveBeenCalledOnce()
    await act(async()=>{
      emit({type:'started',execution_id:'run',skill:'rewrite'})
      emit({type:'stage_started',call_id:'call',name:'skill'})
      emit({type:'stage_finished',call_id:'call',name:'skill'})
      emit({type:'text_delta',text:'partial'})
      emit({type:'completed',execution_id:'run',text:'final text'})
      finish()
    })
    await act(async()=>frame?.(0))
    expect(current.state.phase).toBe('completed')
    expect(current.state.output).toBe('final text')
    expect(current.state.stages).toEqual([{id:'call',name:'skill',state:'completed'}])
  })
  it('cancels and hides old output when the organization changes', async () => {
    await render()
    await act(async()=>{void current.start({skill:'rewrite',input:'private input'})})
    await act(async()=>{emit({type:'started',execution_id:'old-run',skill:'rewrite'});emit({type:'text_delta',text:'private output'});frame?.(0)})
    expect(container.textContent).toBe('private output')
    const oldEmit=emit
    await render('org-b')
    expect(signal.aborted).toBe(true)
    expect(container.textContent).toBe('')
    await act(async()=>oldEmit({type:'completed',execution_id:'old-run',text:'late private text'}))
    expect(current.state.phase).toBe('idle')
    expect(container.textContent).toBe('')
  })
  it('does not let an in-flight animation frame overwrite cancellation', async () => {
    await render()
    await act(async()=>{void current.start({skill:'rewrite',input:'input'})})
    await act(async()=>{emit({type:'started',execution_id:'run',skill:'rewrite'});emit({type:'text_delta',text:'late'});current.cancel()})
    await act(async()=>frame?.(0))
    expect(signal.aborted).toBe(true)
    expect(current.state.phase).toBe('cancelled')
    expect(current.state.output).not.toBe('late')
    expect(cancelAgentRun).toHaveBeenCalledWith('run','org-a')
  })
})
