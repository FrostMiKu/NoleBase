/**
 * Agent DTOs mirroring `src/web/agent.rs` (HTTP) and the agent side of
 * `src/web/dto.rs` (WebSocket events). Event type names are the serde
 * `snake_case` tags of `WebAgentEvent`.
 */

export type PermissionMode = 'approve' | 'auto' | 'yolo'

/** `GET /api/agent` */
export interface AgentStatus {
  running: boolean
  permission_mode: PermissionMode
}

/** `GET /api/agent/permission` / `PUT /api/agent/permission` */
export interface PermissionState {
  mode: PermissionMode
}

/** Token usage embedded in the `usage` agent event. */
export interface TokenUsage {
  input_tokens: number
  output_tokens: number
  cache_creation_input_tokens: number
  cache_read_input_tokens: number
}

/**
 * How an approval request is presented; mirrors `ApprovalKindDto`
 * (`#[serde(tag = "kind", rename_all = "snake_case")]`).
 */
export type ApprovalKind =
  | { kind: 'diff' }
  | { kind: 'confirm' }
  | { kind: 'destructive_confirm' }
  | { kind: 'command'; purpose: string; label: string; code: string }

/** The `approval_requested` event payload. */
export interface ApprovalRequest {
  title: string
  message: string
  kind: ApprovalKind
}

/** The `ask_user_requested` event payload. */
export interface AskUserRequest {
  question: string
  options: string[]
}

/** The `terminal_input_requested` event payload. */
export interface TerminalInputRequest {
  session_id: string
  purpose: string
  prompt: string
}

export type ToolStatus = 'preparing' | 'running' | 'finished' | 'failed'

/** One background job row from `GET /api/agent/jobs`. */
export interface AgentJobRow {
  id: string
  label: string
  status: 'running' | 'finished' | 'failed' | 'cancelled'
}

/** One compact timeline row: a tool run or a background job. */
export interface ToolRun {
  /** Stable store key; tool ids may only be known after preparation. */
  key: string
  kind: 'tool' | 'job'
  id: string | null
  status: ToolStatus
  label: string
  detail: string | null
}

export interface AgentNotification {
  id: number
  message: string
}

export interface ContextWindowInfo {
  tokens: number
  capacity: number
}

export interface ResponseTimingInfo {
  output_tokens: number
  elapsed_ms: number
}

export type AgentConnectionState = 'connecting' | 'connected' | 'disconnected'

/** One settled transcript entry from `GET /api/agent/history` (mirrors
 * `AgentHistoryEntryDto` in `src/web/dto.rs`; history entries are persisted
 * snapshots, so live-only flags like `streaming`/`active` are absent). */
export type AgentHistoryEntry =
  | { kind: 'prompt'; entry: { text: string } }
  | { kind: 'assistant'; entry: { text: string } }
  | { kind: 'thinking'; entry: { text: string } }
  | { kind: 'tool'; entry: { text: string; preview: string | null } }
  | { kind: 'error'; entry: string }

/** `GET /api/agent/history` */
export interface AgentHistory {
  entries: AgentHistoryEntry[]
  message_count: number
}
