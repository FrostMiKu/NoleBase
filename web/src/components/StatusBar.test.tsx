import { cleanup, render, screen } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'

import { StatusBar } from './StatusBar'
import { editorStore } from '@/editor/editor-state'
import { useAgentStore } from '@/stores/agent-store'
import { useUiStore } from '@/stores/ui-store'

function resetStores() {
  editorStore.close()
  useUiStore.setState({
    workspaceConnection: 'connected',
    workspaceResyncing: false,
    watcherError: null,
  })
  useAgentStore.setState({ connection: 'connected', running: false })
}

beforeEach(resetStores)

afterEach(cleanup)

describe('StatusBar', () => {
  it('shows both connection states and no document', () => {
    render(<StatusBar />)
    expect(screen.getByTestId('status-bar').textContent).toContain('no document')
    expect(screen.getByTestId('status-bar').textContent).toContain('Agent: idle')
    const dots = screen.getAllByTitle('WS: connected')
    expect(dots.length).toBeGreaterThanOrEqual(1)
  })

  it('marks a dirty document as modified', () => {
    editorStore.loadDocument('data/foo.md', 'base')
    editorStore.setLiveDirty(true)
    render(<StatusBar />)
    expect(screen.getByTestId('status-bar').textContent).toContain('Modified')
    expect(screen.getByTestId('status-bar').textContent).toContain('data/foo.md')
  })

  it('surfaces a watcher error', () => {
    useUiStore.setState({ watcherError: 'watcher died' })
    render(<StatusBar />)
    expect(screen.getByTestId('status-bar').textContent).toContain('Workspace watcher error')
  })

  it('shows the running agent', () => {
    useAgentStore.setState({ running: true })
    render(<StatusBar />)
    expect(screen.getByTestId('status-bar').textContent).toContain('Agent: running')
  })

  it('shows a disconnected workspace socket', () => {
    useUiStore.setState({ workspaceConnection: 'disconnected' })
    render(<StatusBar />)
    expect(screen.getAllByTitle('WS: disconnected').length).toBeGreaterThanOrEqual(1)
  })
})
