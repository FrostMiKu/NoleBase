import { act, cleanup, render } from '@testing-library/react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { useAutoSave } from './use-autosave'
import { editorStore } from './editor-state'
import { saveCurrentDocument } from './save'
vi.mock('./save', () => ({ saveCurrentDocument: vi.fn(async () => true) }))
function Probe() {
  useAutoSave()
  return null
}
beforeEach(() => {
  vi.useFakeTimers()
  vi.clearAllMocks()
  editorStore.loadDocument('data/a.md', '')
})
afterEach(() => {
  cleanup()
  editorStore.close()
  vi.useRealTimers()
})
it('debounces every edit, even when the document was already dirty', () => {
  render(<Probe />)
  act(() => {
    editorStore.setLiveDirty(true)
    vi.advanceTimersByTime(1000)
    editorStore.setLiveDirty(true)
    vi.advanceTimersByTime(1000)
  })
  expect(saveCurrentDocument).not.toHaveBeenCalled()
  act(() => vi.advanceTimersByTime(500))
  expect(saveCurrentDocument).toHaveBeenCalledTimes(1)
})
it('cancels the armed save when a conflict arrives', () => {
  render(<Probe />)
  act(() => {
    editorStore.setLiveDirty(true)
    vi.advanceTimersByTime(1000)
    editorStore.markExternalChange()
    vi.advanceTimersByTime(2000)
  })
  expect(saveCurrentDocument).not.toHaveBeenCalled()
})
it('does not retry a failed save forever without another edit', () => {
  render(<Probe />)
  act(() => {
    editorStore.setLiveDirty(true)
    editorStore.beginSave()
    editorStore.saveFailed()
    vi.advanceTimersByTime(10000)
  })
  expect(saveCurrentDocument).not.toHaveBeenCalled()
})
