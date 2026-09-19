import { FileWarning, Save } from 'lucide-react'
import { useNavigate } from '@tanstack/react-router'
import { useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'

import { workspaceApi } from '@/api/workspace'
import { editorStore, useEditorState } from '@/editor/editor-state'
import { DocumentEditor } from '@/editor/DocumentEditor'
import { replaceEditorSource } from '@/editor/editor-bridge'
import { saveCurrentDocument } from '@/editor/save'
import { BacklinksPopover } from '@/features/backlinks/BacklinksPopover'
import { workspaceKeys } from '@/queries/keys'
import { useDocument } from '@/queries/workspace'

/**
 * The /document route body. The document identity comes from the router
 * search param (?path=…) — never from a parallel store.
 */
export function DocumentPage({ path }: { path: string }) {
  const navigate = useNavigate()
  const documentQuery = useDocument(path)
  const editor = useEditorState()

  return (
    <div className="flex h-full min-w-0 flex-col">
      <div className="flex h-9 shrink-0 items-center gap-2 border-b border-border bg-panel px-3">
        <span className="truncate font-mono text-[11px] text-muted" title={path}>
          {path}
        </span>
        {editor.path === path && editor.dirty && (
          <span className="text-[10px] text-warn">● modified</span>
        )}
        <div className="ml-auto flex items-center gap-1">
          <BacklinksPopover path={path} />
          <button
            type="button"
            className="inline-flex h-6 items-center gap-1 rounded-sm border border-border-strong bg-raised px-2 text-[11px] text-fg transition-colors hover:bg-overlay disabled:cursor-not-allowed disabled:opacity-50"
            disabled={editor.path !== path || !editor.dirty || editor.saving}
            onClick={() => void saveCurrentDocument()}
            title="Save (⌘S)"
          >
            <Save size={11} />
            {editor.saving ? 'Saving…' : 'Save'}
          </button>
        </div>
      </div>

      {editor.path === path && editor.externalChangePending && (
        <ExternalChangeBanner path={path} />
      )}

      <div className="min-h-0 flex-1">
        {documentQuery.isLoading && (
          <div className="p-4 text-xs text-faint">Loading document…</div>
        )}
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
          <DocumentEditor path={path} source={documentQuery.data.source} />
        )}
      </div>
    </div>
  )
}

/**
 * A disk change arrived while the editor held unsaved edits. The local
 * text is never touched automatically; the user decides. “Reload from
 * Disk” drops the local edits; “Keep Editing” keeps them and the next
 * explicit save overwrites the disk version (no revision/OCC in the
 * backend).
 */
function ExternalChangeBanner({ path }: { path: string }) {
  const queryClient = useQueryClient()

  const reloadFromDisk = () => {
    void workspaceApi
      .document(path)
      .then((document) => {
        editorStore.loadDocument(path, document.source)
        replaceEditorSource(document.source)
        queryClient.setQueryData(workspaceKeys.document(path), document)
        toast.success('Reloaded from disk')
      })
      .catch((error: unknown) => {
        toast.error(
          `Reload failed: ${error instanceof Error ? error.message : 'unknown error'}`,
        )
      })
  }

  const keepEditing = () => {
    editorStore.clearExternalChange()
    toast.info('Keeping your edits. Saving will overwrite the newer disk version.')
  }

  return (
    <div className="flex items-center gap-3 border-b border-warn/40 bg-warn/10 px-3 py-1.5 text-[11px] text-warn">
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
          Keep Editing
        </button>
      </div>
    </div>
  )
}
