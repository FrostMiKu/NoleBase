import { QueryClient } from '@tanstack/react-query'

/**
 * The application-wide QueryClient, created once. GET queries retry a
 * bounded number of times; mutations never auto-retry; window focus never
 * replaces the workspace WebSocket as the freshness signal.
 */
export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      retry: 2,
      refetchOnWindowFocus: false,
      staleTime: 5_000,
    },
    mutations: {
      retry: 0,
    },
  },
})
