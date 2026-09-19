import { useEffect } from 'react'

import { useUiStore } from '@/stores/ui-store'
import { saveCurrentDocument } from '@/editor/save'
import { editorStore } from '@/editor/editor-state'

/**
 * Global shortcuts: Cmd/Ctrl+K toggles the command palette, Cmd/Ctrl+S
 * saves the open document (also bound inside CodeMirror for editor focus).
 */
export function useKeyboardShortcuts(): void {
  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      const mod = event.metaKey || event.ctrlKey
      if (!mod) return
      const key = event.key.toLowerCase()
      if (key === 'k') {
        event.preventDefault()
        const { commandPaletteOpen, setCommandPaletteOpen } = useUiStore.getState()
        setCommandPaletteOpen(!commandPaletteOpen)
        return
      }
      if (key === 's') {
        if (editorStore.getState().path === null) return
        event.preventDefault()
        void saveCurrentDocument()
      }
    }
    window.addEventListener('keydown', handler)
    return () => window.removeEventListener('keydown', handler)
  }, [])
}
