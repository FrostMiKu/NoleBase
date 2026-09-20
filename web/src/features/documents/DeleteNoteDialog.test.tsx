import userEvent from '@testing-library/user-event'
import type { UserEvent } from '@testing-library/user-event'
import { screen, waitFor } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { editorStore } from '@/editor/editor-state'
import { saveCurrentDocument } from '@/editor/save'
import { workspaceKeys } from '@/queries/keys'
import {
  apiError,
  noContent,
  readOnlyApi,
  renderSidebarApp,
  resetSharedState,
  stubApi,
} from '@/test/sidebar-harness'
import type { NoteSummary } from '@/types/workspace'

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

afterEach(() => {
  resetSharedState()
  vi.unstubAllGlobals()
  bridge.source = ''
})

async function openDeleteDialog(user: UserEvent, rowText: string): Promise<void> {
  await user.pointer({ keys: '[MouseRight]', target: screen.getByText(rowText) })
  await user.click(await screen.findByRole('menuitem', { name: 'Delete' }))
  await screen.findByText('Delete note?')
}

describe('DeleteNoteDialog', () => {
  it('deletes a non-current note and leaves the editor alone', async () => {
    const user = userEvent.setup()
    let deleted = false
    const calls = stubApi((call) => {
      if (call.method === 'DELETE' && call.url.startsWith('/api/document')) {
        deleted = true
        return noContent()
      }
      return readOnlyApi({
        notes: () => (deleted ? NOTES.filter((note) => note.path !== 'data/foo.md') : NOTES),
      })(call)
    })
    editorStore.loadDocument('data/project.md', 'current content')
    const app = await renderSidebarApp({ notes: NOTES })

    await openDeleteDialog(user, 'foo')
    expect(screen.getByText('This will permanently delete “foo”.')).toBeTruthy()
    await user.click(screen.getByTestId('delete-note-confirm'))

    await waitFor(() => expect(screen.queryByText('foo')).toBeNull())
    expect(
      calls.find((call) => call.method === 'DELETE')?.url,
    ).toBe(`/api/document?path=${encodeURIComponent('data/foo.md')}`)
    expect(calls.filter((call) => call.method === 'GET' && call.url === '/api/notes')).toHaveLength(1)
    expect(app.router.state.location.pathname).toBe('/')
    expect(app.navigationLog).toEqual([])
    expect(editorStore.getState().path).toBe('data/project.md')
  })

  it('a plain Enter cancels instead of deleting', async () => {
    const user = userEvent.setup()
    const calls = stubApi(readOnlyApi({ notes: () => NOTES }))
    await renderSidebarApp({ notes: NOTES })

    await openDeleteDialog(user, 'foo')
    // The first focused action is Cancel, so Enter dismisses the dialog
    // without sending the destructive request.
    await user.keyboard('{Enter}')

    await waitFor(() => expect(screen.queryByText('Delete note?')).toBeNull())
    expect(calls.filter((call) => call.method === 'DELETE')).toHaveLength(0)
  })

  it('deleting the open document cancels autosave and leaves', async () => {
    const user = userEvent.setup()
    let deleted = false
    const calls = stubApi((call) => {
      if (call.method === 'DELETE' && call.url.startsWith('/api/document')) {
        deleted = true
        return noContent()
      }
      return readOnlyApi({
        notes: () => (deleted ? NOTES.filter((note) => note.path !== 'data/foo.md') : NOTES),
        documents: { 'data/foo.md': '# foo' },
      })(call)
    })
    // Unsaved edits on the document being deleted.
    bridge.source = '# foo local edit'
    editorStore.loadDocument('data/foo.md', '# foo')
    editorStore.setLiveDirty(true)
    const app = await renderSidebarApp({
      notes: NOTES,
      initialHref: `/document?path=${encodeURIComponent('data/foo.md')}`,
    })

    await openDeleteDialog(user, 'foo')
    await user.click(screen.getByTestId('delete-note-confirm'))

    await waitFor(() => expect(app.router.state.location.pathname).toBe('/'))
    expect(app.navigationLog).toEqual(['replace'])
    // No save of the doomed content — not before, not after the DELETE.
    expect(calls.filter((call) => call.method === 'PUT')).toHaveLength(0)
    expect(editorStore.getState().autosaveBlock).toBe('data/foo.md')
    expect(await saveCurrentDocument()).toBe(false)
    expect(calls.filter((call) => call.method === 'PUT')).toHaveLength(0)
    // The dead document's cache is gone.
    expect(app.queryClient.getQueryData(workspaceKeys.document('data/foo.md'))).toBeUndefined()
    await waitFor(() => expect(screen.queryByText('Delete note?')).toBeNull())
  })

  it('a failed delete keeps the editor, its content, and autosave', async () => {
    const user = userEvent.setup()
    const calls = stubApi((call) => {
      if (call.method === 'DELETE' && call.url.startsWith('/api/document')) {
        return apiError(500, 'storage unavailable', 'internal_error')
      }
      if (call.method === 'PUT' && call.url === '/api/document') return noContent()
      return readOnlyApi({ notes: () => NOTES })(call)
    })
    bridge.source = '# foo local edit'
    editorStore.loadDocument('data/foo.md', '# foo')
    editorStore.setLiveDirty(true)
    const app = await renderSidebarApp({
      notes: NOTES,
      initialHref: `/document?path=${encodeURIComponent('data/foo.md')}`,
    })

    await openDeleteDialog(user, 'foo')
    await user.click(screen.getByTestId('delete-note-confirm'))

    expect(await screen.findByTestId('dialog-error')).toHaveTextContent('storage unavailable')
    expect(app.router.state.location.pathname).toBe('/document')
    expect(app.navigationLog).toEqual([])
    // Autosave is unblocked again and the local text survived.
    expect(editorStore.getState().path).toBe('data/foo.md')
    expect(editorStore.getState().autosaveBlock).toBeNull()
    expect(bridge.source).toBe('# foo local edit')
    expect(await saveCurrentDocument()).toBe(true)
    await waitFor(() =>
      expect(calls.some((call) => call.method === 'PUT' && call.url === '/api/document')).toBe(true),
    )
    // The server list was never told the note disappeared.
    expect(screen.getByText('foo')).toBeTruthy()
  })
})
