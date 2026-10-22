import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it } from 'vitest'

import { DailyQuickInput, detectCompletion } from './DailyQuickInput'
import { workspaceKeys } from '@/queries/keys'
import type { NoteSummary, TagSummary } from '@/types/workspace'

afterEach(() => {
  cleanup()
  sessionStorage.clear()
})

async function renderInput(data: { tags?: TagSummary[]; notes?: NoteSummary[] }) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  queryClient.setQueryData(workspaceKeys.daily(), [])
  queryClient.setQueryData(workspaceKeys.tags(), data.tags ?? [])
  queryClient.setQueryData(workspaceKeys.notes(), data.notes ?? [])
  return render(
    <QueryClientProvider client={queryClient}>
      <DailyQuickInput />
    </QueryClientProvider>,
  )
}

describe('detectCompletion', () => {
  it('detects an active #tag trigger', () => {
    expect(detectCompletion('hello #rus', 10)).toMatchObject({ kind: 'tag', query: 'rus' })
    expect(detectCompletion('hello #rus', 9)).toMatchObject({ kind: 'tag', query: 'ru' })
    expect(detectCompletion('#', 1)).toMatchObject({ kind: 'tag', query: '' })
  })

  it('does not treat a mid-word hash as a tag trigger', () => {
    expect(detectCompletion('c# comment', 10)).toBeNull()
  })

  it('closes the tag completion at whitespace', () => {
    expect(detectCompletion('#rust done', 10)).toBeNull()
  })

  it('detects an active [[wikilink trigger', () => {
    expect(detectCompletion('see [[note-a', 12)).toMatchObject({ kind: 'wiki', query: 'note-a' })
    expect(detectCompletion('see [[', 6)).toMatchObject({ kind: 'wiki', query: '' })
  })

  it('stops the wikilink at the closing brackets', () => {
    expect(detectCompletion('[[note-a]] and', 14)).toBeNull()
  })

  it('is null with no trigger', () => {
    expect(detectCompletion('plain text', 10)).toBeNull()
  })
})

describe('DailyQuickInput popup', () => {
  it('opens the tag popup on # and accepts with Enter', async () => {
    const user = userEvent.setup()
    await renderInput({
      tags: [
        { name: 'rust', documents: 1, mentions: 1 },
        { name: 't-design', documents: 1, mentions: 1 },
      ],
    })
    const input = screen.getByTestId('daily-quick-input')
    await user.type(input, '#t')
    const popup = screen.getByTestId('completion-popup')
    expect(popup.textContent).toContain('#rust')

    await user.keyboard('{Enter}')
    expect((input as HTMLTextAreaElement).value).toBe('#t-design ')
    expect(screen.queryByTestId('completion-popup')).toBeNull()
  })
})

describe('completion viewport', () => {
  it('reaches matches beyond the first eight, follows only at the edge, and never wraps', async () => {
    const user = userEvent.setup()
    await renderInput({
      notes: Array.from({ length: 12 }, (_, index) => ({
        name: `note-${String(index).padStart(2, '0')}`,
        path: `data/n${index}.md`,
        archived: false,
        modified: null,
      })),
    })
    await user.type(screen.getByTestId('daily-quick-input'), '[['.replaceAll('[', '{[}'))
    await user.keyboard('{ArrowDown>9/}')
    const popup = screen.getByTestId('completion-popup')
    expect(popup).toHaveAttribute('data-viewport-start', '2')
    expect(screen.getByRole('option', { selected: true })).toHaveTextContent('note-09')
    await user.keyboard('{ArrowUp}')
    expect(popup).toHaveAttribute('data-viewport-start', '2')
    expect(screen.getByRole('option', { selected: true })).toHaveTextContent('note-08')
    expect(screen.getByRole('option', { selected: true })).toHaveClass('selection-row')
    await user.keyboard('{ArrowDown>20/}')
    expect(screen.getByRole('option', { selected: true })).toHaveTextContent('note-11')
    await user.keyboard('{ArrowUp>20/}')
    expect(screen.getByRole('option', { selected: true })).toHaveTextContent('note-00')
  })
})
