import { useNavigate } from '@tanstack/react-router'

import { PanelLeft, PanelRight, Plus, Search } from 'lucide-react'
import { useMediaQuery } from '@/hooks/use-media-query'

import { useUiStore } from '@/stores/ui-store'

/**
 * Slim top bar: the NólëBase home link (back to the workspace overview)
 * and the palette shortcut hint. The open document is identified in the
 * editor toolbar, not here.
 */
export function TopBar() {
  const setCommandPaletteOpen = useUiStore((state) => state.setCommandPaletteOpen)
  const navigate = useNavigate()
  const compact = useMediaQuery('(max-width: 1023px)')
  const ui = useUiStore()

  return (
    <header className="flex h-11 shrink-0 items-center gap-3 border-b border-border bg-panel px-3">
      <button
        type="button"
        aria-label="Toggle files"
        title="Toggle files"
        className="toolbar-button"
        aria-expanded={compact ? ui.mobilePanel === 'files' : ui.leftSidebarOpen}
        onClick={() =>
          compact
            ? ui.setMobilePanel(ui.mobilePanel === 'files' ? null : 'files')
            : ui.toggleLeftSidebar()
        }
      >
        <PanelLeft size={16} />
      </button>
      <button
        type="button"
        onClick={() => void navigate({ to: '/' })}
        className="rounded-sm text-[12px] font-semibold tracking-wide text-fg transition-colors hover:text-muted"
      >
        NólëBase
      </button>
      <div className="ml-auto flex items-center gap-1">
        <button
          type="button"
          className="toolbar-button"
          aria-label="New note"
          title="New note"
          onClick={() => ui.setCreateNoteOpen(true)}
        >
          <Plus size={16} />
        </button>
        <button
          type="button"
          aria-label="Open command palette"
          title="Commands (Ctrl/⌘ K)"
          className="toolbar-button gap-2"
          onClick={() => setCommandPaletteOpen(true)}
        >
          <Search size={15} />
          <span className="hidden font-mono text-[10px] sm:inline">⌘K</span>
        </button>
        <button
          type="button"
          className="toolbar-button"
          aria-label="Toggle tasks and agent"
          title="Toggle tasks and agent"
          aria-expanded={compact ? ui.mobilePanel === 'tools' : ui.rightSidebarOpen}
          onClick={() =>
            compact
              ? ui.setMobilePanel(ui.mobilePanel === 'tools' ? null : 'tools')
              : ui.toggleRightSidebar()
          }
        >
          <PanelRight size={16} />
        </button>
      </div>
    </header>
  )
}
