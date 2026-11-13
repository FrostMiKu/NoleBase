import { ChevronRight } from 'lucide-react'
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from '@/components/ui/collapsible'
import { ContextMenu, ContextMenuContent, ContextMenuTrigger } from '@/components/ui/context-menu'
import { useUiStore, type SidebarSectionKey } from '@/stores/ui-store'
interface SectionProps {
  sectionKey: SidebarSectionKey
  label: string
  count?: number
  /** Header action rendered beside the count (e.g. the Notes `+`). */
  action?: React.ReactNode
  children: React.ReactNode
}

/**
 * A collapsible sidebar section, mirroring the TUI's section behaviour:
 * the header toggles the list, the expanded state survives navigation.
 */
export function Section({ sectionKey, label, count, action, children }: SectionProps) {
  const expanded = useUiStore((state) => state.sidebarSections[sectionKey])
  const toggle = useUiStore((state) => state.toggleSidebarSection)
  return (
    <Collapsible open={expanded} onOpenChange={() => toggle(sectionKey)} data-section={sectionKey}>
      <div className="flex items-center gap-1 px-2 pt-3 pb-1">
        <CollapsibleTrigger className="group flex min-w-0 flex-1 items-center gap-1 text-[10px] font-semibold tracking-wider text-faint uppercase transition-colors hover:text-muted">
          <ChevronRight
            size={10}
            className={`transition-transform group-data-[state=open]:rotate-90`}
          />
          <span>{label}</span>
        </CollapsibleTrigger>
        {action}
        {count !== undefined && <span className="font-mono text-[10px]">{count}</span>}
      </div>
      <CollapsibleContent>
        <div className="px-1 pb-1">{children}</div>
      </CollapsibleContent>
    </Collapsible>
  )
}

interface RowProps extends React.ComponentPropsWithoutRef<'button'> {
  active?: boolean
  children: React.ReactNode
}

export function Row({ active = false, onClick, children, ...rest }: RowProps) {
  return (
    <button
      type="button"
      onClick={onClick}
      {...rest}
      aria-current={active ? 'page' : undefined}
      className={`selection-row flex w-full items-center rounded-sm px-2 py-[3px] text-left text-[12px] transition-colors ${
        active ? 'bg-accent/20 text-accent-strong' : 'text-fg hover:bg-raised'
      }`}
    >
      <span className="min-w-0 flex-1 truncate">{children}</span>
    </button>
  )
}

/** A row wrapped in its right-click file actions. */
export function ContextRow({
  active,
  title,
  onOpen,
  items,
  children,
}: {
  active?: boolean
  title?: string
  onOpen: () => void
  items: React.ReactNode
  children: React.ReactNode
}) {
  return (
    <ContextMenu>
      <ContextMenuTrigger asChild>
        <Row active={active} onClick={onOpen} title={title}>
          {children}
        </Row>
      </ContextMenuTrigger>
      <ContextMenuContent>{items}</ContextMenuContent>
    </ContextMenu>
  )
}

