import { openAuthenticatedStream } from './client'

/** Same-origin fallback when a Gallery download ticket cannot feed Canvas directly. */
export async function fetchGalleryItemContent(itemId: string): Promise<Blob> {
  const response = await openAuthenticatedStream(
    `/gallery/items/${encodeURIComponent(itemId)}/content`,
  )
  return response.blob()
}
