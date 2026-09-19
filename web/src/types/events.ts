/**
 * WebSocket event unions. These are the serde wire shapes of
 * `WebWorkspaceEvent` and `WebAgentEvent` in `src/web/dto.rs`
 * (`#[serde(tag = "type", rename_all = "snake_case")]`); the names below
 * are the contract, not conventions.
 */

export type WorkspaceEvent =
  | { type: 'content_changed'; paths: string[] }
  | { type: 'attachments_changed'; paths: string[] }
  | { type: 'agent_reads_dirty'; paths: string[] }
  | { type: 'watcher_failed'; message: string }
  | { type: 'resync_required'; skipped: number }

export type AgentEvent =
  | { type: 'assistant_delta'; text: string }
  | { type: 'assistant_message_finished'; text: string; final_output: boolean }
  | { type: 'thinking_delta'; text: string }
  | { type: 'thinking_finished' }
  | { type: 'tool_preparing'; index: number; message: string }
  | { type: 'tool_preparation_finished'; index: number; id: string | null }
  | { type: 'job_started'; id: string; label: string }
  | { type: 'job_settled'; id: string; failed: boolean }
  | { type: 'buffered_input_consumed'; count: number }
  | { type: 'job_delivered'; labels: string[] }
  | { type: 'tool_started'; id: string; message: string }
  | { type: 'tool_updated'; id: string; message: string }
  | { type: 'tool_finished'; id: string; message: string; preview: string | null }
  | { type: 'usage'; usage: import('./agent').TokenUsage }
  | { type: 'context_window'; tokens: number; capacity: number }
  | { type: 'response_timing'; output_tokens: number; elapsed_ms: number }
  | { type: 'retry' }
  | { type: 'round'; current: number }
  | { type: 'conversation_updated'; message_count: number }
  | { type: 'notification'; message: string }
  | { type: 'file_moved'; from: string; to: string }
  | { type: 'open_file'; path: string }
  | { type: 'approval_requested'; title: string; message: string; kind: import('./agent').ApprovalKind }
  | { type: 'ask_user_requested'; question: string; options: string[] }
  | {
      type: 'terminal_input_requested'
      session_id: string
      purpose: string
      prompt: string
    }
  | { type: 'stopped'; reason: string }
  | { type: 'finished'; ok: boolean; text: string | null; error: string | null }
  | { type: 'resync_required'; skipped: number }

/**
 * Parse one raw WebSocket text frame into a typed event. Unknown `type`
 * tags surface as `{ ok: false, type }` so callers can report them in dev
 * instead of silently dropping them.
 */
export function parseEvent<T extends { type: string }>(
  data: string,
  expected: readonly T['type'][],
): { ok: true; event: T } | { ok: false; type: string | null } {
  let value: unknown
  try {
    value = JSON.parse(data)
  } catch {
    return { ok: false, type: null }
  }
  if (typeof value !== 'object' || value === null || !('type' in value)) {
    return { ok: false, type: null }
  }
  const type = (value as { type: unknown }).type
  if (typeof type !== 'string' || !expected.includes(type as T['type'])) {
    return { ok: false, type: typeof type === 'string' ? type : null }
  }
  return { ok: true, event: value as T }
}
