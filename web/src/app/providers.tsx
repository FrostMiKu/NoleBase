import { QueryClientProvider } from '@tanstack/react-query'
import { RouterProvider } from '@tanstack/react-router'
import { Toaster } from 'sonner'

import { queryClient } from '@/queries/query-client'
import { router } from '@/router'

/** Application providers: TanStack Query, the router, and toasts. */
export function Providers() {
  return (
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
      <Toaster theme="dark" position="bottom-right" duration={5000} />
    </QueryClientProvider>
  )
}
