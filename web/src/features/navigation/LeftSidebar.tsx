import { useState } from 'react'
import { useNavigate } from '@tanstack/react-router'
import { ChevronRight, Plus } from 'lucide-react'

import { Collapsible, CollapsibleContent, CollapsibleTrigger } from '@/components/ui/collapsible'
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuTrigger,
} from '@/components/ui/context-menu'
import { CreateNoteDialog } from '@/features/documents/CreateNoteDialog'
import { DeleteNoteDialog } from '@/features/documents/DeleteNoteDialog'
import { RenameNoteDialog } from '@/features/documents/RenameNoteDialog'
import { DeleteDailyDialog } from '@/features/navigation/DeleteDailyDialog'
import { MoveDailyDialog } from '@/features/navigation/MoveDailyDialog'
import { useEditorState } from '@/editor/editor-state'
import { useArchiveDocument, useRestoreDocument } from '@/queries/lifecycle'
import { useDailyNotes, useNotes } from '@/queries/workspace'
import { useUiStore, type SidebarSectionKey } from '@/stores/ui-store'
import type { DailyNote, NoteSummary } from '@/types/workspace'

interface SectionProps {
  sectionKey: SidebarSectionKey
  label: string
  count?: number
  /** Header action rendered beside the count (e.g. the Notes `+`). */
  action?: React.ReactNode
  children: React.ReactNode
}

/**
 * A collapsible sidebar section, mirroring the TUI's section behaviour:
 * the header toggles the list, the expanded state survives navigation.
 */
function Section({ sectionKey, label, count, action, children }: SectionProps) {
  const expanded = useUiStore((state) => state.sidebarSections[sectionKey])
  const toggle = useUiStore((state) => state.toggleSidebarSection)
  return (
    <Collapsible open={expanded} onOpenChange={() => toggle(sectionKey)} data-section={sectionKey}>
      <div className="flex items-center gap-1 px-2 pt-3 pb-1">
        <CollapsibleTrigger className="group flex min-w-0 flex-1 items-center gap-1 text-[10px] font-semibold tracking-wider text-faint uppercase transition-colors hover:text-muted">
          <ChevronRight
            size={10}
            className={`transition-transform group-data-[state=open]:rotate-90`}
          />
          <span>{label}</span>
        </CollapsibleTrigger>
        {action}
        {count !== undefined && <span className="font-mono text-[10px]">{count}</span>}
      </div>
      <CollapsibleContent>
        <div className="px-1 pb-1">{children}</div>
      </CollapsibleContent>
    </Collapsible>
  )
}

interface RowProps extends React.ComponentPropsWithoutRef<'button'> {
  active?: boolean
  children: React.ReactNode
}

function Row({ active = false, onClick, children, ...rest }: RowProps) {
  return (
    <button
      type="button"
      onClick={onClick}
      {...rest}
      aria-current={active ? 'page' : undefined}
      className={`selection-row flex w-full items-center rounded-sm px-2 py-[3px] text-left text-[12px] transition-colors ${
        active ? 'bg-accent/20 text-accent-strong' : 'text-fg hover:bg-raised'
      }`}
    >
      <span className="truncate">{children}</span>
    </button>
  )
}

/** A row wrapped in its right-click file actions. */
function ContextRow({
  active,
  title,
  onOpen,
  items,
  children,
}: {
  active?: boolean
  title?: string
  onOpen: () => void
  items: React.ReactNode
  children: React.ReactNode
}) {
  return (
    <ContextMenu>
      <ContextMenuTrigger asChild>
        <Row active={active} onClick={onOpen} title={title}>
          {children}
        </Row>
      </ContextMenuTrigger>
      <ContextMenuContent>{items}</ContextMenuContent>
    </ContextMenu>
  )
}

/**
 * The file list: Notes, Archives, and Daily as collapsible flat lists.
 * Notes and Archives split the same endpoint by `archived`; Daily comes
 * from the daily endpoint. Every row carries a right-click context menu
 * with the TUI's file actions (the only document action menu — `···`
 * menus are deliberately not duplicated elsewhere).
 */
export function LeftSidebar() {
  const navigate = useNavigate()
  const notesQuery = useNotes()
  const dailyQuery = useDailyNotes()
  const { path: openPath } = useEditorState()

  const archive = useArchiveDocument()
  const restore = useRestoreDocument()

  const [createOpen, setCreateOpen] = useState(false)
  const [renameTarget, setRenameTarget] = useState<NoteSummary | null>(null)
  const [deleteTarget, setDeleteTarget] = useState<NoteSummary | null>(null)
  const [deleteDailyTarget, setDeleteDailyTarget] = useState<DailyNote | null>(null)
  const [moveDailyTarget, setMoveDailyTarget] = useState<DailyNote | null>(null)

  const [filter, setFilter] = useState('')
  const notes = (notesQuery.data ?? []).filter((note) =>
    note.name.toLowerCase().includes(filter.toLowerCase()),
  )
  const activeNotes = notes.filter((note) => !note.archived)
  const archives = notes.filter((note) => note.archived)
  const daily = [...(dailyQuery.data ?? [])].reverse()

  const openDocument = (path: string) => void navigate({ to: '/document', search: { path } })

  return (
    <nav
      aria-label="Workspace files"
      className="flex h-full flex-col overflow-y-auto pb-2"
      data-testid="left-sidebar"
    >
      <div className="sticky top-0 z-10 bg-panel p-2">
        <input
          aria-label="Filter files"
          placeholder="Filter files…"
          value={filter}
          onChange={(event) => setFilter(event.target.value)}
          className="w-full rounded border border-border bg-bg px-2 py-1.5 text-xs outline-none focus:border-accent"
        />
      </div>
      <Section
        sectionKey="notes"
        label="Notes"
        count={activeNotes.length}
        action={
          <button
            type="button"
            title="Create note"
            aria-label="Create note"
            data-testid="create-note-button"
            onClick={() => setCreateOpen(true)}
            className="rounded p-0.5 text-faint transition-colors hover:bg-raised hover:text-fg"
          >
            <Plus size={11} />
          </button>
        }
      >
        {notesQuery.isLoading && <LoadingRow />}
        {notesQuery.isSuccess && activeNotes.length === 0 && (
          <p className="px-2 py-2 text-xs text-faint">
            {filter ? 'No matching notes.' : 'No notes yet. Use + to create one.'}
          </p>
        )}
        {activeNotes.map((note) => (
          <ContextRow
            key={note.path}
            active={openPath === note.path}
            title={note.path}
            onOpen={() => openDocument(note.path)}
            items={
              <>
                <ContextMenuItem onSelect={() => setRenameTarget(note)}>Rename</ContextMenuItem>
                <ContextMenuItem
                  onSelect={() => archive.mutate(note.path)}
                  disabled={archive.isPending && archive.variables === note.path}
                >
                  Archive
                </ContextMenuItem>
                <ContextMenuItem destructive onSelect={() => setDeleteTarget(note)}>
                  Delete
                </ContextMenuItem>
              </>
            }
          >
            {note.name}
          </ContextRow>
        ))}
        {notesQuery.isError && <ErrorRow message="Notes failed to load" />}
      </Section>

      <Section sectionKey="archives" label="Archives" count={archives.length}>
        {archives.map((note) => (
          <ContextRow
            key={note.path}
            active={openPath === note.path}
            title={note.path}
            onOpen={() => openDocument(note.path)}
            items={
              <>
                <ContextMenuItem onSelect={() => setRenameTarget(note)}>Rename</ContextMenuItem>
                <ContextMenuItem
                  onSelect={() => restore.mutate(note.path)}
                  disabled={restore.isPending && restore.variables === note.path}
                >
                  Restore
                </ContextMenuItem>
                <ContextMenuItem destructive onSelect={() => setDeleteTarget(note)}>
                  Delete
                </ContextMenuItem>
              </>
            }
          >
            <span className="text-muted">{note.name}</span>
          </ContextRow>
        ))}
      </Section>

      <Section sectionKey="daily" label="Daily" count={daily.length}>
        {daily
          .filter((note) => filter === '' || note.date.includes(filter))
          .map((note) => (
            <ContextRow
              key={note.path}
              active={openPath === note.path}
              title={note.path}
              onOpen={() => openDocument(note.path)}
              items={
                <>
                  <ContextMenuItem onSelect={() => setMoveDailyTarget(note)}>Move</ContextMenuItem>
                  <ContextMenuItem destructive onSelect={() => setDeleteDailyTarget(note)}>
                    Delete
                  </ContextMenuItem>
                </>
              }
            >
              <span className="font-mono text-[11px]">{note.date}</span>
            </ContextRow>
          ))}
      </Section>

      {createOpen && <CreateNoteDialog onClose={() => setCreateOpen(false)} />}
      {renameTarget !== null && (
        <RenameNoteDialog note={renameTarget} onClose={() => setRenameTarget(null)} />
      )}
      {deleteTarget !== null && (
        <DeleteNoteDialog note={deleteTarget} onClose={() => setDeleteTarget(null)} />
      )}
      {deleteDailyTarget !== null && (
        <DeleteDailyDialog note={deleteDailyTarget} onClose={() => setDeleteDailyTarget(null)} />
      )}
      {moveDailyTarget !== null && (
        <MoveDailyDialog note={moveDailyTarget} onClose={() => setMoveDailyTarget(null)} />
      )}
    </nav>
  )
}

function LoadingRow() {
  return <p className="px-2 py-0.5 text-[11px] text-faint">loading…</p>
}

function ErrorRow({ message }: { message: string }) {
  return <p className="px-2 py-0.5 text-[11px] text-danger">{message}</p>
}
