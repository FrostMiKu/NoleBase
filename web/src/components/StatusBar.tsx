import { useEditorState } from '@/editor/editor-state'
import { useAgentStore } from '@/stores/agent-store'
import { useUiStore } from '@/stores/ui-store'

function ConnectionDot({ state, label }: { state: 'connecting' | 'connected' | 'disconnected'; label: string }) {
  const color =
    state === 'connected'
      ? 'bg-ok'
      : state === 'connecting'
        ? 'bg-warn'
        : 'bg-danger'
  return (
    <span className="inline-flex items-center gap-1.5" title={`WS: ${state}`}>
      <span className={`inline-block h-1.5 w-1.5 rounded-full ${color}`} />
      <span>{label}</span>
    </span>
  )
}

/** The single dense status strip at the bottom of the shell. */
export function StatusBar() {
  const workspaceConnection = useUiStore((state) => state.workspaceConnection)
  const watcherError = useUiStore((state) => state.watcherError)
  const workspaceResyncing = useUiStore((state) => state.workspaceResyncing)
  const agentConnection = useAgentStore((state) => state.connection)
  const agentRunning = useAgentStore((state) => state.running)
  const { dirty, path, saving } = useEditorState()

  return (
    <footer
      className="flex h-6 shrink-0 items-center gap-3 border-t border-border bg-panel px-3 font-mono text-[11px] text-muted"
      data-testid="status-bar"
    >
      <ConnectionDot state={workspaceConnection} label="WS" />
      <ConnectionDot state={agentConnection} label="Agent" />
      {workspaceResyncing && <span className="text-warn">Resyncing…</span>}
      {watcherError !== null && (
        <span className="text-danger" title={watcherError}>
          Workspace watcher error
        </span>
      )}
      <span className="text-faint">|</span>
      <span>{agentRunning ? 'Agent: running' : 'Agent: idle'}</span>
      <span className="ml-auto inline-flex items-center gap-2">
        {dirty && (
          <span className="flex items-center gap-1 text-warn">
            <span className="inline-block h-1.5 w-1.5 rounded-full bg-warn" />
            {saving ? 'Saving' : 'Modified'}
          </span>
        )}
        <span className="max-w-[40vw] truncate" title={path ?? undefined}>
          {path ?? 'no document'}
        </span>
      </span>
    </footer>
  )
}
