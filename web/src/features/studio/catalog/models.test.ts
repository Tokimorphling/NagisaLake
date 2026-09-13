import { describe, expect, it } from 'vitest'
import type { JobSummary, Workflow, WorkflowInput } from '@/api/types'
import { compatibleParameters, filterHistory, promptField, promptText, transferredPrompt, workflowKey, workflowsForMedia } from './models'

const parameter = (name: string, type = 'string'): WorkflowInput => ({ name, type, kind: 'parameter', content_type: null, required: true, default: '', options: [] })
const workflow: Workflow = { id:'real-model', version:'v1', output_types:['video/mp4'], workers:[], available:false, manifest_consistent:true,
  manifest:{ schema_version:1, display_name:'Actual published model', description:null, inputs:[parameter('negative_prompt'), parameter('prompt'), parameter('width','integer'), {...parameter('reference'),kind:'artifact',content_type:'image/png'}], outputs:[{name:'video',content_type:'video/mp4'}], warnings:[] } }
const job = (state: JobSummary['state'], id: string, parameters: JobSummary['parameters']): JobSummary => ({ id, state, parameters, workflow_id:'offline-workflow',workflow_version:'v1',input_artifact_ids:[],output_artifact_ids:[],worker_id:'worker',session_id:'session',progress:null,prompt_id:null,error:null,created_at_unix_ms:1,updated_at_unix_ms:1 })

describe('Studio workflow and transfer boundaries', () => {
  it('uses only actual workflow media and reference declarations', () => {
    expect(workflowsForMedia([workflow], 'video', 'reference')).toEqual([workflow])
    expect(workflowsForMedia([workflow], 'image')).toEqual([])
    expect(workflowsForMedia([workflow], 'video', 'text')).toEqual([])
    const text = {...workflow,id:'other-real-model',manifest:{...workflow.manifest!,inputs:[parameter('prompt')]}}
    expect(workflowsForMedia([workflow,text], 'video','text')).toEqual([text])
    expect(workflowKey(workflow)).not.toBe(workflowKey({...workflow,version:'v2'}))
  })
  it('does not map the main editor onto a negative prompt or numeric field', () => {
    expect(promptField(workflow)).toBe('prompt')
    expect(promptField({...workflow,manifest:{...workflow.manifest!,inputs:[parameter('negative_prompt'),parameter('text','integer')]}})).toBeNull()
    expect(promptText({negative_prompt:'do not use',prompt:'use this'})).toBe('use this')
    expect(promptText({negative_prompt:'do not use'})).toBe('')
    expect(promptText(null)).toBe('')
  })
  it('drops foreign parameters and never reuses artifact IDs as a parameter seed', () => {
    expect(compatibleParameters(workflow,{prompt:'hello',width:1024,reference:'someone-elses-input',secret:'hidden'})).toEqual({prompt:'hello',width:1024})
    expect(compatibleParameters(workflow,null)).toEqual({})
  })
  it('accepts an in-memory prompt transfer only for its owning organization', () => {
    const transfer = {studioPrompt:{organizationId:'org-a',userId:'user-a',prompt:'private draft'}}
    expect(transferredPrompt(transfer,'org-a','user-a')).toBe('private draft')
    expect(transferredPrompt(transfer,'org-b','user-a')).toBe('')
    expect(transferredPrompt(transfer,'org-a','user-b')).toBe('')
    expect(transferredPrompt({studioPrompt:null},'org-a','user-a')).toBe('')
  })
  it('searches loaded history without requiring an online workflow catalog', () => {
    const jobs = [job('running','active',{prompt:'lake'}),job('completed','done',{prompt:'mountain'}),job('failed','failed',{negative_prompt:'lake'})]
    expect(filterHistory(jobs,'lake','all').map((item)=>item.id)).toEqual(['active'])
    expect(filterHistory(jobs,'','completed').map((item)=>item.id)).toEqual(['done'])
    expect(filterHistory(jobs,'','active').map((item)=>item.id)).toEqual(['active'])
    expect(filterHistory(jobs,'','failed').map((item)=>item.id)).toEqual(['failed'])
  })
})
