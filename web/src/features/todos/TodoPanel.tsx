import { ListTodo } from 'lucide-react'

import { useTodos } from '@/queries/workspace'

/**
 * Read-only todo panel (right sidebar). Checkboxes are disabled visuals —
 * the backend has no todo mutation contract this phase.
 */
export function TodoPanel() {
  const todosQuery = useTodos()
  const todos = todosQuery.data ?? []

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
      <ul className="space-y-0.5 px-3 pb-3">
        {todos.map((todo, index) => (
          <li key={index} className="flex items-start gap-2 py-0.5">
            <input
              type="checkbox"
              checked={todo.checked}
              disabled
              readOnly
              className="mt-[3px] h-3 w-3 accent-[var(--color-accent)]"
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
