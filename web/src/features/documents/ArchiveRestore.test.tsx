import userEvent from '@testing-library/user-event'
import type { UserEvent } from '@testing-library/user-event'
import { screen, waitFor } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { editorStore } from '@/editor/editor-state'
import { saveCurrentDocument } from '@/editor/save'
import { workspaceKeys } from '@/queries/keys'
import {
  noContent,
  ok,
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
  { path: 'archives/kept.md', name: 'kept', archived: true, modified: null },
]

afterEach(() => {
  resetSharedState()
  vi.unstubAllGlobals()
  bridge.source = ''
})

async function chooseMenuItem(user: UserEvent, rowText: string, item: string): Promise<void> {
  await user.pointer({ keys: '[MouseRight]', target: screen.getByText(rowText) })
  await user.click(await screen.findByRole('menuitem', { name: item }))
}

describe('Archive', () => {
  it('archives a non-current note directly, refreshing the sidebar', async () => {
    const user = userEvent.setup()
    let archived = false
    const calls = stubApi((call) => {
      if (call.method === 'POST' && call.url === '/api/document/archive') {
        archived = true
        return ok({ path: 'archives/foo.md' })
      }
      return readOnlyApi({
        notes: () =>
          archived
            ? [
                { path: 'archives/foo.md', name: 'foo', archived: true, modified: null },
                NOTES[1] as NoteSummary,
              ]
            : NOTES,
      })(call)
    })
    editorStore.loadDocument('archives/kept.md', 'current content')
    const app = await renderSidebarApp({ notes: NOTES })

    await chooseMenuItem(user, 'foo', 'Archive')

    expect(
      calls.find((call) => call.method === 'POST' && call.url === '/api/document/archive')?.body,
    ).toEqual({ path: 'data/foo.md' })
    // The sidebar moved the row into Archives without waiting for the watcher.
    await waitFor(() => expect(screen.queryByText('Notes')).toBeTruthy())
    expect(calls.filter((call) => call.method === 'GET' && call.url === '/api/notes')).toHaveLength(1)
    expect(app.router.state.location.pathname).toBe('/')
    expect(app.navigationLog).toEqual([])
    expect(editorStore.getState().path).toBe('archives/kept.md')
  })

  it('archives the open document: flush, retarget, no stale PUTs', async () => {
    const user = userEvent.setup()
    let archived = false
    const calls = stubApi((call) => {
      if (call.method === 'PUT' && call.url === '/api/document') return noContent()
      if (call.method === 'POST' && call.url === '/api/document/archive') {
        archived = true
        return ok({ path: 'archives/foo.md' })
      }
      return readOnlyApi({
        notes: () =>
          archived
            ? [{ path: 'archives/foo.md', name: 'foo', archived: true, modified: null }]
            : [NOTES[0] as NoteSummary],
        documents: { 'data/foo.md': '# foo', 'archives/foo.md': '# foo' },
      })(call)
    })
    bridge.source = '# foo local edit'
    editorStore.loadDocument('data/foo.md', '# foo')
    editorStore.setLiveDirty(true)
    const app = await renderSidebarApp({
      notes: [NOTES[0] as NoteSummary],
      initialHref: `/document?path=${encodeURIComponent('data/foo.md')}`,
    })

    await chooseMenuItem(user, 'foo', 'Archive')

    // The user stays on the document, now filed under archives/.
    await waitFor(() =>
      expect(app.router.state.location.search).toEqual({ path: 'archives/foo.md' }),
    )
    expect(app.navigationLog).toEqual(['replace'])
    // Flush first, then archive.
    const flushIndex = calls.findIndex(
      (call) => call.method === 'PUT' && (call.body as { path?: string } | undefined)?.path === 'data/foo.md',
    )
    const archiveIndex = calls.findIndex(
      (call) => call.method === 'POST' && call.url === '/api/document/archive',
    )
    expect(flushIndex).toBeGreaterThanOrEqual(0)
    expect(archiveIndex).toBeGreaterThan(flushIndex)
    expect(calls[flushIndex]?.body).toEqual({ path: 'data/foo.md', source: '# foo local edit' })
    // The old identity is gone and blocked; the new one loads.
    expect(app.queryClient.getQueryData(workspaceKeys.document('data/foo.md'))).toBeUndefined()
    expect(editorStore.getState().autosaveBlock).toBe('data/foo.md')
    expect(await saveCurrentDocument()).toBe(false)
    bridge.source = '# foo edited again'
    editorStore.loadDocument('archives/foo.md', '# foo')
    expect(editorStore.getState().autosaveBlock).toBeNull()
    expect(await saveCurrentDocument()).toBe(true)
  })
})

describe('Restore', () => {
  it('restores a non-current archive directly', async () => {
    const user = userEvent.setup()
    let restored = false
    const calls = stubApi((call) => {
      if (call.method === 'POST' && call.url === '/api/document/restore') {
        restored = true
        return ok({ path: 'data/foo.md' })
      }
      return readOnlyApi({
        notes: () =>
          restored
            ? [{ path: 'data/foo.md', name: 'foo', archived: false, modified: null }]
            : [{ path: 'archives/foo.md', name: 'foo', archived: true, modified: null }],
      })(call)
    })
    const app = await renderSidebarApp({
      notes: [{ path: 'archives/foo.md', name: 'foo', archived: true, modified: null }],
    })

    await user.click(screen.getByText('Archives', { selector: 'span' }))
    await chooseMenuItem(user, 'foo', 'Restore')

    expect(
      calls.find((call) => call.method === 'POST' && call.url === '/api/document/restore')?.body,
    ).toEqual({ path: 'archives/foo.md' })
    expect(calls.filter((call) => call.method === 'GET' && call.url === '/api/notes')).toHaveLength(1)
    expect(app.router.state.location.pathname).toBe('/')
    expect(app.navigationLog).toEqual([])
  })

  it('restores the open archive: flush, retarget to data/', async () => {
    const user = userEvent.setup()
    let restored = false
    const calls = stubApi((call) => {
      if (call.method === 'PUT' && call.url === '/api/document') return noContent()
      if (call.method === 'POST' && call.url === '/api/document/restore') {
        restored = true
        return ok({ path: 'data/foo.md' })
      }
      return readOnlyApi({
        notes: () =>
          restored
            ? [{ path: 'data/foo.md', name: 'foo', archived: false, modified: null }]
            : [{ path: 'archives/foo.md', name: 'foo', archived: true, modified: null }],
        documents: { 'archives/foo.md': '# foo', 'data/foo.md': '# foo' },
      })(call)
    })
    bridge.source = '# foo local edit'
    editorStore.loadDocument('archives/foo.md', '# foo')
    editorStore.setLiveDirty(true)
    const app = await renderSidebarApp({
      notes: [{ path: 'archives/foo.md', name: 'foo', archived: true, modified: null }],
      initialHref: `/document?path=${encodeURIComponent('archives/foo.md')}`,
    })

    await user.click(screen.getByText('Archives', { selector: 'span' }))
    await chooseMenuItem(user, 'foo', 'Restore')

    await waitFor(() =>
      expect(app.router.state.location.search).toEqual({ path: 'data/foo.md' }),
    )
    expect(app.navigationLog).toEqual(['replace'])
    const flushIndex = calls.findIndex(
      (call) => call.method === 'PUT' && (call.body as { path?: string } | undefined)?.path === 'archives/foo.md',
    )
    const restoreIndex = calls.findIndex(
      (call) => call.method === 'POST' && call.url === '/api/document/restore',
    )
    expect(flushIndex).toBeGreaterThanOrEqual(0)
    expect(restoreIndex).toBeGreaterThan(flushIndex)
    expect(app.queryClient.getQueryData(workspaceKeys.document('archives/foo.md'))).toBeUndefined()
    expect(editorStore.getState().autosaveBlock).toBe('archives/foo.md')
  })
})
