import type {
  AgentNotification,
  ApprovalRequest,
  AskUserRequest,
  ContextWindowInfo,
  ResponseTimingInfo,
  TerminalInputRequest,
  ToolRun,
  TokenUsage,
} from '@/types/agent'
import type { AgentEvent } from '@/types/events'

/**
 * Live runtime projection of the agent event stream. This is a projection,
 * not chat history: each new run replaces the previous run's text, tools,
 * and pending interactions (notifications are kept, capped).
 */
export interface AgentState {
  running: boolean

  assistantText: string
  thinkingText: string

  tools: ToolRun[]

  approval: ApprovalRequest | null
  askUser: AskUserRequest | null
  terminalInput: TerminalInputRequest | null

  usage: TokenUsage | null
  contextWindow: ContextWindowInfo | null
  timing: ResponseTimingInfo | null

  round: number | null
  retries: number
  messageCount: number | null

  stopReason: string | null
  finished: { ok: boolean; text: string | null; error: string | null } | null

  streamInterrupted: boolean

  notifications: AgentNotification[]
  /** Monotonic sequence for notification keys. */
  notificationSeq: number

  runtimeNotes: string[]
}

export const MAX_NOTIFICATIONS = 50
const MAX_RUNTIME_NOTES = 20

export function initialAgentState(): AgentState {
  return {
    running: false,
    assistantText: '',
    thinkingText: '',
    tools: [],
    approval: null,
    askUser: null,
    terminalInput: null,
    usage: null,
    contextWindow: null,
    timing: null,
    round: null,
    retries: 0,
    messageCount: null,
    stopReason: null,
    finished: null,
    streamInterrupted: false,
    notifications: [],
    notificationSeq: 0,
    runtimeNotes: [],
  }
}

function appendRuntimeNote(notes: string[], note: string): string[] {
  const next = [...notes, note]
  return next.length > MAX_RUNTIME_NOTES ? next.slice(next.length - MAX_RUNTIME_NOTES) : next
}

function updateTool(tools: ToolRun[], key: string, update: (tool: ToolRun) => ToolRun): ToolRun[] {
  const index = tools.findIndex((tool) => tool.key === key)
  if (index === -1) return tools
  const next = [...tools]
  next[index] = update(next[index] as ToolRun)
  return next
}

function findToolKey(tools: ToolRun[], predicate: (tool: ToolRun) => boolean): string | null {
  return tools.find(predicate)?.key ?? null
}

/**
 * A new run started (the `POST /api/agent/run` returned 202). Transient
 * per-run state resets; notifications are kept for the session, capped.
 */
export function beginAgentRun(state: AgentState): AgentState {
  return {
    ...state,
    running: true,
    assistantText: '',
    thinkingText: '',
    tools: [],
    approval: null,
    askUser: null,
    terminalInput: null,
    round: null,
    retries: 0,
    stopReason: null,
    finished: null,
    streamInterrupted: false,
    runtimeNotes: [],
  }
}

function clearPendingInteractions(state: AgentState): AgentState {
  return { ...state, approval: null, askUser: null, terminalInput: null }
}

/**
 * Pure reducer for the `/api/ws/agent` event stream. Exhaustive over
 * `AgentEvent`; the `default` branch is unreachable for known events and
 * exists only so TypeScript flags any future unhandled variant.
 */
export function reduceAgentEvent(state: AgentState, event: AgentEvent): AgentState {
  switch (event.type) {
    case 'assistant_delta':
      return { ...state, assistantText: state.assistantText + event.text }

    case 'assistant_message_finished':
      // The finished message is authoritative over the delta accumulation
      // (protects against a dropped delta mid-stream).
      return { ...state, assistantText: event.text }

    case 'thinking_delta':
      return { ...state, thinkingText: state.thinkingText + event.text }

    case 'thinking_finished':
      // No payload; the accumulated thinking text stays available.
      return state

    case 'tool_preparing': {
      const key = `preparing:${event.index}`
      const existing = findToolKey(state.tools, (tool) => tool.key === key)
      const tool: ToolRun = {
        key,
        kind: 'tool',
        id: null,
        status: 'preparing',
        label: event.message,
        detail: null,
      }
      return {
        ...state,
        tools: existing !== null ? updateTool(state.tools, key, () => tool) : [...state.tools, tool],
      }
    }

    case 'tool_preparation_finished': {
      const key = `preparing:${event.index}`
      const existing = findToolKey(state.tools, (tool) => tool.key === key)
      if (existing === null) {
        // Preparation finished without a preparing marker (e.g. after a
        // reconnect): synthesize the row so the id is tracked.
        return {
          ...state,
          tools: [
            ...state.tools,
            {
              key: event.id !== null ? `tool:${event.id}` : key,
              kind: 'tool',
              id: event.id,
              status: 'running',
              label: 'prepared',
              detail: null,
            },
          ],
        }
      }
      return {
        ...state,
        tools: updateTool(state.tools, key, (tool) => ({
          ...tool,
          id: event.id,
          status: 'running',
          key: event.id !== null ? `tool:${event.id}` : tool.key,
        })),
      }
    }

    case 'tool_started': {
      const key = `tool:${event.id}`
      const existing = findToolKey(state.tools, (tool) => tool.id === event.id)
      const tool: ToolRun = {
        key: existing ?? key,
        kind: 'tool',
        id: event.id,
        status: 'running',
        label: event.message,
        detail: null,
      }
      return {
        ...state,
        tools:
          existing !== null
            ? updateTool(state.tools, existing, () => tool)
            : [...state.tools, tool],
      }
    }

    case 'tool_updated': {
      const key = findToolKey(state.tools, (tool) => tool.id === event.id)
      if (key === null) return state
      return { ...state, tools: updateTool(state.tools, key, (tool) => ({ ...tool, label: event.message })) }
    }

    case 'tool_finished': {
      const key = findToolKey(state.tools, (tool) => tool.id === event.id)
      if (key === null) {
        return {
          ...state,
          tools: [
            ...state.tools,
            {
              key: `tool:${event.id}`,
              kind: 'tool',
              id: event.id,
              status: 'finished',
              label: event.message,
              detail: event.preview,
            },
          ],
        }
      }
      return {
        ...state,
        tools: updateTool(state.tools, key, (tool) => ({
          ...tool,
          status: 'finished',
          label: event.message,
          detail: event.preview,
        })),
      }
    }

    case 'job_started':
      return {
        ...state,
        tools: [
          ...state.tools,
          {
            key: `job:${event.id}`,
            kind: 'job',
            id: event.id,
            status: 'running',
            label: event.label,
            detail: null,
          },
        ],
      }

    case 'job_settled': {
      const key = `job:${event.id}`
      if (findToolKey(state.tools, (tool) => tool.key === key) === null) {
        return {
          ...state,
          tools: [
            ...state.tools,
            {
              key,
              kind: 'job',
              id: event.id,
              status: event.failed ? 'failed' : 'finished',
              label: 'background job',
              detail: null,
            },
          ],
        }
      }
      return {
        ...state,
        tools: updateTool(state.tools, key, (tool) => ({
          ...tool,
          status: event.failed ? 'failed' : 'finished',
        })),
      }
    }

    case 'buffered_input_consumed':
      return {
        ...state,
        runtimeNotes: appendRuntimeNote(state.runtimeNotes, `Consumed ${event.count} buffered input(s)`),
      }

    case 'job_delivered':
      return {
        ...state,
        runtimeNotes: appendRuntimeNote(state.runtimeNotes, `Delivered: ${event.labels.join(', ')}`),
      }

    case 'usage':
      return { ...state, usage: event.usage }

    case 'context_window':
      return { ...state, contextWindow: { tokens: event.tokens, capacity: event.capacity } }

    case 'response_timing':
      return { ...state, timing: { output_tokens: event.output_tokens, elapsed_ms: event.elapsed_ms } }

    case 'retry':
      return {
        ...state,
        retries: state.retries + 1,
        runtimeNotes: appendRuntimeNote(state.runtimeNotes, 'Retrying request'),
      }

    case 'round':
      return { ...state, round: event.current }

    case 'conversation_updated':
      // Only a count crosses the wire; the web UI is a live projection,
      // not a conversation browser.
      return { ...state, messageCount: event.message_count }

    case 'notification': {
      const seq = state.notificationSeq + 1
      const notifications = [...state.notifications, { id: seq, message: event.message }]
      return {
        ...state,
        notificationSeq: seq,
        notifications:
          notifications.length > MAX_NOTIFICATIONS
            ? notifications.slice(notifications.length - MAX_NOTIFICATIONS)
            : notifications,
      }
    }

    case 'file_moved':
      // Handled by the app shell (navigation), not the projection.
      return state

    case 'open_file':
      // Handled by the app shell (navigation), not the projection.
      return state

    case 'approval_requested':
      return {
        ...state,
        running: true,
        approval: { title: event.title, message: event.message, kind: event.kind },
      }

    case 'ask_user_requested':
      return {
        ...state,
        running: true,
        askUser: { question: event.question, options: event.options },
      }

    case 'terminal_input_requested':
      return {
        ...state,
        running: true,
        terminalInput: {
          session_id: event.session_id,
          purpose: event.purpose,
          prompt: event.prompt,
        },
      }

    case 'stopped':
      return {
        ...clearPendingInteractions(state),
        running: false,
        stopReason: event.reason,
      }

    case 'finished': {
      const next: AgentState = {
        ...clearPendingInteractions(state),
        running: false,
        finished: { ok: event.ok, text: event.text, error: event.error },
      }
      return event.text !== null ? { ...next, assistantText: event.text } : next
    }

    case 'resync_required':
      // Events were dropped by a slow consumer; the missing output cannot
      // be recovered, so the UI says so until the next run.
      return { ...state, streamInterrupted: true }

    default: {
      const unreachable: never = event
      // Known-at-compile-time events never reach this point; new backend
      // variants must be handled explicitly above.
      console.warn('[nole] unhandled agent event', unreachable)
      return state
    }
  }
}
