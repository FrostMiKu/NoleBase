import { cleanup, render, screen } from '@testing-library/react'
import { afterEach, describe, expect, it } from 'vitest'

import { AgentStatusPanel } from './AgentStatusPanel'
import { useAgentStore } from '@/stores/agent-store'
import type { AgentTurn } from '@/features/agent/agent-reducer'

function turnWithJob(id: number, label: string, status: 'running' | 'finished' | 'failed'): AgentTurn {
  return {
    id,
    prompt: null,
    assistantText: '',
    thinkingText: '',
    tools: [
      {
        key: `job:${id}`,
        kind: 'job',
        id: `j${id}`,
        status,
        label,
        detail: null,
      },
    ],
    runtimeNotes: [],
    usage: null,
    timing: null,
    round: null,
    retries: 0,
    finished: null,
    stopReason: null,
  }
}

afterEach(() => {
  useAgentStore.setState({ turns: [], running: false })
})

afterEach(cleanup)

describe('AgentStatusPanel', () => {
  it('lists background jobs from every turn with their state', () => {
    useAgentStore.setState({
      turns: [
        turnWithJob(1, 'explore arxiv', 'finished'),
        turnWithJob(2, 'index notes', 'running'),
      ],
      running: true,
    })
    render(<AgentStatusPanel />)
    const text = screen.getByTestId('agent-status-panel').textContent ?? ''
    expect(text).toContain('explore arxiv')
    expect(text).toContain('index notes')
    expect(text).toContain('1 active background job(s)')
    expect(text).toContain('running')
  })

  it('renders the empty state without jobs', () => {
    useAgentStore.setState({ turns: [], running: false })
    render(<AgentStatusPanel />)
    expect(screen.getByTestId('agent-status-panel').textContent).toContain(
      'No background jobs yet',
    )
  })
})
