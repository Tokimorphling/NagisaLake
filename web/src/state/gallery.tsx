/**
 * Server-backed Gallery state facade.
 *
 * Gallery entries and media tickets intentionally live in the query cache only;
 * no public media, Prompt, or expiring URL is persisted in browser storage.
 */
export {
  useGalleryDownload,
  useGalleryItems,
  useUnpublishGalleryItem,
} from '@/api/queries'
export type { GalleryItem } from '@/api/types'
