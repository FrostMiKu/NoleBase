/**
 * The single HTTP entry point for the frontend. URLs are origin-relative
 * (`/api/...`); in dev the Vite proxy forwards them, in production Axum
 * serves them from the same origin the app was loaded from.
 */

export class ApiError extends Error {
  readonly code: string
  readonly status: number

  constructor(code: string, status: number, message: string) {
    super(message)
    this.name = 'ApiError'
    this.code = code
    this.status = status
  }
}

interface ApiErrorBody {
  error?: { code?: unknown; message?: unknown }
}

/** Parse a backend error response; falls back for non-JSON bodies. */
export async function parseErrorResponse(response: Response): Promise<ApiError> {
  const fallback = () =>
    new ApiError('unknown_error', response.status, response.statusText || 'Request failed')
  let body: ApiErrorBody
  try {
    body = (await response.json()) as ApiErrorBody
  } catch {
    return fallback()
  }
  const error = body.error
  if (typeof error !== 'object' || error === null) {
    return fallback()
  }
  const code = typeof error.code === 'string' ? error.code : 'unknown_error'
  const message =
    typeof error.message === 'string' && error.message !== ''
      ? error.message
      : response.statusText || 'Request failed'
  return new ApiError(code, response.status, message)
}

async function request<T>(method: string, url: string, body?: unknown): Promise<T> {
  const response = await fetch(url, {
    method,
    headers: body === undefined ? undefined : { 'content-type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  })
  if (!response.ok) {
    throw await parseErrorResponse(response)
  }
  if (response.status === 204) {
    return undefined as T
  }
  try {
    return (await response.json()) as T
  } catch {
    throw new ApiError('invalid_response', response.status, 'Response was not valid JSON')
  }
}

export function apiGet<T>(url: string): Promise<T> {
  return request<T>('GET', url)
}

export function apiPost<T>(url: string, body?: unknown): Promise<T> {
  return request<T>('POST', url, body ?? {})
}

export function apiPut<T>(url: string, body?: unknown): Promise<T> {
  return request<T>('PUT', url, body ?? {})
}
