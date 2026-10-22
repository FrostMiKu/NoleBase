import { useState } from 'react'
import { useNavigate } from '@tanstack/react-router'

import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogTitle,
  buttonStyles,
} from '@/components/ui/dialog'
import { currentEditorSource } from './editor-bridge'
import { editorStore, useEditorState } from './editor-state'
import { permitNavigationOnce } from './navigation-guard'
import { saveDocumentTo } from './save'

/**
 * Resolution for `file_moved` while the editor holds unsaved edits: the
 * old path is gone, so saving there must never happen. The user either
 * carries the edits to the new location or abandons them. “Keep editing”
 * dismisses the dialog but keeps the banner; saving stays blocked.
 */
export function FileMovedConflict() {
  const { movedTo, path } = useEditorState()
  const navigate = useNavigate()
  const [busy, setBusy] = useState(false)
  const [dismissedFor, setDismissedFor] = useState<string | null>(null)

  if (movedTo === null || path === null) return null

  const liveSource = () => currentEditorSource() ?? editorStore.getState().baselineSource
  const keepEditing = () => setDismissedFor(movedTo)
  const dialogOpen = dismissedFor !== movedTo

  const openNewLocation = () => {
    permitNavigationOnce()
    editorStore.discard()
    void navigate({ to: '/document', search: { path: movedTo } })
  }

  const carryEditsToNewLocation = () => {
    const source = liveSource()
    setBusy(true)
    void saveDocumentTo(movedTo, source).then((saved) => {
      setBusy(false)
      if (!saved) return
      permitNavigationOnce()
      editorStore.loadDocument(movedTo, source)
      void navigate({ to: '/document', search: { path: movedTo } })
    })
  }

  return (
    <>
      <Dialog
        open={dialogOpen}
        onOpenChange={(next) => {
          if (!next && !busy) keepEditing()
        }}
      >
        <DialogContent
          onEscapeKeyDown={(event) => {
            event.preventDefault()
            if (!busy) keepEditing()
          }}
          onInteractOutside={(event) => event.preventDefault()}
          aria-describedby={undefined}
        >
          <DialogTitle>This file was moved on disk</DialogTitle>
          <DialogDescription>
            “{path}” now lives at “{movedTo}”. Saving to the old path is disabled — carry your edits
            to the new location or discard them.
          </DialogDescription>
          <div className="mt-4 flex items-center justify-end gap-2">
            <button
              type="button"
              className={buttonStyles.secondary}
              onClick={keepEditing}
              disabled={busy}
            >
              Keep editing
            </button>
            <button
              type="button"
              className={buttonStyles.danger}
              onClick={openNewLocation}
              disabled={busy}
            >
              Discard edits, open new
            </button>
            <button
              type="button"
              className={buttonStyles.primary}
              onClick={carryEditsToNewLocation}
              disabled={busy}
            >
              {busy ? 'Saving…' : 'Save edits to new location'}
            </button>
          </div>
        </DialogContent>
      </Dialog>
      <FileMovedBanner
        movedTo={movedTo}
        onResolve={() => setDismissedFor(null)}
        hidden={dialogOpen}
      />
    </>
  )
}

/**
 * Persistent inline banner while the moved conflict is unresolved.
 * Recovers the same choices without blocking the UI.
 */
function FileMovedBanner({
  movedTo,
  onResolve,
  hidden,
}: {
  movedTo: string
  onResolve: () => void
  hidden: boolean
}) {
  if (hidden) return null
  return (
    <div className="flex items-center gap-2 border-b border-warn/40 bg-warn/10 px-3 py-1.5 text-[11px] text-warn">
      <span>
        This file moved on disk to <span className="font-mono">{movedTo}</span>. Saving is disabled
        until resolved.
      </span>
      <button type="button" className={`${buttonStyles.secondary} ml-auto`} onClick={onResolve}>
        Resolve
      </button>
    </div>
  )
}
