import { useNavigate } from '@tanstack/react-router'
import { FileText } from 'lucide-react'
import { useNotes, useWorkspaceOverview } from '@/queries/workspace'
import { DailyQuickInput } from './DailyQuickInput'

export function WorkspaceHome() {
  const overview = useWorkspaceOverview()
  const notes = useNotes()
  const navigate = useNavigate()
  const recent = [...(notes.data ?? [])]
    .filter((note) => !note.archived)
    .sort((a, b) => (b.modified ?? '').localeCompare(a.modified ?? ''))
    .slice(0, 6)
  return (
    <div className="h-full overflow-y-auto">
      <div className="mx-auto max-w-4xl space-y-4 p-4">
        <header className="flex flex-wrap items-center gap-x-4 gap-y-2">
          <h1 className="text-[15px] font-semibold">Workspace</h1>
          <dl className="flex flex-wrap gap-x-3 gap-y-1 text-[11px] text-muted">
            {(
              [
                ['Notes', overview.data?.notes],
                ['Archives', overview.data?.archives],
                ['Daily notes', overview.data?.daily],
                ['Tasks', overview.data?.todos],
              ] as const
            ).map(([label, count]) => (
              <div key={label} className="flex gap-1">
                <dd className="text-fg">{count ?? '–'}</dd>
                <dt>{label}</dt>
              </div>
            ))}
          </dl>
        </header>
        {overview.isError && (
          <div role="alert" className="text-xs text-danger">
            Could not load workspace.{' '}
            <button type="button" className="underline" onClick={() => void overview.refetch()}>
              Retry
            </button>
          </div>
        )}
        <section aria-label="Quick capture">
          <h2 className="mb-2 text-sm font-semibold">Capture to today</h2>
          <DailyQuickInput />
        </section>
        <section aria-label="Recent notes">
          <h2 className="text-sm font-semibold">Recent notes</h2>
          <div className="mt-1 space-y-0.5">
            {recent.map((note) => (
              <button
                type="button"
                key={note.path}
                onClick={() => void navigate({ to: '/document', search: { path: note.path } })}
                className="flex w-full items-center gap-3 rounded border border-transparent px-2 py-2 text-left text-sm hover:border-border hover:bg-panel"
              >
                <FileText size={15} className="text-accent" />
                <span className="truncate">{note.name}</span>
              </button>
            ))}
            {notes.isSuccess && recent.length === 0 && (
              <p className="text-xs text-muted">
                Create your first note, or start with a daily capture above.
              </p>
            )}
          </div>
        </section>
      </div>
    </div>
  )
}
