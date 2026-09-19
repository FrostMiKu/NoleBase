import type { EditorView } from '@codemirror/view'

/**
 * Process-wide handle on the single active CodeMirror view. Only one
 * document is open at a time; the save command and external-reload paths
 * need to read/replace its content without prop drilling the view.
 */
let activeView: EditorView | null = null

export function attachEditorView(view: EditorView): void {
  activeView = view
}

export function detachEditorView(view: EditorView): void {
  if (activeView === view) activeView = null
}

/** The live document text, or null with no editor mounted. */
export function currentEditorSource(): string | null {
  return activeView ? activeView.state.doc.toString() : null
}

/** Replace the whole document (external reload / document switch). */
export function replaceEditorSource(source: string): void {
  if (!activeView) return
  activeView.dispatch({
    changes: { from: 0, to: activeView.state.doc.length, insert: source },
    filter: false,
  })
}
