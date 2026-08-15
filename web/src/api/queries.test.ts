import { describe, expect, it } from 'vitest'
import type { InfiniteData } from '@tanstack/react-query'
import type { GalleryItemsPage } from './types'
import { galleryPagesToItems } from './queries'

describe('Gallery query projection', () => {
  it('preserves server order while flattening loaded cursor pages', () => {
    const first = {
      id: 'first',
      artifact: { id: 'a', name: 'a.png', content_type: 'image/png', size_bytes: 1, sha256: 'a' },
      job_id: 'job-a',
      workflow_id: 'workflow',
      workflow_version: 'v1',
      display_name: 'First',
      parameters: {},
      media_kind: 'image' as const,
      content_url: '/api/v1/gallery/items/first/content',
      published_at_unix_ms: 2,
      can_unpublish: true,
    }
    const second = { ...first, id: 'second', display_name: 'Second', can_unpublish: false }
    const data: InfiniteData<GalleryItemsPage> = {
      pages: [
        { items: [first], next_cursor: 'next' },
        { items: [second], next_cursor: null },
      ],
      pageParams: [null, 'next'],
    }

    expect(galleryPagesToItems(data).map((item) => item.id)).toEqual(['first', 'second'])
  })
})
