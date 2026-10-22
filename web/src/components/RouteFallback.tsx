import { Link, type ErrorComponentProps } from '@tanstack/react-router'

export function RouteNotFound() {
  return (
    <div className="flex h-full flex-col items-center justify-center gap-3 p-6 text-center">
      <h1 className="text-lg font-semibold">Page not found</h1>
      <p className="text-sm text-muted">This address does not point to a workspace page.</p>
      <Link to="/" className="text-sm text-accent underline">
        Back to workspace
      </Link>
    </div>
  )
}

export function RouteError({ error, reset }: ErrorComponentProps) {
  return (
    <div
      role="alert"
      className="flex h-full flex-col items-center justify-center gap-3 p-6 text-center"
    >
      <h1 className="text-lg font-semibold">This page could not be opened</h1>
      <p className="max-w-lg text-sm break-words text-muted">
        {error instanceof Error ? error.message : 'An unexpected error occurred.'}
      </p>
      <button type="button" onClick={reset} className="text-sm text-accent underline">
        Try again
      </button>
      <Link to="/" className="text-sm text-accent underline">
        Back to workspace
      </Link>
    </div>
  )
}
