import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { session } from './client'
import { sha256Hex } from './hash'
import { uploadArtifact } from './upload'

vi.mock('./hash',()=>({sha256Hex:vi.fn()}))
let pendingPut = false
let started: () => void
let xhr: FakeXHR
class FakeXHR {
  method=''; url=''; headers: Record<string,string>={}; response='';status=200;statusText='OK'
  upload={onprogress:null as ((event: ProgressEvent)=>void)|null}
  onload: (()=>void)|null=null; onerror:(()=>void)|null=null; onabort:(()=>void)|null=null
  constructor() { xhr=this }
  open(method:string,url:string) {this.method=method;this.url=url}
  setRequestHeader(name:string,value:string) {this.headers[name]=value}
  send(_body: Blob) {started?.();if(!pendingPut)queueMicrotask(()=>this.onload?.())}
  abort() {this.onabort?.()}
}
const ticket={artifact:{id:'artifact',state:'pending_upload'},upload:{url:'https://storage.test/upload',method:'PUT',headers:{'Content-Type':'text/plain','x-signed':'yes'},expires_at_unix_ms:Date.now()+100000}}
const fetchMock=vi.fn(async (_url: RequestInfo|URL,_init?:RequestInit)=>new Response(JSON.stringify(ticket),{headers:{'content-type':'application/json'}}))
beforeEach(()=>{
  pendingPut=false;started=()=>{}
  vi.stubGlobal('XMLHttpRequest',FakeXHR);vi.stubGlobal('fetch',fetchMock)
  vi.mocked(sha256Hex).mockResolvedValue('a'.repeat(64))
  session.setAuth({access_token:'test-token',token_type:'Bearer',access_expires_at:Date.now()+100000,refresh_expires_at:Date.now()+100000,csrf_token:'csrf',current_organization_id:'org-a',user:{id:'user',email:'user@example.test',status:'active',email_verified:true,created_at:1}})
})
afterEach(()=>{session.clear();vi.clearAllMocks();vi.unstubAllGlobals()})

describe('scoped cancellable artifact uploads',()=>{
  it('pins reserve and completion to the organization captured before hashing',async()=>{
    vi.mocked(sha256Hex).mockImplementation(async()=>{session.setOrganization('org-b');return 'a'.repeat(64)})
    await expect(uploadArtifact(new File(['hello'],'input.txt',{type:'text/plain'}))).resolves.toBe('artifact')
    expect(fetchMock).toHaveBeenCalledTimes(2)
    for(const [,options] of fetchMock.mock.calls) expect(new Headers(options?.headers).get('X-Organization-ID')).toBe('org-a')
    expect(xhr.method).toBe('PUT')
    expect(xhr.headers).toEqual(ticket.upload.headers)
    expect(xhr.headers.Authorization).toBeUndefined()
  })
  it('aborts an in-flight PUT without completing the reservation',async()=>{
    pendingPut=true
    const controller=new AbortController()
    const putStarted=new Promise<void>((resolve)=>{started=resolve})
    const result=uploadArtifact(new File(['hello'],'input.txt'),undefined,{organizationId:'org-a',signal:controller.signal})
    await putStarted
    controller.abort()
    await expect(result).rejects.toMatchObject({name:'AbortError'})
    expect(fetchMock).toHaveBeenCalledOnce()
  })
  it('does not reserve an artifact if cancelled while hashing',async()=>{
    let finish!: (hash:string)=>void
    vi.mocked(sha256Hex).mockImplementation(()=>new Promise<string>((resolve)=>{finish=resolve}))
    const controller=new AbortController()
    const result=uploadArtifact(new File(['hello'],'input.txt'),undefined,{signal:controller.signal})
    controller.abort();finish('a'.repeat(64))
    await expect(result).rejects.toMatchObject({name:'AbortError'})
    expect(fetchMock).not.toHaveBeenCalled()
  })
})
