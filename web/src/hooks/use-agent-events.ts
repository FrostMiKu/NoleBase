import { useEffect, useRef } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'

import { agentApi } from '@/api/agent'
import { reconnectDelayMs, wsUrl } from '@/api/websocket'
import { agentKeys } from '@/queries/keys'
import { useAgentStore } from '@/stores/agent-store'
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

/** Whether this is the app's first agent sync (page load), not a reconnect. */
let firstAgentSync = true

/**
 * Sync the durable session facts after (re)connecting: `running`, cached
 * permission, the job registry, and — on first load or after a stream
 * interruption — the persisted transcript, so the previous session shows
 * up the way it does in the TUI.
 */
async function syncAgentFromServer(queryClient: ReturnType<typeof useQueryClient>): Promise<void> {
  try {
    const [status, permission, jobRows, history] = await Promise.all([
      agentApi.status(),
      agentApi.permission(),
      agentApi.jobs(),
      agentApi.history(),
    ])
    useAgentStore.getState().setRunning(status.running)
    useAgentStore.getState().setJobs(
      jobRows.jobs.map((row) => ({
        id: row.id,
        label: row.label,
        status: row.status,
        turnId: null,
      })),
    )
    queryClient.setQueryData(agentKeys.state(), status)
    queryClient.setQueryData(agentKeys.permission(), permission)

    const store = useAgentStore.getState()
    if (firstAgentSync || store.turns.length === 0 || store.streamInterrupted) {
      firstAgentSync = false
      // The server transcript is authoritative: seeding recovers output a
      // dropped stream lost, so the interruption notice can be dismissed.
      store.seedHistory(history.entries, status.running, history.message_count)
      if (store.streamInterrupted) {
        store.dismissInterruption()
      }
    }
  } catch (error) {
    toast.error(
      `Agent sync failed: ${error instanceof Error ? error.message : 'unknown error'}`,
    )
  }
}

/**
 * Owns the `/api/ws/agent` connection. Every event flows through the pure
 * reducer in the Zustand agent store; navigation events (`open_file`,
 * `file_moved`) are delegated to the app shell. Lost output is never
 * faked: an interruption stays visible until the next run.
 */
export function useAgentEvents(handlers: AgentEventHandlers): void {
  const queryClient = useQueryClient()
  const handlersRef = useRef(handlers)
  handlersRef.current = handlers

  useEffect(() => {
    let closed = false
    let attempt = 0
    let socket: WebSocket | null = null
    let reconnectTimer: ReturnType<typeof setTimeout> | undefined

    const connect = () => {
      if (closed) return
      useAgentStore.getState().setConnection('connecting')
      socket = new WebSocket(wsUrl('/api/ws/agent'))

      socket.onopen = () => {
        attempt = 0
        useAgentStore.getState().setConnection('connected')
        // Reconnect or first connect: resync the durable session facts
        // over HTTP. The dropped stream itself is not recoverable.
        void syncAgentFromServer(queryClient)
      }

      socket.onmessage = (message) => {
        const parsed = parseEvent<AgentEvent>(
          typeof message.data === 'string' ? message.data : '',
          AGENT_EVENT_TYPES,
        )
        if (!parsed.ok) {
          if (parsed.type !== null) {
            console.warn('[nole] unknown agent event', parsed.type)
          }
          return
        }
        const event = parsed.event
        if (event.type === 'open_file') {
          handlersRef.current.onOpenFile(event.path)
          return
        }
        if (event.type === 'file_moved') {
          handlersRef.current.onFileMoved(event.from, event.to)
          return
        }
        if (event.type === 'resync_required' || event.type === 'job_settled') {
          // A settled job may trigger a session-initiated wake-up run;
          // resync the running flag so the composer stays honest.
          void syncAgentFromServer(queryClient)
        }
        useAgentStore.getState().dispatchEvent(event)
      }

      socket.onclose = () => {
        useAgentStore.getState().markDisconnected()
        if (!closed) {
          reconnectTimer = setTimeout(connect, reconnectDelayMs(attempt))
          attempt += 1
        }
      }

      socket.onerror = () => socket?.close()
    }

    connect()
    return () => {
      closed = true
      if (reconnectTimer !== undefined) clearTimeout(reconnectTimer)
      socket?.close()
    }
  }, [queryClient])
}
