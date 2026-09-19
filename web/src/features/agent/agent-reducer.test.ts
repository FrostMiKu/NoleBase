import { describe, expect, it, vi } from 'vitest'

import {
  MAX_NOTIFICATIONS,
  MAX_TURNS,
  beginAgentRun,
  initialAgentState,
  reduceAgentEvent,
} from './agent-reducer'
import type { AgentEvent } from '@/types/events'
import type { AgentState, AgentTurn } from './agent-reducer'

function stateWith(overrides: Partial<AgentState> = {}): AgentState {
  return { ...initialAgentState(), ...overrides }
}

function reduce(state: AgentState, event: AgentEvent): AgentState {
  return reduceAgentEvent(state, event)
}

function begin(state: AgentState, prompt: string): AgentState {
  return beginAgentRun(state, prompt)
}

function lastTurn(state: AgentState): AgentTurn {
  const turn = state.turns[state.turns.length - 1]
  if (turn === undefined) throw new Error('no turn')
  return turn
}

/** A complete small turn: prompt in, deltas, finish marker. */
function runTurn(state: AgentState, prompt: string, text: string): AgentState {
  let next = begin(state, prompt)
  next = reduce(next, { type: 'assistant_delta', text })
  next = reduce(next, { type: 'finished', ok: true, text, error: null })
  return next
}

describe('turn accumulation (multi-turn transcript)', () => {
  it('appends a second run instead of overwriting the first turn', () => {
    let state = stateWith()
    state = runTurn(state, 'first question', 'first answer')
    state = runTurn(state, 'second question', 'second answer')

    expect(state.turns).toHaveLength(2)
    expect(state.turns[0]).toMatchObject({
      prompt: 'first question',
      assistantText: 'first answer',
      finished: { ok: true, text: 'first answer', error: null },
    })
    expect(state.turns[1]).toMatchObject({
      prompt: 'second question',
      assistantText: 'second answer',
    })
    expect(state.running).toBe(false)
  })

  it('carries the prompt into the turn and keeps streaming events in it', () => {
    let state = begin(stateWith(), 'find todos')
    state = reduce(state, { type: 'assistant_delta', text: 'Looking…' })
    expect(lastTurn(state)).toMatchObject({ prompt: 'find todos', assistantText: 'Looking…' })
  })

  it('opens an implicit turn for events from runs started elsewhere', () => {
    let state = stateWith()
    state = reduce(state, { type: 'assistant_delta', text: 'external output' })
    expect(state.turns).toHaveLength(1)
    expect(lastTurn(state)).toMatchObject({ prompt: null, assistantText: 'external output' })
    expect(state.running).toBe(true)
  })

  it('opens a fresh implicit turn when events continue after a closed turn', () => {
    let state = runTurn(stateWith(), 'q', 'a')
    state = reduce(state, { type: 'assistant_delta', text: 'more' })
    expect(state.turns).toHaveLength(2)
    expect(lastTurn(state).assistantText).toBe('more')
  })

  it('caps the transcript at the maximum and keeps the newest turns', () => {
    let state = stateWith()
    for (let i = 0; i < MAX_TURNS + 5; i += 1) {
      state = runTurn(state, `q${i}`, `a${i}`)
    }
    expect(state.turns).toHaveLength(MAX_TURNS)
    expect(state.turns[0]?.prompt).toBe('q5')
    expect(lastTurn(state).prompt).toBe(`q${MAX_TURNS + 4}`)
  })

  it('beginAgentRun clears pending interactions and the interruption flag', () => {
    let state = stateWith({ streamInterrupted: true })
    state = reduce(state, { type: 'ask_user_requested', question: 'q', options: [] })
    state = begin(state, 'next')
    expect(state.askUser).toBeNull()
    expect(state.streamInterrupted).toBe(false)
    expect(state.running).toBe(true)
  })
})

describe('assistant text streaming', () => {
  it('appends assistant deltas into a single string within the turn', () => {
    let state = begin(stateWith(), 'p')
    state = reduce(state, { type: 'assistant_delta', text: 'Hello' })
    state = reduce(state, { type: 'assistant_delta', text: ', ' })
    state = reduce(state, { type: 'assistant_delta', text: 'world' })
    expect(lastTurn(state).assistantText).toBe('Hello, world')
  })

  it('lets assistant_message_finished replace the accumulated deltas', () => {
    let state = begin(stateWith(), 'p')
    state = reduce(state, { type: 'assistant_delta', text: 'Hello, worl' })
    state = reduce(state, {
      type: 'assistant_message_finished',
      text: 'Hello, world!',
      final_output: true,
    })
    expect(lastTurn(state).assistantText).toBe('Hello, world!')
  })
})

describe('thinking stream', () => {
  it('appends thinking deltas separately from assistant text', () => {
    let state = begin(stateWith(), 'p')
    state = reduce(state, { type: 'thinking_delta', text: 'pondering' })
    state = reduce(state, { type: 'assistant_delta', text: 'answer' })
    state = reduce(state, { type: 'thinking_delta', text: ' more' })
    const turn = lastTurn(state)
    expect(turn.thinkingText).toBe('pondering more')
    expect(turn.assistantText).toBe('answer')
  })

  it('keeps the thinking text on thinking_finished', () => {
    let state = begin(stateWith(), 'p')
    state = reduce(state, { type: 'thinking_delta', text: 'reasoning' })
    state = reduce(state, { type: 'thinking_finished' })
    expect(lastTurn(state).thinkingText).toBe('reasoning')
  })
})

describe('tool lifecycle', () => {
  it('creates a preparing row on tool_preparing', () => {
    let state = begin(stateWith(), 'p')
    state = reduce(state, { type: 'tool_preparing', index: 0, message: 'Reading file' })
    expect(lastTurn(state).tools[0]).toMatchObject({
      key: 'preparing:0',
      kind: 'tool',
      id: null,
      status: 'preparing',
      label: 'Reading file',
    })
  })

  it('transitions to running with the id on tool_preparation_finished', () => {
    let state = begin(stateWith(), 'p')
    state = reduce(state, { type: 'tool_preparing', index: 0, message: 'Reading file' })
    state = reduce(state, { type: 'tool_preparation_finished', index: 0, id: 'tool-1' })
    expect(lastTurn(state).tools[0]).toMatchObject({
      key: 'tool:tool-1',
      id: 'tool-1',
      status: 'running',
    })
  })

  it('handles tool_started for an id never prepared (post-reconnect)', () => {
    let state = begin(stateWith(), 'p')
    state = reduce(state, { type: 'tool_started', id: 'tool-9', message: 'grep' })
    expect(lastTurn(state).tools[0]).toMatchObject({ id: 'tool-9', status: 'running', label: 'grep' })
  })

  it('updates the label on tool_updated without changing status', () => {
    let state = begin(stateWith(), 'p')
    state = reduce(state, { type: 'tool_started', id: 't1', message: 'running…' })
    state = reduce(state, { type: 'tool_updated', id: 't1', message: '50%' })
    expect(lastTurn(state).tools[0]).toMatchObject({ id: 't1', status: 'running', label: '50%' })
  })

  it('marks finished and records the preview on tool_finished', () => {
    let state = begin(stateWith(), 'p')
    state = reduce(state, { type: 'tool_started', id: 't1', message: 'grep' })
    state = reduce(state, { type: 'tool_finished', id: 't1', message: 'done', preview: '3 hits' })
    expect(lastTurn(state).tools[0]).toMatchObject({
      id: 't1',
      status: 'finished',
      label: 'done',
      detail: '3 hits',
    })
  })

  it('synthesizes a finished row when tool_finished has no known id', () => {
    let state = begin(stateWith(), 'p')
    state = reduce(state, { type: 'tool_finished', id: 'zz', message: 'done', preview: null })
    expect(lastTurn(state).tools[0]).toMatchObject({ id: 'zz', status: 'finished' })
  })

  it('keeps tools of different turns separate', () => {
    let state = begin(stateWith(), 'p1')
    state = reduce(state, { type: 'tool_started', id: 'a1', message: 'turn one tool' })
    state = reduce(state, { type: 'finished', ok: true, text: 'done', error: null })
    state = begin(state, 'p2')
    state = reduce(state, { type: 'tool_started', id: 'b1', message: 'turn two tool' })
    expect(state.turns[0]?.tools).toHaveLength(1)
    expect(state.turns[0]?.tools[0]?.id).toBe('a1')
    expect(lastTurn(state).tools[0]?.id).toBe('b1')
  })
})

describe('background jobs and notices', () => {
  it('tracks job_started and job_settled in the session registry', () => {
    let state = begin(stateWith(), 'p')
    state = reduce(state, { type: 'job_started', id: 'j1', label: 'indexing' })
    expect(state.jobs).toHaveLength(1)
    expect(state.jobs[0]).toMatchObject({ id: 'j1', label: 'indexing', status: 'running' })
    state = reduce(state, { type: 'job_settled', id: 'j1', failed: true })
    expect(state.jobs[0]).toMatchObject({ id: 'j1', status: 'failed' })
  })

  it('updates the spawning turn job even when other turns followed (regression)', () => {
    // The real-world flow: turn 2 spawns a subagent job and closes; the
    // user chats on (turn 3); only then does the job settle. The settled
    // event must update the job registry — never synthesize a second row
    // in the latest turn nor leave the first one spinning.
    let state = runTurn(stateWith(), 'spawn subagent', 'dispatched to background')
    state = reduce(state, { type: 'job_started', id: 'job-1', label: 'explore' })
    state = reduce(state, { type: 'finished', ok: true, text: 'dispatched', error: null })
    state = runTurn(state, 'say something short', 'short story')
    expect(state.turns).toHaveLength(2)

    state = reduce(state, { type: 'job_settled', id: 'job-1', failed: false })

    expect(state.jobs).toHaveLength(1)
    expect(state.jobs[0]).toMatchObject({ id: 'job-1', label: 'explore', status: 'finished' })
    // No phantom job rows in either turn's tool timeline.
    for (const turn of state.turns) {
      expect(turn.tools.every((tool) => tool.kind === 'tool')).toBe(true)
    }
  })

  it('records buffered input and delivery notices', () => {
    let state = begin(stateWith(), 'p')
    state = reduce(state, { type: 'buffered_input_consumed', count: 2 })
    state = reduce(state, { type: 'job_delivered', labels: ['a.md', 'b.md'] })
    expect(lastTurn(state).runtimeNotes).toEqual([
      'Consumed 2 buffered input(s)',
      'Delivered: a.md, b.md',
    ])
  })
})

describe('usage, context and rounds', () => {
  it('stores token usage on the turn', () => {
    let state = begin(stateWith(), 'p')
    state = reduce(state, {
      type: 'usage',
      usage: {
        input_tokens: 100,
        output_tokens: 20,
        cache_creation_input_tokens: 5,
        cache_read_input_tokens: 50,
      },
    })
    expect(lastTurn(state).usage).toEqual({
      input_tokens: 100,
      output_tokens: 20,
      cache_creation_input_tokens: 5,
      cache_read_input_tokens: 50,
    })
  })

  it('stores the context window at session level', () => {
    const state = reduce(stateWith(), { type: 'context_window', tokens: 900, capacity: 2000 })
    expect(state.contextWindow).toEqual({ tokens: 900, capacity: 2000 })
    expect(state.turns).toHaveLength(0)
  })

  it('records response timing, rounds and retries; timing may follow finish', () => {
    let state = begin(stateWith(), 'p')
    state = reduce(state, { type: 'response_timing', output_tokens: 42, elapsed_ms: 1500 })
    state = reduce(state, { type: 'round', current: 3 })
    state = reduce(state, { type: 'retry' })
    state = reduce(state, { type: 'finished', ok: true, text: 'done', error: null })
    // timing after finish must not open a phantom turn
    state = reduce(state, { type: 'response_timing', output_tokens: 50, elapsed_ms: 2100 })
    expect(state.turns).toHaveLength(1)
    const turn = lastTurn(state)
    expect(turn.timing).toEqual({ output_tokens: 50, elapsed_ms: 2100 })
    expect(turn.round).toBe(3)
    expect(turn.retries).toBe(1)
    expect(turn.finished).toEqual({ ok: true, text: 'done', error: null })
  })

  it('updates lightweight conversation metadata only', () => {
    const state = reduce(stateWith(), { type: 'conversation_updated', message_count: 7 })
    expect(state.messageCount).toBe(7)
  })
})

describe('pending interactions', () => {
  it('surfaces an approval request without clearing the stream', () => {
    let state = begin(stateWith(), 'p')
    state = reduce(state, {
      type: 'approval_requested',
      title: 'Run command',
      message: 'about to rm -rf',
      kind: { kind: 'command', purpose: 'cleanup', label: 'rm -rf build', code: 'rm -rf build' },
    })
    expect(state.approval).not.toBeNull()
    expect(state.approval?.kind.kind).toBe('command')
  })

  it('surfaces ask-user requests with options', () => {
    let state = begin(stateWith(), 'p')
    state = reduce(state, {
      type: 'ask_user_requested',
      question: 'Which file?',
      options: ['a.md', 'b.md'],
    })
    expect(state.askUser).toEqual({ question: 'Which file?', options: ['a.md', 'b.md'] })
  })

  it('surfaces private terminal input requests', () => {
    let state = begin(stateWith(), 'p')
    state = reduce(state, {
      type: 'terminal_input_requested',
      session_id: 's1',
      purpose: 'sudo',
      prompt: 'password:',
    })
    expect(state.terminalInput).toEqual({ session_id: 's1', purpose: 'sudo', prompt: 'password:' })
  })

  it('clears pending interactions on stopped and records the reason', () => {
    let state = begin(stateWith(), 'p')
    state = reduce(state, { type: 'ask_user_requested', question: 'q', options: [] })
    state = reduce(state, { type: 'stopped', reason: 'tool_approval_denied' })
    expect(state.running).toBe(false)
    expect(state.askUser).toBeNull()
    expect(lastTurn(state).stopReason).toBe('tool_approval_denied')
  })

  it('clears pending interactions on finished success and adopts final text', () => {
    let state = begin(stateWith(), 'p')
    state = reduce(state, { type: 'approval_requested', title: 't', message: 'm', kind: { kind: 'diff' } })
    state = reduce(state, { type: 'finished', ok: true, text: 'All done', error: null })
    expect(state.running).toBe(false)
    expect(state.approval).toBeNull()
    expect(lastTurn(state).finished).toEqual({ ok: true, text: 'All done', error: null })
    expect(lastTurn(state).assistantText).toBe('All done')
  })

  it('records finished failure with the error text', () => {
    let state = begin(stateWith(), 'p')
    state = reduce(state, { type: 'finished', ok: false, text: null, error: 'provider down' })
    expect(state.running).toBe(false)
    expect(lastTurn(state).finished).toEqual({ ok: false, text: null, error: 'provider down' })
    expect(lastTurn(state).assistantText).toBe('')
  })
})

describe('interruption and session facts', () => {
  it('flags stream interruption on resync_required', () => {
    const state = reduce(stateWith(), { type: 'resync_required', skipped: 12 })
    expect(state.streamInterrupted).toBe(true)
  })

  it('keeps the file navigation events out of the projection', () => {
    const state = stateWith()
    const next = reduce(reduce(state, { type: 'open_file', path: 'data/a.md' }), {
      type: 'file_moved',
      from: 'a.md',
      to: 'b.md',
    })
    expect(next).toEqual(initialAgentState())
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

  it('keeps notifications across runs', () => {
    let state = stateWith()
    state = reduce(state, { type: 'notification', message: 'kept' })
    state = begin(state, 'next prompt')
    expect(state.notifications.map((n) => n.message)).toEqual(['kept'])
  })
})

describe('parse safety', () => {
  it('rejects unknown event types instead of guessing', async () => {
    const { parseEvent } = await import('@/types/events')
    const known = ['assistant_delta'] as const
    expect(parseEvent<import('@/types/events').AgentEvent>(
      JSON.stringify({ type: 'assistant_delta', text: 'hi' }),
      known,
    )).toMatchObject({ ok: true })
    expect(parseEvent(JSON.stringify({ type: 'brand_new_event' }), known)).toMatchObject({
      ok: false,
      type: 'brand_new_event',
    })
    expect(parseEvent('not json', known)).toMatchObject({ ok: false, type: null })
  })
})

// Keep the console.warn default branch honest without silencing it globally.
vi.spyOn(console, 'warn').mockImplementation(() => {})
