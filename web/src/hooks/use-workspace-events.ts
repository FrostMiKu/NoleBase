import { useEffect, useRef } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'

import { reconnectDelayMs, wsUrl } from '@/api/websocket'
import { editorStore } from '@/editor/editor-state'
import { workspaceKeys } from '@/queries/keys'
import { useUiStore } from '@/stores/ui-store'
import type { WorkspaceEvent } from '@/types/events'
import { parseEvent } from '@/types/events'

export interface WorkspaceEventDeps {
  queryClient: Pick<import('@tanstack/react-query').QueryClient, 'invalidateQueries'>
  /** The currently open document path, or null. */
  getCurrentPath: () => string | null
  onWatcherError: (message: string) => void
  /** A content change touched `path` (used to flag dirty-conflict state). */
  onExternalChange: (path: string) => void
  onResyncStart: () => void
  onResyncEnd: () => void
}

/**
 * The currently open document path as the event handler sees it. The app
 * shell keeps this in sync from the router (the authoritative selected
 * document state) via {@link setCurrentDocumentPath}.
 */
const currentDocumentPath: { current: string | null } = { current: null }

export function setCurrentDocumentPath(path: string | null): void {
  currentDocumentPath.current = path
}

/**
 * Pure event application so invalidation scopes are unit-testable.
 *
 * - `content_changed`: refresh the workspace content families; the current
 *   document only when the change touched it.
 * - `resync_required` (or a reconnect): refresh everything cached, without
 *   clearing the cache.
 * - `attachments_changed`: no attachment UI exists in this phase — the
 *   lists that could show attachments are unaffected.
 * - `agent_reads_dirty`: a TUI/agent read-cache concern; the web client
 *   keeps no such cache.
 * - `watcher_failed`: reported to the caller (toast + status bar).
 */
export function applyWorkspaceEvent(deps: WorkspaceEventDeps, event: WorkspaceEvent): void {
  switch (event.type) {
    case 'content_changed': {
      invalidateWorkspaceContent(deps.queryClient, deps.getCurrentPath(), {
        changedPaths: event.paths,
        onExternalChange: deps.onExternalChange,
      })
      return
    }
    case 'attachments_changed':
      // No attachment surface yet; document lists don't depend on
      // attachment files.
      return
    case 'agent_reads_dirty':
      // The web client never caches file reads for the agent.
      return
    case 'watcher_failed':
      deps.onWatcherError(event.message)
      return
    case 'resync_required':
      return resyncWorkspace(deps)
    default: {
      const unreachable: never = event
      console.warn('[nole] unhandled workspace event', unreachable)
    }
  }
}

export function invalidateWorkspaceContent(
  queryClient: WorkspaceEventDeps['queryClient'],
  currentPath: string | null,
  options: {
    changedPaths: string[] | null
    onExternalChange: (path: string) => void
  },
): void {
  const { changedPaths, onExternalChange } = options
  void queryClient.invalidateQueries({ queryKey: workspaceKeys.summary() })
  void queryClient.invalidateQueries({ queryKey: workspaceKeys.notes() })
  void queryClient.invalidateQueries({ queryKey: workspaceKeys.daily() })
  void queryClient.invalidateQueries({ queryKey: workspaceKeys.todos() })
  void queryClient.invalidateQueries({ queryKey: workspaceKeys.tags() })
  void queryClient.invalidateQueries({ queryKey: workspaceKeys.searchFamily() })
  void queryClient.invalidateQueries({ queryKey: workspaceKeys.tagFamily() })

  if (currentPath === null) return
  if (changedPaths !== null && !changedPaths.includes(currentPath)) return
  // The DocumentEditor reconciles the refetched content against the dirty
  // state: clean editors adopt it, dirty editors get the pending flag.
  void queryClient.invalidateQueries({ queryKey: workspaceKeys.document(currentPath) })
  void queryClient.invalidateQueries({ queryKey: workspaceKeys.backlinks(currentPath) })
  onExternalChange(currentPath)
}

/** Full resync: mark every workspace cache stale; the cache is preserved. */
export async function resyncWorkspace(deps: WorkspaceEventDeps): Promise<void> {
  deps.onResyncStart()
  await deps.queryClient.invalidateQueries({ queryKey: workspaceKeys.root })
  deps.onResyncEnd()
}

const WORKSPACE_EVENT_TYPES = [
  'content_changed',
  'attachments_changed',
  'agent_reads_dirty',
  'watcher_failed',
  'resync_required',
] as const

/**
 * Owns the `/api/ws/workspace` connection: fixed backoff reconnect
 * (1s/2s/5s/5s…), connection state in the UI store, resync after any
 * reconnect, and pure event application above.
 */
export function useWorkspaceEvents(): void {
  const queryClient = useQueryClient()
  const queryClientRef = useRef(queryClient)
  queryClientRef.current = queryClient

  useEffect(() => {
    let closed = false
    let attempt = 0
    let socket: WebSocket | null = null
    let reconnectTimer: ReturnType<typeof setTimeout> | undefined
    let everConnected = false

    const deps: WorkspaceEventDeps = {
      queryClient: queryClientRef.current,
      getCurrentPath: () => currentDocumentPath.current,
      onWatcherError: (message) => {
        toast.error(`Workspace watcher error: ${message}`)
        useUiStore.getState().setWatcherError(message)
      },
      onExternalChange: (path) => {
        const state = editorStore.getState()
        if (
          state.path === path &&
          state.dirty &&
          !state.externalChangePending &&
          state.movedTo === null
        ) {
          editorStore.markExternalChange()
        }
      },
      onResyncStart: () => useUiStore.getState().setWorkspaceResyncing(true),
      onResyncEnd: () => useUiStore.getState().setWorkspaceResyncing(false),
    }

    const connect = () => {
      if (closed) return
      useUiStore.getState().setWorkspaceConnection('connecting')
      socket = new WebSocket(wsUrl('/api/ws/workspace'))

      socket.onopen = () => {
        attempt = 0
        useUiStore.getState().setWorkspaceConnection('connected')
        if (everConnected) {
          // Reconnect: our consumer dropped out; assume missed events and
          // refresh everything the UI shows.
          void resyncWorkspace(deps)
        }
        everConnected = true
      }

      socket.onmessage = (message) => {
        const parsed = parseEvent<WorkspaceEvent>(
          typeof message.data === 'string' ? message.data : '',
          WORKSPACE_EVENT_TYPES,
        )
        if (!parsed.ok) {
          if (parsed.type !== null) {
            console.warn('[nole] unknown workspace event', parsed.type)
          }
          return
        }
        applyWorkspaceEvent(deps, parsed.event)
      }

      socket.onclose = () => {
        useUiStore.getState().setWorkspaceConnection('disconnected')
        if (!closed) {
          reconnectTimer = setTimeout(connect, reconnectDelayMs(attempt))
          attempt += 1
        }
      }

      socket.onerror = () => socket?.close()
    }

    connect()
    return () => {
      closed = true
      if (reconnectTimer !== undefined) clearTimeout(reconnectTimer)
      socket?.close()
    }
  }, [])
}
