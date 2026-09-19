import { toast } from 'sonner'

import { workspaceApi } from '@/api/workspace'
import { workspaceKeys } from '@/queries/keys'
import { queryClient } from '@/queries/query-client'
import { editorStore } from './editor-state'
import { currentEditorSource } from './editor-bridge'

/**
 * Persist the current document through `PUT /api/document`. Explicit save
 * only (Cmd/Ctrl+S, toolbar button, command palette); no autosave. On
 * failure the dirty flag stands and the local text is never rolled back.
 */
export async function saveCurrentDocument(): Promise<boolean> {
  const { path, saving, movedTo } = editorStore.getState()
  if (path === null || saving) return false
  const source = currentEditorSource()
  if (source === null) return false
  if (movedTo !== null) {
    toast.error('This file was moved on disk. Resolve the banner above before saving.')
    return false
  }
  return persistDocument(path, source)
}

/** Save content to an explicit path (the file-moved conflict resolution). */
export async function saveDocumentTo(path: string, source: string): Promise<boolean> {
  return persistDocument(path, source)
}

async function persistDocument(path: string, source: string): Promise<boolean> {
  editorStore.beginSave()
  try {
    await workspaceApi.writeDocument(path, source)
    editorStore.saveSucceeded(source)
    // The document content is known: write it into the cache instead of
    // refetching. The watcher will still fire a content_changed event,
    // which is a no-op for this document once the cache matches.
    queryClient.setQueryData(workspaceKeys.document(path), { path, source })
    // Lightweight lists may be affected immediately; the watcher event
    // will cover anything missed.
    queryClient.invalidateQueries({ queryKey: workspaceKeys.summary() })
    queryClient.invalidateQueries({ queryKey: workspaceKeys.notes() })
    queryClient.invalidateQueries({ queryKey: workspaceKeys.tags() })
    queryClient.invalidateQueries({ queryKey: workspaceKeys.todos() })
    queryClient.invalidateQueries({ queryKey: workspaceKeys.searchFamily() })
    return true
  } catch (error) {
    editorStore.saveFailed()
    const message = error instanceof Error ? error.message : 'Unknown error'
    toast.error(`Save failed: ${message}`)
    return false
  }
}
