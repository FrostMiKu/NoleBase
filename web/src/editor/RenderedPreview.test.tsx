import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, render, screen } from '@testing-library/react'
import { afterEach, expect, it, vi } from 'vitest'
import { workspaceApi } from '@/api/workspace'
import { RenderedPreview } from './RenderedPreview'
afterEach(() => {
  cleanup()
  vi.restoreAllMocks()
})
it('renders unsaved source in an opaque sandbox and surfaces image warnings', async () => {
  vi.spyOn(workspaceApi, 'previewDocument').mockResolvedValue({
    html: '<!doctype html><h1>Unsaved preview</h1>',
    warnings: ['Image missing.png could not be embedded'],
  })
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } })
  render(
    <QueryClientProvider client={client}>
      <RenderedPreview path="data/a.mb" source="Unsaved source" />
    </QueryClientProvider>,
  )
  const frame = await screen.findByTitle('Rendered document')
  expect(frame).toHaveAttribute('sandbox', 'allow-scripts')
  expect(frame).toHaveAttribute('srcdoc', '<!doctype html><h1>Unsaved preview</h1>')
  expect(screen.getByText('1 image warning(s)')).toBeTruthy()
  expect(workspaceApi.previewDocument).toHaveBeenCalledWith('data/a.mb', 'Unsaved source')
})
