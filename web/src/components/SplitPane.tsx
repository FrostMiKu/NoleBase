import { Group, Panel, Separator, type GroupProps, type PanelProps } from 'react-resizable-panels'

/**
 * Thin wrappers over react-resizable-panels (v4 `Group`/`Panel`/`Separator`)
 * giving every split surface the same dense look. Layout percentages are
 * persisted by the caller via `onResize`.
 */

export function SplitGroup(props: GroupProps) {
  return <Group className="h-full w-full" {...props} />
}

export function SplitPanel(props: PanelProps) {
  return <Panel className="min-w-0 overflow-hidden" {...props} />
}

export function SplitSeparator({ vertical = false }: { vertical?: boolean }) {
  return (
    <Separator
      className={
        vertical
          ? 'h-px w-full bg-border transition-colors hover:bg-border-strong'
          : 'w-px bg-border transition-colors hover:bg-border-strong'
      }
    />
  )
}

export type { PanelSize } from 'react-resizable-panels'
