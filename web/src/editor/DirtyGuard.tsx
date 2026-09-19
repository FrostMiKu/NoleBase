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
 * Any navigation that would abandon the dirty document is blocked —
 * internal navigations, browser Back/Forward, address-bar changes alike.
 * TanStack Router's blocker is the single enforcement point; app buttons
 * never bypass it. Rendered once in the app shell.
 */
export function DirtyGuard() {
  const { dirty, path } = useEditorState()

  const blocker = useBlocker({
    withResolver: true,
    shouldBlockFn: ({ current, next }) => {
      if (consumeNavigationPermission()) return false
      const state = editorStore.getState()
      if (!state.dirty || state.path === null) return false
      return isLeavingDocument(state.path, current, next)
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
          // Save failed: stay on the document; the toast explains why.
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
