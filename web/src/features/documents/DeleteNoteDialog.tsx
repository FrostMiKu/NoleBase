import { useState } from 'react'

import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogTitle,
  buttonStyles,
} from '@/components/ui/dialog'
import { useDeleteDocument } from '@/queries/lifecycle'
import type { NoteSummary } from '@/types/workspace'

/**
 * The confirmation behind Delete on Notes and Archives rows. There is no
 * form and Cancel is the first focusable action, so a stray Enter cancels
 * instead of deleting. On failure the dialog stays open with the server
 * message inline; the editor (if this is the open document) is untouched.
 */
export function DeleteNoteDialog({ note, onClose }: { note: NoteSummary; onClose: () => void }) {
  const deleteNote = useDeleteDocument()
  const [error, setError] = useState<string | null>(null)

  const confirm = () => {
    if (deleteNote.isPending) return
    setError(null)
    deleteNote.mutate(note.path, {
      onSuccess: () => onClose(),
      onError: (mutationError) =>
        setError(mutationError instanceof Error ? mutationError.message : 'Unknown error'),
    })
  }

  return (
    <Dialog
      open
      onOpenChange={(next) => {
        if (!next && !deleteNote.isPending) onClose()
      }}
    >
      <DialogContent
        onEscapeKeyDown={(event) => {
          if (deleteNote.isPending) event.preventDefault()
        }}
        onInteractOutside={(event) => event.preventDefault()}
      >
        <DialogTitle>Delete note?</DialogTitle>
        <DialogDescription>
          This will permanently delete “{note.name}”.
        </DialogDescription>
        {error && (
          <p role="alert" data-testid="dialog-error" className="mt-2 text-[11px] text-danger">
            {error}
          </p>
        )}
        <div className="mt-4 flex items-center justify-end gap-2">
          <button
            type="button"
            className={buttonStyles.secondary}
            onClick={onClose}
            disabled={deleteNote.isPending}
          >
            Cancel
          </button>
          <button
            type="button"
            className={buttonStyles.danger}
            onClick={confirm}
            disabled={deleteNote.isPending}
            data-testid="delete-note-confirm"
          >
            {deleteNote.isPending ? 'Deleting…' : 'Delete'}
          </button>
        </div>
      </DialogContent>
    </Dialog>
  )
}
