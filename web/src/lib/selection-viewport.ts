import { useState } from 'react'

/** Selection moves inside the window; the window follows only at its edge. */
export function selection_viewport_start(selection: number, offset: number, size: number): number {
  if (selection < offset) return selection
  if (selection >= offset + size) return selection - size + 1
  return offset
}

export function useSelectionViewport(count: number, size = 8) {
  const [selection, setSelection] = useState(0)
  const [offset, setOffset] = useState(0)
  const selected = Math.min(selection, Math.max(0, count - 1))
  const start = selection_viewport_start(
    selected,
    Math.min(offset, Math.max(0, count - size)),
    size,
  )
  if (start !== offset) setOffset(start)
  if (selected !== selection) setSelection(selected)
  return {
    selection: selected,
    offset: start,
    select: setSelection,
    reset: () => {
      setSelection(0)
      setOffset(0)
    },
    move: (delta: number) =>
      setSelection((index) => Math.max(0, Math.min(count - 1, index + delta))),
  }
}
