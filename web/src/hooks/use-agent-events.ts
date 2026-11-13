import { useEffect, useRef } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'

import { agentApi } from '@/api/agent'
import { reconnectDelayMs, wsUrl } from '@/api/websocket'
import { agentKeys } from '@/queries/keys'
import { useAgentStore, isSessionVisible } from '@/stores/agent-store'
import type { AgentEvent } from '@/types/events'
import { parseEvent } from '@/types/events'

export interface AgentEventHandlers {
  /** Navigate to a workspace-relative document path (dirty-guarded). */
  onOpenFile: (path: string) => void
  /** The agent reported a file move; the app shell decides the reaction. */
  onFileMoved: (from: string, to: string) => void
}

const AGENT_EVENT_TYPES = [
  'assistant_delta',
  'assistant_message_finished',
  'thinking_delta',
  'thinking_finished',
  'tool_preparing',
  'tool_preparation_finished',
  'job_started',
  'job_settled',
  'buffered_input_consumed',
  'job_delivered',
  'tool_started',
  'tool_updated',
  'tool_finished',
  'usage',
  'context_window',
  'response_timing',
  'retry',
  'round',
  'conversation_updated',
  'notification',
  'file_moved',
  'open_file',
  'approval_requested',
  'ask_user_requested',
  'terminal_input_requested',
  'stopped',
  'finished',
  'resync_required',
] as const

/** One subscription follows the selected Session; other Session runtimes keep working. */
export function useAgentEvents(handlers: AgentEventHandlers): void {
  const queryClient = useQueryClient()
  const id = useAgentStore((state) => state.activeSessionId)
  const handlersRef = useRef(handlers)
  handlersRef.current = handlers

  useEffect(() => {
    if (id === null) return
    let closed = false
    let attempt = 0
    let socket: WebSocket | null = null
    let timer: ReturnType<typeof setTimeout> | undefined
    const current = () => !closed && useAgentStore.getState().activeSessionId === id
    const markRead = () => {
      if (document.visibilityState !== 'hidden' && isSessionVisible(id)) void agentApi.read(id).then(() => queryClient.invalidateQueries({ queryKey: agentKeys.sessions() })).catch(() => {})
    }
    const connect = () => {
      if (!current()) return
      useAgentStore.getState().setConnection('connecting')
      socket = new WebSocket(wsUrl(`/api/agents/${encodeURIComponent(id)}/events`))
      socket.onopen = () => { attempt = 0 }
      socket.onmessage = (message) => {
        if (!current() || typeof message.data !== 'string') return
        let raw: Record<string, unknown>
        try { raw = JSON.parse(message.data) as Record<string, unknown> } catch { return }
        if (raw['type'] === 'session_snapshot') {
          const store = useAgentStore.getState()
          store.setRunning(raw['running'] === true)
          store.seedHistory(raw['entries'] as import('@/types/agent').AgentHistoryEntry[], raw['running'] === true, Number(raw['message_count']), raw['live_tools'] as import('@/types/agent').AgentLiveTool[])
          store.setConnection('connected')
          store.dismissInterruption()
          queryClient.setQueryData(agentKeys.permission(id), { mode: raw['permission'] })
          for (const event of (raw['interactions'] ?? []) as AgentEvent[]) store.dispatchEvent(event)
          if (typeof raw['save_error'] === 'string') toast.error(`Session save failed: ${raw['save_error']}`)
          void agentApi.jobs(id).then((rows) => {
            if (current()) useAgentStore.getState().setJobs(rows.jobs.map((row) => ({ ...row, turnId: null })))
          }).catch(() => {})
          markRead()
          return
        }
        const parsed = parseEvent<AgentEvent>(message.data, AGENT_EVENT_TYPES)
        if (!parsed.ok) return
        const event = parsed.event
        if (event.type === 'resync_required') { socket?.close(); return }
        if (event.type === 'open_file') { handlersRef.current.onOpenFile(event.path); return }
        if (event.type === 'file_moved') { handlersRef.current.onFileMoved(event.from, event.to); return }
        useAgentStore.getState().dispatchEvent(event)
        if (event.type === 'finished' || event.type === 'stopped') markRead()
      }
      socket.onclose = () => {
        if (!current()) return
        useAgentStore.getState().markDisconnected()
        timer = setTimeout(connect, reconnectDelayMs(attempt++))
      }
      socket.onerror = () => socket?.close()
    }
    document.addEventListener('visibilitychange', markRead)
    connect()
    return () => { document.removeEventListener('visibilitychange', markRead); closed = true; if (timer !== undefined) clearTimeout(timer); socket?.close() }
  }, [id, queryClient])
}
