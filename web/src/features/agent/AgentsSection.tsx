import { useState } from 'react'
import { useNavigate } from '@tanstack/react-router'
import { Plus, LoaderCircle } from 'lucide-react'
import { ContextMenuItem } from '@/components/ui/context-menu'
import { Section, ContextRow } from '@/features/navigation/SidebarSection'
import { useAgentSessions } from '@/queries/agent'
import { useAgentStore } from '@/stores/agent-store'
import type { AgentSessionSummary } from '@/types/agent'
import { SessionDialog } from './SessionDialog'

export function AgentsSection({ filter }: { filter: string }) {
  const query = useAgentSessions()
  const active = useAgentStore((state) => state.activeSessionId)
  const navigate = useNavigate()
  const [dialog, setDialog] = useState<{ kind: 'create' | 'rename' | 'delete'; session?: AgentSessionSummary } | null>(null)
  const sessions = (query.data?.sessions ?? []).filter((session) => session.title.toLowerCase().includes(filter.toLowerCase()))
  return <Section sectionKey="agents" label="Agents" count={sessions.length} action={
    <button aria-label="New Agent session" title="New Agent session" onClick={() => setDialog({ kind: 'create' })} className="rounded p-0.5 text-faint hover:text-fg"><Plus size={11} /></button>
  }>
    {sessions.map((session) => <ContextRow key={session.id} active={session.id === active} onOpen={() => {
      useAgentStore.getState().selectSession(session.id)
      void navigate({ to: '/agent', search: { session: session.id } })
    }} items={<>
      <ContextMenuItem onSelect={() => setDialog({ kind: 'rename', session })}>Rename</ContextMenuItem>
      <ContextMenuItem destructive onSelect={() => setDialog({ kind: 'delete', session })}>Delete</ContextMenuItem>
    </>}>
      <span className="flex min-w-0 items-center gap-2"><span className="min-w-0 flex-1 truncate">{session.title}</span>
        {session.running ? <LoaderCircle size={11} className="shrink-0 animate-spin text-accent" aria-label="Running" /> : session.unread ? <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-ok" aria-label="Unread" /> : null}
      </span>
    </ContextRow>)}
    {query.isError && <p className="px-2 text-xs text-danger">Sessions failed to load.</p>}
    {dialog && <SessionDialog {...dialog} onClose={() => setDialog(null)} />}
  </Section>
}
