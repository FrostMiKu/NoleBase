import { useState } from 'react'
import { useNavigate } from '@tanstack/react-router'
import { ChevronRight } from 'lucide-react'

import { Collapsible, CollapsibleContent, CollapsibleTrigger } from '@/components/ui/collapsible'
import { useTagDocuments, useTags } from '@/queries/workspace'

/**
 * The `/tags` route: every tag as a collapsible group with its documents
 * (fetched when the group opens) — the tag-centred file list.
 */
export function TagsPage() {
  const tagsQuery = useTags()
  const tags = tagsQuery.data ?? []

  return (
    <div className="flex h-full flex-col overflow-y-auto p-4">
      <h1 className="mb-1 text-[15px] font-semibold text-fg">Tags</h1>
      <p className="mb-3 font-mono text-[11px] text-faint">{`${tags.length} tag(s)`}</p>

      {tagsQuery.isLoading && <p className="text-xs text-faint">Loading tags…</p>}
      {tagsQuery.isError && (
        <p className="text-xs text-danger">
          Failed to load tags:{' '}
          {tagsQuery.error instanceof Error ? tagsQuery.error.message : 'unknown error'}
        </p>
      )}

      <div>
        {tags.map((tag) => (
          <TagGroup key={tag.name} name={tag.name} documents={tag.documents} mentions={tag.mentions} />
        ))}
      </div>
    </div>
  )
}

function TagGroup({
  name,
  documents,
  mentions,
}: {
  name: string
  documents: number
  mentions: number
}) {
  const [expanded, setExpanded] = useState(false)
  const documentsQuery = useTagDocuments(expanded ? name : '')

  return (
    <Collapsible open={expanded} onOpenChange={setExpanded} className="mb-1" data-tag={name}>
      <CollapsibleTrigger className="group flex w-full items-center gap-1.5 rounded-sm border border-border bg-panel px-2.5 py-1.5 text-left transition-colors hover:border-border-strong">
        <ChevronRight
          size={11}
          className="text-faint transition-transform group-data-[state=open]:rotate-90"
        />
        <span className="text-[13px] text-accent">#{name}</span>
        <span className="ml-auto font-mono text-[10px] text-faint">
          {documents} doc(s) · {mentions} mention(s)
        </span>
      </CollapsibleTrigger>
      <CollapsibleContent>
        <div className="border border-t-0 border-border px-2 py-1">
          {documentsQuery.isLoading && <p className="px-1 py-1 text-[11px] text-faint">loading…</p>}
          {documentsQuery.isError && (
            <p className="px-1 py-1 text-[11px] text-danger">
              {documentsQuery.error instanceof Error
                ? documentsQuery.error.message
                : 'failed to load documents'}
            </p>
          )}
          {(documentsQuery.data ?? []).map((document) => (
            <TagDocumentRow key={document.path} path={document.path} modified={document.modified} />
          ))}
          {documentsQuery.data !== undefined && documentsQuery.data.length === 0 && (
            <p className="px-1 py-1 text-[11px] text-faint">No documents carry this tag.</p>
          )}
        </div>
      </CollapsibleContent>
    </Collapsible>
  )
}

function TagDocumentRow({ path, modified }: { path: string; modified: string | null }) {
  const navigate = useNavigate()
  return (
    <button
      type="button"
      className="flex w-full items-center gap-3 rounded-sm px-2 py-1 text-left transition-colors hover:bg-raised"
      onClick={() => void navigate({ to: '/document', search: { path } })}
    >
      <span className="truncate font-mono text-[12px] text-fg">{path}</span>
      {modified !== null && (
        <span className="ml-auto shrink-0 font-mono text-[10px] text-faint">
          {new Date(modified).toLocaleDateString()}
        </span>
      )}
    </button>
  )
}
