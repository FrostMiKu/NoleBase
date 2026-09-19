/**
 * The per-open-document lifecycle state (§ editor dirty model). CodeMirror
 * owns the text; this tiny external store owns only the lifecycle flags
 * and the last-saved baseline. It exists so the status bar, the save
 * command, and the navigation guard share one authoritative answer without
 * re-rendering React on every keystroke (updates notify only on
 * transitions).
 */

import { useSyncExternalStore } from 'react'

export interface EditorDocumentState {
  /** Workspace-relative path of the open document, or null when none. */
  path: string | null
  /** The content as of the last load/save; the dirty reference point. */
  baselineSource: string
  dirty: boolean
  saving: boolean
  /** Disk changed while the editor had unsaved edits. */
  externalChangePending: boolean
  /**
   * The file was moved on disk (`file_moved` from the agent stream) while
   * the editor had unsaved edits. Saving to the old path is disabled until
   * the conflict is resolved through the banner/dialog actions.
   */
  movedTo: string | null
}

interface EditorStore {
  subscribe: (listener: () => void) => () => void
  getState: () => EditorDocumentState
  loadDocument: (path: string, source: string) => void
  setLiveDirty: (dirty: boolean) => void
  /** Current text no longer matches what the store last saw (e.g. after an undo). */
  beginSave: () => void
  saveSucceeded: (savedSource: string) => void
  saveFailed: () => void
  markExternalChange: () => void
  clearExternalChange: () => void
  markMoved: (to: string) => void
  clearMoved: () => void
  discard: () => void
  /** No document is open (the editor surface unmounted). */
  close: () => void
}

const initial: EditorDocumentState = {
  path: null,
  baselineSource: '',
  dirty: false,
  saving: false,
  externalChangePending: false,
  movedTo: null,
}

let state: EditorDocumentState = initial
const listeners = new Set<() => void>()

function set(next: Partial<EditorDocumentState>): void {
  let changed = false
  for (const key of Object.keys(next) as (keyof EditorDocumentState)[]) {
    if (state[key] !== next[key]) {
      changed = true
      break
    }
  }
  if (!changed) return
  state = { ...state, ...next }
  for (const listener of listeners) listener()
}

export const editorStore: EditorStore = {
  subscribe(listener) {
    listeners.add(listener)
    return () => listeners.delete(listener)
  },
  getState: () => state,

  loadDocument(path, source) {
    set({ path, baselineSource: source, dirty: false, saving: false, externalChangePending: false, movedTo: null })
  },

  setLiveDirty(dirty) {
    set({ dirty })
  },

  beginSave() {
    set({ saving: true })
  },

  saveSucceeded(savedSource) {
    set({ baselineSource: savedSource, dirty: false, saving: false, externalChangePending: false })
  },

  saveFailed() {
    set({ saving: false })
  },

  markExternalChange() {
    set({ externalChangePending: true })
  },

  clearExternalChange() {
    set({ externalChangePending: false })
  },

  markMoved(to) {
    set({ movedTo: to, externalChangePending: false })
  },

  clearMoved() {
    set({ movedTo: null })
  },

  discard() {
    // Abandon unsaved edits: the baseline stands in as the "live" content
    // so the dirty flag clears before the pending navigation proceeds.
    set({ dirty: false, externalChangePending: false, movedTo: null })
  },

  close() {
    set({
      path: null,
      baselineSource: '',
      dirty: false,
      saving: false,
      externalChangePending: false,
      movedTo: null,
    })
  },
}

/** React binding: re-renders only on lifecycle transitions. */
export function useEditorState(): EditorDocumentState {
  return useSyncExternalStore(editorStore.subscribe, editorStore.getState, editorStore.getState)
}
