import { useNavigate } from '@tanstack/react-router'
import { CalendarDays, FileText, Search } from 'lucide-react'

import { useWorkspaceOverview } from '@/queries/workspace'
import { useUiStore } from '@/stores/ui-store'

/** The `/` default page: a compact workspace overview. */
export function WorkspaceHome() {
  const overview = useWorkspaceOverview()
  const navigate = useNavigate()

  return (
    <div className="flex h-full flex-col items-start gap-1 overflow-y-auto p-6 text-sm">
      <h1 className="text-[15px] font-semibold text-fg">Workspace</h1>
      <p className="mb-3 text-xs text-muted">
        Local-first knowledge workspace. Press <kbd className="font-mono">⌘K</kbd> for commands.
      </p>

      {overview.isError && (
        <p className="text-xs text-danger">
          Workspace overview failed to load:{' '}
          {overview.error instanceof Error ? overview.error.message : 'unknown error'}
        </p>
      )}

      <div className="grid grid-cols-2 gap-2">
        <Stat label="Notes" value={overview.data?.notes} />
        <Stat label="Archives" value={overview.data?.archives} />
        <Stat label="Daily notes" value={overview.data?.daily} />
        <Stat label="Open todos" value={overview.data?.todos} />
      </div>
      {overview.data !== undefined && (
        <p className="mt-1 text-[11px] text-faint">
          Index {overview.data.index_ready ? 'ready' : 'still building…'}
        </p>
      )}

      <div className="mt-4 flex flex-col items-start gap-1">
        <Shortcut
          icon={<CalendarDays size={13} />}
          label="Open Daily"
          onClick={() => void navigate({ to: '/daily' })}
        />
        <Shortcut
          icon={<Search size={13} />}
          label="Search workspace"
          onClick={() => void navigate({ to: '/search' })}
        />
        <Shortcut
          icon={<FileText size={13} />}
          label="Browse notes (left sidebar)"
          onClick={() => useUiStore.getState().toggleLeftSidebar()}
        />
      </div>
    </div>
  )
}

function Stat({ label, value }: { label: string; value: number | undefined }) {
  return (
    <div className="min-w-32 rounded-sm border border-border bg-panel px-3 py-2">
      <p className="text-[10px] tracking-wider text-faint uppercase">{label}</p>
      <p className="font-mono text-lg text-fg">{value ?? '–'}</p>
    </div>
  )
}

function Shortcut({
  icon,
  label,
  onClick,
}: {
  icon: React.ReactNode
  label: string
  onClick: () => void
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="inline-flex items-center gap-2 rounded-sm px-2 py-1 text-xs text-muted transition-colors hover:bg-raised hover:text-fg"
    >
      {icon}
      {label}
    </button>
  )
}
