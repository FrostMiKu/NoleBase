import { AgentPanel } from '@/features/agent/AgentPanel'

/**
 * The `/agent` route: the TUI-style main Agent view, occupying the whole
 * center area. It renders the same live projection as the sidebar panel
 * (one Zustand store, one WS stream); the sidebar agent panel is hidden
 * while this view is active.
 */
export function AgentPage() {
  return (
    <div className="h-full min-w-0">
      <AgentPanel />
    </div>
  )
}
