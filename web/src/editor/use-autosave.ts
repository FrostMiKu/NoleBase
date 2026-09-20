import { useEffect } from 'react'

import { editorStore } from './editor-state'
import { saveCurrentDocument } from './save'

/** Idle delay before an unsaved edit is persisted automatically. */
const AUTOSAVE_DELAY_MS = 1_500

// Module-level so lifecycle flows (editor/lifecycle.ts) can cancel the
// armed timer the moment they take ownership of the open document.
let pendingTimer: ReturnType<typeof setTimeout> | undefined

/** Cancel a scheduled (debounced) autosave that has not fired yet. */
export function cancelScheduledAutosave(): void {
  if (pendingTimer !== undefined) {
    clearTimeout(pendingTimer)
    pendingTimer = undefined
  }
}

/**
 * Automatic saving for the open document. Every store notification while
 * the editor is dirty restarts the idle timer, so a burst of typing saves
 * once, after the burst settles. Conflicts hold autosave off (the banner
 * owns them) and the navigation guard flushes pending edits explicitly.
 * A path owned by a lifecycle mutation never schedules (§ lifecycle.ts).
 */
export function useAutoSave(): void {
  useEffect(() => {
    const schedule = () => {
      const state = editorStore.getState()
      if (!state.dirty || state.path === null || state.saving) return
      // Conflict states are user decisions, not autosave moments.
      if (state.externalChangePending || state.movedTo !== null) return
      if (state.autosaveBlock === state.path) {
        cancelScheduledAutosave()
        return
      }
      if (pendingTimer !== undefined) clearTimeout(pendingTimer)
      pendingTimer = setTimeout(() => {
        pendingTimer = undefined
        void saveCurrentDocument()
      }, AUTOSAVE_DELAY_MS)
    }

    schedule()
    const unsubscribe = editorStore.subscribe(schedule)
    return () => {
      unsubscribe()
      cancelScheduledAutosave()
    }
  }, [])
}

/** Flush any pending autosave immediately (navigation guard, shortcuts). */
export function flushPendingSave(): void {
  const state = editorStore.getState()
  if (!state.dirty || state.path === null || state.saving) return
  void saveCurrentDocument()
}
