import { useMemo, useState } from 'react'

import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogTitle,
  buttonStyles,
} from '@/components/ui/dialog'
import { useMoveDaily } from '@/queries/lifecycle'
import { useNotes } from '@/queries/workspace'
import type { DailyNote } from '@/types/workspace'

/**
 * "Move daily note into…" — a filter box over the normal (`data/`) notes.
 * The list is the existing `GET /api/notes` cache filtered client-side;
 * archives and daily files are never eligible targets. Enter moves the
 * highlighted note; on failure the dialog stays open with the server
 * message inline and the selection intact.
 */
export function MoveDailyDialog({ note, onClose }: { note: DailyNote; onClose: () => void }) {
  const move = useMoveDaily()
  const notesQuery = useNotes()
  const [query, setQuery] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [activeIndex, setActiveIndex] = useState(0)

  const targets = useMemo(
    () =>
      (notesQuery.data ?? [])
        .filter((candidate) => !candidate.archived)
        .map((candidate) => ({ path: candidate.path, name: candidate.name })),
    [notesQuery.data],
  )
  const needle = query.trim().toLowerCase()
  const filtered = needle === ''
    ? targets
    : targets.filter((target) => target.name.toLowerCase().includes(needle))

  const moveTo = (targetPath: string) => {
    if (move.isPending) return
    setError(null)
    move.mutate(
      { note, targetPath },
      {
        onSuccess: () => onClose(),
        onError: (mutationError) =>
          setError(mutationError instanceof Error ? mutationError.message : 'Unknown error'),
      },
    )
  }

  const highlighted = filtered[activeIndex]

  return (
    <Dialog
      open
      onOpenChange={(next) => {
        if (!next && !move.isPending) onClose()
      }}
    >
      <DialogContent
        className="w-[min(380px,90vw)]"
        onEscapeKeyDown={(event) => {
          if (move.isPending) event.preventDefault()
        }}
        onInteractOutside={(event) => event.preventDefault()}
      >
        <DialogTitle>Move daily note</DialogTitle>
        <DialogDescription>
          Move this daily note into: <span className="font-mono">{note.date}</span>
        </DialogDescription>
        <div className="mt-3 flex flex-col gap-2">
          <input
            autoFocus
            value={query}
            placeholder="Filter notes…"
            aria-label="Filter notes"
            data-testid="move-daily-filter"
            onChange={(event) => {
              setQuery(event.target.value)
              setActiveIndex(0)
            }}
            onKeyDown={(event) => {
              if (highlighted === undefined) return
              if (event.key === 'ArrowDown') {
                event.preventDefault()
                setActiveIndex((index) => Math.min(index + 1, filtered.length - 1))
              } else if (event.key === 'ArrowUp') {
                event.preventDefault()
                setActiveIndex((index) => Math.max(index - 1, 0))
              } else if (event.key === 'Enter') {
                event.preventDefault()
                moveTo(highlighted.path)
              }
            }}
            className="h-7 rounded-sm border border-border-strong bg-raised px-2 text-xs text-fg outline-none focus-visible:border-accent"
          />
          <ul
            role="listbox"
            aria-label="Target notes"
            data-testid="move-daily-targets"
            className="max-h-52 overflow-y-auto rounded-sm border border-border"
          >
            {filtered.map((target, index) => (
              <li key={target.path}>
                <button
                  type="button"
                  role="option"
                  aria-selected={index === activeIndex}
                  onClick={() => {
                    setActiveIndex(index)
                    moveTo(target.path)
                  }}
                  className={`flex w-full items-center px-2 py-1 text-left text-[12px] transition-colors ${
                    index === activeIndex
                      ? 'bg-accent/20 text-accent-strong'
                      : 'text-fg hover:bg-raised'
                  }`}
                >
                  <span className="truncate">{target.name}</span>
                  <span className="ml-auto pl-2 font-mono text-[10px] text-faint">
                    {target.path}
                  </span>
                </button>
              </li>
            ))}
            {filtered.length === 0 && (
              <li className="px-2 py-1.5 text-[11px] text-faint">No matching notes.</li>
            )}
          </ul>
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
              disabled={move.isPending}
            >
              Cancel
            </button>
            <button
              type="button"
              className={buttonStyles.primary}
              onClick={() => {
                if (highlighted !== undefined) moveTo(highlighted.path)
              }}
              disabled={highlighted === undefined || move.isPending}
              data-testid="move-daily-submit"
            >
              {move.isPending ? 'Moving…' : 'Move'}
            </button>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  )
}
