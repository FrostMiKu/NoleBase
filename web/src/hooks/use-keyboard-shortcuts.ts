import { useEffect } from 'react'

import { useUiStore } from '@/stores/ui-store'
import { saveCurrentDocument } from '@/editor/save'
import { editorStore } from '@/editor/editor-state'

/** An overlay owns Escape (palette, dialogs, popovers, CM search). */
function escapeIsClaimedByOverlay(): boolean {
  return (
    document.querySelector('[role="dialog"], [cmdk-root], .cm-panels, [data-radix-popper-content-wrapper]') !==
    null
  )
}

/**
 * Global shortcuts: Cmd/Ctrl+K toggles the command palette, Cmd/Ctrl+S
 * flushes the open document, and Escape on the document route goes back
 * to the page the editor was entered from (autosave keeps it safe).
 */
export function useKeyboardShortcuts(): void {
  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      const mod = event.metaKey || event.ctrlKey
      const key = event.key.toLowerCase()
      if (mod && key === 'k') {
        event.preventDefault()
        const { commandPaletteOpen, setCommandPaletteOpen } = useUiStore.getState()
        setCommandPaletteOpen(!commandPaletteOpen)
        return
      }
      if (mod && key === 's') {
        if (editorStore.getState().path === null) return
        event.preventDefault()
        void saveCurrentDocument()
        return
      }
      if (event.key === 'Escape' && !mod && window.location.pathname === '/document') {
        if (escapeIsClaimedByOverlay()) return
        if (editorStore.getState().path === null) return
        event.preventDefault()
        window.history.back()
      }
    }
    window.addEventListener('keydown', handler)
    return () => window.removeEventListener('keydown', handler)
  }, [])
}
