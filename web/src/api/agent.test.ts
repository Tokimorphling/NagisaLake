import { afterEach, describe, expect, it, vi } from 'vitest'
import { streamAgentRun, cancelAgentRun, type AgentEvent } from './agent'
import { session } from './client'

function authenticate() {
  session.setAuth({ access_token: 'access-token', token_type: 'Bearer', access_expires_at: Date.now() + 60_000,
    refresh_expires_at: Date.now() + 120_000, csrf_token: 'csrf-token', current_organization_id: 'ambient-org',
    user: { id:'user', email:'user@example.com',status:'active',email_verified:true,created_at:Date.now() } })
}
const frame = (value: AgentEvent) => `event: ${value.type}\ndata: ${JSON.stringify(value)}\n\n`
const response = (data: string) => new Response(data, {headers:{'content-type':'text/event-stream'}})
afterEach(() => { session.clear(); vi.unstubAllGlobals() })

describe('agent API', () => {
  it('starts exactly one authenticated POST with pinned organization and forwards the signal', async () => {
    authenticate()
    const fetchMock = vi.fn(async (_url: RequestInfo | URL, _options?: RequestInit) => response(
      frame({type:'started',execution_id:'run',skill:'rewrite'}) + frame({type:'completed',execution_id:'run',text:'你好'}),
    ))
    vi.stubGlobal('fetch', fetchMock)
    const signal = new AbortController().signal
    const seen: AgentEvent[] = []
    await streamAgentRun({skill:'rewrite',input:'hello'}, 'chosen-org', signal, (event) => seen.push(event))
    expect(fetchMock).toHaveBeenCalledOnce()
    const [url, options] = fetchMock.mock.calls[0]!
    expect(url).toBe('/api/v1/agent/runs/stream')
    expect(options?.method).toBe('POST')
    expect(options?.signal).toBe(signal)
    expect(new Headers(options?.headers).get('Authorization')).toBe('Bearer access-token')
    expect(new Headers(options?.headers).get('X-Organization-ID')).toBe('chosen-org')
    expect(seen.at(-1)).toEqual({type:'completed',execution_id:'run',text:'你好'})
  })
  it('never replays a POST after a truncated SSE response', async () => {
    authenticate()
    const fetchMock = vi.fn(async () => response(frame({type:'started',execution_id:'run',skill:'rewrite'})))
    vi.stubGlobal('fetch', fetchMock)
    await expect(streamAgentRun({skill:'rewrite',input:'hello'},'org',new AbortController().signal,()=>{})).rejects.toThrow('未收到最终结果')
    expect(fetchMock).toHaveBeenCalledOnce()
  })
  it('does not interpret malformed or unknown events as successful output', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => response('data: {"type":"completed","text":"missing id"}\n\n')))
    await expect(streamAgentRun({skill:'rewrite',input:'hello'},'org',new AbortController().signal,()=>{})).rejects.toThrow('无效的 Agent 事件')
  })
  it('stops reading after a terminal event even if the server keeps the stream open', async () => {
    const cancelled = vi.fn()
    const body = new ReadableStream<Uint8Array>({
      start(controller) { controller.enqueue(new TextEncoder().encode(frame({type:'completed',execution_id:'run',text:'final'}))) },
      cancel: cancelled,
    })
    vi.stubGlobal('fetch', vi.fn(async () => new Response(body, {headers:{'content-type':'text/event-stream'}})))
    const seen: AgentEvent[] = []
    await streamAgentRun({skill:'rewrite',input:'hello'},'org',new AbortController().signal,(event)=>seen.push(event))
    expect(seen).toEqual([{type:'completed',execution_id:'run',text:'final'}])
    expect(cancelled).toHaveBeenCalledOnce()
  })
  it('does not swallow a consumer callback error while processing a terminal event', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => response(frame({type:'completed',execution_id:'run',text:'final'}))))
    await expect(streamAgentRun({skill:'rewrite',input:'hello'},'org',new AbortController().signal,()=>{throw new Error('consumer failure')})).rejects.toThrow('consumer failure')
  })
  it('uses the same authenticated command contract for cancellation', async () => {
    authenticate()
    const fetchMock = vi.fn(async (_url: RequestInfo | URL, _options?: RequestInit) => new Response(null, {status:204}))
    vi.stubGlobal('fetch', fetchMock)
    await cancelAgentRun('run/a','chosen-org')
    expect(fetchMock.mock.calls[0]?.[0]).toBe('/api/v1/agent/runs/run%2Fa/cancel')
    expect(fetchMock.mock.calls[0]?.[1]?.method).toBe('POST')
  })
})
