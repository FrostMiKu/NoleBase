import { useState } from 'react'
import { useNavigate } from '@tanstack/react-router'
import { CalendarDays, FolderInput, MoreHorizontal, Trash2 } from 'lucide-react'
import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover'

import { useDailyNotes } from '@/queries/workspace'
import { NoteMarkdown } from '@/features/documents/NoteMarkdown'
import { DailyQuickInput } from './DailyQuickInput'
import { MoveDailyDialog } from './MoveDailyDialog'
import { DeleteDailyDialog } from './DeleteDailyDialog'
import type { DailyNote } from '@/types/workspace'

export function DailyPage() {
  const dailyQuery = useDailyNotes()
  const navigate = useNavigate()
  const [filter, setFilter] = useState('')
  const [move, setMove] = useState<DailyNote | null>(null)
  const [remove, setRemove] = useState<DailyNote | null>(null)
  const [actionsPath, setActionsPath] = useState<string | null>(null)
  const daily = [...(dailyQuery.data ?? [])]
    .sort((a, b) => b.date.localeCompare(a.date))
    .filter((note) => `${note.date} ${note.body}`.toLowerCase().includes(filter.toLowerCase()))

  return (
    <div className="h-full overflow-y-auto">
      <div className="mx-auto max-w-4xl space-y-3 p-4">
        <div className="flex items-center justify-between gap-3">
          <h1 className="text-[15px] font-semibold text-fg">Daily</h1>
          <input
            aria-label="Filter daily notes"
            placeholder="Filter by date or text…"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            className="min-w-0 w-48 max-w-[75%] rounded border border-border bg-panel px-2 py-1.5 text-xs outline-none focus:border-accent"
          />
        </div>
        <DailyQuickInput />
        {dailyQuery.isLoading && (
          <p role="status" className="py-2 text-sm text-muted">
            Loading daily notes…
          </p>
        )}
        {dailyQuery.isError && (
          <div
            role="alert"
            className="my-4 rounded border border-danger/40 p-4 text-xs text-danger"
          >
            <p>Could not load daily notes. {dailyQuery.error.message}</p>
            <button
              type="button"
              onClick={() => void dailyQuery.refetch()}
              className="mt-2 underline"
            >
              Retry
            </button>
          </div>
        )}
        {dailyQuery.isSuccess && daily.length === 0 && (
          <div className="rounded border border-dashed border-border p-4">
            <p className="text-sm text-fg">
              {filter ? 'No matching daily notes' : 'Your first thought starts here'}
            </p>
            <p className="mt-1 text-xs text-muted">
              {filter
                ? 'Try a different date or phrase.'
                : 'Write above to save a note to today. Use #tags, [[links]], and - [ ] tasks.'}
            </p>
          </div>
        )}
        <div className="flex flex-col gap-3" data-testid="daily-river">
          {daily.map((note) => (
            <article
              key={note.path}
              className="min-w-0 rounded-md border border-border bg-panel"
              data-testid="daily-card"
            >
              <div className="flex h-10 items-center gap-1 px-3">
                <button
                  type="button"
                  onClick={() => void navigate({ to: '/document', search: { path: note.path } })}
                  className="mr-auto flex items-center gap-2 text-accent-strong"
                  title={`Open ${note.path}`}
                >
                  <CalendarDays size={14} />
                  <span className="font-mono text-xs">{note.date}</span>
                </button>
                <Popover
                  open={actionsPath === note.path}
                  onOpenChange={(open) => setActionsPath(open ? note.path : null)}
                >
                  <PopoverTrigger
                    className="toolbar-button"
                    aria-label={`Actions for ${note.date}`}
                  >
                    <MoreHorizontal size={15} />
                  </PopoverTrigger>
                  <PopoverContent
                    align="end"
                    compact
                    aria-label={`Actions for ${note.date}`}
                  >
                    <button
                      type="button"
                      className="flex w-full items-center gap-2 rounded px-2 py-2 text-xs hover:bg-raised"
                      onClick={() => {
                        setActionsPath(null)
                        setMove(note)
                      }}
                    >
                      <FolderInput size={14} /> Move into note
                    </button>
                    <button
                      type="button"
                      className="flex w-full items-center gap-2 rounded px-2 py-2 text-xs text-danger hover:bg-raised"
                      onClick={() => {
                        setActionsPath(null)
                        setRemove(note)
                      }}
                    >
                      <Trash2 size={14} /> Delete daily note
                    </button>
                  </PopoverContent>
                </Popover>
              </div>
              <div className="px-3 pb-3">
                <NoteMarkdown text={note.body} path={note.path} />
              </div>
            </article>
          ))}
        </div>
        {move && <MoveDailyDialog note={move} onClose={() => setMove(null)} />}
        {remove && <DeleteDailyDialog note={remove} onClose={() => setRemove(null)} />}
      </div>
    </div>
  )
}
