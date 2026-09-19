import { useNavigate } from '@tanstack/react-router'

import { useTagDocuments } from '@/queries/workspace'

/** The /tag/$tag route: every document carrying the tag. */
export function TagPage({ tag }: { tag: string }) {
  const documentsQuery = useTagDocuments(tag)
  const navigate = useNavigate()
  const documents = documentsQuery.data ?? []

  return (
    <div className="flex h-full flex-col overflow-y-auto p-4">
      <h1 className="mb-1 text-[15px] font-semibold text-fg">
        <span className="text-accent">#{tag}</span>
      </h1>
      <p className="mb-3 font-mono text-[11px] text-faint">{documents.length} document(s)</p>

      {documentsQuery.isLoading && <p className="text-xs text-faint">Loading…</p>}
      {documentsQuery.isError && (
        <p className="text-xs text-danger">
          Failed to load tag documents:{' '}
          {documentsQuery.error instanceof Error ? documentsQuery.error.message : 'unknown error'}
        </p>
      )}

      <ul className="space-y-0.5">
        {documents.map((document) => (
          <li key={document.path}>
            <button
              type="button"
              className="w-full rounded-sm px-2 py-1 text-left transition-colors hover:bg-raised"
              onClick={() =>
                void navigate({ to: '/document', search: { path: document.path } })
              }
            >
              <span className="font-mono text-[12px] text-fg">{document.path}</span>
              {document.modified !== null && (
                <span className="ml-3 font-mono text-[10px] text-faint">
                  {new Date(document.modified).toLocaleDateString()}
                </span>
              )}
            </button>
          </li>
        ))}
      </ul>
    </div>
  )
}
