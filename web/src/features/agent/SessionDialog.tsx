import { useState } from 'react'
import { useNavigate } from '@tanstack/react-router'
import { useQueryClient } from '@tanstack/react-query'
import { agentApi } from '@/api/agent'
import { agentKeys } from '@/queries/keys'
import { useAgentStore } from '@/stores/agent-store'
import type { AgentSessionSummary } from '@/types/agent'
import { Dialog, DialogContent, DialogTitle, DialogDescription, buttonStyles } from '@/components/ui/dialog'

export function SessionDialog({ kind, session, onClose }: {
  kind: 'create' | 'rename' | 'delete'; session?: AgentSessionSummary; onClose: () => void
}) {
  const [title, setTitle] = useState(session?.title ?? '')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const client = useQueryClient()
  const navigate = useNavigate()
  const submit = async () => {
    if (busy) return
    setBusy(true)
    setError(null)
    try {
      if (kind === 'create') {
        const created = await agentApi.create(title.trim())
        useAgentStore.getState().selectSession(created.id)
        void navigate({ to: '/agent', search: { session: created.id } })
      } else if (session && kind === 'rename') {
        await agentApi.rename(session.id, title.trim())
      } else if (session) {
        await agentApi.delete(session.id)
        if (useAgentStore.getState().activeSessionId === session.id) {
          useAgentStore.getState().selectSession(null)
          void navigate({ to: '/agent', search: {} })
        }
      }
      await client.invalidateQueries({ queryKey: agentKeys.sessions() })
      onClose()
    } catch (error) { setError(error instanceof Error ? error.message : 'Session command failed') }
    finally { setBusy(false) }
  }
  return <Dialog open onOpenChange={(open) => { if (!open && !busy) onClose() }}>
    <DialogContent onInteractOutside={(event) => event.preventDefault()}>
      <DialogTitle>{kind === 'create' ? 'New Agent session' : kind === 'rename' ? 'Rename Agent session' : 'Delete Agent session'}</DialogTitle>
      <DialogDescription>{kind === 'delete' ? `Delete ${session?.title ?? 'this Session'} and its conversation?` : 'Give this Session a name.'}</DialogDescription>
      <form className="mt-3 flex flex-col gap-3" onSubmit={(event) => { event.preventDefault(); void submit() }}>
        {kind !== 'delete' && <label className="flex flex-col gap-1 text-xs">Name
          <input autoFocus value={title} onChange={(event) => setTitle(event.target.value)} maxLength={200} className="rounded border border-border bg-bg px-2 py-1.5" />
        </label>}
        {error && <p role="alert" className="text-xs text-danger">{error}</p>}
        <div className="flex justify-end gap-2">
          <button type="button" disabled={busy} className={buttonStyles.secondary} onClick={onClose}>Cancel</button>
          <button disabled={busy || (kind !== 'delete' && !title.trim())} className={kind === 'delete' ? buttonStyles.danger : buttonStyles.primary}>{busy ? 'Saving…' : kind === 'delete' ? 'Delete' : 'Save'}</button>
        </div>
      </form>
    </DialogContent>
  </Dialog>
}
