import { expect, it } from 'vitest'
import { acknowledgeAgentRun, initialAgentState, reduceAgentEvent } from './agent-reducer'
it('keeps streamed text when the run response arrives after websocket output', () => {
  const state = reduceAgentEvent(initialAgentState(), {
    type: 'assistant_delta',
    text: 'Already responding',
  })
  const acknowledged = acknowledgeAgentRun(state, 'My question', 0)
  expect(acknowledged.turns).toHaveLength(1)
  expect(acknowledged.turns[0]).toMatchObject({
    prompt: 'My question',
    assistantText: 'Already responding',
  })
})
