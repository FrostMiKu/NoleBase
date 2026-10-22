import { useUiStore } from '@/stores/ui-store'

function ConnectionDot({
  state,
  label,
}: {
  state: 'connecting' | 'connected' | 'disconnected'
  label: string
}) {
  const color = state === 'connected' ? 'bg-ok' : state === 'connecting' ? 'bg-warn' : 'bg-danger'
  return (
    <span className="inline-flex items-center gap-1.5" title={`WS: ${state}`}>
      <span className={`inline-block h-1.5 w-1.5 rounded-full ${color}`} />
      <span>{label}</span>
    </span>
  )
}

/** Reserve a status strip only when the workspace needs attention. */
export function StatusBar() {
  const workspaceConnection = useUiStore((state) => state.workspaceConnection)
  const watcherError = useUiStore((state) => state.watcherError)
  const workspaceResyncing = useUiStore((state) => state.workspaceResyncing)
  if (workspaceConnection === 'connected' && !workspaceResyncing && watcherError === null) {
    return null
  }

  return (
    <footer
      className="flex h-6 shrink-0 items-center gap-3 border-t border-border bg-panel px-3 font-mono text-[11px] text-muted"
      data-testid="status-bar"
    >
      <ConnectionDot state={workspaceConnection} label="WS" />
      {workspaceConnection !== 'connected' && (
        <span role="status" className="truncate text-warn">
          {workspaceConnection === 'connecting' ? 'Connecting…' : 'Disconnected — reconnecting…'}
        </span>
      )}
      {workspaceResyncing && <span className="text-warn">Resyncing…</span>}
      {watcherError !== null && (
        <span className="text-danger" title={watcherError}>
          Workspace watcher error
        </span>
      )}
    </footer>
  )
}
