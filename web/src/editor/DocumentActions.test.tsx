import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { toast } from 'sonner'
import { workspaceApi } from '@/api/workspace'
import { editorStore } from './editor-state'
import * as bridge from './editor-bridge'
import { DocumentActions } from './DocumentActions'

vi.mock('@/queries/lifecycle', () => ({
  useArchiveDocument: () => ({ isPending: false }),
  useRestoreDocument: () => ({ isPending: false }),
}))
beforeEach(() => {
  vi.stubGlobal(
    'URL',
    class extends URL {
      static createObjectURL = vi.fn(() => 'blob:download')
      static revokeObjectURL = vi.fn()
    },
  )
})
afterEach(() => {
  cleanup()
  vi.restoreAllMocks()
  vi.unstubAllGlobals()
})

it('exports the current unsaved text from the overflow and closes after success', async () => {
  editorStore.loadDocument('data/a.mb', 'Saved source')
  vi.spyOn(bridge, 'currentEditorSource').mockReturnValue('Unsaved source')
  vi.spyOn(workspaceApi, 'previewDocument').mockResolvedValue({
    html: '<h1>Unsaved</h1>',
    warnings: [],
  })
  const download = vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(() => {})
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } })
  const user = userEvent.setup()
  render(
    <QueryClientProvider client={client}>
      <DocumentActions path="data/a.mb" source="Saved source" />
    </QueryClientProvider>,
  )
  expect(screen.queryByRole('button', { name: 'Download HTML' })).not.toBeInTheDocument()
  await user.click(screen.getByRole('button', { name: 'More document actions' }))
  await user.click(screen.getByRole('button', { name: 'Download HTML' }))
  await waitFor(() => expect(download).toHaveBeenCalledOnce())
  expect(workspaceApi.previewDocument).toHaveBeenCalledWith('data/a.mb', 'Unsaved source')
  expect(download.mock.instances[0]).toHaveProperty('download', 'a.html')
  await waitFor(() =>
    expect(screen.queryByRole('button', { name: 'Download HTML' })).not.toBeInTheDocument(),
  )
})

it('keeps the export action available to retry after a rendering failure', async () => {
  vi.spyOn(workspaceApi, 'previewDocument').mockRejectedValue(new Error('Renderer unavailable'))
  const error = vi.spyOn(toast, 'error').mockImplementation(() => 0)
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } })
  const user = userEvent.setup()
  render(
    <QueryClientProvider client={client}>
      <DocumentActions path="data/a.mb" source="Saved source" />
    </QueryClientProvider>,
  )
  await user.click(screen.getByRole('button', { name: 'More document actions' }))
  await user.click(screen.getByRole('button', { name: 'Download HTML' }))
  await waitFor(() =>
    expect(error).toHaveBeenCalledWith('HTML export failed: Renderer unavailable'),
  )
  expect(screen.getByRole('button', { name: 'Download HTML' })).toBeEnabled()
})
