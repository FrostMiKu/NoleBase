import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, render, screen } from '@testing-library/react'
import { afterEach, describe, expect, it } from 'vitest'

import { TodoPanel } from './TodoPanel'
import { workspaceKeys } from '@/queries/keys'
import type { TodoItem } from '@/types/workspace'

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

afterEach(cleanup)

describe('TodoPanel', () => {
  it('renders read-only todos with disabled checkboxes', () => {
    renderWithTodos([
      { checked: false, text: 'write the web ui' },
      { checked: true, text: 'ship the backend' },
    ])
    expect(screen.getByTestId('todo-panel').textContent).toContain('write the web ui')
    expect(screen.getByTestId('todo-panel').textContent).toContain('ship the backend')
    const checkboxes = screen.getAllByRole('checkbox') as HTMLInputElement[]
    expect(checkboxes).toHaveLength(2)
    for (const box of checkboxes) {
      expect(box.disabled).toBe(true)
    }
    expect(checkboxes[1]?.checked).toBe(true)
  })

  it('renders the empty state without checkboxes', () => {
    renderWithTodos([])
    expect(screen.getByTestId('todo-panel').textContent).not.toContain('todos')
    expect(screen.queryByRole('checkbox')).toBeNull()
  })
})
