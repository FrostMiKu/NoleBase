import { useState } from 'react'

import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogTitle,
  buttonStyles,
} from '@/components/ui/dialog'
import { useDeleteDaily } from '@/queries/lifecycle'
import type { DailyNote } from '@/types/workspace'

/**
 * The confirmation behind Delete on Daily rows. If the day being deleted is
 * the open document, the deletion flow cancels its pending edits and the
 * navigation leaves for the workspace home.
 */
export function DeleteDailyDialog({ note, onClose }: { note: DailyNote; onClose: () => void }) {
  const deleteDaily = useDeleteDaily()
  const [error, setError] = useState<string | null>(null)

  const confirm = () => {
    if (deleteDaily.isPending) return
    setError(null)
    deleteDaily.mutate(note, {
      onSuccess: () => onClose(),
      onError: (mutationError) =>
        setError(mutationError instanceof Error ? mutationError.message : 'Unknown error'),
    })
  }

  return (
    <Dialog
      open
      onOpenChange={(next) => {
        if (!next && !deleteDaily.isPending) onClose()
      }}
    >
      <DialogContent
        onEscapeKeyDown={(event) => {
          if (deleteDaily.isPending) event.preventDefault()
        }}
        onInteractOutside={(event) => event.preventDefault()}
      >
        <DialogTitle>Delete daily note?</DialogTitle>
        <DialogDescription>Delete daily note for {note.date}?</DialogDescription>
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
            disabled={deleteDaily.isPending}
          >
            Cancel
          </button>
          <button
            type="button"
            className={buttonStyles.danger}
            onClick={confirm}
            disabled={deleteDaily.isPending}
            data-testid="delete-daily-confirm"
          >
            {deleteDaily.isPending ? 'Deleting…' : 'Delete'}
          </button>
        </div>
      </DialogContent>
    </Dialog>
  )
}
