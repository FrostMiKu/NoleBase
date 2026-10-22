import { beforeEach, afterEach, describe, expect, it, vi } from 'vitest'
import { editorStore } from './editor-state'
import { persistDocument, saveCurrentDocument } from './save'
import { workspaceApi } from '@/api/workspace'
const bridge = vi.hoisted(() => ({ source: '' }))
vi.mock('./editor-bridge', () => ({ currentEditorSource: () => bridge.source }))
vi.mock('@/api/workspace', () => ({ workspaceApi: { writeDocument: vi.fn() } }))
vi.mock('@/queries/query-client', () => ({
  queryClient: { setQueryData: vi.fn(), invalidateQueries: vi.fn() },
}))
vi.mock('sonner', () => ({ toast: { error: vi.fn() } }))
beforeEach(() => {
  editorStore.close()
  vi.clearAllMocks()
  editorStore.loadDocument('data/a.md', 'base')
  bridge.source = 'first'
  editorStore.setLiveDirty(true)
})
afterEach(() => editorStore.close())
function pendingWrite() {
  let resolve!: () => void
  vi.mocked(workspaceApi.writeDocument).mockImplementationOnce(
    () =>
      new Promise<null>((done) => {
        resolve = () => done(null)
      }),
  )
  return () => resolve()
}
describe('saving edits safely', () => {
  it('keeps typing during a save dirty, and saves it on the next request', async () => {
    const done = pendingWrite()
    const saving = saveCurrentDocument()
    bridge.source = 'second'
    editorStore.setLiveDirty(true)
    done()
    expect(await saving).toBe(false)
    expect(editorStore.getState()).toMatchObject({
      dirty: true,
      baselineSource: 'first',
      saving: false,
    })
    vi.mocked(workspaceApi.writeDocument).mockResolvedValue(null)
    expect(await saveCurrentDocument()).toBe(true)
    expect(workspaceApi.writeDocument).toHaveBeenLastCalledWith('data/a.md', 'second')
  })
  it('an old request never marks a newly opened document saved', async () => {
    const done = pendingWrite()
    const saving = persistDocument('data/a.md', 'first')
    editorStore.loadDocument('data/b.md', 'another')
    bridge.source = 'another edit'
    editorStore.setLiveDirty(true)
    done()
    await saving
    expect(editorStore.getState()).toMatchObject({
      path: 'data/b.md',
      baselineSource: 'another',
      dirty: true,
    })
  })
  it('refuses to overwrite an unresolved external change', async () => {
    editorStore.markExternalChange()
    expect(await saveCurrentDocument()).toBe(false)
    expect(workspaceApi.writeDocument).not.toHaveBeenCalled()
  })
  it('waits for an in-flight save before a navigation flush', async () => {
    const done = pendingWrite()
    const first = saveCurrentDocument()
    const navigation = saveCurrentDocument()
    done()
    expect(await first).toBe(true)
    expect(await navigation).toBe(true)
    expect(workspaceApi.writeDocument).toHaveBeenCalledTimes(1)
  })
})
