import { ListTodo } from 'lucide-react'

import { useTodos, useToggleTodo } from '@/queries/workspace'

/**
 * Todo panel (right sidebar). Each checkbox flips the task's completion
 * state through the backend toggle endpoint; the array index is the task's
 * identity on the wire, so the list order must stay as delivered.
 */
export function TodoPanel() {
  const todosQuery = useTodos()
  const todos = todosQuery.data ?? []
  const toggle = useToggleTodo()

  return (
    <div className="flex h-full flex-col overflow-y-auto" data-testid="todo-panel">
      <p className="flex items-center gap-1.5 px-3 pt-2.5 pb-1 text-[10px] font-semibold tracking-wider text-faint uppercase">
        <ListTodo size={11} />
        Todos
        <span className="ml-auto font-mono text-[10px]">{todos.length}</span>
      </p>
      {todosQuery.isLoading && <p className="px-3 py-1 text-[11px] text-faint">loading…</p>}
      {todosQuery.isError && (
        <p className="px-3 py-1 text-[11px] text-danger">
          {todosQuery.error instanceof Error ? todosQuery.error.message : 'Failed to load'}
        </p>
      )}
      {todosQuery.isSuccess && todos.length === 0 && (
        <p className="px-3 py-1 text-xs text-muted">
          No tasks yet. Add a - [ ] task to a daily note.
        </p>
      )}
      <ul className="space-y-0.5 px-3 pb-2">
        {todos.map((todo, index) => (
          <li key={index} className="flex items-start gap-2 py-0.5">
            <input
              type="checkbox"
              checked={todo.checked}
              disabled={toggle.isPending}
              onChange={() => toggle.mutate(index)}
              aria-label={`Toggle task: ${todo.text}`}
              className="mt-[3px] h-3 w-3 cursor-pointer accent-[var(--color-accent)] hover:opacity-80"
            />
            <span
              className={`text-[12px] leading-snug ${todo.checked ? 'text-faint line-through' : 'text-fg'}`}
            >
              {todo.text}
            </span>
          </li>
        ))}
      </ul>
    </div>
  )
}
