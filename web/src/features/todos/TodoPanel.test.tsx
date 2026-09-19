import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { TodoPanel } from './TodoPanel'
import { workspaceApi } from '@/api/workspace'
import { workspaceKeys } from '@/queries/keys'
import type { TodoItem } from '@/types/workspace'

vi.mock('@/api/workspace', () => ({
  workspaceApi: {
    todos: vi.fn(() => Promise.resolve<TodoItem[]>([])),
    toggleTodo: vi.fn(() =>
      Promise.resolve([
        { checked: false, text: 'write the web ui' },
        { checked: true, text: 'ship the backend' },
      ]),
    ),
  },
}))

function renderWithTodos(todos: TodoItem[]) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  queryClient.setQueryData(workspaceKeys.todos(), todos)
  return render(
    <QueryClientProvider client={queryClient}>
      <TodoPanel />
    </QueryClientProvider>,
  )
}

afterEach(() => {
  cleanup()
  vi.clearAllMocks()
})

describe('TodoPanel', () => {
  it('renders todos with clickable checkboxes', () => {
    renderWithTodos([
      { checked: false, text: 'write the web ui' },
      { checked: true, text: 'ship the backend' },
    ])
    expect(screen.getByTestId('todo-panel').textContent).toContain('write the web ui')
    expect(screen.getByTestId('todo-panel').textContent).toContain('ship the backend')
    const checkboxes = screen.getAllByRole('checkbox') as HTMLInputElement[]
    expect(checkboxes).toHaveLength(2)
    for (const box of checkboxes) {
      expect(box.disabled).toBe(false)
    }
    expect(checkboxes[1]?.checked).toBe(true)
  })

  it('toggles a task when its checkbox is clicked', async () => {
    const user = userEvent.setup()
    renderWithTodos([
      { checked: false, text: 'write the web ui' },
      { checked: false, text: 'ship the backend' },
    ])

    // The clicked checkbox's array index is the task's wire identity.
    await user.click(screen.getAllByRole('checkbox')[1]!)
    await waitFor(() => {
      expect(vi.mocked(workspaceApi.toggleTodo)).toHaveBeenCalledWith(1)
    })

    // The refreshed server list replaces the cache: the second task now
    // renders done, the first stays open.
    await waitFor(() => {
      const checkboxes = screen.getAllByRole('checkbox') as HTMLInputElement[]
      expect(checkboxes[0]?.checked).toBe(false)
      expect(checkboxes[1]?.checked).toBe(true)
      expect(screen.getByText('ship the backend')).toHaveClass('line-through')
      expect(screen.getByText('write the web ui')).not.toHaveClass('line-through')
    })
  })

  it('renders the empty state without checkboxes', () => {
    renderWithTodos([])
    expect(screen.getByTestId('todo-panel').textContent).not.toContain('todos')
    expect(screen.queryByRole('checkbox')).toBeNull()
  })
})
