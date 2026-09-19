import { useEffect, useRef, useState } from 'react'
import { useNavigate } from '@tanstack/react-router'
import { Search as SearchIcon } from 'lucide-react'

import { useDebouncedValue } from '@/hooks/use-debounced-value'
import { useSearchResults } from '@/queries/workspace'
import type { SearchHit } from '@/types/workspace'

interface SearchPageProps {
  /** Router-authoritative query (may be undefined). */
  q: string | undefined
}

/**
 * The /search route. The input is local state; the debounced value (250ms)
 * is written back to the router search param, and only the router value
 * drives the request — no keypress fires an API call directly.
 */
export function SearchPage({ q }: SearchPageProps) {
  const navigate = useNavigate()
  const [input, setInput] = useState(q ?? '')
  const debounced = useDebouncedValue(input, 250)
  const lastPushed = useRef(q ?? '')

  useEffect(() => {
    if (debounced === lastPushed.current) return
    lastPushed.current = debounced
    void navigate({
      to: '/search',
      search: { q: debounced.trim() === '' ? undefined : debounced },
      replace: true,
    })
  }, [debounced, navigate])

  // Router -> input when navigation comes from elsewhere (back/forward).
  useEffect(() => {
    if ((q ?? '') !== lastPushed.current) {
      lastPushed.current = q ?? ''
      setInput(q ?? '')
    }
  }, [q])

  const query = q?.trim() ?? ''
  const results = useSearchResults(query)

  return (
    <div className="flex h-full flex-col overflow-y-auto p-4">
      <h1 className="mb-3 text-[15px] font-semibold text-fg">Search</h1>
      <div className="mb-4 flex max-w-xl items-center gap-2 rounded-sm border border-border-strong bg-panel px-2.5 py-1.5 focus-within:border-accent">
        <SearchIcon size={13} className="text-faint" />
        <input
          autoFocus
          type="text"
          value={input}
          placeholder="Search note contents…"
          onChange={(event) => setInput(event.target.value)}
          className="w-full bg-transparent text-[13px] text-fg outline-none placeholder:text-faint"
        />
      </div>

      {query === '' && <p className="text-xs text-faint">Type to search the workspace.</p>}
      {query !== '' && results.isLoading && (
        <p className="text-xs text-faint">Searching…</p>
      )}
      {results.isError && (
        <p className="text-xs text-danger">
          Search failed:{' '}
          {results.error instanceof Error ? results.error.message : 'unknown error'}
        </p>
      )}
      {results.data !== undefined && (
        <p className="mb-2 font-mono text-[11px] text-faint">
          {results.data.hits.length} hit(s) for “{results.data.query}”
        </p>
      )}

      <HitList hits={results.data?.hits ?? []} />
    </div>
  )
}

function HitList({ hits }: { hits: SearchHit[] }) {
  const navigate = useNavigate()
  if (hits.length === 0) return null
  return (
    <ul className="max-w-3xl space-y-1">
      {hits.map((hit, index) => (
        <li key={`${hit.path ?? 'doc'}:${hit.line_no}:${index}`}>
          <button
            type="button"
            disabled={hit.path === null}
            className="w-full rounded-sm border border-transparent px-2 py-1 text-left transition-colors enabled:hover:border-border enabled:hover:bg-raised disabled:opacity-60"
            onClick={() => {
              if (hit.path !== null) {
                void navigate({ to: '/document', search: { path: hit.path } })
              }
            }}
          >
            {hit.path !== null && (
              <span className="mr-2 font-mono text-[11px] text-accent">{hit.path}</span>
            )}
            <span className="font-mono text-[10px] text-faint">L{hit.line_no}</span>
            <span className="ml-2 text-[12px] text-fg">{hit.text}</span>
          </button>
        </li>
      ))}
    </ul>
  )
}
