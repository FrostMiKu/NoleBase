import { useNavigate } from '@tanstack/react-router'
import { ChevronRight } from 'lucide-react'

import { Collapsible, CollapsibleContent, CollapsibleTrigger } from '@/components/ui/collapsible'
import { useEditorState } from '@/editor/editor-state'
import { useDailyNotes, useNotes, useTags } from '@/queries/workspace'
import { useUiStore, type SidebarSectionKey } from '@/stores/ui-store'

interface SectionProps {
  sectionKey: SidebarSectionKey
  label: string
  count?: number
  children: React.ReactNode
}

/**
 * A collapsible sidebar section, mirroring the TUI's section behaviour:
 * the header toggles the list, the expanded state survives navigation.
 */
function Section({ sectionKey, label, count, children }: SectionProps) {
  const expanded = useUiStore((state) => state.sidebarSections[sectionKey])
  const toggle = useUiStore((state) => state.toggleSidebarSection)
  return (
    <Collapsible
      open={expanded}
      onOpenChange={() => toggle(sectionKey)}
      data-section={sectionKey}
    >
      <CollapsibleTrigger className="group flex w-full items-center gap-1 px-2 pt-3 pb-1 text-[10px] font-semibold tracking-wider text-faint uppercase transition-colors hover:text-muted">
        <ChevronRight
          size={10}
          className={`transition-transform group-data-[state=open]:rotate-90`}
        />
        <span>{label}</span>
        {count !== undefined && <span className="ml-auto font-mono text-[10px]">{count}</span>}
      </CollapsibleTrigger>
      <CollapsibleContent>
        <div className="px-1 pb-1">{children}</div>
      </CollapsibleContent>
    </Collapsible>
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
 * Left navigation: Daily, Notes, Archives, Tags — each a collapsible flat
 * list. Notes and Archives split the same endpoint by `archived`.
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
    <nav className="flex h-full flex-col overflow-y-auto pb-2" data-testid="left-sidebar">
      <Section sectionKey="daily" label="Daily" count={daily.length}>
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
      </Section>

      <Section sectionKey="notes" label="Notes" count={active.length}>
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
      </Section>

      <Section sectionKey="archives" label="Archives" count={archives.length}>
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
      </Section>

      <Section sectionKey="tags" label="Tags">
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
      </Section>

      <div className="px-2 pt-3">
        <button
          type="button"
          onClick={() => void navigate({ to: '/search' })}
          className="inline-flex items-center gap-1.5 text-[10px] font-semibold tracking-wider text-faint uppercase transition-colors hover:text-muted"
        >
          Search workspace…
        </button>
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
