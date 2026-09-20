import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import {
  RouterProvider,
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  Outlet,
  type AnyRouter,
} from '@tanstack/react-router'
import { cleanup, render, screen } from '@testing-library/react'
import type { UserEvent } from '@testing-library/user-event'
import { vi } from 'vitest'

import { LeftSidebar } from '@/features/navigation/LeftSidebar'
import { editorStore } from '@/editor/editor-state'
import { workspaceKeys } from '@/queries/keys'
import { useDocument } from '@/queries/workspace'
import { useUiStore } from '@/stores/ui-store'
import type { DailyNote, NoteSummary } from '@/types/workspace'

/**
 * Shared harness for sidebar lifecycle tests: a memory-history router with
 * the real LeftSidebar plus a document-route probe that mounts the real
 * `useDocument` query (so invalidations/refetches are observable through
 * the fetch stub, without mounting CodeMirror).
 */

// jsdom lacks the pointer/layout APIs Radix menus and portals consult.
if (!('ResizeObserver' in window)) {
  class ResizeObserverStub {
    observe(): void {}
    unobserve(): void {}
    disconnect(): void {}
  }
  (window as { ResizeObserver?: unknown }).ResizeObserver = ResizeObserverStub
}
const elementProto = window.HTMLElement.prototype as unknown as Record<string, unknown>
elementProto.scrollIntoView = elementProto.scrollIntoView ?? (() => {})
elementProto.hasPointerCapture = elementProto.hasPointerCapture ?? (() => false)
elementProto.setPointerCapture = elementProto.setPointerCapture ?? (() => {})
elementProto.releasePointerCapture = elementProto.releasePointerCapture ?? (() => {})

export interface ApiCall {
  method: string
  url: string
  body: unknown
}

export interface ApiResponse {
  status: number
  body?: unknown
}

/** Install a recording fetch stub; every request goes through `handler`. */
export function stubApi(handler: (call: ApiCall) => ApiResponse): ApiCall[] {
  const calls: ApiCall[] = []
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url =
        typeof input === 'string' ? input : input instanceof URL ? input.href : input.url
      const method = (init?.method ?? 'GET').toUpperCase()
      let body: unknown
      if (typeof init?.body === 'string') {
        try {
          body = JSON.parse(init.body)
        } catch {
          body = init.body
        }
      }
      const call: ApiCall = { method, url, body }
      calls.push(call)
      const response = handler(call)
      const headers =
        response.body === undefined ? undefined : { 'content-type': 'application/json' }
      return new Response(
        response.body === undefined ? null : JSON.stringify(response.body),
        { status: response.status, headers },
      )
    }),
  )
  return calls
}

export const ok = (body?: unknown): ApiResponse => ({ status: 200, body })
export const created = (body: unknown): ApiResponse => ({ status: 201, body })
export const noContent = (): ApiResponse => ({ status: 204 })
export const apiError = (status: number, message: string, code = 'error'): ApiResponse => ({
  status,
  body: { error: { code, message } },
})

export function queryPath(url: string): string | null {
  const query = url.split('?')[1] ?? ''
  return new URLSearchParams(query).get('path')
}

/** Read-side defaults; tests branch on mutations first, then delegate. */
export function readOnlyApi(options: {
  notes: () => NoteSummary[]
  daily?: () => DailyNote[]
  documents?: Record<string, string>
}): (call: ApiCall) => ApiResponse {
  return ({ method, url }) => {
    if (method === 'GET' && url.startsWith('/api/notes')) return ok(options.notes())
    if (method === 'GET' && url.startsWith('/api/daily')) return ok(options.daily?.() ?? [])
    if (method === 'GET' && url.startsWith('/api/document')) {
      const path = queryPath(url)
      const source = path !== null ? options.documents?.[path] : undefined
      return source === undefined ? apiError(404, 'Document not found', 'not_found') : ok({ path, source })
    }
    if (method === 'GET') return ok({})
    return ok({ path: 'data/unknown.md' })
  }
}

function DocumentProbe() {
  const search = documentRoute.useSearch()
  const path = search.path ?? ''
  const documentQuery = useDocument(path)
  return (
    <div data-testid="document-probe">
      {documentQuery.data?.source ?? (documentQuery.isError ? 'error' : 'loading')}
    </div>
  )
}

// The probe route needs its search validator before the component refs it.
const rootRoute = createRootRoute({
  component: () => (
    <div>
      <LeftSidebar />
      <Outlet />
    </div>
  ),
})
const indexRoute = createRoute({ getParentRoute: () => rootRoute, path: '/', component: () => null })
const documentRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/document',
  validateSearch: (search: Record<string, unknown>): { path?: string } => {
    const path = search['path']
    if (path === undefined) return {}
    if (typeof path !== 'string' || path.trim() === '') {
      throw new Error('document path must be a non-empty string')
    }
    return { path }
  },
  component: DocumentProbe,
})

export interface RenderedApp {
  router: AnyRouter
  queryClient: QueryClient
  /** Every committed navigation's mode, in order (`push` grows browser
   * history, `replace` does not — the distinction the lifecycle flows own). */
  navigationLog: Array<'push' | 'replace'>
}

export async function renderSidebarApp(options: {
  notes?: NoteSummary[]
  daily?: DailyNote[]
  /** Initial href, e.g. `/document?path=data%2Ffoo.md`. */
  initialHref?: string
} = {}): Promise<RenderedApp> {
  const queryClient = new QueryClient({
    defaultOptions: {
      queries: { retry: false, staleTime: Infinity },
      mutations: { retry: false },
    },
  })
  if (options.notes !== undefined) queryClient.setQueryData(workspaceKeys.notes(), options.notes)
  if (options.daily !== undefined) queryClient.setQueryData(workspaceKeys.daily(), options.daily)

  const router = createRouter({
    routeTree: rootRoute.addChildren([indexRoute, documentRoute]),
    history: createMemoryHistory({ initialEntries: [options.initialHref ?? '/'] }),
    defaultPreload: false,
  })
  await router.load()
  render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  )
  const navigationLog: Array<'push' | 'replace'> = []
  const originalPush = router.history.push.bind(router.history)
  const originalReplace = router.history.replace.bind(router.history)
  router.history.push = (...args: Parameters<typeof originalPush>) => {
    navigationLog.push('push')
    return originalPush(...args)
  }
  router.history.replace = (...args: Parameters<typeof originalReplace>) => {
    navigationLog.push('replace')
    return originalReplace(...args)
  }
  return { router, queryClient, navigationLog }
}

/** Isolate the shared editor/UI stores between tests. */
export function resetSharedState(): void {
  cleanup()
  editorStore.close()
  useUiStore.setState({ sidebarSections: { notes: true, archives: false, daily: false } })
}

/** Open a collapsed sidebar section by clicking its header. */
export async function openSection(user: UserEvent, label: string): Promise<void> {
  await user.click(screen.getByText(label, { selector: 'span' }))
}
