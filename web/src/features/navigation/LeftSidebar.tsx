import { useNavigate } from '@tanstack/react-router'

import { useDailyNotes, useNotes, useTags } from '@/queries/workspace'
import { useEditorState } from '@/editor/editor-state'

function SectionHeader({ label, count }: { label: string; count?: number }) {
  return (
    <p className="flex items-center justify-between px-2 pt-3 pb-1 text-[10px] font-semibold tracking-wider text-faint uppercase">
      <span>{label}</span>
      {count !== undefined && <span className="font-mono text-[10px]">{count}</span>}
    </p>
  )
}

interface RowProps {
  active?: boolean
  onClick: () => void
  children: React.ReactNode
  title?: string
}

function Row({ active = false, onClick, children, title }: RowProps) {
  return (
    <button
      type="button"
      onClick={onClick}
      title={title}
      className={`flex w-full items-center rounded-sm px-2 py-[3px] text-left text-[12px] transition-colors ${
        active ? 'bg-accent/20 text-accent-strong' : 'text-fg hover:bg-raised'
      }`}
    >
      <span className="truncate">{children}</span>
    </button>
  )
}

/**
 * Left navigation: Daily, Notes, Archives, Tags, Search. Notes and
 * Archives are flat lists split from the same endpoint by `archived`.
 */
export function LeftSidebar() {
  const navigate = useNavigate()
  const notesQuery = useNotes()
  const dailyQuery = useDailyNotes()
  const tagsQuery = useTags()
  const { path: openPath } = useEditorState()

  const notes = notesQuery.data ?? []
  const active = notes.filter((note) => !note.archived)
  const archives = notes.filter((note) => note.archived)
  const daily = [...(dailyQuery.data ?? [])].reverse()

  return (
    <nav className="flex h-full flex-col overflow-y-auto py-1" data-testid="left-sidebar">
      <SectionHeader label="Search" />
      <div className="px-2">
        <Row onClick={() => void navigate({ to: '/search' })}>Search workspace…</Row>
      </div>

      <SectionHeader label="Daily" count={daily.length} />
      <div className="px-1">
        {dailyQuery.isLoading && <LoadingRow />}
        {daily.map((note) => (
          <Row
            key={note.path}
            active={openPath === note.path}
            title={note.path}
            onClick={() => void navigate({ to: '/document', search: { path: note.path } })}
          >
            {note.date}
          </Row>
        ))}
        {dailyQuery.isError && <ErrorRow message="Daily notes failed to load" />}
      </div>

      <SectionHeader label="Notes" count={active.length} />
      <div className="px-1">
        {notesQuery.isLoading && <LoadingRow />}
        {active.map((note) => (
          <Row
            key={note.path}
            active={openPath === note.path}
            title={note.path}
            onClick={() => void navigate({ to: '/document', search: { path: note.path } })}
          >
            {note.path}
          </Row>
        ))}
        {notesQuery.isError && <ErrorRow message="Notes failed to load" />}
      </div>

      <SectionHeader label="Archives" count={archives.length} />
      <div className="px-1">
        {archives.map((note) => (
          <Row
            key={note.path}
            active={openPath === note.path}
            title={note.path}
            onClick={() => void navigate({ to: '/document', search: { path: note.path } })}
          >
            <span className="text-muted">{note.path}</span>
          </Row>
        ))}
      </div>

      <SectionHeader label="Tags" />
      <div className="px-1 pb-3">
        {tagsQuery.isLoading && <LoadingRow />}
        {(tagsQuery.data ?? []).map((tag) => (
          <Row
            key={tag.name}
            title={`${tag.documents} documents, ${tag.mentions} mentions`}
            onClick={() => void navigate({ to: '/tag/$tag', params: { tag: tag.name } })}
          >
            <span className="inline-flex w-full items-center justify-between gap-2">
              <span className="truncate text-accent">#{tag.name}</span>
              <span className="font-mono text-[10px] text-faint">{tag.documents}</span>
            </span>
          </Row>
        ))}
        {tagsQuery.isError && <ErrorRow message="Tags failed to load" />}
      </div>
    </nav>
  )
}

function LoadingRow() {
  return <p className="px-2 py-0.5 text-[11px] text-faint">loading…</p>
}

function ErrorRow({ message }: { message: string }) {
  return <p className="px-2 py-0.5 text-[11px] text-danger">{message}</p>
}
