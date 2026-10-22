import * as PopoverPrimitive from '@radix-ui/react-popover'
import type { ComponentProps } from 'react'

/** Dense dark Radix Popover wrappers. */

export function Popover(props: ComponentProps<typeof PopoverPrimitive.Root>) {
  return <PopoverPrimitive.Root {...props} />
}

export function PopoverTrigger(props: ComponentProps<typeof PopoverPrimitive.Trigger>) {
  return <PopoverPrimitive.Trigger {...props} />
}

export function PopoverContent({
  className = '',
  sideOffset = 4,
  compact = false,
  ...props
}: ComponentProps<typeof PopoverPrimitive.Content> & { compact?: boolean }) {
  return (
    <PopoverPrimitive.Portal>
      <PopoverPrimitive.Content
        sideOffset={sideOffset}
        className={`z-60 ${compact ? 'w-52' : 'w-72'} max-w-[calc(100vw-1rem)] rounded-md border border-border-strong bg-overlay p-2 shadow-lg outline-none ${className}`}
        {...props}
      />
    </PopoverPrimitive.Portal>
  )
}
