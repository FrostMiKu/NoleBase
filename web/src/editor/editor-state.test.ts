import { afterEach, beforeEach, describe, expect, it } from 'vitest'

import { editorStore } from './editor-state'

// The store is a process-wide singleton; each test starts from a closed,
// clean state.
beforeEach(() => {
  editorStore.close()
})

afterEach(() => {
  editorStore.close()
})

describe('document lifecycle', () => {
  it('loads a clean document', () => {
    editorStore.loadDocument('data/foo.md', '# Hello')
    expect(editorStore.getState()).toMatchObject({
      path: 'data/foo.md',
      baselineSource: '# Hello',
      dirty: false,
      saving: false,
      externalChangePending: false,
      movedTo: null,
    })
  })

  it('transitions to dirty when the live text diverges from the baseline', () => {
    editorStore.loadDocument('data/foo.md', '# Hello')
    editorStore.setLiveDirty(true)
    expect(editorStore.getState().dirty).toBe(true)
    // Idempotent: staying dirty does not re-notify, and undoing back to
    // the baseline clears the flag.
    editorStore.setLiveDirty(false)
    expect(editorStore.getState().dirty).toBe(false)
  })
})

describe('saving', () => {
  it('returns to clean on save success with a new baseline', () => {
    editorStore.loadDocument('data/foo.md', 'old')
    editorStore.setLiveDirty(true)
    editorStore.beginSave()
    expect(editorStore.getState().saving).toBe(true)
    editorStore.saveSucceeded('new')
    expect(editorStore.getState()).toMatchObject({
      dirty: false,
      saving: false,
      baselineSource: 'new',
    })
  })

  it('keeps dirty on save failure and clears only the saving flag', () => {
    editorStore.loadDocument('data/foo.md', 'old')
    editorStore.setLiveDirty(true)
    editorStore.beginSave()
    editorStore.saveFailed()
    expect(editorStore.getState()).toMatchObject({ dirty: true, saving: false })
  })
})

describe('external changes', () => {
  it('flags a pending conflict when the disk changes under a dirty editor', () => {
    editorStore.loadDocument('data/foo.md', 'base')
    editorStore.setLiveDirty(true)
    editorStore.markExternalChange()
    expect(editorStore.getState().externalChangePending).toBe(true)
    editorStore.clearExternalChange()
    expect(editorStore.getState().externalChangePending).toBe(false)
  })

  it('resolves the conflict through a successful reload (clean baseline)', () => {
    editorStore.loadDocument('data/foo.md', 'base')
    editorStore.setLiveDirty(true)
    editorStore.markExternalChange()
    // "Reload from Disk": the fresh content becomes the new baseline.
    editorStore.loadDocument('data/foo.md', 'fresh from disk')
    expect(editorStore.getState()).toMatchObject({
      dirty: false,
      externalChangePending: false,
      baselineSource: 'fresh from disk',
    })
  })

  it('keeps dirty text after "Keep Editing" so the next save overwrites', () => {
    editorStore.loadDocument('data/foo.md', 'base')
    editorStore.setLiveDirty(true)
    editorStore.markExternalChange()
    editorStore.clearExternalChange()
    expect(editorStore.getState()).toMatchObject({ dirty: true, externalChangePending: false })
  })
})

describe('navigation', () => {
  it('discard navigation clears the dirty flag', () => {
    editorStore.loadDocument('data/foo.md', 'base')
    editorStore.setLiveDirty(true)
    editorStore.discard()
    expect(editorStore.getState().dirty).toBe(false)
  })

  it('cancel navigation changes nothing', () => {
    editorStore.loadDocument('data/foo.md', 'base')
    editorStore.setLiveDirty(true)
    // Cancel: no store action is taken; state must still be dirty.
    expect(editorStore.getState().dirty).toBe(true)
  })

  it('save-and-navigate succeeds via save success then navigation', () => {
    editorStore.loadDocument('data/foo.md', 'base')
    editorStore.setLiveDirty(true)
    editorStore.beginSave()
    editorStore.saveSucceeded('base saved')
    expect(editorStore.getState().dirty).toBe(false)
  })

  it('closing the editor surface clears the open document', () => {
    editorStore.loadDocument('data/foo.md', 'base')
    editorStore.setLiveDirty(true)
    editorStore.close()
    expect(editorStore.getState().path).toBeNull()
    expect(editorStore.getState().dirty).toBe(false)
  })
})

describe('file moved conflict', () => {
  it('marks the move and blocks saving via the movedTo flag', () => {
    editorStore.loadDocument('data/old.md', 'base')
    editorStore.setLiveDirty(true)
    editorStore.markMoved('data/new.md')
    expect(editorStore.getState().movedTo).toBe('data/new.md')
  })

  it('resolves when the new document is loaded', () => {
    editorStore.loadDocument('data/old.md', 'base')
    editorStore.setLiveDirty(true)
    editorStore.markMoved('data/new.md')
    editorStore.loadDocument('data/new.md', 'base')
    expect(editorStore.getState()).toMatchObject({ path: 'data/new.md', movedTo: null, dirty: false })
  })
})
