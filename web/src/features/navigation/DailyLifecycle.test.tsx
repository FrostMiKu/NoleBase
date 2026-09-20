import userEvent from '@testing-library/user-event'
import type { UserEvent } from '@testing-library/user-event'
import { screen, waitFor } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { editorStore } from '@/editor/editor-state'
import { workspaceKeys } from '@/queries/keys'
import {
  apiError,
  noContent,
  ok,
  readOnlyApi,
  renderSidebarApp,
  resetSharedState,
  stubApi,
} from '@/test/sidebar-harness'
import type { DailyNote, NoteSummary } from '@/types/workspace'

const bridge = vi.hoisted(() => ({ source: '' }))
vi.mock('@/editor/editor-bridge', () => ({
  attachEditorView: vi.fn(),
  detachEditorView: vi.fn(),
  currentEditorSource: () => bridge.source,
  replaceEditorSource: vi.fn(),
}))

const NOTES: NoteSummary[] = [
  { path: 'data/project.md', name: 'project', archived: false, modified: null },
  { path: 'data/inbox.md', name: 'inbox', archived: false, modified: null },
  { path: 'archives/old.md', name: 'old', archived: true, modified: null },
]

const DAILY: DailyNote[] = [
  { date: '2026-01-15', body: 'day one', path: 'daily/2026-01-15.md' },
  { date: '2026-01-14', body: 'day two', path: 'daily/2026-01-14.md' },
]

const TARGET = 'data/project.md'
const TARGET_CONTENT = `# project

## 2026-01-15

day one
`

afterEach(() => {
  resetSharedState()
  vi.unstubAllGlobals()
  bridge.source = ''
})

async function openDailySection(user: UserEvent): Promise<void> {
  await user.click(screen.getByText('Daily', { selector: 'span' }))
}

async function chooseDailyMenuItem(
  user: UserEvent,
  date: string,
  item: string,
): Promise<void> {
  await user.pointer({ keys: '[MouseRight]', target: screen.getByText(date) })
  await user.click(await screen.findByRole('menuitem', { name: item }))
}

describe('Daily delete', () => {
  it('confirms and deletes a non-current daily note', async () => {
    const user = userEvent.setup()
    let deleted = false
    const calls = stubApi((call) => {
      if (call.method === 'DELETE' && call.url === '/api/daily/2026-01-15') {
        deleted = true
        return noContent()
      }
      return readOnlyApi({
        notes: () => NOTES,
        daily: () => (deleted ? DAILY.slice(1) : DAILY),
      })(call)
    })
    const app = await renderSidebarApp({ notes: NOTES, daily: DAILY })

    await openDailySection(user)
    await chooseDailyMenuItem(user, '2026-01-15', 'Delete')
    expect(screen.getByText('Delete daily note?')).toBeTruthy()
    expect(screen.getByText('Delete daily note for 2026-01-15?')).toBeTruthy()
    await user.click(screen.getByTestId('delete-daily-confirm'))

    await waitFor(() => expect(screen.queryByText('2026-01-15')).toBeNull())
    expect(calls.some((call) => call.method === 'DELETE' && call.url === '/api/daily/2026-01-15')).toBe(true)
    expect(calls.filter((call) => call.method === 'GET' && call.url === '/api/daily')).toHaveLength(1)
    expect(app.router.state.location.pathname).toBe('/')
    expect(app.navigationLog).toEqual([])
  })

  it('deleting the open daily note leaves it and never saves it', async () => {
    const user = userEvent.setup()
    let deleted = false
    const calls = stubApi((call) => {
      if (call.method === 'DELETE' && call.url === '/api/daily/2026-01-15') {
        deleted = true
        return noContent()
      }
      return readOnlyApi({
        notes: () => NOTES,
        daily: () => (deleted ? DAILY.slice(1) : DAILY),
        documents: { 'daily/2026-01-15.md': 'day one' },
      })(call)
    })
    bridge.source = 'day one local edit'
    editorStore.loadDocument('daily/2026-01-15.md', 'day one')
    editorStore.setLiveDirty(true)
    const app = await renderSidebarApp({
      notes: NOTES,
      daily: DAILY,
      initialHref: `/document?path=${encodeURIComponent('daily/2026-01-15.md')}`,
    })

    await openDailySection(user)
    await chooseDailyMenuItem(user, '2026-01-15', 'Delete')
    await user.click(screen.getByTestId('delete-daily-confirm'))

    await waitFor(() => expect(app.router.state.location.pathname).toBe('/'))
    expect(app.navigationLog).toEqual(['replace'])
    expect(calls.filter((call) => call.method === 'PUT')).toHaveLength(0)
    expect(app.queryClient.getQueryData(workspaceKeys.document('daily/2026-01-15.md'))).toBeUndefined()
    // The deleted day is never requested again by the daily river either.
    expect(
      calls.filter((call) => call.method === 'GET' && call.url === '/api/daily/2026-01-15'),
    ).toHaveLength(0)
  })
})

describe('Daily move', () => {
  it('moves into a filtered normal note and lands on it', async () => {
    const user = userEvent.setup()
    let moved = false
    const calls = stubApi((call) => {
      if (call.method === 'POST' && call.url === '/api/daily/2026-01-15/move') {
        moved = true
        return ok({ path: TARGET })
      }
      return readOnlyApi({
        notes: () => NOTES,
        daily: () => (moved ? DAILY.slice(1) : DAILY),
        documents: { [TARGET]: moved ? TARGET_CONTENT : '# project' },
      })(call)
    })
    const app = await renderSidebarApp({ notes: NOTES, daily: DAILY })

    await openDailySection(user)
    await chooseDailyMenuItem(user, '2026-01-15', 'Move')
    const list = await screen.findByTestId('move-daily-targets')
    // Only normal notes are eligible; archives never appear.
    expect(list.textContent).toContain('project')
    expect(list.textContent).toContain('inbox')
    expect(list.textContent).not.toContain('old')
    // Client-side filter narrows by name.
    await user.type(screen.getByTestId('move-daily-filter'), 'proj')
    expect(screen.getByTestId('move-daily-targets').textContent).toContain('project')
    expect(screen.getByTestId('move-daily-targets').textContent).not.toContain('inbox')

    await user.click(screen.getByRole('option', { name: /project/ }))

    expect(
      calls.find((call) => call.method === 'POST' && call.url === '/api/daily/2026-01-15/move')?.body,
    ).toEqual({ target_path: TARGET })
    await waitFor(() => expect(app.router.state.location.search).toEqual({ path: TARGET }))
    expect(app.navigationLog).toEqual(['replace'])
    // Daily list, notes list, and the target document all refreshed.
    await waitFor(() => expect(screen.queryByText('2026-01-15')).toBeNull())
    expect(calls.some((call) => call.method === 'GET' && call.url === '/api/daily')).toBe(true)
    expect(calls.some((call) => call.method === 'GET' && call.url === '/api/notes')).toBe(true)
    await waitFor(() => {
      expect(
        calls.some(
          (call) => call.method === 'GET' && call.url === `/api/document?path=${encodeURIComponent(TARGET)}`,
        ),
      ).toBe(true)
    })
    // The merged content reaches the document view.
    await waitFor(() =>
      expect(screen.getByTestId('document-probe')).toHaveTextContent('## 2026-01-15'),
    )
  })

  it('moves a daily note that is not the open document, still landing on the target', async () => {
    const user = userEvent.setup()
    stubApi((call) => {
      if (call.method === 'POST' && call.url === '/api/daily/2026-01-15/move') {
        return ok({ path: TARGET })
      }
      return readOnlyApi({
        notes: () => NOTES,
        daily: () => DAILY.slice(1),
        documents: { [TARGET]: TARGET_CONTENT },
      })(call)
    })
    const app = await renderSidebarApp({ notes: NOTES, daily: DAILY })

    await openDailySection(user)
    await chooseDailyMenuItem(user, '2026-01-15', 'Move')
    await user.type(screen.getByTestId('move-daily-filter'), 'proj')
    await user.keyboard('{Enter}')

    await waitFor(() => expect(app.router.state.location.search).toEqual({ path: TARGET }))
    expect(app.navigationLog).toEqual(['replace'])
  })

  it('a failed move keeps the dialog open with the server error', async () => {
    const user = userEvent.setup()
    stubApi((call) => {
      if (call.method === 'POST' && call.url === '/api/daily/2026-01-15/move') {
        return apiError(404, 'Target note not found', 'not_found')
      }
      return readOnlyApi({ notes: () => NOTES, daily: () => DAILY })(call)
    })
    const app = await renderSidebarApp({ notes: NOTES, daily: DAILY })

    await openDailySection(user)
    await chooseDailyMenuItem(user, '2026-01-15', 'Move')
    await user.type(screen.getByTestId('move-daily-filter'), 'proj')
    await user.click(screen.getByTestId('move-daily-submit'))

    expect(await screen.findByTestId('dialog-error')).toHaveTextContent('Target note not found')
    expect(screen.getByTestId('move-daily-filter')).toHaveValue('proj')
    expect(app.router.state.location.pathname).toBe('/')
    // The daily note is still there (row plus dialog description).
    expect(screen.getAllByText('2026-01-15').length).toBeGreaterThanOrEqual(2)
  })
})
