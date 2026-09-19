/**
 * WebSocket URLs are derived from the browser origin: http→ws, https→wss.
 * No backend host is ever hardcoded in frontend code.
 */
export function wsUrl(path: string): string {
  const protocol = window.location.protocol === 'https:' ? 'wss' : 'ws'
  return `${protocol}://${window.location.host}${path}`
}

/** Fixed reconnect backoff: 1s, 2s, 5s, then 5s forever. */
export function reconnectDelayMs(attempt: number): number {
  if (attempt <= 0) return 1_000
  if (attempt === 1) return 2_000
  return 5_000
}
