/**
 * Central TanStack Query key registry. Components never hand-write query
 * key strings. The `*Family` helpers invalidate whole key branches (used
 * by realtime resync) without touching other caches.
 */

export const workspaceKeys = {
  root: ['workspace'] as const,
  summary: () => [...workspaceKeys.root, 'summary'] as const,
  notes: () => [...workspaceKeys.root, 'notes'] as const,
  daily: () => [...workspaceKeys.root, 'daily'] as const,
  todos: () => [...workspaceKeys.root, 'todos'] as const,
  document: (path: string) => [...workspaceKeys.root, 'document', path] as const,
  backlinks: (path: string) => [...workspaceKeys.root, 'backlinks', path] as const,
  search: (query: string) => [...workspaceKeys.root, 'search', query] as const,
  searchFamily: () => [...workspaceKeys.root, 'search'] as const,
  tags: () => [...workspaceKeys.root, 'tags'] as const,
  tag: (tag: string) => [...workspaceKeys.root, 'tag', tag] as const,
  tagFamily: () => [...workspaceKeys.root, 'tag'] as const,
}

export const agentKeys = {
  root: ['agent'] as const,
  state: () => [...agentKeys.root, 'state'] as const,
  permission: () => [...agentKeys.root, 'permission'] as const,
}
