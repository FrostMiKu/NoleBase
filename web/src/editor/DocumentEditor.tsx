import { Compartment, EditorState, StateEffect } from '@codemirror/state'
import { EditorView } from '@codemirror/view'
import { useEffect, useRef } from 'react'

import { attachEditorView, detachEditorView, replaceEditorSource } from './editor-bridge'
import { createEditorState } from './extensions'
import { editorStore, useEditorState } from './editor-state'
import { saveCurrentDocument } from './save'

interface DocumentEditorProps {
  /** Workspace-relative path of the document that should be displayed. */
  path: string
  /** Latest known disk content for `path` (undefined while loading). */
  source: string | undefined
  line?: number
}

/**
 * React wrapper around the single CodeMirror 6 view. React owns nothing
 * about the text: CodeMirror is the editing authority. This component only
 * (a) creates/destroys the view, (b) swaps documents on `path` change, and
 * (c) applies external reloads when the editor is clean.
 */
export function DocumentEditor({ path, source, line }: DocumentEditorProps) {
  const { saving, autosaveBlock } = useEditorState()
  const editable = useRef(new Compartment())
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
    view.dispatch({ effects: StateEffect.appendConfig.of(editable.current.of([])) })
    viewRef.current = view
    attachEditorView(view)
    return () => {
      detachEditorView(view)
      view.destroy()
      viewRef.current = null
      editorStore.close()
    }
  }, [])

  useEffect(() => {
    viewRef.current?.dispatch({
      effects: editable.current.reconfigure([
        EditorState.readOnly.of(autosaveBlock === path),
        EditorView.editable.of(autosaveBlock !== path),
      ]),
    })
  }, [autosaveBlock, path])

  // Document switch / external reload reconciliation.
  useEffect(() => {
    if (source === undefined || saving) return
    const state = editorStore.getState()
    if (state.path === path && state.baselineSource === source) return
    if (state.path === path && state.dirty) {
      // A fetched disk version differs from the baseline. Preserve local
      // edits and let the conflict banner own the resolution.
      editorStore.markExternalChange()
      return
    }
    editorStore.loadDocument(path, source)
    replaceEditorSource(source)
  }, [path, source, saving])

  useEffect(() => {
    const view = viewRef.current
    if (!view || line === undefined || source === undefined) return
    const target = view.state.doc.line(Math.max(1, Math.min(line, view.state.doc.lines)))
    view.dispatch({
      selection: { anchor: target.from },
      effects: EditorView.scrollIntoView(target.from, { y: 'center' }),
    })
  }, [path, line, source !== undefined])

  return (
    <div
      ref={containerRef}
      className="h-full w-full overflow-hidden"
      data-testid="document-editor"
    />
  )
}
