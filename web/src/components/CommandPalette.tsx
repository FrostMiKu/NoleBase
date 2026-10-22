import { useId, useState } from 'react'
import { useNavigate } from '@tanstack/react-router'

import { useUiStore } from '@/stores/ui-store'
import { saveCurrentDocument } from '@/editor/save'
import { useNotes } from '@/queries/workspace'
import { useSelectionViewport } from '@/lib/selection-viewport'
import {
  Dialog,
  DialogPortal,
  DialogOverlay,
  DialogContent,
  DialogTitle,
  DialogDescription,
} from './ui/dialog'

export function CommandPalette() {
  const open = useUiStore((state) => state.commandPaletteOpen)
  const setOpen = useUiStore((state) => state.setCommandPaletteOpen)
  return (
    <Dialog open={open} onOpenChange={setOpen}>
      {open && <PaletteContent />}
    </Dialog>
  )
}

function PaletteContent() {
  const navigate = useNavigate()
  const notes = useNotes()
  const [query, setQuery] = useState('')
  const id = useId()
  const ui = useUiStore.getState
  const commands = [
    { id: 'home', label: 'Open Workspace', run: () => void navigate({ to: '/' }) },
    { id: 'daily', label: 'Open Daily', run: () => void navigate({ to: '/daily' }) },
    { id: 'new', label: 'Create Note', run: () => ui().setCreateNoteOpen(true) },
    { id: 'search', label: 'Search Workspace', run: () => void navigate({ to: '/search' }) },
    { id: 'tags', label: 'Browse Tags', run: () => void navigate({ to: '/tags' }) },
    {
      id: 'agent',
      label: 'Focus Agent',
      run: () => {
        void navigate({ to: '/agent' })
        ui().requestAgentFocus()
      },
    },
    { id: 'save', label: 'Save Document', run: () => void saveCurrentDocument() },
    { id: 'left', label: 'Toggle Left Sidebar', run: () => ui().toggleLeftSidebar() },
    { id: 'right', label: 'Toggle Right Sidebar', run: () => ui().toggleRightSidebar() },
    ...(notes.data ?? []).map((note) => ({
      id: note.path,
      label: `Open ${note.name}`,
      detail: note.path,
      run: () => void navigate({ to: '/document', search: { path: note.path } }),
    })),
  ]
  const filtered = commands.filter((item) =>
    `${item.label} ${item.id}`.toLowerCase().includes(query.toLowerCase()),
  )
  const viewport = useSelectionViewport(filtered.length)
  const run = (index: number) => {
    const item = filtered[index]
    if (!item) return
    ui().setCommandPaletteOpen(false)
    item.run()
  }
  return (
    <DialogPortal>
      <DialogOverlay />
      <DialogContent size="wide">
        <DialogTitle>Command palette</DialogTitle>
        <DialogDescription className="sr-only">Find a command or open a note.</DialogDescription>
        <input
          autoFocus
          aria-label="Search commands and notes"
          role="combobox"
          aria-expanded
          aria-controls={id}
          aria-activedescendant={filtered.length ? `${id}-${viewport.selection}` : undefined}
          value={query}
          placeholder="Type a command or note name…"
          onChange={(event) => {
            setQuery(event.target.value)
            viewport.reset()
          }}
          onKeyDown={(event) => {
            if (event.nativeEvent.isComposing || event.nativeEvent.keyCode === 229) return
            if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
              event.preventDefault()
              viewport.move(event.key === 'ArrowDown' ? 1 : -1)
            } else if (event.key === 'Enter') {
              event.preventDefault()
              run(viewport.selection)
            }
          }}
          className="mt-2 w-full rounded border border-border-strong bg-bg px-3 py-2 text-sm outline-none focus:border-accent"
        />
        <ul
          id={id}
          role="listbox"
          aria-label="Commands and notes"
          data-viewport-start={viewport.offset}
          className="mt-1 min-h-8 py-1"
        >
          {filtered.slice(viewport.offset, viewport.offset + 8).map((item, row) => {
            const index = row + viewport.offset
            return (
              <li key={item.id}>
                <button
                  id={`${id}-${index}`}
                  type="button"
                  role="option"
                  aria-selected={index === viewport.selection}
                  onClick={() => run(index)}
                  className="selection-row flex h-9 w-full items-center justify-between gap-3 rounded px-3 text-left text-xs aria-selected:bg-accent/20 aria-selected:text-accent-strong hover:bg-raised"
                >
                  <span className="truncate">{item.label}</span>
                  {'detail' in item && (
                    <span className="truncate text-[10px] text-faint">{item.detail}</span>
                  )}
                </button>
              </li>
            )
          })}
          {filtered.length === 0 && (
            <li className="p-3 text-xs text-muted">No matching command or note.</li>
          )}
        </ul>
        <p className="mt-2 text-[10px] text-faint">↑ ↓ to select · Enter to open · Esc to close</p>
      </DialogContent>
    </DialogPortal>
  )
}
