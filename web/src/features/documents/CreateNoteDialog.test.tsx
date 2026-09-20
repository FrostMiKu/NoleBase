import userEvent from '@testing-library/user-event'
import type { UserEvent } from '@testing-library/user-event'
import { screen, waitFor } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'

import {
  apiError,
  created,
  readOnlyApi,
  renderSidebarApp,
  resetSharedState,
  stubApi,
  type ApiCall,
} from '@/test/sidebar-harness'
import type { NoteSummary } from '@/types/workspace'

const NOTES: NoteSummary[] = [
  { path: 'data/foo.md', name: 'foo', archived: false, modified: null },
]

const CREATED_PATH = 'data/created.md'

afterEach(() => {
  resetSharedState()
  vi.unstubAllGlobals()
})

async function openCreateDialog(user: UserEvent): Promise<void> {
  await user.click(screen.getByTestId('create-note-button'))
  await screen.findByTestId('create-note-name')
}

describe('CreateNoteDialog', () => {
  it('creates a note and pushes to the server-returned path', async () => {
    const user = userEvent.setup()
    let noteCreated = false
    const calls = stubApi((call) => {
      if (call.method === 'POST' && call.url === '/api/notes') {
        noteCreated = true
        return created({ path: CREATED_PATH })
      }
      return readOnlyApi({
        notes: () =>
          noteCreated
            ? [...NOTES, { path: CREATED_PATH, name: 'created', archived: false, modified: null }]
            : NOTES,
        documents: { [CREATED_PATH]: '# created' },
      })(call)
    })
    const app = await renderSidebarApp({ notes: NOTES })

    await openCreateDialog(user)
    await user.type(screen.getByTestId('create-note-name'), 'foo')
    await user.click(screen.getByTestId('create-note-submit'))

    // The new path comes only from the response; navigation is a push.
    await waitFor(() => expect(app.router.state.location.search).toEqual({ path: CREATED_PATH }))
    expect(app.navigationLog).toContain('push')
    expect(app.navigationLog).not.toContain('replace')
    expect(
      calls.find((call) => call.method === 'POST' && call.url === '/api/notes')?.body,
    ).toEqual({ name: 'foo', from_template: false })
    // Active invalidation refetched the notes list (no WebSocket here).
    expect(calls.filter((call) => call.method === 'GET' && call.url === '/api/notes')).toHaveLength(1)
    // The new document was fetched with its real content.
    await waitFor(() => {
      expect(
        calls.some((call) => call.method === 'GET' && call.url === `/api/document?path=${encodeURIComponent(CREATED_PATH)}`),
      ).toBe(true)
    })
    expect(screen.queryByTestId('create-note-name')).toBeNull()
    expect(await screen.findByText('created')).toBeTruthy()
  })

  it('sends from_template when the checkbox is checked', async () => {
    const user = userEvent.setup()
    const calls = stubApi((call) => {
      if (call.method === 'POST' && call.url === '/api/notes') return created({ path: CREATED_PATH })
      return readOnlyApi({ notes: () => NOTES })(call)
    })
    const app = await renderSidebarApp({ notes: NOTES })

    await openCreateDialog(user)
    await user.type(screen.getByTestId('create-note-name'), 'foo')
    await user.click(screen.getByTestId('create-note-template'))
    await user.keyboard('{Enter}')

    await waitFor(() => expect(app.router.state.location.search).toEqual({ path: CREATED_PATH }))
    expect(
      calls.find((call) => call.method === 'POST' && call.url === '/api/notes')?.body,
    ).toEqual({ name: 'foo', from_template: true })
  })

  it('Enter in the name field submits the dialog', async () => {
    const user = userEvent.setup()
    const calls = stubApi((call) => {
      if (call.method === 'POST' && call.url === '/api/notes') return created({ path: CREATED_PATH })
      return readOnlyApi({ notes: () => NOTES })(call)
    })
    await renderSidebarApp({ notes: NOTES })

    await openCreateDialog(user)
    await user.type(screen.getByTestId('create-note-name'), 'foo{Enter}')

    await waitFor(() => {
      expect(calls.some((call) => call.method === 'POST' && call.url === '/api/notes')).toBe(true)
    })
  })

  it('a failed create keeps the dialog open with the error and the input', async () => {
    const user = userEvent.setup()
    stubApi((call) => {
      if (call.method === 'POST' && call.url === '/api/notes') {
        return apiError(400, 'Invalid note name', 'bad_request')
      }
      return readOnlyApi({ notes: () => NOTES })(call)
    })
    const app = await renderSidebarApp({ notes: NOTES })

    await openCreateDialog(user)
    await user.type(screen.getByTestId('create-note-name'), 'foo')
    await user.click(screen.getByTestId('create-note-submit'))

    expect(await screen.findByTestId('dialog-error')).toHaveTextContent('Invalid note name')
    expect(screen.getByTestId('create-note-name')).toHaveValue('foo')
    expect(app.router.state.location.pathname).toBe('/')
  })

  it('double submit while pending produces exactly one request', async () => {
    const user = userEvent.setup()
    const calls: ApiCall[] = []
    let release: (() => void) | undefined
    const gate = new Promise<void>((resolve) => {
      release = resolve
    })
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input.toString()
        const method = (init?.method ?? 'GET').toUpperCase()
        calls.push({ method, url, body: init?.body ? JSON.parse(String(init.body)) : undefined })
        if (method === 'POST' && url === '/api/notes') {
          await gate
          return new Response(JSON.stringify({ path: CREATED_PATH }), {
            status: 201,
            headers: { 'content-type': 'application/json' },
          })
        }
        if (method === 'GET') return new Response(JSON.stringify(NOTES), { status: 200 })
        return new Response(null, { status: 204 })
      }),
    )
    await renderSidebarApp({ notes: NOTES })

    await openCreateDialog(user)
    await user.type(screen.getByTestId('create-note-name'), 'foo')
    await user.click(screen.getByTestId('create-note-submit'))
    await user.click(screen.getByTestId('create-note-submit'))
    release?.()
    await waitFor(() => {
      expect(calls.filter((call) => call.method === 'POST' && call.url === '/api/notes')).toHaveLength(1)
    })
    // The dialog only closes on success.
    await waitFor(() => expect(screen.queryByTestId('create-note-name')).toBeNull())
    expect(calls.some((call) => call.method === 'GET' && call.url === '/api/notes')).toBe(true)
  })
})
