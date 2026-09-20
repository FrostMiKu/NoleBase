import * as ContextMenuPrimitive from '@radix-ui/react-context-menu'
import type { ComponentProps } from 'react'

/** Dense dark Radix ContextMenu wrappers — the sidebar's right-click file
 * actions. Radix owns open/close (outside click, Esc), arrow-key
 * navigation, and focus management; right-click never reaches the wrapped
 * row's click handler. */

export function ContextMenu(props: ComponentProps<typeof ContextMenuPrimitive.Root>) {
  return <ContextMenuPrimitive.Root {...props} />
}

export function ContextMenuTrigger(props: ComponentProps<typeof ContextMenuPrimitive.Trigger>) {
  return <ContextMenuPrimitive.Trigger {...props} />
}

export function ContextMenuContent({
  className = '',
  ...props
}: ComponentProps<typeof ContextMenuPrimitive.Content>) {
  return (
    <ContextMenuPrimitive.Portal>
      <ContextMenuPrimitive.Content
        className={`z-60 min-w-[150px] overflow-hidden rounded-md border border-border-strong bg-overlay p-1 shadow-lg outline-none ${className}`}
        {...props}
      />
    </ContextMenuPrimitive.Portal>
  )
}

interface ContextMenuItemProps extends ComponentProps<typeof ContextMenuPrimitive.Item> {
  /** Destructive actions (Delete) render in the danger colour. */
  destructive?: boolean
}

export function ContextMenuItem({ className = '', destructive, ...props }: ContextMenuItemProps) {
  return (
    <ContextMenuPrimitive.Item
      className={`flex cursor-pointer items-center rounded-sm px-2 py-1 text-[11px] outline-none select-none transition-colors ${
        destructive
          ? 'text-danger data-[highlighted]:bg-danger/20'
          : 'text-fg data-[highlighted]:bg-accent/20 data-[highlighted]:text-accent-strong'
      } ${className}`}
      {...props}
    />
  )
}
