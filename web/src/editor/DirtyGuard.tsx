import { useEffect } from 'react'
import { useBlocker } from '@tanstack/react-router'

import { editorStore, useEditorState } from '@/editor/editor-state'
import { consumeNavigationPermission } from '@/editor/navigation-guard'
import { saveCurrentDocument } from '@/editor/save'
import { DirtyGuardDialog } from '@/editor/DirtyGuardDialog'

function documentPathOf(pathname: string, search: unknown): string | null {
  if (pathname !== '/document') return null
  if (typeof search !== 'object' || search === null || !('path' in search)) return null
  const path = (search as { path: unknown }).path
  return typeof path === 'string' ? path : null
}

function isLeavingDocument(
  openPath: string,
  current: { pathname: string },
  next: { pathname: string; search: unknown },
): boolean {
  if (current.pathname !== '/document') return false
  if (next.pathname !== '/document') return true
  return documentPathOf(next.pathname, next.search) !== openPath
}

/**
 * Autosave makes navigation safe: leaving a dirty document first flushes
 * the pending edit, and only a FAILED save blocks the navigation and
 * raises the dialog (retry via Save and Continue, or Discard). Covers
 * internal navigations, browser Back/Forward, and address-bar changes
 * alike. Rendered once in the app shell.
 */
export function DirtyGuard() {
  const { dirty, path } = useEditorState()

  const blocker = useBlocker({
    withResolver: true,
    // Our dirty-only handler below owns tab close/reload. The router's
    // default would warn even on a clean document or the home page.
    enableBeforeUnload: false,
    shouldBlockFn: async ({ current, next }) => {
      if (consumeNavigationPermission()) return false
      const state = editorStore.getState()
      if (!state.dirty || state.path === null) return false
      if (!isLeavingDocument(state.path, current, next)) return false
      // Autosave-on-navigate: a successful flush lets the navigation
      // through; only a failure blocks and asks.
      const saved = await saveCurrentDocument()
      return !saved
    },
  })

  // A dirty document must never vanish on tab close or reload without the
  // browser's own confirmation.
  useEffect(() => {
    if (!dirty) return
    const handler = (event: BeforeUnloadEvent) => {
      event.preventDefault()
      event.returnValue = ''
    }
    window.addEventListener('beforeunload', handler)
    return () => window.removeEventListener('beforeunload', handler)
  }, [dirty])

  if (blocker.status !== 'blocked' || !dirty || path === null) return null

  const targetPath = documentPathOf(blocker.next.pathname, blocker.next.search)

  return (
    <DirtyGuardDialog
      documentPath={path}
      targetPath={targetPath}
      onSaveAndContinue={async () => {
        const saved = await saveCurrentDocument()
        if (saved) {
          blocker.proceed()
        } else {
          // Save failed again: stay on the document; the toast explains.
          blocker.reset()
        }
      }}
      onDiscard={() => {
        editorStore.discard()
        blocker.proceed()
      }}
      onCancel={() => blocker.reset()}
    />
  )
}
