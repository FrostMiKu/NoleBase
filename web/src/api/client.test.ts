import { afterEach, describe, expect, it, vi } from 'vitest'

import { ApiError, apiGet, apiPost, apiPut, parseErrorResponse } from './client'

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'content-type': 'application/json' },
  })
}

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('error mapping', () => {
  const cases: { status: number; code: string; message: string }[] = [
    { status: 400, code: 'bad_request', message: 'missing document path' },
    { status: 404, code: 'not_found', message: 'Document not found' },
    { status: 409, code: 'conflict', message: 'an agent run is already active' },
    { status: 500, code: 'internal_error', message: 'Internal server error' },
  ]

  for (const { status, code, message } of cases) {
    it(`maps HTTP ${status} to ApiError code "${code}"`, async () => {
      vi.stubGlobal(
        'fetch',
        vi.fn(async () => jsonResponse(status, { error: { code, message } })),
      )
      const error = await apiGet('/api/document?path=x').then(
        () => null,
        (e: unknown) => e,
      )
      expect(error).toBeInstanceOf(ApiError)
      const apiError = error as ApiError
      expect(apiError.status).toBe(status)
      expect(apiError.code).toBe(code)
      expect(apiError.message).toBe(message)
    })
  }

  it('falls back to a generic code when the error body is not JSON', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => new Response('gateway exploded', { status: 502 })),
    )
    const error = await parseErrorResponse(
      new Response('gateway exploded', { status: 502 }),
    )
    expect(error).toBeInstanceOf(ApiError)
    expect(error.code).toBe('unknown_error')
    expect(error.status).toBe(502)
  })

  it('falls back when the error body has an unexpected shape', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => jsonResponse(500, { completely: 'different' })),
    )
    const error = await parseErrorResponse(
      new Response(JSON.stringify({ completely: 'different' }), { status: 500 }),
    )
    expect(error.code).toBe('unknown_error')
    expect(error.status).toBe(500)
  })

  it('falls back to statusText when the error message is missing', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => jsonResponse(404, { error: { code: 'not_found' } })),
    )
    const error = await parseErrorResponse(
      new Response(JSON.stringify({ error: { code: 'not_found' } }), {
        status: 404,
        statusText: 'Not Found',
      }),
    )
    expect(error.message).toBe('Not Found')
  })
})

describe('success mapping', () => {
  it('returns parsed JSON on 200', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => jsonResponse(200, { path: 'data/a.md', source: 'body' })),
    )
    await expect(apiGet('/api/document?path=data/a.md')).resolves.toEqual({
      path: 'data/a.md',
      source: 'body',
    })
  })

  it('returns undefined for 204 responses (mutations)', async () => {
    const fetchMock = vi.fn(async () => new Response(null, { status: 204 }))
    vi.stubGlobal('fetch', fetchMock)
    await expect(apiPut('/api/document', { path: 'p', source: 's' })).resolves.toBeUndefined()
    expect(fetchMock).toHaveBeenCalledWith('/api/document', {
      method: 'PUT',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ path: 'p', source: 's' }),
    })
  })

  it('sends an empty object body for bodyless POSTs', async () => {
    const fetchMock = vi.fn(async () => new Response(null, { status: 204 }))
    vi.stubGlobal('fetch', fetchMock)
    await expect(apiPost('/api/agent/cancel')).resolves.toBeUndefined()
    expect(fetchMock).toHaveBeenCalledWith('/api/agent/cancel', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: '{}',
    })
  })
})
