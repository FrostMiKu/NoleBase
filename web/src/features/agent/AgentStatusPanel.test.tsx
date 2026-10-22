import { cleanup, render, screen } from '@testing-library/react'
import { afterEach, describe, expect, it } from 'vitest'

import { AgentStatusPanel } from './AgentStatusPanel'
import { useAgentStore } from '@/stores/agent-store'
import type { JobRun } from '@/features/agent/agent-reducer'

function job(id: string, label: string, status: JobRun['status'], turnId: number | null): JobRun {
  return { id, label, status, turnId }
}

afterEach(cleanup)

describe('AgentStatusPanel', () => {
  it('lists session jobs once each with their lifecycle state', () => {
    useAgentStore.setState({
      jobs: [
        job('job-1', 'explore arxiv', 'finished', 2),
        job('job-2', 'index notes', 'running', 3),
      ],
      running: true,
    })
    render(<AgentStatusPanel />)
    const text = screen.getByTestId('agent-status-panel').textContent ?? ''
    expect(text).toContain('explore arxiv')
    expect(text).toContain('index notes')
    expect(text).toContain('1 active · 2 total')
  })

  it('renders the empty state without jobs', () => {
    useAgentStore.setState({ jobs: [], running: false })
    render(<AgentStatusPanel />)
    expect(screen.getByTestId('agent-status-panel').textContent).toContain('No background jobs yet')
  })
})
