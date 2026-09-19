import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import {
  RouterProvider,
  createMemoryHistory,
  createRootRoute,
  createRouter,
} from '@tanstack/react-router'
import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it } from 'vitest'

import { TagsPage } from './TagsPage'
import { workspaceKeys } from '@/queries/keys'
import type { TagDocument, TagSummary } from '@/types/workspace'

// A minimal memory-history router keeps document-row navigation off the
// real browser history in jsdom.
async function renderTagsPage(tags: TagSummary[], documentsByTag: Record<string, TagDocument[]>) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  queryClient.setQueryData(workspaceKeys.tags(), tags)
  for (const [tag, documents] of Object.entries(documentsByTag)) {
    queryClient.setQueryData(workspaceKeys.tag(tag), documents)
  }
  const rootRoute = createRootRoute({ component: TagsPage })
  const router = createRouter({
    routeTree: rootRoute,
    history: createMemoryHistory({ initialEntries: ['/tags'] }),
  })
  await router.load()
  return render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  )
}

describe('TagsPage', () => {
  it('lists tags collapsed and reveals documents when a group opens', async () => {
    const user = userEvent.setup()
    await renderTagsPage(
      [
        { name: 'rust', documents: 2, mentions: 5 },
        { name: 'design', documents: 1, mentions: 1 },
      ],
      {
        rust: [
          { path: 'data/note-a.md', modified: null },
          { path: 'data/note-b.md', modified: null },
        ],
      },
    )

    expect(screen.getByText('#rust')).toBeTruthy()
    expect(screen.getByText('#design')).toBeTruthy()
    // Documents stay hidden while collapsed.
    expect(screen.queryByText('data/note-a.md')).toBeNull()

    await user.click(screen.getByText('#rust'))
    await waitFor(() => {
      expect(screen.getByText('data/note-a.md')).toBeTruthy()
      expect(screen.getByText('data/note-b.md')).toBeTruthy()
    })
    // The untouched group stays collapsed.
    expect(screen.queryByText('data/old-note.md')).toBeNull()
  })

  it('shows the empty state when no tags exist', async () => {
    await renderTagsPage([], {})
    expect(screen.getByText('0 tag(s)')).toBeTruthy()
  })
})
