import { useState } from 'react'

import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogTitle,
  buttonStyles,
} from '@/components/ui/dialog'
import { useRenameDocument } from '@/queries/lifecycle'
import type { NoteSummary } from '@/types/workspace'

/**
 * Shared by Notes and Archives rows. The input starts from the current
 * filename without its extension (`data/foo.md` → `foo`); the backend owns
 * normalization, collision checks, and wikilink migration. On 409
 * (collision / ambiguous wikilink identity) the dialog stays open with the
 * server message inline and the note's path untouched.
 */
export function RenameNoteDialog({ note, onClose }: { note: NoteSummary; onClose: () => void }) {
  const rename = useRenameDocument()
  const [name, setName] = useState(note.name)
  const [error, setError] = useState<string | null>(null)

  const submit = () => {
    const trimmed = name.trim()
    if (trimmed === '' || rename.isPending) return
    setError(null)
    rename.mutate(
      { path: note.path, newName: trimmed },
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
        if (!next && !rename.isPending) onClose()
      }}
    >
      <DialogContent
        onEscapeKeyDown={(event) => {
          if (rename.isPending) event.preventDefault()
        }}
        onInteractOutside={(event) => event.preventDefault()}
      >
        <DialogTitle>Rename note</DialogTitle>
        <DialogDescription>{note.path}</DialogDescription>
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
              data-testid="rename-note-name"
              className="h-7 rounded-sm border border-border-strong bg-raised px-2 text-xs text-fg outline-none focus-visible:border-accent"
            />
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
              disabled={rename.isPending}
            >
              Cancel
            </button>
            <button
              type="submit"
              className={buttonStyles.primary}
              disabled={name.trim() === '' || name.trim() === note.name || rename.isPending}
              data-testid="rename-note-submit"
            >
              {rename.isPending ? 'Renaming…' : 'Rename'}
            </button>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  )
}
