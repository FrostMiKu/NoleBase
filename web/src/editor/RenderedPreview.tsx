import { useQuery } from '@tanstack/react-query'
import { workspaceKeys } from '@/queries/keys'
import { workspaceApi } from '@/api/workspace'
import { useDebouncedValue } from '@/hooks/use-debounced-value'

/** The export renderer supplies its own CSP. An opaque sandbox origin
 * permits its pinned diagram/math runtimes without exposing the app. */
export function RenderedPreview({ path, source }: { path: string; source: string }) {
  const snapshot = useDebouncedValue(source, 500)
  const preview = useQuery({
    queryKey: workspaceKeys.preview(path, snapshot),
    queryFn: () => workspaceApi.previewDocument(path, snapshot),
    retry: false,
    gcTime: 0,
    staleTime: Infinity,
  })
  const updating = preview.isFetching || snapshot !== source
  return (
    <section
      className="relative flex h-full min-h-0 min-w-0 flex-1 flex-col"
      aria-label="Full document preview"
    >
      {updating && (
        <span
          role="status"
          className="pointer-events-none absolute right-3 top-3 rounded bg-panel/90 px-2 py-1 text-xs text-muted"
        >
          Rendering…
        </span>
      )}
      {preview.isError && (
        <div role="alert" className="p-4 text-sm text-danger">
          Preview failed: {preview.error.message}{' '}
          <button type="button" className="underline" onClick={() => void preview.refetch()}>
            Retry
          </button>
        </div>
      )}
      {preview.data && (
        <>
          {preview.data.warnings.length > 0 && (
            <details className="shrink-0 border-b border-border px-3 py-2 text-xs text-warn">
              <summary>{preview.data.warnings.length} image warning(s)</summary>
              <ul className="max-h-24 overflow-y-auto">
                {preview.data.warnings.map((warning, index) => (
                  <li key={index}>{warning}</li>
                ))}
              </ul>
            </details>
          )}
          <iframe
            title="Rendered document"
            sandbox="allow-scripts"
            referrerPolicy="no-referrer"
            srcDoc={preview.data.html}
            className="min-h-0 w-full flex-1 border-0 bg-white"
          />
        </>
      )}
    </section>
  )
}
