import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, expect, it, vi } from 'vitest'
import { CommandPalette } from './CommandPalette'
import { workspaceKeys } from '@/queries/keys'
import { useUiStore } from '@/stores/ui-store'
vi.mock('@tanstack/react-router', () => ({ useNavigate: () => vi.fn() }))
afterEach(() => {
  cleanup()
  useUiStore.setState({ commandPaletteOpen: false })
})
it('keeps the scrolled window in place when selection moves up', async () => {
  const user = userEvent.setup()
  const client = new QueryClient({ defaultOptions: { queries: { staleTime: Infinity } } })
  client.setQueryData(
    workspaceKeys.notes(),
    Array.from({ length: 12 }, (_, i) => ({
      name: `note-${i}`,
      path: `data/${i}.md`,
      archived: false,
    })),
  )
  useUiStore.setState({ commandPaletteOpen: true })
  render(
    <QueryClientProvider client={client}>
      <CommandPalette />
    </QueryClientProvider>,
  )
  const input = screen.getByRole('combobox')
  await user.click(input)
  await user.keyboard('{ArrowDown>10/}')
  const list = screen.getByRole('listbox')
  expect(list).toHaveAttribute('data-viewport-start', '3')
  await user.keyboard('{ArrowUp}')
  expect(list).toHaveAttribute('data-viewport-start', '3')
  expect(screen.getByRole('option', { selected: true })).toHaveClass('selection-row')
  await user.keyboard('{ArrowUp>30/}')
  expect(screen.getByRole('option', { selected: true })).toHaveTextContent('Open Workspace')
})
