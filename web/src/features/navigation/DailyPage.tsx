import { useNavigate } from '@tanstack/react-router'

import { useDailyNotes } from '@/queries/workspace'

/** The /daily route: every daily note, newest first. */
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
      <ul className="space-y-0.5">
        {daily.map((note) => (
          <li key={note.path}>
            <button
              type="button"
              className="w-full rounded-sm px-2 py-1 text-left transition-colors hover:bg-raised"
              onClick={() => void navigate({ to: '/document', search: { path: note.path } })}
            >
              <span className="font-mono text-[12px] text-accent">{note.date}</span>
              <span className="ml-3 truncate text-[11px] text-faint">{note.path}</span>
            </button>
          </li>
        ))}
      </ul>
    </div>
  )
}
