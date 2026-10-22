import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { act, cleanup, render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { agentApi } from '@/api/agent'
import { useAgentStore } from '@/stores/agent-store'
import { initialAgentState } from './agent-reducer'
import { AgentPanel } from './AgentPanel'
import { AgentInteractions } from './AgentInteractions'
vi.mock('./PermissionSelect', () => ({ PermissionSelect: () => null }))
beforeEach(() => useAgentStore.setState({ ...initialAgentState(), connection: 'connected' }))
afterEach(() => {
  cleanup()
  sessionStorage.clear()
  vi.restoreAllMocks()
})
function mount(node = <AgentPanel />) {
  const client = new QueryClient({ defaultOptions: { mutations: { retry: false } } })
  render(<QueryClientProvider client={client}>{node}</QueryClientProvider>)
}
it('offers a Send button, preserves early stream output, and clears only a submitted draft', async () => {
  const user = userEvent.setup()
  let resolve!: (value: null) => void
  vi.spyOn(agentApi, 'run').mockImplementation(
    () =>
      new Promise((done) => {
        resolve = done
      }),
  )
  mount()
  const composer = screen.getByLabelText('Agent prompt')
  await user.type(composer, 'Organize my notes')
  await user.click(screen.getByRole('button', { name: 'Send' }))
  expect(composer).toBeDisabled()
  act(() =>
    useAgentStore.getState().dispatchEvent({ type: 'assistant_delta', text: 'Starting now' }),
  )
  await act(async () => {
    resolve(null)
  })
  expect(composer).toHaveValue('')
  expect(useAgentStore.getState().turns).toHaveLength(1)
  expect(useAgentStore.getState().turns[0]).toMatchObject({
    prompt: 'Organize my notes',
    assistantText: 'Starting now',
  })
})
it('retains the draft after a failed request and across remounts', async () => {
  const user = userEvent.setup()
  vi.spyOn(agentApi, 'run').mockRejectedValue(new Error('No provider configured'))
  mount()
  await user.type(screen.getByLabelText('Agent prompt'), 'Keep this question')
  await user.click(screen.getByRole('button', { name: 'Send' }))
  await waitFor(() => expect(screen.getByLabelText('Agent prompt')).toBeEnabled())
  cleanup()
  mount()
  expect(screen.getByLabelText('Agent prompt')).toHaveValue('Keep this question')
})
it('masks private terminal input and leaves it out of draft storage', async () => {
  const user = userEvent.setup()
  useAgentStore.setState({
    terminalInput: { session_id: 'test', purpose: 'Authenticate', prompt: 'Password' },
  })
  mount(<AgentInteractions />)
  const input = screen.getByLabelText('Private terminal input')
  expect(input).toHaveAttribute('type', 'password')
  await user.type(input, 'test secret')
  expect(sessionStorage.length).toBe(0)
})
