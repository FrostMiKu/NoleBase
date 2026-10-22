import type {
  AgentHistoryEntry,
  AgentNotification,
  ApprovalRequest,
  AskUserRequest,
  ContextWindowInfo,
  TerminalInputRequest,
  ToolRun,
  TokenUsage,
} from '@/types/agent'
import type { AgentEvent } from '@/types/events'

/**
 * A background job (subagent run) tracked at session level. Job identity
 * is global — a job routinely outlives the turn that spawned it — so its
 * lifecycle state lives here, not inside turn timelines.
 */
export interface JobRun {
  id: string
  label: string
  status: 'running' | 'finished' | 'failed' | 'cancelled'
  /** The turn that spawned the job, for display. */
  turnId: number | null
}

/**
 * One conversational turn: the user prompt that started the run plus
 * everything the agent streamed for it. Turns accumulate — a new run
 * appends instead of overwriting the previous turn (the live text of the
 * newest turn is still the only thing the composer interacts with).
 */
export interface AgentTurn {
  id: number
  /** The prompt that started this run; null when the run was started elsewhere. */
  prompt: string | null
  assistantText: string
  thinkingText: string
  tools: ToolRun[]
  runtimeNotes: string[]
  usage: TokenUsage | null
  timing: { output_tokens: number; elapsed_ms: number } | null
  round: number | null
  retries: number
  finished: { ok: boolean; text: string | null; error: string | null } | null
  stopReason: string | null
  /**
   * Seeded from the server transcript (`GET /api/agent/history`) rather
   * than built from live events. Restored turns are closed: live events
   * open fresh turns instead of appending to them.
   */
  restored: boolean
}

/**
 * Live runtime projection of the agent event stream. Turns accumulate up
 * to {@link MAX_TURNS}; the authoritative conversation lives in the
 * backend session, this is the visible window on it.
 */
export interface AgentState {
  running: boolean

  turns: AgentTurn[]
  turnSeq: number

  approval: ApprovalRequest | null
  askUser: AskUserRequest | null
  terminalInput: TerminalInputRequest | null

  /** Session-level background-job registry, ordered by first appearance. */
  jobs: JobRun[]

  contextWindow: ContextWindowInfo | null
  messageCount: number | null

  streamInterrupted: boolean

  notifications: AgentNotification[]
  /** Monotonic sequence for notification keys. */
  notificationSeq: number
}

export const MAX_NOTIFICATIONS = 50
export const MAX_TURNS = 20

export function initialAgentState(): AgentState {
  return {
    running: false,
    turns: [],
    turnSeq: 0,
    approval: null,
    askUser: null,
    terminalInput: null,
    jobs: [],
    contextWindow: null,
    messageCount: null,
    streamInterrupted: false,
    notifications: [],
    notificationSeq: 0,
  }
}

function freshTurn(id: number, prompt: string | null): AgentTurn {
  return {
    id,
    prompt,
    assistantText: '',
    thinkingText: '',
    tools: [],
    runtimeNotes: [],
    usage: null,
    timing: null,
    round: null,
    retries: 0,
    finished: null,
    stopReason: null,
    restored: false,
  }
}

function appendTurn(state: AgentState, prompt: string | null): AgentState {
  const seq = state.turnSeq + 1
  const turns = [...state.turns, freshTurn(seq, prompt)]
  return {
    ...state,
    turnSeq: seq,
    running: true,
    turns: turns.length > MAX_TURNS ? turns.slice(turns.length - MAX_TURNS) : turns,
  }
}

function turnClosed(turn: AgentTurn): boolean {
  return turn.finished !== null || turn.stopReason !== null || turn.restored
}

/**
 * Content events belong to the newest open turn. If the last turn is
 * closed (or none exists), a run is in progress that this client did not
 * begin — open an implicit turn for it.
 */
function withOpenTurn(state: AgentState, update: (turn: AgentTurn) => AgentTurn): AgentState {
  const last = state.turns[state.turns.length - 1]
  const base = last !== undefined && !turnClosed(last) ? state : appendTurn(state, null)
  return updateLastTurn(base, update)
}

/**
 * Meta events (usage, timing, retries, finish markers) attach to the last
 * turn even if it is already closed — they routinely arrive after
 * `finished` — and only create a turn when none exists at all.
 */
function withLastTurn(state: AgentState, update: (turn: AgentTurn) => AgentTurn): AgentState {
  if (state.turns.length === 0) {
    return updateLastTurn(appendTurn(state, null), update)
  }
  return updateLastTurn(state, update)
}

const MAX_JOBS = 50

/** Insert or update a session-level job entry, keyed by job id. */
function upsertJob(
  state: AgentState,
  id: string,
  label: string,
  status: JobRun['status'],
): AgentState {
  const existing = state.jobs.find((job) => job.id === id)
  if (existing !== undefined) {
    return {
      ...state,
      jobs: state.jobs.map((job) =>
        job.id === id ? { ...job, label, status, turnId: job.turnId } : job,
      ),
    }
  }
  const turnId = state.turns[state.turns.length - 1]?.id ?? null
  const jobs = [...state.jobs, { id, label, status, turnId }]
  return {
    ...state,
    jobs: jobs.length > MAX_JOBS ? jobs.slice(jobs.length - MAX_JOBS) : jobs,
  }
}

function updateLastTurn(state: AgentState, update: (turn: AgentTurn) => AgentTurn): AgentState {
  const turns = [...state.turns]
  const last = turns[turns.length - 1]
  if (last === undefined) return state
  turns[turns.length - 1] = update(last)
  return { ...state, turns }
}

function appendRuntimeNote(notes: string[], note: string): string[] {
  const next = [...notes, note]
  return next.length > 20 ? next.slice(next.length - 20) : next
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
 * A new run started (the `POST /api/agent/run` returned 202). Appends a
 * turn carrying the prompt; pending interactions clear; the interruption
 * notice clears with the fresh run. Notifications are kept, capped.
 */
export function beginAgentRun(state: AgentState, prompt: string): AgentState {
  return {
    ...clearPendingInteractions(appendTurn(state, prompt)),
    streamInterrupted: false,
  }
}

/** WS output can arrive before the run HTTP response; attach the prompt
 * to that turn without clearing output, approval, or its finished status. */
export function acknowledgeAgentRun(
  state: AgentState,
  prompt: string,
  previousSeq: number,
): AgentState {
  const streamed = state.turns.find((turn) => turn.id > previousSeq && turn.prompt === null)
  if (!streamed) return beginAgentRun(state, prompt)
  return {
    ...state,
    turns: state.turns.map((turn) => (turn.id === streamed.id ? { ...turn, prompt } : turn)),
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
      return withOpenTurn(state, (turn) => ({
        ...turn,
        assistantText: turn.assistantText + event.text,
      }))

    case 'assistant_message_finished':
      // The finished message is authoritative over the delta accumulation
      // (protects against a dropped delta mid-stream).
      return withOpenTurn(state, (turn) => ({ ...turn, assistantText: event.text }))

    case 'thinking_delta':
      return withOpenTurn(state, (turn) => ({
        ...turn,
        thinkingText: turn.thinkingText + event.text,
      }))

    case 'thinking_finished':
      // No payload; the accumulated thinking text stays available.
      return state

    case 'tool_preparing': {
      const key = `preparing:${event.index}`
      return withOpenTurn(state, (turn) => {
        const existing = findToolKey(turn.tools, (tool) => tool.key === key)
        const tool: ToolRun = {
          key,
          kind: 'tool',
          id: null,
          status: 'preparing',
          label: event.message,
          detail: null,
        }
        return {
          ...turn,
          tools:
            existing !== null ? updateTool(turn.tools, key, () => tool) : [...turn.tools, tool],
        }
      })
    }

    case 'tool_preparation_finished': {
      const key = `preparing:${event.index}`
      return withOpenTurn(state, (turn) => {
        const existing = findToolKey(turn.tools, (tool) => tool.key === key)
        if (existing === null) {
          // Preparation finished without a preparing marker (e.g. after a
          // reconnect): synthesize the row so the id is tracked.
          return {
            ...turn,
            tools: [
              ...turn.tools,
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
          ...turn,
          tools: updateTool(turn.tools, key, (tool) => ({
            ...tool,
            id: event.id,
            status: 'running',
            key: event.id !== null ? `tool:${event.id}` : tool.key,
          })),
        }
      })
    }

    case 'tool_started': {
      const fallbackKey = `tool:${event.id}`
      return withOpenTurn(state, (turn) => {
        const existing = findToolKey(turn.tools, (tool) => tool.id === event.id)
        const tool: ToolRun = {
          key: existing ?? fallbackKey,
          kind: 'tool',
          id: event.id,
          status: 'running',
          label: event.message,
          detail: null,
        }
        return {
          ...turn,
          tools:
            existing !== null
              ? updateTool(turn.tools, existing, () => tool)
              : [...turn.tools, tool],
        }
      })
    }

    case 'tool_updated':
      return withOpenTurn(state, (turn) => {
        const key = findToolKey(turn.tools, (tool) => tool.id === event.id)
        if (key === null) return turn
        return {
          ...turn,
          tools: updateTool(turn.tools, key, (tool) => ({ ...tool, label: event.message })),
        }
      })

    case 'tool_finished':
      return withOpenTurn(state, (turn) => {
        const key = findToolKey(turn.tools, (tool) => tool.id === event.id)
        if (key === null) {
          return {
            ...turn,
            tools: [
              ...turn.tools,
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
          ...turn,
          tools: updateTool(turn.tools, key, (tool) => ({
            ...tool,
            status: 'finished',
            label: event.message,
            detail: event.preview,
          })),
        }
      })

    case 'job_started':
      return upsertJob(state, event.id, event.label, 'running')

    case 'job_settled': {
      // Update the job's own registry entry; if the start event was lost
      // (a lagged stream), surface the settlement as its own entry.
      const existing = state.jobs.find((job) => job.id === event.id)
      const label = existing?.label ?? 'background job'
      return upsertJob(state, event.id, label, event.failed ? 'failed' : 'finished')
    }

    case 'buffered_input_consumed':
      return withOpenTurn(state, (turn) => ({
        ...turn,
        runtimeNotes: appendRuntimeNote(
          turn.runtimeNotes,
          `Consumed ${event.count} buffered input(s)`,
        ),
      }))

    case 'job_delivered':
      return withOpenTurn(state, (turn) => ({
        ...turn,
        runtimeNotes: appendRuntimeNote(turn.runtimeNotes, `Delivered: ${event.labels.join(', ')}`),
      }))

    case 'usage':
      return withLastTurn(state, (turn) => ({ ...turn, usage: event.usage }))

    case 'context_window':
      // Session-level capacity info; not tied to a single turn.
      return { ...state, contextWindow: { tokens: event.tokens, capacity: event.capacity } }

    case 'response_timing':
      return withLastTurn(state, (turn) => ({
        ...turn,
        timing: { output_tokens: event.output_tokens, elapsed_ms: event.elapsed_ms },
      }))

    case 'retry':
      return withLastTurn(state, (turn) => ({
        ...turn,
        retries: turn.retries + 1,
        runtimeNotes: appendRuntimeNote(turn.runtimeNotes, 'Retrying request'),
      }))

    case 'round':
      return withLastTurn(state, (turn) => ({ ...turn, round: event.current }))

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
        ...withOpenTurn(state, (turn) => turn),
        running: true,
        approval: { title: event.title, message: event.message, kind: event.kind },
      }

    case 'ask_user_requested':
      return {
        ...withOpenTurn(state, (turn) => turn),
        running: true,
        askUser: { question: event.question, options: event.options },
      }

    case 'terminal_input_requested':
      return {
        ...withOpenTurn(state, (turn) => turn),
        running: true,
        terminalInput: {
          session_id: event.session_id,
          purpose: event.purpose,
          prompt: event.prompt,
        },
      }

    case 'stopped':
      return {
        ...clearPendingInteractions({
          ...withLastTurn(state, (turn) => ({ ...turn, stopReason: event.reason })),
        }),
        running: false,
      }

    case 'finished':
      return clearPendingInteractions({
        ...withLastTurn(state, (turn) => ({
          ...turn,
          finished: { ok: event.ok, text: event.text, error: event.error },
          assistantText: event.text !== null ? event.text : turn.assistantText,
        })),
        running: false,
      })

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

/**
 * Seed the transcript from the server's persisted history
 * (`GET /api/agent/history`). A `prompt` entry starts a turn; content
 * entries attach to the newest turn, mirroring how live events accumulate.
 *
 * Restored turns are closed so subsequent live events open fresh turns —
 * except the newest one while the agent is still running, which stays open
 * so its streaming deltas keep appending to it.
 */
export function seedAgentHistory(
  state: AgentState,
  entries: AgentHistoryEntry[],
  running: boolean,
  messageCount: number,
): AgentState {
  const turns: AgentTurn[] = []
  let seq = 0
  const openTurn = (prompt: string | null): AgentTurn => {
    seq += 1
    const turn: AgentTurn = { ...freshTurn(seq, prompt), restored: true }
    turns.push(turn)
    return turn
  }
  const lastTurn = (): AgentTurn | undefined => turns[turns.length - 1]

  for (const { kind, entry } of entries) {
    switch (kind) {
      case 'prompt':
        openTurn(entry.text)
        break
      case 'assistant':
        ;(lastTurn() ?? openTurn(null)).assistantText = entry.text
        break
      case 'thinking': {
        const turn = lastTurn() ?? openTurn(null)
        turn.thinkingText =
          turn.thinkingText === '' ? entry.text : `${turn.thinkingText}\n\n${entry.text}`
        break
      }
      case 'tool': {
        const turn = lastTurn() ?? openTurn(null)
        turn.tools.push({
          key: `history:${turn.tools.length}`,
          kind: 'tool',
          id: null,
          status: 'finished',
          label: entry.text,
          detail: entry.preview,
        })
        break
      }
      case 'error':
        ;(lastTurn() ?? openTurn(null)).finished = { ok: false, text: null, error: entry }
        break
    }
  }

  const bounded = turns.length > MAX_TURNS ? turns.slice(turns.length - MAX_TURNS) : turns
  if (running) {
    const newest = bounded[bounded.length - 1]
    if (newest !== undefined) newest.restored = false
  }
  return { ...state, turns: bounded, turnSeq: seq, messageCount }
}
