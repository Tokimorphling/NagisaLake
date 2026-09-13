// @vitest-environment jsdom
import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { MemoryRouter, Route, Routes } from 'react-router-dom'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { Workflow } from '@/api/types'
import { StudioPage } from './StudioPage'

let canCreate = true
let catalog: Workflow[] = []
const refetch = vi.fn()
vi.mock('@/state/auth',()=>({useAuth:()=>({organizationId:'org',user:{id:'user'},atLeast:()=>canCreate})}))
vi.mock('@/state/toast',()=>({useToast:()=>({info:vi.fn()})}))
vi.mock('@/api/queries',()=>({
  useWorkflows:()=>({data:catalog,isPending:false,isError:false,hasNextPage:false,refetch}),
  useDevices:()=>({data:[]}),
  useJobs:()=>({data:{pages:[{items:[]}]},isPending:false,isError:false,isFetching:false,hasNextPage:false,refetch}),
  useGalleryItems:()=>({data:[],isPending:false,isError:false,refetch}),
}))
const workflow=(id:string): Workflow=>({id,version:'v1',output_types:['video/mp4'],available:true,manifest_consistent:true,
  manifest:{schema_version:1,display_name:id,description:null,inputs:[{name:'prompt',kind:'parameter',type:'string',content_type:null,default:'',required:true,options:[]}],outputs:[{name:'video',content_type:'video/mp4'}],warnings:[]},
  workers:[{organization_id:'org',worker_id:'worker',parallelism:1,queue_depth:0,active_jobs:0,queued_jobs:0,available:true}]})
let root: Root
let container: HTMLDivElement
let query: QueryClient
const tree=()=> <QueryClientProvider client={query}><MemoryRouter initialEntries={[{pathname:'/studio/video',state:{studioPrompt:{organizationId:'org',userId:'user',prompt:'keep my draft'}}}]}><Routes><Route path="/studio/:media" element={<StudioPage />} /></Routes></MemoryRouter></QueryClientProvider>
beforeEach(()=>{canCreate=true;catalog=[workflow('H3'),workflow('Wan')];vi.stubGlobal('IS_REACT_ACT_ENVIRONMENT',true);container=document.createElement('div');document.body.append(container);root=createRoot(container);query=new QueryClient();query.setQueryData(['agent-skills','org'],{enabled:false,items:[]})})
afterEach(async()=>{await act(async()=>root.unmount());container.remove();query.clear();vi.unstubAllGlobals()})

describe('Studio generation guards',()=>{
  it('does not silently switch a selected model when it disappears from the catalog',async()=>{
    await act(async()=>root.render(tree()))
    expect((container.querySelector('#studio-workflow') as HTMLSelectElement).value).toBe(JSON.stringify(['H3','v1']))
    catalog=[workflow('Wan')]
    await act(async()=>root.render(tree()))
    expect((container.querySelector('#studio-workflow') as HTMLSelectElement).value).toBe('')
    expect((container.querySelector('#studio-prompt') as HTMLTextAreaElement).value).toBe('keep my draft')
    const submit=[...container.querySelectorAll('button')].find((button)=>button.textContent?.includes('配置并生成'))!
    expect(submit.disabled).toBe(true)
  })
  it('keeps generation and prompt enhancement disabled for a viewer',async()=>{
    canCreate=false
    await act(async()=>root.render(tree()))
    const submit=[...container.querySelectorAll('button')].find((button)=>button.textContent?.includes('配置并生成'))!
    expect(submit.disabled).toBe(true)
    expect((container.querySelector('[role="switch"]') as HTMLButtonElement).disabled).toBe(true)
    const editor=container.querySelector('#studio-prompt')!
    await act(async()=>editor.dispatchEvent(new KeyboardEvent('keydown',{key:'Enter',ctrlKey:true,bubbles:true})))
    expect(document.querySelector('[role="dialog"]')).toBeNull()
  })
})
