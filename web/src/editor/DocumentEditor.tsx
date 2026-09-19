import { EditorView } from '@codemirror/view'
import { useEffect, useRef } from 'react'

import { attachEditorView, detachEditorView, replaceEditorSource } from './editor-bridge'
import { createEditorState } from './extensions'
import { editorStore } from './editor-state'
import { saveCurrentDocument } from './save'

interface DocumentEditorProps {
  /** Workspace-relative path of the document that should be displayed. */
  path: string
  /** Latest known disk content for `path` (undefined while loading). */
  source: string | undefined
}

/**
 * React wrapper around the single CodeMirror 6 view. React owns nothing
 * about the text: CodeMirror is the editing authority. This component only
 * (a) creates/destroys the view, (b) swaps documents on `path` change, and
 * (c) applies external reloads when the editor is clean.
 */
export function DocumentEditor({ path, source }: DocumentEditorProps) {
  const containerRef = useRef<HTMLDivElement | null>(null)
  const viewRef = useRef<EditorView | null>(null)

  // Create the view once for the lifetime of the mounted editor surface.
  useEffect(() => {
    const container = containerRef.current
    if (container === null) return
    const view = new EditorView({
      state: createEditorState(
        '',
        () => void saveCurrentDocument(),
        () => {
          const state = editorStore.getState()
          if (state.path === null) return
          const live = viewRef.current?.state.doc.toString() ?? null
          if (live === null) return
          editorStore.setLiveDirty(live !== state.baselineSource)
        },
      ),
      parent: container,
    })
    viewRef.current = view
    attachEditorView(view)
    return () => {
      detachEditorView(view)
      view.destroy()
      viewRef.current = null
      editorStore.close()
    }
  }, [])

  // Document switch / external reload reconciliation.
  useEffect(() => {
    if (source === undefined) return
    const state = editorStore.getState()
    if (state.path === path && state.baselineSource === source) return
    if (state.path === path && state.dirty) {
      // External change while dirty: the watcher handler has already
      // flagged it; never clobber the user's text here.
      return
    }
    editorStore.loadDocument(path, source)
    replaceEditorSource(source)
  }, [path, source])

  return <div ref={containerRef} className="h-full w-full overflow-hidden" data-testid="document-editor" />
}
