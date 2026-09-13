// @vitest-environment jsdom
import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { Job, Workflow, WorkflowInput } from '@/api/types'
import { endpoints } from '@/api/endpoints'
import { useGenerationSubmit } from './useGenerationSubmit'

let organizationId = 'org-a'
const navigate = vi.fn()
const toast = {success:vi.fn(),error:vi.fn(),fromError:vi.fn()}
vi.mock('react-router-dom', () => ({ useNavigate: () => navigate }))
vi.mock('@/state/auth', () => ({ useAuth: () => ({organizationId}) }))
vi.mock('@/state/toast', () => ({ useToast: () => toast }))
vi.mock('@/api/endpoints', () => ({ endpoints:{submitJob:vi.fn()} }))

const prompt: WorkflowInput = {name:'prompt',kind:'parameter',type:'string',content_type:null,required:true,default:'',options:[]}
const workflow: Workflow = {id:'actual-workflow',version:'v1',output_types:['image/png'],manifest:null,manifest_consistent:true,workers:[],available:true}
const job: Job = {id:'job',workflow_id:workflow.id,workflow_version:'v1',parameters:{prompt:'hello'},input_artifact_ids:[],output_artifact_ids:[],worker_id:'worker',session_id:'session',state:'received',progress:0,prompt_id:null,error:null,created_at_unix_ms:1,updated_at_unix_ms:1,events:[]}
let current: ReturnType<typeof useGenerationSubmit>
let root: Root
let container: HTMLDivElement
let query: QueryClient
const submitted = vi.fn()
function Probe({redirect}: {redirect:boolean}) {
  current=useGenerationSubmit({workflow,parameters:[prompt],artifacts:[],targets:[],values:{prompt:'hello',unlisted:'not sent'},files:{},existingArtifactIds:{},target:'',onSubmitted:submitted,navigateOnSubmit:redirect})
  return <span>{current.submitting ? 'busy' : 'idle'}</span>
}
async function render(redirect=false) { await act(async()=>root.render(<QueryClientProvider client={query}><Probe redirect={redirect} /></QueryClientProvider>)) }
beforeEach(()=>{organizationId='org-a';vi.stubGlobal('IS_REACT_ACT_ENVIRONMENT',true);container=document.createElement('div');document.body.append(container);root=createRoot(container);query=new QueryClient();vi.mocked(endpoints.submitJob).mockResolvedValue(job)})
afterEach(async()=>{await act(async()=>root.unmount());container.remove();query.clear();vi.clearAllMocks();vi.unstubAllGlobals()})

describe('Studio submission preserves existing job logic', () => {
  it('submits only manifest parameters once and can stay in Studio', async () => {
    await render()
    await act(async()=>{await Promise.all([current.submit(),current.submit()])})
    expect(endpoints.submitJob).toHaveBeenCalledOnce()
    const [body,key,scope]=vi.mocked(endpoints.submitJob).mock.calls[0]!
    expect(body.workflow_id).toBe('actual-workflow')
    expect(body.parameters).toEqual({prompt:'hello'})
    expect(key).toBeTruthy()
    expect(scope?.organizationId).toBe('org-a')
    expect(submitted).toHaveBeenCalledWith(job)
    expect(navigate).not.toHaveBeenCalled()
  })
  it('retains the old job-detail redirect for non-Studio callers', async () => {
    await render(true)
    await act(async()=>current.submit())
    expect(navigate).toHaveBeenCalledWith('/jobs/job')
  })
  it('aborts the old scope and ignores a late response after an organization switch', async () => {
    let resolve!: (value: Job)=>void
    vi.mocked(endpoints.submitJob).mockImplementation(()=>new Promise<Job>((done)=>{resolve=done}))
    await render()
    let pending!: Promise<void>
    await act(async()=>{pending=current.submit()})
    const scope=vi.mocked(endpoints.submitJob).mock.calls[0]![2]!
    organizationId='org-b'
    await render()
    expect(scope.organizationId).toBe('org-a')
    expect(scope.signal?.aborted).toBe(true)
    await act(async()=>{resolve(job);await pending})
    expect(submitted).not.toHaveBeenCalled()
    expect(navigate).not.toHaveBeenCalled()
    expect(toast.success).not.toHaveBeenCalled()
    expect(current.submitting).toBe(false)
  })
})
