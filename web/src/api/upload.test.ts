import { afterEach, describe, expect, it, vi } from 'vitest'
import { session } from './client'
import { fetchArtifactContent } from './upload'

afterEach(() => {
  session.clear()
  vi.unstubAllGlobals()
})

describe('fetchArtifactContent', () => {
  it('loads media from the authenticated same-origin content endpoint', async () => {
    session.setAuth({
      access_token: 'access-token',
      token_type: 'Bearer',
      access_expires_at: Date.now() + 60_000,
      refresh_expires_at: Date.now() + 120_000,
      csrf_token: 'csrf-token',
      current_organization_id: 'org-b',
      user: {
        id: 'user-b',
        email: 'b@example.com',
        status: 'active',
        email_verified: true,
        created_at: Date.now(),
      },
    })
    const fetchMock = vi.fn(async (_input: RequestInfo | URL, _init?: RequestInit) =>
      new Response(new Uint8Array([1, 2, 3]), {
        status: 200,
        headers: { 'content-type': 'image/png' },
      }),
    )
    vi.stubGlobal('fetch', fetchMock)

    const blob = await fetchArtifactContent('artifact with spaces')

    expect(fetchMock).toHaveBeenCalledOnce()
    const [url, init] = fetchMock.mock.calls[0] ?? []
    if (!init) throw new Error('fetch init was not provided')
    const headers = new Headers(init.headers)
    expect(url).toBe('/api/v1/artifacts/artifact%20with%20spaces/content')
    expect(init.credentials).toBe('include')
    expect(headers.get('Authorization')).toBe('Bearer access-token')
    expect(headers.get('X-Organization-ID')).toBe('org-b')
    expect(blob.type).toBe('image/png')
    await expect(blob.arrayBuffer()).resolves.toEqual(new Uint8Array([1, 2, 3]).buffer)
  })
})
