import userEvent from '@testing-library/user-event'
import type { UserEvent } from '@testing-library/user-event'
import { screen, waitFor } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'

import {
  openSection,
  readOnlyApi,
  renderSidebarApp,
  resetSharedState,
  stubApi,
} from '@/test/sidebar-harness'
import type { DailyNote, NoteSummary } from '@/types/workspace'

const NOTES: NoteSummary[] = [
  { path: 'data/foo.md', name: 'foo', archived: false, modified: null },
  { path: 'data/project.md', name: 'project', archived: false, modified: null },
  { path: 'archives/old.md', name: 'old', archived: true, modified: null },
]

const DAILY: DailyNote[] = [{ date: '2026-01-15', body: 'day text', path: 'daily/2026-01-15.md' }]

afterEach(() => {
  resetSharedState()
  vi.unstubAllGlobals()
})

async function rightClick(user: UserEvent, text: string): Promise<void> {
  await user.pointer({ keys: '[MouseRight]', target: screen.getByText(text) })
}

function menuItemNames(): string[] {
  return screen
    .getAllByRole('menuitem')
    .map((item) => (item.textContent ?? '').replace('…', '').trim())
}

describe('LeftSidebar context menus', () => {
  it('a normal note offers exactly Rename / Archive / Delete', async () => {
    const user = userEvent.setup()
    stubApi(readOnlyApi({ notes: () => NOTES, daily: () => DAILY }))
    await renderSidebarApp({ notes: NOTES, daily: DAILY })

    await rightClick(user, 'foo')
    await waitFor(() => expect(screen.getByRole('menu')).toBeTruthy())
    expect(menuItemNames()).toEqual(['Rename', 'Archive', 'Delete'])
  })

  it('an archived note offers exactly Rename / Restore / Delete', async () => {
    const user = userEvent.setup()
    stubApi(readOnlyApi({ notes: () => NOTES, daily: () => DAILY }))
    await renderSidebarApp({ notes: NOTES, daily: DAILY })

    await openSection(user, 'Archives')
    await rightClick(user, 'old')
    await waitFor(() => expect(screen.getByRole('menu')).toBeTruthy())
    expect(menuItemNames()).toEqual(['Rename', 'Restore', 'Delete'])
  })

  it('a daily row offers exactly Move / Delete', async () => {
    const user = userEvent.setup()
    stubApi(readOnlyApi({ notes: () => NOTES, daily: () => DAILY }))
    await renderSidebarApp({ notes: NOTES, daily: DAILY })

    await openSection(user, 'Daily')
    await rightClick(user, '2026-01-15')
    await waitFor(() => expect(screen.getByRole('menu')).toBeTruthy())
    expect(menuItemNames()).toEqual(['Move', 'Delete'])
  })

  it('right-clicking a row never navigates to the document', async () => {
    const user = userEvent.setup()
    const calls = stubApi(readOnlyApi({ notes: () => NOTES, daily: () => DAILY }))
    const { router } = await renderSidebarApp({ notes: NOTES, daily: DAILY })

    await rightClick(user, 'foo')
    await waitFor(() => expect(screen.getByRole('menu')).toBeTruthy())

    expect(router.state.location.pathname).toBe('/')
    expect(calls.filter((call) => call.url.startsWith('/api/document'))).toEqual([])
  })

  it('activating a menu item opens its dialog instead of the document', async () => {
    const user = userEvent.setup()
    stubApi(readOnlyApi({ notes: () => NOTES, daily: () => DAILY }))
    const { router } = await renderSidebarApp({ notes: NOTES, daily: DAILY })

    await rightClick(user, 'foo')
    await user.click(await screen.findByRole('menuitem', { name: 'Delete' }))

    expect(await screen.findByText('Delete note?')).toBeTruthy()
    expect(router.state.location.pathname).toBe('/')
  })

  it('Escape closes the open menu', async () => {
    const user = userEvent.setup()
    stubApi(readOnlyApi({ notes: () => NOTES, daily: () => DAILY }))
    await renderSidebarApp({ notes: NOTES, daily: DAILY })

    await rightClick(user, 'foo')
    await waitFor(() => expect(screen.getByRole('menu')).toBeTruthy())
    await user.keyboard('{Escape}')
    await waitFor(() => expect(screen.queryByRole('menu')).toBeNull())
  })

  it('a left click still opens the document', async () => {
    const user = userEvent.setup()
    stubApi(
      readOnlyApi({
        notes: () => NOTES,
        daily: () => DAILY,
        documents: { 'data/foo.md': '# foo' },
      }),
    )
    const { router } = await renderSidebarApp({ notes: NOTES, daily: DAILY })

    await user.click(screen.getByText('foo'))
    await waitFor(() => expect(router.state.location.pathname).toBe('/document'))
    expect(router.state.location.search).toEqual({ path: 'data/foo.md' })
  })
})
