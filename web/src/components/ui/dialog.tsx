import * as DialogPrimitive from '@radix-ui/react-dialog'
import { X } from 'lucide-react'
import type { ComponentProps } from 'react'

/** Dense dark Radix Dialog wrappers shared by all modals. */

export function Dialog(props: ComponentProps<typeof DialogPrimitive.Root>) {
  return <DialogPrimitive.Root {...props} />
}

export function DialogPortal(props: ComponentProps<typeof DialogPrimitive.Portal>) {
  return <DialogPrimitive.Portal {...props} />
}

export function DialogOverlay({ className = '', ...props }: ComponentProps<typeof DialogPrimitive.Overlay>) {
  return (
    <DialogPrimitive.Overlay
      className={`fixed inset-0 z-50 bg-black/55 ${className}`}
      {...props}
    />
  )
}

export function DialogContent({ className = '', children, ...props }: ComponentProps<typeof DialogPrimitive.Content>) {
  return (
    <DialogPrimitive.Content
      className={`fixed top-1/2 left-1/2 z-50 w-[min(440px,90vw)] -translate-x-1/2 -translate-y-1/2
        rounded-md border border-border bg-overlay p-4 shadow-lg outline-none
        data-[state=open]:animate-in ${className}`}
      {...props}
    >
      {children}
      <DialogPrimitive.Close
        className="absolute top-2.5 right-2.5 rounded p-1 text-muted transition-colors hover:bg-raised hover:text-fg"
        aria-label="Close"
      >
        <X size={13} />
      </DialogPrimitive.Close>
    </DialogPrimitive.Content>
  )
}

export function DialogTitle({ className = '', ...props }: ComponentProps<typeof DialogPrimitive.Title>) {
  return (
    <DialogPrimitive.Title className={`text-[13px] font-semibold text-fg ${className}`} {...props} />
  )
}

export function DialogDescription({
  className = '',
  ...props
}: ComponentProps<typeof DialogPrimitive.Description>) {
  return (
    <DialogPrimitive.Description className={`mt-1 text-xs text-muted ${className}`} {...props} />
  )
}

/** Standard compact button styles reused across dialogs and toolbars. */
export const buttonStyles = {
  primary:
    'inline-flex h-7 items-center justify-center rounded-sm border border-accent/60 bg-accent/20 px-2.5 text-xs font-medium text-accent-strong transition-colors hover:bg-accent/30 disabled:cursor-not-allowed disabled:opacity-50',
  secondary:
    'inline-flex h-7 items-center justify-center rounded-sm border border-border-strong bg-raised px-2.5 text-xs text-fg transition-colors hover:bg-overlay disabled:cursor-not-allowed disabled:opacity-50',
  danger:
    'inline-flex h-7 items-center justify-center rounded-sm border border-danger/50 bg-danger/15 px-2.5 text-xs text-danger transition-colors hover:bg-danger/25 disabled:cursor-not-allowed disabled:opacity-50',
  ghost:
    'inline-flex h-7 items-center justify-center rounded-sm px-2 text-xs text-muted transition-colors hover:bg-raised hover:text-fg disabled:cursor-not-allowed disabled:opacity-50',
}
