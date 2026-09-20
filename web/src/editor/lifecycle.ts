/**
 * Autosave coordination for document lifecycle mutations (rename, archive,
 * restore, delete, daily move). While a mutation owns the open document the
 * old path must never receive another PUT: the block stored on the editor
 * state stops the debounced timer AND the explicit save command for exactly
 * that path, while these flows flush through `persistDocument` directly.
 *
 * The block is a path, not a flag: the moment the editor store retargets
 * (`loadDocument` for the mutation response path) or closes, the block
 * releases itself — autosave resumes for the new document without anyone
 * remembering to flip a flag back.
 */

import { editorStore } from './editor-state'
import { currentEditorSource } from './editor-bridge'
import { persistDocument } from './save'
import { cancelScheduledAutosave } from './use-autosave'

/** True while `path` is owned by an in-flight lifecycle mutation. */
export function isAutosaveBlockedFor(path: string): boolean {
  return editorStore.getState().autosaveBlock === path
}

/** Wait until no save request is in flight (the store's `saving` flag). */
export function waitForSaveToSettle(): Promise<void> {
  if (!editorStore.getState().saving) return Promise.resolve()
  return new Promise((resolve) => {
    const unsubscribe = editorStore.subscribe(() => {
      if (!editorStore.getState().saving) {
        unsubscribe()
        resolve()
      }
    })
  })
}

/**
 * The rename/archive/restore/daily-move prologue for the CURRENT document:
 * block new autosaves, let an in-flight save finish, then persist any
 * remaining edits through a direct save (the block never blocks itself).
 *
 * Returns true when the document is safe to mutate. On failure the block is
 * released again, the path stays, and the local text is untouched.
 */
export async function flushBeforeLifecycleMutation(path: string): Promise<boolean> {
  editorStore.blockAutosave(path)
  cancelScheduledAutosave()
  await waitForSaveToSettle()
  const state = editorStore.getState()
  if (state.path !== path) {
    // The open document switched underneath us; there is nothing to flush.
    editorStore.releaseAutosaveBlock(path)
    return true
  }
  if (!state.dirty) return true
  const source = currentEditorSource()
  if (source === null) {
    editorStore.releaseAutosaveBlock(path)
    return false
  }
  const saved = await persistDocument(path, source)
  if (!saved) editorStore.releaseAutosaveBlock(path)
  return saved
}

/**
 * The delete/daily-delete prologue for the CURRENT document: the user has
 * confirmed permanent deletion, so pending edits are cancelled — never
 * flushed — and the path can no longer receive any PUT.
 */
export function cancelEditsForLifecycleDeletion(path: string): void {
  editorStore.blockAutosave(path)
  cancelScheduledAutosave()
  editorStore.discard()
}
