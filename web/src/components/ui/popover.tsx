import * as PopoverPrimitive from '@radix-ui/react-popover'
import type { ComponentProps } from 'react'

/** Dense dark Radix Popover wrappers. */

export function Popover(props: ComponentProps<typeof PopoverPrimitive.Root>) {
  return <PopoverPrimitive.Root {...props} />
}

export function PopoverTrigger(props: ComponentProps<typeof PopoverPrimitive.Trigger>) {
  return <PopoverPrimitive.Trigger {...props} />
}

export function PopoverContent({ className = '', sideOffset = 4, ...props }: ComponentProps<typeof PopoverPrimitive.Content>) {
  return (
    <PopoverPrimitive.Portal>
      <PopoverPrimitive.Content
        sideOffset={sideOffset}
        className={`z-60 w-72 rounded-md border border-border-strong bg-overlay p-2 shadow-lg outline-none ${className}`}
        {...props}
      />
    </PopoverPrimitive.Portal>
  )
}
