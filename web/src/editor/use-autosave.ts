import { useEffect } from 'react'

import { editorStore } from './editor-state'
import { saveCurrentDocument } from './save'

/** Idle delay before an unsaved edit is persisted automatically. */
const AUTOSAVE_DELAY_MS = 1_500

/**
 * Automatic saving for the open document. Every store notification while
 * the editor is dirty restarts the idle timer, so a burst of typing saves
 * once, after the burst settles. Conflicts hold autosave off (the banner
 * owns them) and the navigation guard flushes pending edits explicitly.
 */
export function useAutoSave(): void {
  useEffect(() => {
    let timer: ReturnType<typeof setTimeout> | undefined

    const schedule = () => {
      const state = editorStore.getState()
      if (!state.dirty || state.path === null || state.saving) return
      // Conflict states are user decisions, not autosave moments.
      if (state.externalChangePending || state.movedTo !== null) return
      if (timer !== undefined) clearTimeout(timer)
      timer = setTimeout(() => {
        timer = undefined
        void saveCurrentDocument()
      }, AUTOSAVE_DELAY_MS)
    }

    schedule()
    const unsubscribe = editorStore.subscribe(schedule)
    return () => {
      unsubscribe()
      if (timer !== undefined) clearTimeout(timer)
    }
  }, [])
}

/** Flush any pending autosave immediately (navigation guard, shortcuts). */
export function flushPendingSave(): void {
  const state = editorStore.getState()
  if (!state.dirty || state.path === null || state.saving) return
  void saveCurrentDocument()
}
