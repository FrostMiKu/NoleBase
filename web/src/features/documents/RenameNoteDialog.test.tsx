import userEvent from '@testing-library/user-event'
import type { UserEvent } from '@testing-library/user-event'
import { screen, waitFor } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { editorStore } from '@/editor/editor-state'
import { saveCurrentDocument } from '@/editor/save'
import {
  apiError,
  noContent,
  ok,
  readOnlyApi,
  renderSidebarApp,
  resetSharedState,
  stubApi,
  type ApiCall,
} from '@/test/sidebar-harness'
import { workspaceKeys } from '@/queries/keys'
import type { NoteSummary } from '@/types/workspace'

// The CodeMirror boundary: tests steer the "live" editor text directly.
const bridge = vi.hoisted(() => ({ source: '' }))
vi.mock('@/editor/editor-bridge', () => ({
  attachEditorView: vi.fn(),
  detachEditorView: vi.fn(),
  currentEditorSource: () => bridge.source,
  replaceEditorSource: vi.fn(),
}))

const NOTES: NoteSummary[] = [
  { path: 'data/foo.md', name: 'foo', archived: false, modified: null },
  { path: 'data/project.md', name: 'project', archived: false, modified: null },
]

const FOO_CONTENT = '# foo body'
const BAR_CONTENT = '# bar body (after rename)'

afterEach(() => {
  resetSharedState()
  vi.unstubAllGlobals()
  bridge.source = ''
})

async function openRenameDialog(user: UserEvent, rowText: string): Promise<void> {
  await user.pointer({ keys: '[MouseRight]', target: screen.getByText(rowText) })
  await user.click(await screen.findByRole('menuitem', { name: 'Rename' }))
  await screen.findByTestId('rename-note-name')
}

function renamedNotes(): NoteSummary[] {
  return [
    { path: 'data/bar.md', name: 'bar', archived: false, modified: null },
    NOTES[1] as NoteSummary,
  ]
}

describe('RenameNoteDialog', () => {
  it('renames a non-current note without touching the open editor', async () => {
    const user = userEvent.setup()
    let renamed = false
    const calls = stubApi((call) => {
      if (call.method === 'POST' && call.url === '/api/document/rename') {
        renamed = true
        return ok({ path: 'data/bar.md' })
      }
      return readOnlyApi({ notes: () => (renamed ? renamedNotes() : NOTES) })(call)
    })
    editorStore.loadDocument('data/project.md', 'current content')
    const app = await renderSidebarApp({ notes: NOTES })

    await openRenameDialog(user, 'foo')
    expect(screen.getByTestId('rename-note-name')).toHaveValue('foo')
    await user.clear(screen.getByTestId('rename-note-name'))
    await user.type(screen.getByTestId('rename-note-name'), 'bar')
    await user.click(screen.getByTestId('rename-note-submit'))

    // Sidebar refreshed from the server list…
    await waitFor(() => expect(screen.getByText('bar')).toBeTruthy())
    expect(
      calls.find((call) => call.method === 'POST' && call.url === '/api/document/rename')?.body,
    ).toEqual({ path: 'data/foo.md', new_name: 'bar' })
    // …the notes list was actively invalidated…
    expect(calls.filter((call) => call.method === 'GET' && call.url === '/api/notes')).toHaveLength(1)
    // …and the open editor did not move.
    expect(app.router.state.location.pathname).toBe('/')
    expect(app.navigationLog).toEqual([])
    expect(editorStore.getState().path).toBe('data/project.md')
  })

  it('renames the open document: flush, mutate, replace, retarget', async () => {
    const user = userEvent.setup()
    let renamed = false
    const calls = stubApi((call) => {
      if (call.method === 'PUT' && call.url === '/api/document') return noContent()
      if (call.method === 'POST' && call.url === '/api/document/rename') {
        renamed = true
        return ok({ path: 'data/bar.md' })
      }
      return readOnlyApi({
        notes: () => (renamed ? renamedNotes() : NOTES),
        documents: { 'data/foo.md': FOO_CONTENT, 'data/bar.md': BAR_CONTENT },
      })(call)
    })
    // The editor is dirty on data/foo.md (the document under rename).
    bridge.source = `${FOO_CONTENT} plus local edit`
    editorStore.loadDocument('data/foo.md', FOO_CONTENT)
    editorStore.setLiveDirty(true)
    const app = await renderSidebarApp({
      notes: NOTES,
      initialHref: `/document?path=${encodeURIComponent('data/foo.md')}`,
    })

    await openRenameDialog(user, 'foo')
    await user.clear(screen.getByTestId('rename-note-name'))
    await user.type(screen.getByTestId('rename-note-name'), 'bar')
    await user.click(screen.getByTestId('rename-note-submit'))

    await waitFor(() =>
      expect(app.router.state.location.search).toEqual({ path: 'data/bar.md' }),
    )
    // Navigation replaced, so Back cannot resurrect the dead path.
    expect(app.navigationLog).toEqual(['replace'])
    // The flush PUT landed on the old path BEFORE the rename request.
    const flushIndex = calls.findIndex(
      (call) => call.method === 'PUT' && (call.body as { path?: string } | undefined)?.path === 'data/foo.md',
    )
    const renameIndex = calls.findIndex(
      (call) => call.method === 'POST' && call.url === '/api/document/rename',
    )
    expect(flushIndex).toBeGreaterThanOrEqual(0)
    expect(renameIndex).toBeGreaterThan(flushIndex)
    expect(calls[flushIndex]?.body).toEqual({
      path: 'data/foo.md',
      source: `${FOO_CONTENT} plus local edit`,
    })
    // The new document was fetched through the probe with real content.
    await waitFor(() => expect(screen.getByTestId('document-probe')).toHaveTextContent(BAR_CONTENT))
    // The old identity's cache is gone.
    expect(app.queryClient.getQueryData(workspaceKeys.document('data/foo.md'))).toBeUndefined()

    // While blocked, the old path never receives another save…
    expect(editorStore.getState().autosaveBlock).toBe('data/foo.md')
    expect(await saveCurrentDocument()).toBe(false)
    expect(
      calls.filter(
        (call) => call.method === 'PUT' && (call.body as { path?: string } | undefined)?.path === 'data/foo.md',
      ),
    ).toHaveLength(1)
    // …and after the editor retargets, saves go to the new path.
    bridge.source = `${BAR_CONTENT} edited`
    editorStore.loadDocument('data/bar.md', BAR_CONTENT)
    expect(editorStore.getState().autosaveBlock).toBeNull()
    expect(await saveCurrentDocument()).toBe(true)
    await waitFor(() => {
      expect(
        calls.some(
          (call) => call.method === 'PUT' && (call.body as { path?: string } | undefined)?.path === 'data/bar.md',
        ),
      ).toBe(true)
    })
  })

  it('a failed flush aborts the rename and keeps the local edits', async () => {
    const user = userEvent.setup()
    const calls = stubApi((call) => {
      if (call.method === 'PUT' && call.url === '/api/document') {
        return apiError(500, 'disk on fire', 'internal_error')
      }
      return readOnlyApi({ notes: () => NOTES })(call)
    })
    bridge.source = `${FOO_CONTENT} plus local edit`
    editorStore.loadDocument('data/foo.md', FOO_CONTENT)
    editorStore.setLiveDirty(true)
    const app = await renderSidebarApp({ notes: NOTES })

    await openRenameDialog(user, 'foo')
    await user.clear(screen.getByTestId('rename-note-name'))
    await user.type(screen.getByTestId('rename-note-name'), 'bar')
    await user.click(screen.getByTestId('rename-note-submit'))

    // The save failed: no rename request, no navigation, error inline.
    expect(await screen.findByTestId('dialog-error')).toBeTruthy()
    expect(
      calls.some((call) => call.method === 'POST' && call.url === '/api/document/rename'),
    ).toBe(false)
    expect(app.router.state.location.pathname).toBe('/')
    expect(app.navigationLog).toEqual([])
    // The editor keeps its document, its dirty flag, and its text.
    const state = editorStore.getState()
    expect(state.path).toBe('data/foo.md')
    expect(state.dirty).toBe(true)
    expect(state.autosaveBlock).toBeNull()
    expect(bridge.source).toBe(`${FOO_CONTENT} plus local edit`)
    expect(screen.getByTestId('rename-note-name')).toHaveValue('bar')
  })

  it('a 409 collision keeps the dialog open with the server message', async () => {
    const user = userEvent.setup()
    stubApi((call) => {
      if (call.method === 'POST' && call.url === '/api/document/rename') {
        return apiError(409, 'A note named “bar” already exists', 'conflict')
      }
      return readOnlyApi({ notes: () => NOTES })(call)
    })
    editorStore.loadDocument('data/foo.md', FOO_CONTENT)
    const app = await renderSidebarApp({ notes: NOTES })

    await openRenameDialog(user, 'foo')
    await user.clear(screen.getByTestId('rename-note-name'))
    await user.type(screen.getByTestId('rename-note-name'), 'bar')
    await user.click(screen.getByTestId('rename-note-submit'))

    expect(await screen.findByTestId('dialog-error')).toHaveTextContent(
      'A note named “bar” already exists',
    )
    expect(app.router.state.location.pathname).toBe('/')
    expect(editorStore.getState().path).toBe('data/foo.md')
    expect(screen.getByTestId('rename-note-name')).toHaveValue('bar')
  })

  it('renames an archived note inside archives/', async () => {
    const user = userEvent.setup()
    const archived: NoteSummary[] = [
      { path: 'archives/old.md', name: 'old', archived: true, modified: null },
    ]
    const calls = stubApi((call) => {
      if (call.method === 'POST' && call.url === '/api/document/rename') {
        return ok({ path: 'archives/new.md' })
      }
      return readOnlyApi({ notes: () => archived })(call)
    })
    await renderSidebarApp({ notes: archived })

    await user.click(screen.getByText('Archives', { selector: 'span' }))
    await openRenameDialog(user, 'old')
    await user.clear(screen.getByTestId('rename-note-name'))
    await user.type(screen.getByTestId('rename-note-name'), 'new')
    await user.click(screen.getByTestId('rename-note-submit'))

    expect(
      calls.find((call) => call.method === 'POST' && call.url === '/api/document/rename')?.body,
    ).toEqual({ path: 'archives/old.md', new_name: 'new' })
    await waitFor(() => expect(screen.queryByTestId('rename-note-name')).toBeNull())
  })

  it('does not send a rename request while one is pending', async () => {
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
        if (method === 'POST' && url === '/api/document/rename') await gate
        if (method === 'GET' && url === '/api/notes') {
          return new Response(JSON.stringify(NOTES), { status: 200 })
        }
        if (method === 'GET' && url === '/api/daily') {
          return new Response('[]', { status: 200 })
        }
        return new Response(method === 'GET' ? '{}' : null, { status: method === 'GET' ? 200 : 204 })
      }),
    )
    await renderSidebarApp({ notes: NOTES })

    await openRenameDialog(user, 'foo')
    await user.clear(screen.getByTestId('rename-note-name'))
    await user.type(screen.getByTestId('rename-note-name'), 'bar')
    await user.click(screen.getByTestId('rename-note-submit'))
    await user.click(screen.getByTestId('rename-note-submit'))
    release?.()
    await waitFor(() => {
      expect(
        calls.filter((call) => call.method === 'POST' && call.url === '/api/document/rename'),
      ).toHaveLength(1)
    })
  })
})
