import { useSyncExternalStore } from 'react'

const listeners = new Map<string, Set<() => void>>()
const memory = new Map<string, string>()
const storageKey = (key: string) => `nole:draft:${key}`

function readDraft(key: string): string {
  try {
    return sessionStorage.getItem(storageKey(key)) ?? ''
  } catch {
    return memory.get(key) ?? ''
  }
}

function writeDraft(key: string, value: string): void {
  memory.set(key, value)
  try {
    if (value === '') sessionStorage.removeItem(storageKey(key))
    else sessionStorage.setItem(storageKey(key), value)
  } catch {
    /* Blocked storage must not prevent typing. */
  }
  listeners.get(key)?.forEach((notify) => notify())
}

/** A late submission must never erase a newer draft created after navigation. */
export function clearSubmittedDraft(key: string, submitted: string): void {
  if (readDraft(key) === submitted) writeDraft(key, '')
}

/** One tab-local draft shared by every composer surface, including remounts. */
export function useDraft(key: string): [string, (value: string) => void] {
  const value = useSyncExternalStore(
    (notify) => {
      const subscriptions = listeners.get(key) ?? new Set<() => void>()
      listeners.set(key, subscriptions)
      subscriptions.add(notify)
      return () => {
        subscriptions.delete(notify)
      }
    },
    () => readDraft(key),
    () => '',
  )
  return [value, (next) => writeDraft(key, next)]
}
