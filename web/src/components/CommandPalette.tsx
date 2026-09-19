import { Command } from 'cmdk'
import { useNavigate } from '@tanstack/react-router'

import { useUiStore } from '@/stores/ui-store'
import { saveCurrentDocument } from '@/editor/save'

interface PaletteCommand {
  id: string
  label: string
  shortcut?: string
  keywords?: string
  run: () => void
}

/** Fixed command list — deliberately not a plugin registry. */
function useCommands(): PaletteCommand[] {
  const navigate = useNavigate()
  const ui = useUiStore.getState
  return [
    {
      id: 'open-daily',
      label: 'Open Daily',
      run: () => void navigate({ to: '/daily' }),
    },
    {
      id: 'search-workspace',
      label: 'Search Workspace',
      run: () => void navigate({ to: '/search' }),
    },
    {
      id: 'focus-agent',
      label: 'Focus Agent',
      run: () => {
        void navigate({ to: '/agent' })
        ui().requestAgentFocus()
      },
    },
    {
      id: 'save-document',
      label: 'Save Document',
      shortcut: '⌘S',
      run: () => void saveCurrentDocument(),
    },
    {
      id: 'toggle-left-sidebar',
      label: 'Toggle Left Sidebar',
      run: () => ui().toggleLeftSidebar(),
    },
    {
      id: 'toggle-right-sidebar',
      label: 'Toggle Right Sidebar',
      run: () => ui().toggleRightSidebar(),
    },
  ]
}

/** Cmd/Ctrl+K palette (cmdk). */
export function CommandPalette() {
  const open = useUiStore((state) => state.commandPaletteOpen)
  const setOpen = useUiStore((state) => state.setCommandPaletteOpen)
  const commands = useCommands()

  return (
    <Command.Dialog
      open={open}
      onOpenChange={setOpen}
      label="Command palette"
      className="fixed top-[15vh] left-1/2 z-60 w-[min(480px,90vw)] -translate-x-1/2 overflow-hidden rounded-md border border-border-strong bg-overlay shadow-lg"
      shouldFilter
    >
      <Command.Input
        autoFocus
        placeholder="Type a command…"
        className="w-full border-b border-border bg-transparent px-3 py-2 text-[13px] text-fg outline-none placeholder:text-faint"
      />
      <Command.List className="max-h-72 overflow-y-auto p-1">
        <Command.Empty className="px-2 py-3 text-center text-xs text-faint">
          No matching command
        </Command.Empty>
        {commands.map((command) => (
          <Command.Item
            key={command.id}
            value={`${command.label} ${command.keywords ?? ''}`}
            onSelect={() => {
              setOpen(false)
              command.run()
            }}
            className="flex cursor-pointer items-center justify-between rounded-sm px-2.5 py-1.5 text-xs text-fg data-[selected=true]:bg-accent/20"
          >
            <span>{command.label}</span>
            {command.shortcut !== undefined && (
              <span className="font-mono text-[10px] text-faint">{command.shortcut}</span>
            )}
          </Command.Item>
        ))}
      </Command.List>
    </Command.Dialog>
  )
}
