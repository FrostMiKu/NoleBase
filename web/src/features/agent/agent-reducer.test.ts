import { describe, expect, it } from 'vitest'

import {
  MAX_NOTIFICATIONS,
  beginAgentRun,
  initialAgentState,
  reduceAgentEvent,
} from './agent-reducer'
import type { AgentEvent } from '@/types/events'
import type { AgentState } from './agent-reducer'

function stateWith(overrides: Partial<AgentState> = {}): AgentState {
  return { ...initialAgentState(), ...overrides }
}

function reduce(state: AgentState, event: AgentEvent): AgentState {
  return reduceAgentEvent(state, event)
}

describe('assistant text streaming', () => {
  it('appends assistant deltas into a single string', () => {
    let state = stateWith()
    state = reduce(state, { type: 'assistant_delta', text: 'Hello' })
    state = reduce(state, { type: 'assistant_delta', text: ', ' })
    state = reduce(state, { type: 'assistant_delta', text: 'world' })
    expect(state.assistantText).toBe('Hello, world')
  })

  it('lets assistant_message_finished replace the accumulated deltas', () => {
    let state = stateWith({ assistantText: 'Hello, worl' })
    state = reduce(state, {
      type: 'assistant_message_finished',
      text: 'Hello, world!',
      final_output: true,
    })
    expect(state.assistantText).toBe('Hello, world!')
  })
})

describe('thinking stream', () => {
  it('appends thinking deltas separately from assistant text', () => {
    let state = stateWith()
    state = reduce(state, { type: 'thinking_delta', text: 'pondering' })
    state = reduce(state, { type: 'assistant_delta', text: 'answer' })
    state = reduce(state, { type: 'thinking_delta', text: ' more' })
    expect(state.thinkingText).toBe('pondering more')
    expect(state.assistantText).toBe('answer')
  })

  it('keeps the thinking text on thinking_finished', () => {
    let state = stateWith({ thinkingText: 'reasoning' })
    state = reduce(state, { type: 'thinking_finished' })
    expect(state.thinkingText).toBe('reasoning')
  })
})

describe('tool lifecycle', () => {
  it('creates a preparing row on tool_preparing', () => {
    const state = reduce(stateWith(), { type: 'tool_preparing', index: 0, message: 'Reading file' })
    expect(state.tools).toHaveLength(1)
    expect(state.tools[0]).toMatchObject({
      key: 'preparing:0',
      kind: 'tool',
      id: null,
      status: 'preparing',
      label: 'Reading file',
    })
  })

  it('transitions to running with the id on tool_preparation_finished', () => {
    let state = reduce(stateWith(), { type: 'tool_preparing', index: 0, message: 'Reading file' })
    state = reduce(state, { type: 'tool_preparation_finished', index: 0, id: 'tool-1' })
    expect(state.tools[0]).toMatchObject({
      key: 'tool:tool-1',
      id: 'tool-1',
      status: 'running',
    })
  })

  it('handles tool_started for an id never prepared (post-reconnect)', () => {
    const state = reduce(stateWith(), { type: 'tool_started', id: 'tool-9', message: 'grep' })
    expect(state.tools).toHaveLength(1)
    expect(state.tools[0]).toMatchObject({ id: 'tool-9', status: 'running', label: 'grep' })
  })

  it('updates the label on tool_updated without changing status', () => {
    let state = reduce(stateWith(), { type: 'tool_started', id: 't1', message: 'running…' })
    state = reduce(state, { type: 'tool_updated', id: 't1', message: '50%' })
    expect(state.tools[0]).toMatchObject({ id: 't1', status: 'running', label: '50%' })
  })

  it('marks finished and records the preview on tool_finished', () => {
    let state = reduce(stateWith(), { type: 'tool_started', id: 't1', message: 'grep' })
    state = reduce(state, { type: 'tool_finished', id: 't1', message: 'done', preview: '3 hits' })
    expect(state.tools[0]).toMatchObject({ id: 't1', status: 'finished', label: 'done', detail: '3 hits' })
  })

  it('synthesizes a finished row when tool_finished has no known id', () => {
    const state = reduce(stateWith(), { type: 'tool_finished', id: 'zz', message: 'done', preview: null })
    expect(state.tools).toHaveLength(1)
    expect(state.tools[0]).toMatchObject({ id: 'zz', status: 'finished' })
  })
})

describe('background jobs', () => {
  it('tracks job_started and job_settled', () => {
    let state = reduce(stateWith(), { type: 'job_started', id: 'j1', label: 'indexing' })
    expect(state.tools[0]).toMatchObject({ key: 'job:j1', kind: 'job', status: 'running' })
    state = reduce(state, { type: 'job_settled', id: 'j1', failed: true })
    expect(state.tools[0]).toMatchObject({ key: 'job:j1', status: 'failed' })
  })

  it('records buffered input and delivery notices', () => {
    let state = reduce(stateWith(), { type: 'buffered_input_consumed', count: 2 })
    state = reduce(state, { type: 'job_delivered', labels: ['a.md', 'b.md'] })
    expect(state.runtimeNotes).toEqual(['Consumed 2 buffered input(s)', 'Delivered: a.md, b.md'])
  })
})

describe('usage and context', () => {
  it('stores token usage', () => {
    const state = reduce(stateWith(), {
      type: 'usage',
      usage: {
        input_tokens: 100,
        output_tokens: 20,
        cache_creation_input_tokens: 5,
        cache_read_input_tokens: 50,
      },
    })
    expect(state.usage).toEqual({
      input_tokens: 100,
      output_tokens: 20,
      cache_creation_input_tokens: 5,
      cache_read_input_tokens: 50,
    })
  })

  it('stores the context window fill', () => {
    const state = reduce(stateWith(), { type: 'context_window', tokens: 900, capacity: 2000 })
    expect(state.contextWindow).toEqual({ tokens: 900, capacity: 2000 })
  })

  it('records response timing, rounds and retries', () => {
    let state = reduce(stateWith(), { type: 'response_timing', output_tokens: 42, elapsed_ms: 1500 })
    state = reduce(state, { type: 'round', current: 3 })
    state = reduce(state, { type: 'retry' })
    expect(state.timing).toEqual({ output_tokens: 42, elapsed_ms: 1500 })
    expect(state.round).toBe(3)
    expect(state.retries).toBe(1)
  })

  it('updates lightweight conversation metadata only', () => {
    const state = reduce(stateWith(), { type: 'conversation_updated', message_count: 7 })
    expect(state.messageCount).toBe(7)
  })
})

describe('pending interactions', () => {
  it('surfaces an approval request without clearing the stream', () => {
    const state = reduce(stateWith(), {
      type: 'approval_requested',
      title: 'Run command',
      message: 'about to rm -rf',
      kind: { kind: 'command', purpose: 'cleanup', label: 'rm -rf build', code: 'rm -rf build' },
    })
    expect(state.approval).not.toBeNull()
    expect(state.approval?.kind.kind).toBe('command')
  })

  it('surfaces ask-user requests with options', () => {
    const state = reduce(stateWith(), {
      type: 'ask_user_requested',
      question: 'Which file?',
      options: ['a.md', 'b.md'],
    })
    expect(state.askUser).toEqual({ question: 'Which file?', options: ['a.md', 'b.md'] })
  })

  it('surfaces private terminal input requests', () => {
    const state = reduce(stateWith(), {
      type: 'terminal_input_requested',
      session_id: 's1',
      purpose: 'sudo',
      prompt: 'password:',
    })
    expect(state.terminalInput).toEqual({ session_id: 's1', purpose: 'sudo', prompt: 'password:' })
  })

  it('clears pending interactions on stopped and records the reason', () => {
    let state = stateWith({ running: true })
    state = reduce(state, { type: 'ask_user_requested', question: 'q', options: [] })
    state = reduce(state, { type: 'stopped', reason: 'tool_approval_denied' })
    expect(state.running).toBe(false)
    expect(state.askUser).toBeNull()
    expect(state.stopReason).toBe('tool_approval_denied')
  })

  it('clears pending interactions on finished success and adopts final text', () => {
    let state = stateWith({ running: true })
    state = reduce(state, { type: 'approval_requested', title: 't', message: 'm', kind: { kind: 'diff' } })
    state = reduce(state, { type: 'finished', ok: true, text: 'All done', error: null })
    expect(state.running).toBe(false)
    expect(state.approval).toBeNull()
    expect(state.finished).toEqual({ ok: true, text: 'All done', error: null })
    expect(state.assistantText).toBe('All done')
  })

  it('records finished failure with the error text', () => {
    let state = stateWith({ running: true })
    state = reduce(state, { type: 'finished', ok: false, text: null, error: 'provider down' })
    expect(state.running).toBe(false)
    expect(state.finished).toEqual({ ok: false, text: null, error: 'provider down' })
    expect(state.assistantText).toBe('')
  })
})

describe('interruption and run lifecycle', () => {
  it('flags stream interruption on resync_required', () => {
    const state = reduce(stateWith(), { type: 'resync_required', skipped: 12 })
    expect(state.streamInterrupted).toBe(true)
  })

  it('keeps the file navigation events out of the projection', () => {
    let state = stateWith()
    state = reduce(state, { type: 'open_file', path: 'data/a.md' })
    state = reduce(state, { type: 'file_moved', from: 'a.md', to: 'b.md' })
    expect(state).toEqual(initialAgentState())
  })

  it('caps notifications at the maximum and re-numbers keys monotonically', () => {
    let state = stateWith()
    for (let i = 0; i < MAX_NOTIFICATIONS + 10; i += 1) {
      state = reduce(state, { type: 'notification', message: `n${i}` })
    }
    expect(state.notifications).toHaveLength(MAX_NOTIFICATIONS)
    expect(state.notifications[0]?.message).toBe('n10')
    expect(state.notifications[MAX_NOTIFICATIONS - 1]?.message).toBe(`n${MAX_NOTIFICATIONS + 9}`)
  })

  it('beginAgentRun resets per-run state but keeps notifications and usage', () => {
    let state = stateWith({ running: false })
    state = reduce(state, { type: 'notification', message: 'kept' })
    state = reduce(state, { type: 'usage', usage: { input_tokens: 1, output_tokens: 2, cache_creation_input_tokens: 0, cache_read_input_tokens: 0 } })
    state = reduce(state, { type: 'assistant_delta', text: 'partial' })
    state = reduce(state, { type: 'resync_required', skipped: 1 })

    state = beginAgentRun(state)
    expect(state.running).toBe(true)
    expect(state.assistantText).toBe('')
    expect(state.thinkingText).toBe('')
    expect(state.tools).toEqual([])
    expect(state.approval).toBeNull()
    expect(state.askUser).toBeNull()
    expect(state.terminalInput).toBeNull()
    expect(state.streamInterrupted).toBe(false)
    expect(state.finished).toBeNull()
    expect(state.usage).not.toBeNull()
    expect(state.notifications.map((n) => n.message)).toEqual(['kept'])
  })
})
