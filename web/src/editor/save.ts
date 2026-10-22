import { toast } from 'sonner'

import { workspaceApi } from '@/api/workspace'
import { workspaceKeys } from '@/queries/keys'
import { queryClient } from '@/queries/query-client'
import { editorStore } from './editor-state'
import { currentEditorSource } from './editor-bridge'

/**
 * Persist the current document through `PUT /api/document`. Explicit save
 * and autosave share this path. On
 * failure the dirty flag stands and the local text is never rolled back.
 * Refuses the path while a lifecycle mutation owns it (§ editor/lifecycle).
 */
export async function saveCurrentDocument(): Promise<boolean> {
  const { path } = editorStore.getState()
  if (path === null) return false
  if (activeSave !== null) {
    if (!(await activeSave)) return false
    if (editorStore.getState().path !== path) return false
  }
  const { movedTo, externalChangePending, autosaveBlock } = editorStore.getState()
  if (autosaveBlock === path) return false
  if (externalChangePending) {
    toast.error('Resolve the disk change before saving your edits.')
    return false
  }
  const source = currentEditorSource()
  if (source === null) return false
  if (movedTo !== null) {
    toast.error('This file was moved on disk. Resolve the banner above before saving.')
    return false
  }
  if (source === editorStore.getState().baselineSource) return true
  const saved = await persistDocument(path, source)
  return saved && !editorStore.getState().dirty
}

/** Save content to an explicit path (the file-moved conflict resolution). */
export async function saveDocumentTo(path: string, source: string): Promise<boolean> {
  return persistDocument(path, source)
}

let activeSave: Promise<boolean> | null = null

export async function persistDocument(path: string, source: string): Promise<boolean> {
  if (activeSave !== null && !(await activeSave)) return false
  const request = writeDocument(path, source)
  activeSave = request
  try {
    return await request
  } finally {
    if (activeSave === request) activeSave = null
  }
}

async function writeDocument(path: string, source: string): Promise<boolean> {
  const version = editorStore.getState().documentVersion
  editorStore.beginSave()
  try {
    await workspaceApi.writeDocument(path, source)
    if (editorStore.getState().documentVersion === version) {
      editorStore.saveSucceeded(source, currentEditorSource() ?? source)
    }
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
    queryClient.invalidateQueries({ queryKey: workspaceKeys.daily() })
    queryClient.invalidateQueries({ queryKey: workspaceKeys.searchFamily() })
    return true
  } catch (error) {
    if (editorStore.getState().documentVersion === version) editorStore.saveFailed()
    const message = error instanceof Error ? error.message : 'Unknown error'
    toast.error(`Save failed: ${message}`)
    return false
  }
}
