import type { ApiErrorBody, ApiErrorCode, AuthBody } from './types'

const BASE = '/api/v1'
const CSRF_COOKIE = 'nagisalake_csrf'

/** Auth routes must never trigger a nested refresh attempt. */
const NO_RETRY = ['/auth/login', '/auth/register', '/auth/refresh', '/auth/logout']

export class ApiError extends Error {
  readonly status: number
  readonly code: ApiErrorCode | 'network_error'
  readonly requestId: string | null

  constructor(status: number, code: ApiErrorCode | 'network_error', message: string, requestId: string | null) {
    super(message)
    this.name = 'ApiError'
    this.status = status
    this.code = code
    this.requestId = requestId
  }

  get isAuth(): boolean {
    return this.status === 401
  }

  get isForbidden(): boolean {
    return this.status === 403
  }
}

/**
 * Access token lives in memory only, per docs/PUBLIC_PRODUCT_API_CN.md — never
 * localStorage. A page reload recovers the session from the HttpOnly refresh
 * cookie instead.
 */
let accessToken: string | null = null
let csrfToken: string | null = null
let organizationId: string | null = null

/** Single-flight refresh: concurrent 401s await one in-flight rotation. */
let refreshInFlight: Promise<AuthBody> | null = null

type SessionListener = (reason: 'expired') => void
let onSessionLost: SessionListener | null = null

export const session = {
  setAuth(body: AuthBody) {
    accessToken = body.access_token
    csrfToken = body.csrf_token
    organizationId = body.current_organization_id
  },
  clear() {
    accessToken = null
    csrfToken = null
    organizationId = null
  },
  setOrganization(id: string) {
    organizationId = id
  },
  get organizationId() {
    return organizationId
  },
  get hasToken() {
    return accessToken !== null
  },
  onLost(listener: SessionListener | null) {
    onSessionLost = listener
  },
}

function readCsrfCookie(): string | null {
  const match = document.cookie.split('; ').find((row) => row.startsWith(`${CSRF_COOKIE}=`))
  return match ? decodeURIComponent(match.slice(CSRF_COOKIE.length + 1)) : null
}

async function toApiError(response: Response): Promise<ApiError> {
  const requestId = response.headers.get('X-Request-ID')
  let code: ApiErrorCode | 'network_error' = 'internal_error'
  let message = `请求失败 (HTTP ${response.status})`
  try {
    const body = (await response.json()) as Partial<ApiErrorBody>
    if (body.error) {
      code = body.error.code ?? code
      message = body.error.message || message
    }
  } catch {
    // Non-JSON body (proxy error page, empty 502). Keep the generic message.
  }
  return new ApiError(response.status, code, message, requestId)
}

/**
 * Rotates the session. The Hub requires the refresh cookie, a double-submit
 * CSRF header matching the readable cookie, and an allowed Origin.
 */
export function refresh(): Promise<AuthBody> {
  if (refreshInFlight) return refreshInFlight

  const attempt = (async () => {
    const csrf = readCsrfCookie() ?? csrfToken
    const response = await fetch(`${BASE}/auth/refresh`, {
      method: 'POST',
      credentials: 'include',
      headers: csrf ? { 'X-CSRF-Token': csrf } : {},
    })
    if (!response.ok) throw await toApiError(response)
    const body = (await response.json()) as AuthBody
    session.setAuth(body)
    return body
  })()

  // Clear the latch regardless of outcome so a later 401 can retry, but keep
  // returning this attempt to everyone who joined while it was in flight.
  refreshInFlight = attempt
  void attempt.catch(() => undefined).then(() => {
    if (refreshInFlight === attempt) refreshInFlight = null
  })

  return attempt
}

interface RequestOptions {
  method?: string
  body?: unknown
  /** Overrides the ambient organization for org-scoped routes. */
  organizationId?: string
  idempotencyKey?: string
  signal?: AbortSignal
  /** Skips bearer + refresh handling, for public endpoints. */
  anonymous?: boolean
}

async function fetchResponse(path: string, options: RequestOptions, isRetry: boolean): Promise<Response> {
  const headers = new Headers()
  if (options.body !== undefined) headers.set('Content-Type', 'application/json')
  if (!options.anonymous && accessToken) headers.set('Authorization', `Bearer ${accessToken}`)

  // Browser-only mechanism: the Hub re-checks membership for this header and
  // rejects it for API-key principals.
  const org = options.organizationId ?? organizationId
  if (!options.anonymous && org) headers.set('X-Organization-ID', org)
  if (options.idempotencyKey) headers.set('Idempotency-Key', options.idempotencyKey)

  let response: Response
  try {
    response = await fetch(`${BASE}${path}`, {
      method: options.method ?? 'GET',
      headers,
      credentials: 'include',
      body: options.body === undefined ? undefined : JSON.stringify(options.body),
      signal: options.signal,
    })
  } catch (error) {
    if ((error as Error)?.name === 'AbortError') throw error
    throw new ApiError(0, 'network_error', '无法连接到 Hub，请确认服务已启动', null)
  }

  const retryable = !isRetry && !options.anonymous && !NO_RETRY.some((suffix) => path.startsWith(suffix))
  if (response.status === 401 && retryable) {
    try {
      await refresh()
    } catch {
      session.clear()
      onSessionLost?.('expired')
      throw await toApiError(response)
    }
    return fetchResponse(path, options, true)
  }

  if (!response.ok) {
    const error = await toApiError(response)
    if (error.isAuth && !options.anonymous) {
      session.clear()
      onSessionLost?.('expired')
    }
    throw error
  }

  return response
}

async function send<T>(path: string, options: RequestOptions, isRetry: boolean): Promise<T> {
  const response = await fetchResponse(path, options, isRetry)

  if (response.status === 204) return undefined as T
  const text = await response.text()
  return (text ? JSON.parse(text) : undefined) as T
}

export function request<T>(path: string, options: RequestOptions = {}): Promise<T> {
  return send<T>(path, options, false)
}

/**
 * Opens an authenticated streaming response, such as the job SSE endpoint.
 * Native EventSource cannot send the in-memory Bearer token, so callers use
 * fetch and consume `response.body` instead. The same single-flight refresh
 * path as JSON requests is applied when the access token has expired.
 */
export function openAuthenticatedStream(
  path: string,
  options: Pick<RequestOptions, 'organizationId' | 'signal'> = {},
): Promise<Response> {
  return fetchResponse(path, options, false)
}

export const api = {
  get: <T>(path: string, options?: Omit<RequestOptions, 'method' | 'body'>) =>
    request<T>(path, { ...options, method: 'GET' }),
  post: <T>(path: string, body?: unknown, options?: Omit<RequestOptions, 'method' | 'body'>) =>
    request<T>(path, { ...options, method: 'POST', body }),
  patch: <T>(path: string, body?: unknown, options?: Omit<RequestOptions, 'method' | 'body'>) =>
    request<T>(path, { ...options, method: 'PATCH', body }),
  delete: <T>(path: string, options?: Omit<RequestOptions, 'method' | 'body'>) =>
    request<T>(path, { ...options, method: 'DELETE' }),
}
