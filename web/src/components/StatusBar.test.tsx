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
  it('does not reserve a status strip for a healthy workspace', () => {
    render(<StatusBar />)
    expect(screen.queryByTestId('status-bar')).toBeNull()
  })

  it('does not duplicate the document toolbar save status', () => {
    editorStore.loadDocument('data/foo.md', 'base')
    editorStore.setLiveDirty(true)
    render(<StatusBar />)
    expect(screen.queryByTestId('status-bar')).toBeNull()
  })

  it('shows resynchronization even while connected', () => {
    useUiStore.setState({ workspaceResyncing: true })
    render(<StatusBar />)
    expect(screen.getByTestId('status-bar')).toHaveTextContent('Resyncing…')
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
