import { useEffect, useRef, useState } from 'react'
import { useNavigate } from '@tanstack/react-router'
import { Search as SearchIcon } from 'lucide-react'

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
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined)

  useEffect(() => {
    setInput(q ?? '')
    clearTimeout(timer.current)
    return () => clearTimeout(timer.current)
  }, [q])

  const changeQuery = (value: string) => {
    setInput(value)
    clearTimeout(timer.current)
    timer.current = setTimeout(() => {
      void navigate({ to: '/search', search: { q: value.trim() || undefined }, replace: true })
    }, 250)
  }

  const query = q?.trim() ?? ''
  const results = useSearchResults(query)

  return (
    <div className="flex h-full flex-col overflow-y-auto p-4">
      <header className="mb-3 flex items-center gap-3">
        <h1 className="text-[15px] font-semibold text-fg">Search</h1>
        <div className="flex min-w-0 flex-1 items-center gap-2 rounded-sm border border-border-strong bg-panel px-2.5 py-1.5 focus-within:border-accent">
          <SearchIcon size={13} className="text-faint" />
          <input
            aria-label="Search workspace"
            autoFocus
            type="text"
            value={input}
            placeholder="Search note contents…"
            onChange={(event) => changeQuery(event.target.value)}
            className="w-full bg-transparent text-[13px] text-fg outline-none placeholder:text-faint"
          />
        </div>
      </header>

      {query === '' && <p className="text-xs text-faint">Type to search the workspace.</p>}
      {query !== '' && results.isLoading && <p className="text-xs text-faint">Searching…</p>}
      {results.isError && (
        <p className="text-xs text-danger">
          Search failed: {results.error instanceof Error ? results.error.message : 'unknown error'}
        </p>
      )}
      {results.data !== undefined && (
        <p className="mb-2 font-mono text-[11px] text-faint">
          {results.data.hits.length} hit(s) for “{results.data.query}”
        </p>
      )}

      {results.isSuccess && results.data.hits.length === 0 && query !== '' && (
        <p className="py-2 text-sm text-muted">No matches. Try another word or a shorter phrase.</p>
      )}
      <HitList hits={results.data?.hits ?? []} />
    </div>
  )
}

function HitList({ hits }: { hits: SearchHit[] }) {
  const navigate = useNavigate()
  if (hits.length === 0) return null
  return (
    <ul className="space-y-1">
      {hits.map((hit, index) => (
        <li key={`${hit.path ?? 'doc'}:${hit.line_no}:${index}`}>
          <button
            type="button"
            disabled={hit.path === null}
            className="w-full rounded-sm border border-transparent px-2 py-1 text-left [overflow-wrap:anywhere] transition-colors enabled:hover:border-border enabled:hover:bg-raised disabled:opacity-60"
            onClick={() => {
              if (hit.path !== null) {
                void navigate({ to: '/document', search: { path: hit.path, line: hit.line_no } })
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
