import { useNavigate } from '@tanstack/react-router'
import { CalendarDays, Pencil } from 'lucide-react'

import { useDailyNotes } from '@/queries/workspace'
import { Markdown } from '@/features/agent/Markdown'

/**
 * The /daily route: a river of markdown-rendered daily note cards,
 * newest first. The card header opens the note for editing.
 */
export function DailyPage() {
  const dailyQuery = useDailyNotes()
  const navigate = useNavigate()
  const daily = [...(dailyQuery.data ?? [])].reverse()

  return (
    <div className="flex h-full flex-col overflow-y-auto p-4">
      <h1 className="mb-3 text-[15px] font-semibold text-fg">Daily</h1>
      {dailyQuery.isLoading && <p className="text-xs text-faint">Loading daily notes…</p>}
      {dailyQuery.isError && (
        <p className="text-xs text-danger">
          Failed to load daily notes:{' '}
          {dailyQuery.error instanceof Error ? dailyQuery.error.message : 'unknown error'}
        </p>
      )}
      {daily.length === 0 && !dailyQuery.isLoading && !dailyQuery.isError && (
        <p className="text-xs text-faint">No daily notes yet.</p>
      )}

      <div className="flex flex-col gap-2" data-testid="daily-river">
        {daily.map((note) => (
          <article
            key={note.path}
            className="rounded-md border border-border bg-panel"
            data-testid="daily-card"
          >
            <button
              type="button"
              onClick={() => void navigate({ to: '/document', search: { path: note.path } })}
              className="flex w-full items-center gap-2 border-b border-border px-3 py-1.5 text-left transition-colors hover:bg-raised"
              title={`Open ${note.path}`}
            >
              <CalendarDays size={12} className="shrink-0 text-accent" />
              <span className="font-mono text-[12px] text-accent-strong">{note.date}</span>
              <span className="ml-auto inline-flex items-center gap-1 font-mono text-[10px] text-faint">
                <Pencil size={10} />
                edit
              </span>
            </button>
            <div className="px-3 py-2">
              <Markdown text={note.body} />
            </div>
          </article>
        ))}
      </div>
    </div>
  )
}
