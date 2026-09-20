import { useState } from 'react'

import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogTitle,
  buttonStyles,
} from '@/components/ui/dialog'
import { useCreateNote } from '@/queries/lifecycle'

/**
 * The `+` entry in the Notes section header. Sends the name exactly as
 * typed (the backend owns normalization and extensions) and navigates to
 * the path the response returns. Stays open with the error inline when the
 * mutation fails; the user's input is kept.
 */
export function CreateNoteDialog({ onClose }: { onClose: () => void }) {
  const createNote = useCreateNote()
  const [name, setName] = useState('')
  const [fromTemplate, setFromTemplate] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const submit = () => {
    const trimmed = name.trim()
    if (trimmed === '' || createNote.isPending) return
    setError(null)
    createNote.mutate(
      { name: trimmed, fromTemplate },
      {
        onSuccess: () => onClose(),
        onError: (mutationError) =>
          setError(mutationError instanceof Error ? mutationError.message : 'Unknown error'),
      },
    )
  }

  return (
    <Dialog
      open
      onOpenChange={(next) => {
        if (!next && !createNote.isPending) onClose()
      }}
    >
      <DialogContent
        onEscapeKeyDown={(event) => {
          if (createNote.isPending) event.preventDefault()
        }}
        onInteractOutside={(event) => event.preventDefault()}
      >
        <DialogTitle>Create note</DialogTitle>
        <DialogDescription>New notes are created in the workspace's notes directory.</DialogDescription>
        <form
          className="mt-3 flex flex-col gap-3"
          onSubmit={(event) => {
            event.preventDefault()
            submit()
          }}
        >
          <label className="flex flex-col gap-1.5 text-xs text-fg">
            Name
            <input
              autoFocus
              value={name}
              onChange={(event) => setName(event.target.value)}
              data-testid="create-note-name"
              className="h-7 rounded-sm border border-border-strong bg-raised px-2 text-xs text-fg outline-none focus-visible:border-accent"
            />
          </label>
          <label className="flex items-center gap-2 text-xs text-fg select-none">
            <input
              type="checkbox"
              checked={fromTemplate}
              onChange={(event) => setFromTemplate(event.target.checked)}
              data-testid="create-note-template"
              className="size-3.5 accent-[var(--color-accent)]"
            />
            Use template
          </label>
          {error && (
            <p role="alert" data-testid="dialog-error" className="text-[11px] text-danger">
              {error}
            </p>
          )}
          <div className="mt-1 flex items-center justify-end gap-2">
            <button
              type="button"
              className={buttonStyles.secondary}
              onClick={onClose}
              disabled={createNote.isPending}
            >
              Cancel
            </button>
            <button
              type="submit"
              className={buttonStyles.primary}
              disabled={name.trim() === '' || createNote.isPending}
              data-testid="create-note-submit"
            >
              {createNote.isPending ? 'Creating…' : 'Create'}
            </button>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  )
}
