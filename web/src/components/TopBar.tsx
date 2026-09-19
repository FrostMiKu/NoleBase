import { useUiStore } from '@/stores/ui-store'
import { useEditorState } from '@/editor/editor-state'

/**
 * Slim top bar: app identity, the current document (URL-authoritative),
 * and the palette shortcut hint.
 */
export function TopBar() {
  const setCommandPaletteOpen = useUiStore((state) => state.setCommandPaletteOpen)
  const { dirty, path } = useEditorState()

  return (
    <header className="flex h-8 shrink-0 items-center gap-3 border-b border-border bg-panel px-3">
      <span className="text-[12px] font-semibold tracking-wide text-fg">Nole</span>
      <span className="font-mono text-[11px] text-muted">
        {path !== null ? (
          <>
            {path}
            {dirty && <span className="ml-1.5 text-warn">●</span>}
          </>
        ) : (
          'workspace'
        )}
      </span>
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
