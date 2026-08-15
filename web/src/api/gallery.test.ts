import { afterEach, describe, expect, it, vi } from 'vitest'
import { session } from './client'
import { endpoints } from './endpoints'
import { fetchGalleryItemContent } from './gallery'

function authenticate() {
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
}

afterEach(() => {
  session.clear()
  vi.unstubAllGlobals()
})

describe('gallery endpoints', () => {
  it('publishes exactly one artifact through the authenticated API', async () => {
    authenticate()
    const item = {
      id: 'gallery-1',
      artifact: {
        id: 'artifact-1',
        name: 'output.png',
        content_type: 'image/png',
        size_bytes: 3,
        sha256: 'abc',
      },
      job_id: 'job-1',
      workflow_id: 'workflow-1',
      workflow_version: 'v1',
      display_name: 'Workflow 1',
      parameters: { prompt: 'lake' },
      media_kind: 'image',
      content_url: '/api/v1/gallery/items/gallery-1/content',
      published_at_unix_ms: Date.now(),
      can_unpublish: true,
    } as const
    const fetchMock = vi.fn(async (_input: RequestInfo | URL, _init?: RequestInit) =>
      new Response(JSON.stringify(item), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      }),
    )
    vi.stubGlobal('fetch', fetchMock)

    await expect(endpoints.publishGalleryItem('artifact-1')).resolves.toEqual(item)

    expect(fetchMock).toHaveBeenCalledOnce()
    const [url, init] = fetchMock.mock.calls[0] ?? []
    if (!init) throw new Error('fetch init was not provided')
    const headers = new Headers(init.headers)
    expect(url).toBe('/api/v1/gallery/items')
    expect(init.method).toBe('POST')
    expect(headers.get('Authorization')).toBe('Bearer access-token')
    expect(headers.get('X-Organization-ID')).toBe('org-b')
    expect(headers.get('Content-Type')).toBe('application/json')
    expect(init.body).toBe(JSON.stringify({ artifact_id: 'artifact-1' }))
  })

  it('lists a cursor page, obtains a media ticket, and unpublishes by encoded id', async () => {
    authenticate()
    const responses = [
      new Response(JSON.stringify({ items: [], next_cursor: 'next page' }), {
          status: 200,
          headers: { 'content-type': 'application/json' },
      }),
      new Response(
        JSON.stringify({
          download: {
            method: 'GET',
            url: 'https://objects.example/media',
            headers: {},
            expires_at_unix_ms: Date.now() + 60_000,
          },
        }),
        { status: 200, headers: { 'content-type': 'application/json' } },
      ),
      new Response(null, { status: 204 }),
    ]
    const fetchMock = vi.fn(
      async (_input: RequestInfo | URL, _init?: RequestInit) =>
        responses.shift() ?? new Response(null, { status: 500 }),
    )
    vi.stubGlobal('fetch', fetchMock)

    await endpoints.galleryItems({ limit: 24, cursor: 'cursor value' })
    await endpoints.galleryItemDownload('gallery / 1')
    await endpoints.unpublishGalleryItem('gallery / 1')

    expect(fetchMock.mock.calls.map(([url]) => url)).toEqual([
      '/api/v1/gallery/items?limit=24&cursor=cursor+value',
      '/api/v1/gallery/items/gallery%20%2F%201/download',
      '/api/v1/gallery/items/gallery%20%2F%201',
    ])
    expect(fetchMock.mock.calls[2]?.[1]?.method).toBe('DELETE')
    for (const [, init] of fetchMock.mock.calls) {
      expect(new Headers(init?.headers).get('Authorization')).toBe('Bearer access-token')
    }
  })

  it('fetches Gallery content as an authenticated Blob for Canvas export', async () => {
    authenticate()
    const fetchMock = vi.fn(
      async (_input: RequestInfo | URL, _init?: RequestInit) =>
        new Response(new Uint8Array([1, 2, 3]), {
          status: 200,
          headers: { 'content-type': 'image/png' },
        }),
    )
    vi.stubGlobal('fetch', fetchMock)

    const blob = await fetchGalleryItemContent('gallery / 1')

    expect([...new Uint8Array(await blob.arrayBuffer())]).toEqual([1, 2, 3])
    const [url, init] = fetchMock.mock.calls[0] ?? []
    expect(url).toBe('/api/v1/gallery/items/gallery%20%2F%201/content')
    expect(new Headers(init?.headers).get('Authorization')).toBe('Bearer access-token')
  })
})
