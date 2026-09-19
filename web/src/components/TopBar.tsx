import { useNavigate } from '@tanstack/react-router'

import { useUiStore } from '@/stores/ui-store'

/**
 * Slim top bar: the NólëBase home link (back to the workspace overview)
 * and the palette shortcut hint. The open document is identified in the
 * editor toolbar, not here.
 */
export function TopBar() {
  const setCommandPaletteOpen = useUiStore((state) => state.setCommandPaletteOpen)
  const navigate = useNavigate()

  return (
    <header className="flex h-8 shrink-0 items-center gap-3 border-b border-border bg-panel px-3">
      <button
        type="button"
        onClick={() => void navigate({ to: '/' })}
        className="rounded-sm text-[12px] font-semibold tracking-wide text-fg transition-colors hover:text-muted"
      >
        NólëBase
      </button>
      <span className="text-[11px] text-faint">workspace</span>
      <button
        type="button"
        className="ml-auto rounded-sm border border-border px-1.5 py-0.5 font-mono text-[10px] text-faint transition-colors hover:border-border-strong hover:text-muted"
        onClick={() => setCommandPaletteOpen(true)}
      >
        ⌘K
      </button>
    </header>
  )
}
