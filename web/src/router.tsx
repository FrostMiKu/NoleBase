import { lazy, Suspense } from 'react'
import { RouteError, RouteNotFound } from '@/components/RouteFallback'
import {
  Outlet,
  createRootRoute,
  createRoute,
  createRouter,
  redirect,
} from '@tanstack/react-router'

import { AppShell } from '@/app/AppShell'
import { AgentPage } from '@/features/agent/AgentPage'
const DocumentPage = lazy(() =>
  import('@/editor/DocumentPage').then((module) => ({ default: module.DocumentPage })),
)
import { DailyPage } from '@/features/navigation/DailyPage'
import { WorkspaceHome } from '@/features/navigation/WorkspaceHome'
import { SearchPage } from '@/features/search/SearchPage'
import { TagPage } from '@/features/tags/TagPage'
import { TagsPage } from '@/features/tags/TagsPage'

const rootRoute = createRootRoute({
  component: AppShell,
  notFoundComponent: RouteNotFound,
  errorComponent: RouteError,
})

/** `/` — the workspace default page. */
const indexRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/',
  component: WorkspaceHome,
})

/** `/document?path=<workspace-relative-path>` — document identity lives in
 * the search param so `/`, spaces, and unicode in note paths stay safe. */
const documentRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/document',
  validateSearch: (search: Record<string, unknown>): { path?: string; line?: number } => {
    const path = search['path']
    if (path === undefined) return {}
    if (typeof path !== 'string' || path.trim() === '') {
      throw new Error('document path must be a non-empty string')
    }
    const line = Number(search['line'])
    return { path, ...(Number.isSafeInteger(line) && line > 0 ? { line } : {}) }
  },
  beforeLoad: ({ search }) => {
    if (search.path === undefined) {
      throw redirect({ to: '/', replace: true })
    }
  },
  component: DocumentRoute,
})

function DocumentRoute() {
  const search = documentRoute.useSearch()
  return (
    <Suspense fallback={<p className="p-4 text-sm text-muted">Loading editor…</p>}>
      <DocumentPage path={search.path ?? ''} line={search.line} />
    </Suspense>
  )
}

/** `/search?q=...` */
const searchRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/search',
  validateSearch: (search: Record<string, unknown>): { q?: string } => {
    const q = search['q']
    if (q === undefined) return {}
    if (typeof q !== 'string') {
      throw new Error('search query must be a string')
    }
    return q.trim() === '' ? {} : { q }
  },
  component: SearchRoute,
})

function SearchRoute() {
  const search = searchRoute.useSearch()
  return <SearchPage q={search.q} />
}

/** `/tag/$tag` */
const tagRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/tag/$tag',
  component: TagRoute,
})

function TagRoute() {
  const params = tagRoute.useParams()
  return <TagPage tag={params.tag} />
}

/** `/tags` — the tag-centred file list. */
const tagsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/tags',
  component: TagsPage,
})

/** `/agent` — the TUI-style main Agent view. */
const agentRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/agent',
  component: AgentPage,
})

/** `/daily` */
const dailyRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/daily',
  component: DailyPage,
})

const routeTree = rootRoute.addChildren([
  indexRoute,
  documentRoute,
  searchRoute,
  tagRoute,
  tagsRoute,
  agentRoute,
  dailyRoute,
])

export const router = createRouter({
  routeTree,
  defaultPreload: false,
})

declare module '@tanstack/react-router' {
  interface Register {
    router: typeof router
  }
}

export { Outlet }
