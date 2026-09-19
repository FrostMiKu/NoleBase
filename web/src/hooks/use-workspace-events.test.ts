import { QueryClient } from '@tanstack/react-query'
import { describe, expect, it, vi } from 'vitest'

import { applyWorkspaceEvent, type WorkspaceEventDeps } from './use-workspace-events'
import { workspaceKeys } from '@/queries/keys'
import type { WorkspaceEvent } from '@/types/events'

function makeDeps(currentPath: string | null) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  // Seed the queries the UI would have cached. No observers are attached,
  // so invalidation marks them stale without any network activity.
  queryClient.setQueryData(workspaceKeys.summary(), { notes: 0 })
  queryClient.setQueryData(workspaceKeys.notes(), [])
  queryClient.setQueryData(workspaceKeys.daily(), [])
  queryClient.setQueryData(workspaceKeys.todos(), [])
  queryClient.setQueryData(workspaceKeys.tags(), [])
  queryClient.setQueryData(workspaceKeys.search('needle'), { query: 'needle', hits: [] })
  queryClient.setQueryData(workspaceKeys.tag('taggy'), [])
  queryClient.setQueryData(workspaceKeys.document('data/foo.md'), {
    path: 'data/foo.md',
    source: 'source',
  })
  queryClient.setQueryData(workspaceKeys.document('data/other.md'), {
    path: 'data/other.md',
    source: 'source',
  })
  queryClient.setQueryData(workspaceKeys.backlinks('data/foo.md'), {
    path: 'data/foo.md',
    backlinks: [],
  })

  const onExternalChange = vi.fn()
  const onWatcherError = vi.fn()
  const onResyncStart = vi.fn()
  const onResyncEnd = vi.fn()

  const deps: WorkspaceEventDeps = {
    queryClient,
    getCurrentPath: () => currentPath,
    onWatcherError,
    onExternalChange,
    onResyncStart,
    onResyncEnd,
  }

  const isInvalidated = (key: readonly unknown[]) =>
    queryClient.getQueryState(key)?.isInvalidated === true

  return { deps, isInvalidated, onExternalChange, onWatcherError, onResyncStart, onResyncEnd }
}

function contentChanged(paths: string[]): WorkspaceEvent {
  return { type: 'content_changed', paths }
}

describe('content_changed invalidation scope', () => {
  it('refreshes workspace families but not the current document when untouched', () => {
    const { deps, isInvalidated, onExternalChange } = makeDeps('data/foo.md')
    applyWorkspaceEvent(deps, contentChanged(['data/other.md', 'daily/2026-01-01.md']))

    expect(isInvalidated(workspaceKeys.summary())).toBe(true)
    expect(isInvalidated(workspaceKeys.notes())).toBe(true)
    expect(isInvalidated(workspaceKeys.daily())).toBe(true)
    expect(isInvalidated(workspaceKeys.todos())).toBe(true)
    expect(isInvalidated(workspaceKeys.tags())).toBe(true)
    expect(isInvalidated(workspaceKeys.search('needle'))).toBe(true)
    expect(isInvalidated(workspaceKeys.tag('taggy'))).toBe(true)

    expect(isInvalidated(workspaceKeys.document('data/foo.md'))).toBe(false)
    expect(isInvalidated(workspaceKeys.backlinks('data/foo.md'))).toBe(false)
    expect(onExternalChange).not.toHaveBeenCalled()
  })

  it('refreshes the current document and backlinks when it changed', async () => {
    const { deps, isInvalidated, onExternalChange } = makeDeps('data/foo.md')
    applyWorkspaceEvent(deps, contentChanged(['data/foo.md']))

    expect(isInvalidated(workspaceKeys.document('data/foo.md'))).toBe(true)
    expect(isInvalidated(workspaceKeys.backlinks('data/foo.md'))).toBe(true)
    expect(onExternalChange).toHaveBeenCalledWith('data/foo.md')
  })

  it('does not touch any document when none is open', () => {
    const { deps, isInvalidated, onExternalChange } = makeDeps(null)
    applyWorkspaceEvent(deps, contentChanged(['data/foo.md']))

    expect(isInvalidated(workspaceKeys.document('data/foo.md'))).toBe(false)
    expect(onExternalChange).not.toHaveBeenCalled()
    expect(isInvalidated(workspaceKeys.notes())).toBe(true)
  })
})

describe('resync_required', () => {
  it('invalidates everything cached including the current document', async () => {
    const { deps, isInvalidated, onResyncStart, onResyncEnd } = makeDeps('data/foo.md')
    await applyWorkspaceEvent(deps, { type: 'resync_required', skipped: 5 })

    expect(isInvalidated(workspaceKeys.summary())).toBe(true)
    expect(isInvalidated(workspaceKeys.notes())).toBe(true)
    expect(isInvalidated(workspaceKeys.daily())).toBe(true)
    expect(isInvalidated(workspaceKeys.todos())).toBe(true)
    expect(isInvalidated(workspaceKeys.tags())).toBe(true)
    expect(isInvalidated(workspaceKeys.search('needle'))).toBe(true)
    expect(isInvalidated(workspaceKeys.tag('taggy'))).toBe(true)
    expect(isInvalidated(workspaceKeys.document('data/foo.md'))).toBe(true)
    expect(isInvalidated(workspaceKeys.document('data/other.md'))).toBe(true)
    expect(onResyncStart).toHaveBeenCalled()
    expect(onResyncEnd).toHaveBeenCalled()
  })
})

describe('watcher_failed', () => {
  it('reports the failure to the UI layer', () => {
    const { deps, onWatcherError } = makeDeps('data/foo.md')
    applyWorkspaceEvent(deps, { type: 'watcher_failed', message: 'inotify died' })
    expect(onWatcherError).toHaveBeenCalledWith('inotify died')
  })
})

describe('no-op events', () => {
  it('ignores attachments_changed and agent_reads_dirty explicitly', () => {
    const { deps, isInvalidated, onWatcherError, onExternalChange } = makeDeps('data/foo.md')
    applyWorkspaceEvent(deps, { type: 'attachments_changed', paths: ['img.png'] })
    applyWorkspaceEvent(deps, { type: 'agent_reads_dirty', paths: ['data/foo.md'] })
    expect(isInvalidated(workspaceKeys.summary())).toBe(false)
    expect(isInvalidated(workspaceKeys.notes())).toBe(false)
    expect(isInvalidated(workspaceKeys.document('data/foo.md'))).toBe(false)
    expect(onWatcherError).not.toHaveBeenCalled()
    expect(onExternalChange).not.toHaveBeenCalled()
  })
})
