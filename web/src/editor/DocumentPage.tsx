import { useState } from 'react'
import { Check, FileWarning, LoaderCircle, Save } from 'lucide-react'
import { useNavigate } from '@tanstack/react-router'
import { useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'

import { workspaceApi } from '@/api/workspace'
import { editorStore, useEditorState } from '@/editor/editor-state'
import { DocumentEditor } from '@/editor/DocumentEditor'
import { RenderedPreview } from './RenderedPreview'
import { DocumentActions } from './DocumentActions'
import { saveCurrentDocument } from './save'
import { NoteMarkdown } from '@/features/documents/NoteMarkdown'
import { currentEditorSource, replaceEditorSource } from '@/editor/editor-bridge'
import { useAutoSave } from '@/editor/use-autosave'
import { BacklinksPopover } from '@/features/backlinks/BacklinksPopover'
import { workspaceKeys } from '@/queries/keys'
import { useDocument } from '@/queries/workspace'

/**
 * The /document route body. The document identity comes from the router
 * search param (?path=…) — never from a parallel store.
 */
export function DocumentPage({ path, line }: { path: string; line?: number }) {
  const [mode, setMode] = useState<'edit' | 'read' | 'split' | 'rendered'>('edit')
  const navigate = useNavigate()
  const documentQuery = useDocument(path)
  const editor = useEditorState()
  useAutoSave()
  const isCurrent = editor.path === path
  const saveStatus = !isCurrent
    ? 'Loading document'
    : editor.saveError
      ? 'Save failed — click to retry. Your changes are still here.'
      : editor.saving
        ? 'Saving…'
        : editor.dirty
          ? 'Unsaved changes — click to save now'
          : 'All changes saved'

  return (
    <div className="flex h-full min-w-0 flex-col">
      <div
        aria-label="Document controls"
        className="flex h-11 shrink-0 items-center gap-1 border-b border-border bg-panel px-2 sm:gap-2 sm:px-3"
      >
        <span className="min-w-0 flex-1 truncate text-xs text-muted" title={path}>
          <span className="hidden sm:inline">{path.slice(0, path.lastIndexOf('/') + 1)}</span>
          <span className="text-fg">{path.split('/').pop()}</span>
        </span>
        <div className="flex shrink-0 items-center gap-1">
          <select
            aria-label="Document view"
            value={mode}
            onChange={(event) => setMode(event.target.value as typeof mode)}
            className="h-8 max-w-28 rounded border-0 bg-transparent px-1 text-xs text-muted hover:bg-raised"
          >
            <option value="edit">Edit</option>
            <option value="read">Read</option>
            <option value="split">Split</option>
            <option value="rendered">Full preview</option>
          </select>
          <button
            type="button"
            className="toolbar-button disabled:cursor-default"
            title={saveStatus}
            aria-label={isCurrent && editor.saveError ? 'Retry save' : 'Save document'}
            disabled={
              editor.path !== path ||
              !editor.dirty ||
              editor.saving ||
              editor.externalChangePending ||
              editor.movedTo !== null
            }
            onClick={() => void saveCurrentDocument()}
          >
            {isCurrent && editor.saving ? (
              <LoaderCircle size={15} className="animate-spin" />
            ) : isCurrent && (editor.dirty || editor.saveError) ? (
              <Save size={15} className={editor.saveError ? 'text-danger' : 'text-warn'} />
            ) : (
              <Check size={15} className="text-faint" />
            )}
          </button>
          <span role="status" className="sr-only">
            {saveStatus}
          </span>
          <BacklinksPopover path={path} compact />
          {documentQuery.data && (
            <DocumentActions key={path} path={path} source={documentQuery.data.source} />
          )}
        </div>
      </div>

      {editor.path === path && editor.externalChangePending && <ExternalChangeBanner path={path} />}

      <div className="min-h-0 flex-1 overflow-hidden">
        {documentQuery.isLoading && <div className="p-4 text-xs text-faint">Loading document…</div>}
        {documentQuery.isError && (
          <div className="flex flex-col items-start gap-2 p-4 text-xs text-danger">
            <span className="inline-flex items-center gap-1.5">
              <FileWarning size={13} />
              Could not load “{path}”
            </span>
            <span className="text-muted">
              {documentQuery.error instanceof Error ? documentQuery.error.message : 'Unknown error'}
            </span>
            <button
              type="button"
              className="text-accent hover:underline"
              onClick={() => void navigate({ to: '/' })}
            >
              Back to workspace
            </button>
          </div>
        )}
        {documentQuery.data !== undefined && (
          <div className={`flex h-full min-h-0 ${mode === 'split' ? 'flex-col lg:flex-row' : ''}`}>
            <div
              className={
                mode === 'read' || mode === 'rendered' ? 'hidden' : 'min-h-0 min-w-0 flex-1'
              }
            >
              <DocumentEditor
                key={path}
                path={path}
                source={documentQuery.data.source}
                line={line}
              />
            </div>
            {mode !== 'edit' && mode !== 'rendered' && (
              <div
                aria-label="Document preview"
                className={`min-h-0 min-w-0 flex-1 overflow-y-auto p-4 ${mode === 'split' ? 'border-t border-border lg:border-t-0 lg:border-l' : ''}`}
              >
                <div className="mx-auto max-w-3xl">
                  <NoteMarkdown
                    path={path}
                    text={
                      editor.path === path
                        ? (currentEditorSource() ?? documentQuery.data.source)
                        : documentQuery.data.source
                    }
                  />
                </div>
              </div>
            )}
            {mode === 'rendered' && (
              <RenderedPreview
                key={path}
                path={path}
                source={
                  editor.path === path
                    ? (currentEditorSource() ?? documentQuery.data.source)
                    : documentQuery.data.source
                }
              />
            )}
          </div>
        )}
      </div>
    </div>
  )
}

/**
 * A disk change arrived while the editor held unsaved edits. The local
 * text is never touched automatically; the user decides. “Reload from
 * Disk” drops the local edits; “Use my version” resumes saving the local
 * version over the disk version. The backend has no revision/OCC token.
 */
function ExternalChangeBanner({ path }: { path: string }) {
  const queryClient = useQueryClient()

  const reloadFromDisk = () => {
    void workspaceApi
      .document(path)
      .then((document) => {
        if (editorStore.getState().path !== path) return
        editorStore.loadDocument(path, document.source)
        replaceEditorSource(document.source)
        queryClient.setQueryData(workspaceKeys.document(path), document)
        toast.success('Reloaded from disk')
      })
      .catch((error: unknown) => {
        toast.error(`Reload failed: ${error instanceof Error ? error.message : 'unknown error'}`)
      })
  }

  const keepEditing = () => {
    editorStore.clearExternalChange()
    toast.info('Your version will be saved over the newer disk version.')
  }

  return (
    <div className="flex flex-wrap items-center gap-3 border-b border-warn/40 bg-warn/10 px-3 py-1.5 text-[11px] text-warn">
      <span>This file changed on disk.</span>
      <div className="ml-auto flex items-center gap-1.5">
        <button
          type="button"
          className="inline-flex h-6 items-center rounded-sm border border-warn/50 px-2 transition-colors hover:bg-warn/20"
          onClick={reloadFromDisk}
        >
          Reload from Disk
        </button>
        <button
          type="button"
          className="inline-flex h-6 items-center rounded-sm px-2 text-muted transition-colors hover:text-fg"
          onClick={keepEditing}
        >
          Use my version
        </button>
      </div>
    </div>
  )
}
