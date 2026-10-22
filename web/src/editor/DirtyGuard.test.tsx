import { act, cleanup, render } from '@testing-library/react'
import { afterEach, expect, it, vi } from 'vitest'
import { DirtyGuard } from './DirtyGuard'
import { editorStore } from './editor-state'
const block = vi.hoisted(() => vi.fn(() => ({ status: 'idle' })))
vi.mock('@tanstack/react-router', () => ({ useBlocker: block }))
afterEach(() => {
  cleanup()
  editorStore.close()
  vi.clearAllMocks()
})
it('protects reload only while the document has unsaved edits', () => {
  editorStore.loadDocument('data/a.md', 'base')
  render(<DirtyGuard />)
  expect(block).toHaveBeenCalledWith(expect.objectContaining({ enableBeforeUnload: false }))
  const clean = new Event('beforeunload', { cancelable: true })
  window.dispatchEvent(clean)
  expect(clean.defaultPrevented).toBe(false)
  act(() => editorStore.setLiveDirty(true))
  const dirty = new Event('beforeunload', { cancelable: true })
  window.dispatchEvent(dirty)
  expect(dirty.defaultPrevented).toBe(true)
  act(() => editorStore.saveSucceeded('saved'))
  const saved = new Event('beforeunload', { cancelable: true })
  window.dispatchEvent(saved)
  expect(saved.defaultPrevented).toBe(false)
})
