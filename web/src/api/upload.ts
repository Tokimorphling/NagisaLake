import { ApiError, openAuthenticatedStream } from './client'
import { endpoints } from './endpoints'
import { sha256Hex } from './hash'

export { sha256Hex } from './hash'

export interface UploadProgress {
  stage: 'hashing' | 'uploading' | 'completing'
  fileName: string
  /** 0-100 upload completion for the current stage; hashing/completing report 0. */
  percent: number
  /** 1-based index of the current file in a multi-file upload sequence. */
  fileIndex?: number
  /** Total number of files in the upload sequence. */
  fileTotal?: number
}

/**
 * PUT the file to object storage via XMLHttpRequest so we can observe
 * upload progress. fetch() cannot report request-body progress.
 */
function putWithProgress(
  url: string,
  method: string,
  headers: Record<string, string>,
  body: Blob,
  onPercent?: (percent: number) => void,
): Promise<Response> {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest()
    xhr.open(method, url, true)
    for (const [key, value] of Object.entries(headers)) {
      // Content-Length is set by the browser; setting it manually can break
      // the SigV4 signature if it was not part of the signed headers.
      xhr.setRequestHeader(key, value)
    }
    xhr.upload.onprogress = (event) => {
      if (event.lengthComputable && onPercent) {
        onPercent(Math.round((event.loaded / event.total) * 100))
      }
    }
    xhr.onload = () => {
      const response = new Response(xhr.response, {
        status: xhr.status,
        statusText: xhr.statusText,
      })
      resolve(response)
    }
    xhr.onerror = () => reject(new Error('network_error'))
    xhr.send(body)
  })
}

/**
 * Three-step artifact upload: reserve, PUT straight to object storage using the
 * server-signed request, then complete so the Hub can verify size and hash.
 * Media never travels through the Hub's JSON body.
 */
export async function uploadArtifact(
  file: File,
  onProgress?: (progress: UploadProgress) => void,
  options?: { fileIndex?: number; fileTotal?: number },
): Promise<string> {
  const fileIndex = options?.fileIndex
  const fileTotal = options?.fileTotal
  onProgress?.({ stage: 'hashing', fileName: file.name, percent: 0, fileIndex, fileTotal })
  const sha256 = await sha256Hex(file)
  const contentType = file.type || 'application/octet-stream'

  const reserved = await endpoints.createUpload({
    name: file.name,
    content_type: contentType,
    size_bytes: file.size,
    sha256,
  })

  onProgress?.({ stage: 'uploading', fileName: file.name, percent: 0, fileIndex, fileTotal })
  // Send exactly the signed method/url/headers. Adding or omitting a header
  // breaks the SigV4 signature.
  let response: Response
  try {
    response = await putWithProgress(
      reserved.upload.url,
      reserved.upload.method,
      reserved.upload.headers,
      file,
      (percent) =>
        onProgress?.({
          stage: 'uploading',
          fileName: file.name,
          percent,
          fileIndex,
          fileTotal,
        }),
    )
  } catch {
    throw new ApiError(
      0,
      'network_error',
      '直传对象存储失败，请确认 bucket 已允许该前端 origin 的 CORS PUT 请求',
      null,
    )
  }
  if (!response.ok) {
    throw new ApiError(
      response.status,
      'upstream_error',
      `对象存储拒绝了上传 (HTTP ${response.status})`,
      null,
    )
  }

  onProgress?.({ stage: 'completing', fileName: file.name, percent: 100, fileIndex, fileTotal })
  await endpoints.completeUpload(reserved.artifact.id, {
    artifact_id: reserved.artifact.id,
    size_bytes: file.size,
    sha256,
  })

  return reserved.artifact.id
}

/** Resolves a short-lived presigned GET and opens it in a new tab. */
export async function downloadArtifact(artifactId: string): Promise<void> {
  const { download, artifact } = await endpoints.download(artifactId)
  const anchor = document.createElement('a')
  anchor.href = download.url
  anchor.rel = 'noopener'
  anchor.target = '_blank'
  anchor.download = artifact.name
  document.body.appendChild(anchor)
  anchor.click()
  anchor.remove()
}

/**
 * Downloads artifact bytes through the authenticated, same-origin Hub route.
 *
 * Parameter cards prefer a presigned GET so large media stays off the Hub.
 * This remains the one-shot fallback when the ticket needs request headers or
 * the object store does not allow the frontend origin to read media via CORS.
 */
export async function fetchArtifactContent(artifactId: string): Promise<Blob> {
  const response = await openAuthenticatedStream(
    `/artifacts/${encodeURIComponent(artifactId)}/content`,
  )
  return response.blob()
}
