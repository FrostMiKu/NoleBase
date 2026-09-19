import { cleanup, render, screen } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'

import { StatusBar } from './StatusBar'
import { editorStore } from '@/editor/editor-state'
import { useUiStore } from '@/stores/ui-store'

function resetStores() {
  editorStore.close()
  useUiStore.setState({
    workspaceConnection: 'connected',
    workspaceResyncing: false,
    watcherError: null,
  })
}

beforeEach(resetStores)

afterEach(cleanup)

describe('StatusBar', () => {
  it('shows the workspace dot only, without agent status', () => {
    render(<StatusBar />)
    expect(screen.getAllByTitle('WS: connected')).toHaveLength(1)
    expect(screen.queryByTitle('WS: disconnected')).toBeNull()
    expect(screen.getByTestId('status-bar').textContent).not.toContain('Agent')
  })

  it('marks a dirty document as modified without showing its path', () => {
    editorStore.loadDocument('data/foo.md', 'base')
    editorStore.setLiveDirty(true)
    render(<StatusBar />)
    expect(screen.getByTestId('status-bar').textContent).toContain('Modified')
    expect(screen.getByTestId('status-bar').textContent).not.toContain('data/foo.md')
  })

  it('surfaces a watcher error', () => {
    useUiStore.setState({ watcherError: 'watcher died' })
    render(<StatusBar />)
    expect(screen.getByTestId('status-bar').textContent).toContain('Workspace watcher error')
  })

  it('shows a disconnected workspace socket', () => {
    useUiStore.setState({ workspaceConnection: 'disconnected' })
    render(<StatusBar />)
    expect(screen.getAllByTitle('WS: disconnected').length).toBeGreaterThanOrEqual(1)
  })
})
