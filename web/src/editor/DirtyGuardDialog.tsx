import { useState } from 'react'

import { Dialog, DialogContent, DialogDescription, DialogTitle, buttonStyles } from '@/components/ui/dialog'

interface DirtyGuardDialogProps {
  documentPath: string
  targetPath: string | null
  onSaveAndContinue: () => void | Promise<void>
  onDiscard: () => void
  onCancel: () => void
}

/**
 * The unsaved-changes gate (Save and Continue / Discard / Cancel). Open
 * while a blocked navigation is pending; not dismissible via the overlay
 * so the user must choose explicitly.
 */
export function DirtyGuardDialog({
  documentPath,
  targetPath,
  onSaveAndContinue,
  onDiscard,
  onCancel,
}: DirtyGuardDialogProps) {
  const [busy, setBusy] = useState(false)

  return (
    <Dialog open onOpenChange={(next) => !next && onCancel()}>
      <DialogContent
        onEscapeKeyDown={(event) => {
          event.preventDefault()
          onCancel()
        }}
        onInteractOutside={(event) => event.preventDefault()}
        aria-describedby={undefined}
      >
        <DialogTitle>Unsaved changes</DialogTitle>
        <DialogDescription>
          {targetPath === null
            ? `“${documentPath}” has unsaved edits. Choose how to continue.`
            : `Leave “${documentPath}” for “${targetPath}”? Unsaved edits will be lost unless saved.`}
        </DialogDescription>
        <div className="mt-4 flex items-center justify-end gap-2">
          <button type="button" className={buttonStyles.secondary} onClick={onCancel}>
            Cancel
          </button>
          <button
            type="button"
            className={buttonStyles.danger}
            onClick={onDiscard}
            disabled={busy}
          >
            Discard
          </button>
          <button
            type="button"
            className={buttonStyles.primary}
            onClick={() => {
              setBusy(true)
              void Promise.resolve(onSaveAndContinue()).finally(() => setBusy(false))
            }}
            disabled={busy}
          >
            {busy ? 'Saving…' : 'Save and Continue'}
          </button>
        </div>
      </DialogContent>
    </Dialog>
  )
}
