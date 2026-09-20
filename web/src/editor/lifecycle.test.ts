import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { editorStore } from './editor-state'
import { cancelEditsForLifecycleDeletion, flushBeforeLifecycleMutation } from './lifecycle'
import { persistDocument } from './save'

const bridge = vi.hoisted(() => ({ source: null as string | null }))
vi.mock('@/editor/editor-bridge', () => ({
  attachEditorView: vi.fn(),
  detachEditorView: vi.fn(),
  currentEditorSource: () => bridge.source,
  replaceEditorSource: vi.fn(),
}))

vi.mock('./save', () => ({
  saveCurrentDocument: vi.fn(async () => true),
  saveDocumentTo: vi.fn(async () => true),
  persistDocument: vi.fn(async () => true),
}))

const persist = vi.mocked(persistDocument)

beforeEach(() => {
  editorStore.close()
  bridge.source = null
  persist.mockReset()
  persist.mockResolvedValue(true)
})

afterEach(() => {
  editorStore.close()
})

describe('flushBeforeLifecycleMutation', () => {
  it('persists dirty content directly and keeps the block armed', async () => {
    bridge.source = 'live text'
    editorStore.loadDocument('data/foo.md', 'base')
    editorStore.setLiveDirty(true)

    await expect(flushBeforeLifecycleMutation('data/foo.md')).resolves.toBe(true)
    expect(persist).toHaveBeenCalledWith('data/foo.md', 'live text')
    expect(editorStore.getState().autosaveBlock).toBe('data/foo.md')
  })

  it('skips the save when the document is clean', async () => {
    editorStore.loadDocument('data/foo.md', 'base')
    await expect(flushBeforeLifecycleMutation('data/foo.md')).resolves.toBe(true)
    expect(persist).not.toHaveBeenCalled()
    expect(editorStore.getState().autosaveBlock).toBe('data/foo.md')
  })

  it('waits for an in-flight save before flushing', async () => {
    bridge.source = 'live text'
    editorStore.loadDocument('data/foo.md', 'base')
    editorStore.setLiveDirty(true)
    editorStore.beginSave()

    let settled = false
    const flushing = flushBeforeLifecycleMutation('data/foo.md').then((result) => {
      settled = true
      return result
    })
    // The in-flight save owns the document; no second save is issued yet.
    await Promise.resolve()
    expect(settled).toBe(false)
    expect(persist).not.toHaveBeenCalled()

    editorStore.saveSucceeded('live text')
    await expect(flushing).resolves.toBe(true)
  })

  it('releases the block when the save fails', async () => {
    persist.mockResolvedValue(false)
    bridge.source = 'live text'
    editorStore.loadDocument('data/foo.md', 'base')
    editorStore.setLiveDirty(true)

    await expect(flushBeforeLifecycleMutation('data/foo.md')).resolves.toBe(false)
    expect(editorStore.getState().autosaveBlock).toBeNull()
    expect(editorStore.getState().dirty).toBe(true)
  })

  it('releases the block when there is no editor text at all', async () => {
    // currentEditorSource() is null: nothing mounted, nothing to flush.
    editorStore.loadDocument('data/foo.md', 'base')
    editorStore.setLiveDirty(true)

    await expect(flushBeforeLifecycleMutation('data/foo.md')).resolves.toBe(false)
    expect(editorStore.getState().autosaveBlock).toBeNull()
  })
})

describe('cancelEditsForLifecycleDeletion', () => {
  it('discards pending edits and blocks the path from saving', () => {
    bridge.source = 'live text'
    editorStore.loadDocument('data/foo.md', 'base')
    editorStore.setLiveDirty(true)

    cancelEditsForLifecycleDeletion('data/foo.md')

    expect(editorStore.getState().dirty).toBe(false)
    expect(editorStore.getState().autosaveBlock).toBe('data/foo.md')
  })
})

describe('the autosave block lifecycle', () => {
  it('releases itself when the editor retargets to another path', () => {
    editorStore.loadDocument('data/foo.md', 'base')
    editorStore.blockAutosave('data/foo.md')
    expect(editorStore.getState().autosaveBlock).toBe('data/foo.md')

    editorStore.loadDocument('data/bar.md', 'base')
    expect(editorStore.getState().autosaveBlock).toBeNull()
  })

  it('survives a same-path reload and releases explicitly on demand', () => {
    editorStore.loadDocument('data/foo.md', 'base')
    editorStore.blockAutosave('data/foo.md')

    editorStore.loadDocument('data/foo.md', 'reloaded')
    expect(editorStore.getState().autosaveBlock).toBe('data/foo.md')

    editorStore.releaseAutosaveBlock('data/foo.md')
    expect(editorStore.getState().autosaveBlock).toBeNull()
  })

  it('releases when the editor surface closes', () => {
    editorStore.loadDocument('data/foo.md', 'base')
    editorStore.blockAutosave('data/foo.md')
    editorStore.close()
    expect(editorStore.getState().autosaveBlock).toBeNull()
  })
})
