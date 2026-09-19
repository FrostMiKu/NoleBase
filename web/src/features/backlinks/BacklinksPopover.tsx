import { Link2 } from 'lucide-react'
import { useNavigate } from '@tanstack/react-router'

import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover'
import { useBacklinks } from '@/queries/workspace'

/**
 * Backlinks for the current document, shown in a popover from the editor
 * toolbar (never a permanent column).
 */
export function BacklinksPopover({ path }: { path: string }) {
  const backlinksQuery = useBacklinks(path)
  const navigate = useNavigate()
  const backlinks = backlinksQuery.data?.backlinks ?? []

  /** Bare note name: strip the directory and the extension. */
  const displayName = (source: string) =>
    source.split('/').pop()?.replace(/\.[^.]+$/, '') ?? source

  return (
    <Popover>
      <PopoverTrigger
        className="inline-flex h-6 items-center gap-1 rounded-sm border border-border-strong bg-raised px-2 text-[11px] text-fg transition-colors hover:bg-overlay disabled:cursor-not-allowed disabled:opacity-50"
        disabled={backlinksQuery.isLoading}
        title="Backlinks"
      >
        <Link2 size={11} />
        Backlinks{backlinks.length > 0 ? ` (${backlinks.length})` : ''}
      </PopoverTrigger>
      <PopoverContent align="end">
        <p className="mb-1.5 text-[10px] font-semibold tracking-wide text-faint uppercase">
          Linked from
        </p>
        {backlinksQuery.isError && (
          <p className="px-1 py-2 text-[11px] text-danger">
            {backlinksQuery.error instanceof Error ? backlinksQuery.error.message : 'Failed to load'}
          </p>
        )}
        {!backlinksQuery.isError && backlinks.length === 0 && (
          <p className="px-1 py-2 text-[11px] text-faint">No documents link here yet.</p>
        )}
        <ul className="max-h-64 space-y-0.5 overflow-y-auto">
          {backlinks.map((source) => (
            <li key={source}>
              <button
                type="button"
                className="w-full truncate rounded-sm px-1.5 py-1 text-left text-[11px] text-fg transition-colors hover:bg-accent/20"
                title={source}
                onClick={() => {
                  void navigate({ to: '/document', search: { path: source } })
                }}
              >
                {displayName(source)}
              </button>
            </li>
          ))}
        </ul>
      </PopoverContent>
    </Popover>
  )
}
