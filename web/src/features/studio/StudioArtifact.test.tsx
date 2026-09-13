// @vitest-environment jsdom
import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { DownloadResponse } from '@/api/types'
import { endpoints } from '@/api/endpoints'
import { StudioArtifact } from './StudioArtifact'

vi.mock('@/api/endpoints',()=>({endpoints:{download:vi.fn()}}))
let root: Root
let container: HTMLDivElement
let query: QueryClient
const response=(contentType='image/png',headers:Record<string,string>={}): DownloadResponse=>({artifact:{id:'output',job_id:'job',name:'output.png',content_type:contentType,size_bytes:10,sha256:'a'.repeat(64),state:'ready'},download:{method:'GET',url:'https://objects.example/output',headers,expires_at_unix_ms:Date.now()+100000}})
beforeEach(()=>{vi.stubGlobal('IS_REACT_ACT_ENVIRONMENT',true);container=document.createElement('div');document.body.append(container);root=createRoot(container);query=new QueryClient({defaultOptions:{queries:{retry:false}}})})
afterEach(async()=>{await act(async()=>root.unmount());container.remove();query.clear();vi.clearAllMocks();vi.unstubAllGlobals()})
async function render(visible=true) { await act(async()=>root.render(<QueryClientProvider client={query}><StudioArtifact artifactId="output" organizationId="org" visible={visible} /></QueryClientProvider>)) }

describe('Studio media preview safety',()=>{
  it('keeps a download action when the browser cannot decode the media',async()=>{
    query.setQueryData(['studio-artifact','org','output'],response())
    await render()
    await act(async()=>{container.querySelector('img')!.dispatchEvent(new Event('error'))})
    expect(container.textContent).toContain('浏览器不支持该格式')
    expect(container.querySelector('a[download]')?.getAttribute('href')).toBe('https://objects.example/output')
  })
  it('does not request a ticket for an initially hidden result pane',async()=>{
    await render(false)
    expect(endpoints.download).not.toHaveBeenCalled()
  })
  it.each([
    {contentType:'image/svg+xml',headers:{} as Record<string,string>},
    {contentType:'image/png',headers:{'x-required':'signed'}},
  ])('does not inline unsupported content/headers: $contentType',async({contentType,headers})=>{
    query.setQueryData(['studio-artifact','org','output'],response(contentType,headers))
    await render()
    expect(container.querySelector('img,video,iframe,object')).toBeNull()
  })
})
